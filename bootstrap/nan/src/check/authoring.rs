//! What the language server offers while code is written: the names that may follow a position,
//! the head of the function a call being written calls, the types of bindings written without
//! one, and the class of each name of a file.

use std::collections::{BTreeMap, HashMap};

use super::conform::type_key;
use super::db::{ItemId, ModuleId, TypeKey};
use super::navigation::{Named, each_expr, file_checker};
use super::references::Target;
use super::resolve::Named as Resolved;
use super::tir::{TBlock, TBody, TKind, TPatternKind, TStmtKind};
use super::ty::Scalar;
use super::{Checker, Member, decl};
use crate::format::bindings::bindings;
use crate::syntax::ast::{self, ItemKind, Visibility};
use crate::syntax::closing::close;
use crate::syntax::grammar::{Keyword, Punct};
use crate::syntax::token::TokenKind;
use crate::workspace::Workspace;

/// What a completion offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offered {
    Local,
    Field,
    Variant,
    Function,
    Method,
    Module,
    Struct,
    Enum,
    Interface,
    Const,
    Keyword,
}

/// A name that may follow a position: its text, what it names, and a line describing it.
pub struct Completion {
    pub label: String,
    pub offered: Offered,
    pub detail: String,
}

/// The head of the function a call being written calls, its parameters as written in it, and
/// the parameter the argument being written takes.
pub struct Signature {
    pub label: String,
    pub parameters: Vec<String>,
    pub active: usize,
}

/// The class of a name, for highlighting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Namespace,
    Type,
    Struct,
    Enum,
    Interface,
    Parameter,
    Variable,
    Property,
    EnumMember,
    Function,
    Method,
    Keyword,
    Comment,
    Text,
    Number,
}

/// A run of a file's bytes as highlighting classes it, with whether it declares a name and
/// whether that name names a const.
pub struct Classed {
    pub start: u32,
    pub end: u32,
    pub class: Class,
    pub declaration: bool,
    pub readonly: bool,
}

/// The names that may follow a byte of a workspace file, the identifier ending there written in
/// part: after `.`, the members of the type before it; after `::`, the items of the module the
/// path before it names; otherwise the locals bound before it, the items of its module, the
/// names starting a path, the core vocabulary and the keywords. Each once, by label, the folded
/// label starting with the folded part written.
pub fn completions(workspace: &Workspace, path: &str, offset: u32) -> Vec<Completion> {
    let Some(text) = workspace.bytes(path) else {
        return Vec::new();
    };
    let offset = (offset as usize).min(text.len());
    let start = text[..offset]
        .iter()
        .rposition(|byte| !(byte.is_ascii_alphanumeric() || *byte == b'_'))
        .map_or(0, |at| at + 1);
    let written = decl::fold(&String::from_utf8_lossy(&text[start..offset]));
    let changed = HashMap::from([(path.to_owned(), closed_source(text, start))]);
    let closed = workspace.with_sources(&changed);
    let Some(checker) = file_checker(&closed, path) else {
        return Vec::new();
    };
    let found = match checker.item_holding(path, start as u32) {
        None => keyword_completions(),
        Some(item) if text[..start].ends_with(b"::") => {
            checker.path_completions(item, &text[..start - 2])
        }
        Some(item) if text[..start].ends_with(b".") => {
            checker.member_completions(item, start as u32 - 1)
        }
        Some(item) => checker.name_completions(item, start as u32),
    };
    let mut by_label: BTreeMap<String, Completion> = BTreeMap::new();
    for completion in found {
        if decl::fold(&completion.label).starts_with(&written) {
            by_label
                .entry(completion.label.clone())
                .or_insert(completion);
        }
    }
    by_label.into_values().collect()
}

/// The head of the function the call written at a byte of a workspace file calls, when the byte
/// lies within that call's parentheses: its first line as consumers see it.
pub fn signature(workspace: &Workspace, path: &str, offset: u32) -> Option<Signature> {
    let (_, file) = workspace.file(path)?;
    let syntax = &file.syntax;
    let mut depth = 0;
    let mut commas = 0;
    let mut open = None;
    for token in (0..syntax.tokens.len()).rev() {
        let at = &syntax.tokens[token];
        if at.end > offset || at.kind.is_virtual() {
            if at.kind == TokenKind::Col0 && at.start < offset {
                break;
            }
            continue;
        }
        match at.kind {
            TokenKind::Punct(Punct::RParen | Punct::RBracket) => depth += 1,
            TokenKind::Punct(Punct::LParen) if depth == 0 => {
                open = Some(token);
                break;
            }
            TokenKind::Punct(Punct::LParen | Punct::LBracket) => depth -= 1,
            TokenKind::Punct(Punct::LBrace | Punct::RBrace) => return None,
            TokenKind::Punct(Punct::Comma) if depth == 0 => commas += 1,
            _ => {}
        }
    }
    let callee = syntax.previous(open?)?;
    if syntax.tokens[callee].kind != TokenKind::Ident {
        return None;
    }
    let mut spaced = syntax.source.clone();
    spaced.insert(offset as usize, b' ');
    let changed = HashMap::from([(path.to_owned(), closed_source(&spaced, offset as usize + 1))]);
    let closed = workspace.with_sources(&changed);
    let checker = file_checker(&closed, path)?;
    let start = syntax.tokens[callee].start;
    let Some((Named::Target(Target::Item(function)), _)) = checker.named_at(path, start) else {
        return None;
    };
    let ItemKind::Fn(head) = &checker.db.ast(function).kind else {
        return None;
    };
    let label = checker.interface_text(function, None).into_iter().next()?;
    let parameters = parameters(&label);
    let receiver = syntax
        .previous(callee)
        .filter(|dot| syntax.tokens[*dot].kind == TokenKind::Punct(Punct::Dot))
        .and_then(|dot| syntax.previous(dot))
        .is_some_and(|value| {
            !syntax
                .text(value)
                .starts_with(|first: char| first.is_ascii_uppercase())
        });
    let implied =
        usize::from(receiver && head.head.params.first().is_some_and(ast::Param::is_self));
    let argument = labelled_argument(syntax, open?, offset);
    let active = argument
        .and_then(|name| {
            head.head
                .params
                .iter()
                .position(|param| param.name() == name)
        })
        .unwrap_or(commas + implied);
    Some(Signature {
        label,
        parameters,
        active,
    })
}

/// The label the argument written up to a byte gives itself, when it starts with one.
fn labelled_argument(
    syntax: &crate::syntax::file::SourceFile,
    open: usize,
    offset: u32,
) -> Option<&str> {
    let mut start = open;
    let mut depth = 0;
    for token in open + 1..syntax.tokens.len() {
        let at = &syntax.tokens[token];
        if at.start >= offset {
            break;
        }
        match at.kind {
            TokenKind::Punct(Punct::LParen | Punct::LBracket) => depth += 1,
            TokenKind::Punct(Punct::RParen | Punct::RBracket) => depth -= 1,
            TokenKind::Punct(Punct::Comma) if depth == 0 => start = token,
            _ => {}
        }
    }
    let name = syntax.next(start + 1);
    let colon = syntax.next(name + 1);
    let labelled = syntax.tokens.get(name)?.kind == TokenKind::Ident
        && syntax.tokens.get(colon)?.kind == TokenKind::Punct(Punct::Colon)
        && syntax.tokens[colon].end <= offset;
    labelled.then(|| syntax.text(name))
}

/// The parameters a head's text writes between its parentheses, separated at its top-level
/// commas.
fn parameters(head: &str) -> Vec<String> {
    let Some(open) = head.find('(') else {
        return Vec::new();
    };
    let mut parameters = Vec::new();
    let mut depth = 0;
    let mut start = open + 1;
    for (at, character) in head.char_indices().skip_while(|(at, _)| *at <= open) {
        match character {
            '(' | '[' => depth += 1,
            ']' => depth -= 1,
            ')' if depth == 0 => {
                parameters.push(head[start..at].trim().to_owned());
                break;
            }
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parameters.push(head[start..at].trim().to_owned());
                start = at + 1;
            }
            _ => {}
        }
    }
    parameters.retain(|parameter| !parameter.is_empty());
    parameters
}

/// The bindings of a workspace file's items overlapping bytes, `let`, `var` and `for` bindings
/// written without a type, each with the byte after its name and its type.
pub fn type_hints(
    workspace: &Workspace,
    path: &str,
    (start, end): (u32, u32),
) -> Vec<(u32, String)> {
    let Some(checker) = file_checker(workspace, path) else {
        return Vec::new();
    };
    let mut hints = Vec::new();
    for index in 0..checker.db.items.len() {
        let item = ItemId(index as u32);
        let data = checker.db.item(item);
        if data.file.path != path {
            continue;
        }
        let syntax = data.syntax();
        let lines = syntax.item_lines(&syntax.items[data.index]);
        let first = syntax.lines[*lines.start() as usize - 1].start;
        let last = syntax.lines[*lines.end() as usize - 1].end;
        if last < start || first > end {
            continue;
        }
        let Some(body) = checker.typed(item) else {
            continue;
        };
        for local in bound_untyped(&body) {
            let local = body.local(local);
            let token = &syntax.tokens[local.token as usize];
            let next = syntax.next(local.token as usize + 1);
            let typed = syntax.tokens.get(next).map(|after| after.kind)
                == Some(TokenKind::Punct(Punct::Colon));
            if !typed && (start..=end).contains(&token.end) {
                hints.push((token.end, checker.show(&local.ty, data.module)));
            }
        }
    }
    hints.sort();
    hints
}

/// The locals a typed body's `let` and `for` statements bind as their whole pattern, and its
/// `var` statements.
fn bound_untyped(body: &TBody) -> Vec<super::tir::LocalId> {
    let mut blocks: Vec<&TBlock> = Vec::new();
    each_expr(body, &mut |expr| match &expr.kind {
        TKind::Block(block) | TKind::If { then: block, .. } => blocks.push(block),
        _ => {}
    });
    let mut bound = Vec::new();
    while let Some(block) = blocks.pop() {
        for stmt in &block.stmts {
            match &stmt.kind {
                TStmtKind::Let {
                    pattern, otherwise, ..
                } => {
                    if let TPatternKind::Binding(local) = pattern.kind {
                        bound.push(local);
                    }
                    blocks.extend(otherwise);
                }
                TStmtKind::Var { local, .. } => bound.push(*local),
                TStmtKind::For { pattern, body, .. } => {
                    if let TPatternKind::Binding(local) = pattern.kind {
                        bound.push(local);
                    }
                    blocks.push(body);
                }
                TStmtKind::While { body, .. } | TStmtKind::Loop { body, .. } => blocks.push(body),
                _ => {}
            }
        }
    }
    bound
}

/// The names of a workspace file classed for highlighting, its keywords, literals and comments,
/// by position.
pub fn classes(workspace: &Workspace, path: &str) -> Vec<Classed> {
    let Some((_, file)) = workspace.file(path) else {
        return Vec::new();
    };
    let syntax = &file.syntax;
    let mut named: HashMap<u32, (Class, bool, bool)> = HashMap::new();
    if let Some(checker) = file_checker(workspace, path) {
        for index in 0..checker.db.items.len() {
            let item = ItemId(index as u32);
            if checker.db.item(item).file.path == path {
                checker.class_names(item, &mut named);
            }
        }
    }
    let mut classed = Vec::new();
    for (index, token) in syntax.tokens.iter().enumerate() {
        let class = match token.kind {
            TokenKind::Keyword(_) => (Class::Keyword, false, false),
            TokenKind::Int | TokenKind::Float => (Class::Number, false, false),
            TokenKind::Text
            | TokenKind::Bytes
            | TokenKind::Char
            | TokenKind::Byte
            | TokenKind::TextLine => (Class::Text, false, false),
            TokenKind::Doc | TokenKind::FileDoc => (Class::Comment, false, false),
            TokenKind::Ident => match named.get(&(index as u32)) {
                Some(class) => *class,
                None => continue,
            },
            _ => continue,
        };
        classed.push(Classed {
            start: token.start,
            end: token.end,
            class: class.0,
            declaration: class.1,
            readonly: class.2,
        });
    }
    for line in &syntax.lines {
        if let Some(comment) = line.comment {
            classed.push(Classed {
                start: comment,
                end: line.end,
                class: Class::Comment,
                declaration: false,
                readonly: false,
            });
        }
    }
    classed.sort_by_key(|classed| classed.start);
    classed
}

/// The keywords, offered where no item holds a position.
fn keyword_completions() -> Vec<Completion> {
    Keyword::ALL
        .iter()
        .map(|keyword| Completion {
            label: keyword.spelling().to_owned(),
            offered: Offered::Keyword,
            detail: String::new(),
        })
        .collect()
}

/// The source a file's completion reads up to a byte: the text before it closed, then the
/// file's lines from the first after the byte's line that starts at column 0 with neither `}`
/// nor a blank, which start the items after the one holding the byte.
fn closed_source(text: &[u8], end: usize) -> Vec<u8> {
    let mut source = close(&text[..end]).source;
    if !source.ends_with(b"\n") {
        source.push(b'\n');
    }
    let mut at = text[end..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(text.len(), |line_end| end + line_end + 1);
    while at < text.len() {
        if !matches!(text[at], b' ' | b'}' | b'\n' | b'\r' | b'\t') {
            source.push(b'\n');
            source.extend_from_slice(&text[at..]);
            break;
        }
        at = text[at..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(text.len(), |line_end| at + line_end + 1);
    }
    source
}

impl Checker<'_> {
    /// What an item names, offered as a completion.
    fn offered(&self, item: ItemId) -> Option<Completion> {
        let data = self.db.item(item);
        let offered = match &data.ast.kind {
            ItemKind::Fn(function) if function.label.owner.is_some() => Offered::Method,
            ItemKind::Fn(_) => Offered::Function,
            ItemKind::Struct(_) | ItemKind::Layout(_) | ItemKind::Registers(_) => Offered::Struct,
            ItemKind::Enum(_) => Offered::Enum,
            ItemKind::Interface(_) | ItemKind::Protocol(_) => Offered::Interface,
            ItemKind::Const(_) | ItemKind::Registry(_) | ItemKind::Contribute(_) => Offered::Const,
            _ => return None,
        };
        let name = data.ast.name()?;
        let label = name.rsplit('.').next().unwrap_or(&name).to_owned();
        Some(Completion {
            label,
            offered,
            detail: self.head_written(item),
        })
    }

    /// The first line of an item's head as written, without a trailing `{`.
    fn head_written(&self, item: ItemId) -> String {
        let data = self.db.item(item);
        let line = crate::output::printable(data.syntax().line_text(data.head_line()));
        line.trim().trim_end_matches('{').trim_end().to_owned()
    }

    /// The members the type before a `.` at a byte of an item offers: a type path's variants and
    /// the functions named after it; a value's fields its module sees and the functions named
    /// after its type that take `self`.
    fn member_completions(&self, item: ItemId, dot: u32) -> Vec<Completion> {
        let module = self.db.item(item).module;
        if let Some(owner) = self.type_before(item, dot) {
            let mut found = self.variants(owner);
            found.extend(self.functions_after(TypeKey::Item(owner), module, false));
            return found;
        }
        let Some(body) = self.typed(item) else {
            return Vec::new();
        };
        let syntax = self.db.item(item).syntax();
        let mut receiver: Option<&super::tir::TExpr> = None;
        each_expr(&body, &mut |expr| {
            let ends = syntax.tokens[expr.last as usize].end == dot;
            let shorter = receiver.is_none_or(|known| expr.token > known.token);
            if ends && shorter {
                receiver = Some(expr);
            }
        });
        let Some(key) = receiver.and_then(|receiver| type_key(&receiver.ty)) else {
            return Vec::new();
        };
        let mut found = match key {
            TypeKey::Item(owner) => self.fields(owner, module),
            _ => Vec::new(),
        };
        found.extend(self.functions_after(key, module, true));
        found
    }

    /// The struct or enum the path an item writes ending before a byte names.
    fn type_before(&self, item: ItemId, at: u32) -> Option<ItemId> {
        let holder = self.holder(item);
        let syntax = &holder.file.syntax;
        for (path, _) in holder.paths() {
            if syntax.tokens[path.last().token as usize].end != at {
                continue;
            }
            if let Ok(Resolved::Item(found)) = self.db.resolve(holder.module, &path)
                && matches!(
                    self.db.ast(found).kind,
                    ItemKind::Struct(_) | ItemKind::Enum(_)
                )
            {
                return Some(found);
            }
        }
        None
    }

    /// The variants of an enum.
    fn variants(&self, owner: ItemId) -> Vec<Completion> {
        let ItemKind::Enum(enumeration) = &self.db.ast(owner).kind else {
            return Vec::new();
        };
        let syntax = self.db.item(owner).syntax();
        enumeration
            .variants
            .iter()
            .map(|variant| Completion {
                label: variant.name.text.clone(),
                offered: Offered::Variant,
                detail: crate::output::printable(
                    syntax.line_text(syntax.tokens[variant.name.token as usize].line),
                )
                .trim()
                .to_owned(),
            })
            .collect()
    }

    /// The fields of a struct a module sees.
    fn fields(&self, owner: ItemId, module: ModuleId) -> Vec<Completion> {
        let ItemKind::Struct(structure) = &self.db.ast(owner).kind else {
            return Vec::new();
        };
        let syntax = self.db.item(owner).syntax();
        let own_module = self.db.module_of(owner) == module;
        let own_package = self.db.package_of(owner) == self.db.module(module).package;
        structure
            .fields()
            .filter(|field| match field.visibility {
                Visibility::Pub => true,
                Visibility::Pkg => own_package,
                Visibility::Private => own_module,
            })
            .map(|field| Completion {
                label: field.name.text.clone(),
                offered: Offered::Field,
                detail: crate::output::printable(
                    syntax.line_text(syntax.tokens[field.name.token as usize].line),
                )
                .trim()
                .to_owned(),
            })
            .collect()
    }

    /// The functions named after a type that a module sees, those taking `self` alone when
    /// `methods` holds.
    fn functions_after(&self, key: TypeKey, module: ModuleId, methods: bool) -> Vec<Completion> {
        let Some(members) = self.db.members.get(&key) else {
            return Vec::new();
        };
        members
            .values()
            .filter_map(|functions| {
                functions
                    .iter()
                    .copied()
                    .find(|function| self.db.visible(module, *function))
            })
            .filter(|function| {
                let ItemKind::Fn(head) = &self.db.ast(*function).kind else {
                    return false;
                };
                !methods || head.head.params.first().is_some_and(ast::Param::is_self)
            })
            .filter_map(|function| self.offered(function))
            .collect()
    }

    /// The items and modules of the module the path before a `::` names, as an item's module
    /// sees them.
    fn path_completions(&self, item: ItemId, before: &[u8]) -> Vec<Completion> {
        let module = self.db.item(item).module;
        let mut segments = Vec::new();
        let mut end = before.len();
        loop {
            let start = before[..end]
                .iter()
                .rposition(|byte| !(byte.is_ascii_alphanumeric() || *byte == b'_'))
                .map_or(0, |at| at + 1);
            if start == end {
                break;
            }
            segments.push(ast::Name {
                text: String::from_utf8_lossy(&before[start..end]).into_owned(),
                token: 0,
            });
            if !before[..start].ends_with(b"::") {
                break;
            }
            end = start - 2;
        }
        segments.reverse();
        let Ok(Resolved::Module(named)) = self.db.resolve(module, &ast::Path { segments }) else {
            return Vec::new();
        };
        let mut found = self.module_items(named, module);
        let package = self.db.module(named).package;
        if self.db.module(named).root {
            found.extend(self.modules_of(package));
        }
        found
    }

    /// The items a module declares that another module sees, each name once.
    fn module_items(&self, declaring: ModuleId, seeing: ModuleId) -> Vec<Completion> {
        self.db
            .module(declaring)
            .names
            .values()
            .filter_map(|items| {
                items
                    .iter()
                    .copied()
                    .find(|item| self.db.visible(seeing, *item))
            })
            .filter_map(|item| self.offered(item))
            .collect()
    }

    /// A package's modules below its root.
    fn modules_of(&self, package: super::db::PackageId) -> Vec<Completion> {
        self.db
            .package(package)
            .modules
            .iter()
            .filter(|module| !self.db.module(**module).root)
            .map(|module| Completion {
                label: self.db.module(*module).name.clone(),
                offered: Offered::Module,
                detail: self.db.module_path(*module),
            })
            .collect()
    }

    /// The names that may start an expression at a byte of an item: the locals bound before it,
    /// the items of its module, the aliases and modules a path may start with, the core
    /// vocabulary, the scalar types and the keywords.
    fn name_completions(&self, item: ItemId, at: u32) -> Vec<Completion> {
        let data = self.db.item(item);
        let module = data.module;
        let holder = self.holder(item);
        let syntax = &holder.file.syntax;
        let body = self.typed(item);
        let mut found = Vec::new();
        for first in bindings(syntax, holder.tree).of.values() {
            if syntax.tokens[*first as usize].end > at {
                continue;
            }
            let ty = body
                .as_ref()
                .and_then(|body| body.locals.iter().find(|local| local.token == *first))
                .map(|local| self.show(&local.ty, module))
                .unwrap_or_default();
            found.push(Completion {
                label: syntax.text(*first as usize).to_owned(),
                offered: Offered::Local,
                detail: ty,
            });
        }
        found.extend(self.module_items(module, module));
        let package = self.db.module(module).package;
        let mut roots: Vec<&str> = self
            .db
            .package(package)
            .deps
            .keys()
            .map(String::as_str)
            .collect();
        roots.extend(self.db.std.keys().copied());
        found.extend(roots.into_iter().map(|alias| Completion {
            label: alias.to_owned(),
            offered: Offered::Module,
            detail: String::new(),
        }));
        found.extend(self.modules_of(package));
        if let Some(core) = self.db.package(self.db.core()).root {
            found.extend(self.module_items(core, module));
        }
        found.extend(Scalar::ALL.iter().map(|scalar| Completion {
            label: scalar.name().to_owned(),
            offered: Offered::Struct,
            detail: String::new(),
        }));
        found.extend(keyword_completions());
        found
    }

    /// Classes the names an item writes, by token: its declared names and members, its labels,
    /// its bindings, the members its bodies name and the items and modules its paths name.
    fn class_names(&self, item: ItemId, named: &mut HashMap<u32, (Class, bool, bool)>) {
        let data = self.db.item(item);
        let holder = self.holder(item);
        let syntax = &holder.file.syntax;
        for (path, _) in holder.paths() {
            for at in 0..path.segments.len() {
                let written = ast::Path {
                    segments: path.segments[..=at].to_vec(),
                };
                let class = match self.db.resolve(holder.module, &written) {
                    Ok(Resolved::Module(_)) => Some((Class::Namespace, false)),
                    Ok(Resolved::Item(found)) => self.item_class(found),
                    Ok(Resolved::Variant(..)) => Some((Class::EnumMember, false)),
                    Ok(Resolved::Scalar(_) | Resolved::Unit | Resolved::Never | Resolved::Dyn) => {
                        Some((Class::Type, false))
                    }
                    Err(_) => None,
                };
                if let Some((class, readonly)) = class {
                    named.insert(path.segments[at].token, (class, false, readonly));
                }
            }
        }
        self.typed(item);
        for (holding, token, member) in self.members_named.borrow().iter() {
            if *holding != item {
                continue;
            }
            let class = match member {
                Member::Function(function) => match self.item_class(*function) {
                    Some((class, _)) => class,
                    None => continue,
                },
                Member::Method(..) => Class::Method,
                Member::Field(..) => Class::Property,
                Member::Variant(..) => Class::EnumMember,
            };
            named.insert(*token, (class, false, false));
        }
        let bound = bindings(syntax, holder.tree);
        for (token, first) in &bound.of {
            let class = match bound.parameters.contains(first) {
                true => Class::Parameter,
                false => Class::Variable,
            };
            named.insert(*token, (class, token == first, false));
        }
        let own = self.item_class(item);
        for label in holder.labels() {
            if let (Some(last), Some((class, readonly))) = (label.last(), own) {
                named.insert(*last, (class, false, readonly));
            }
        }
        if let Some((class, readonly)) = own
            && let Some(name) = decl::declared_name(data.ast)
        {
            named.insert(name.token, (class, true, readonly));
        }
        let members: Vec<(ast::Tok, Class)> = match &data.ast.kind {
            ItemKind::Struct(structure) => structure
                .fields()
                .map(|field| (field.name.token, Class::Property))
                .collect(),
            ItemKind::Enum(enumeration) => enumeration
                .variants
                .iter()
                .map(|variant| (variant.name.token, Class::EnumMember))
                .collect(),
            ItemKind::Interface(interface) => interface
                .methods
                .iter()
                .map(|method| (method.name.token, Class::Method))
                .collect(),
            _ => Vec::new(),
        };
        for (token, class) in members {
            named.insert(token, (class, true, false));
        }
    }

    /// The class of the names naming an item, with whether it names a const.
    fn item_class(&self, item: ItemId) -> Option<(Class, bool)> {
        Some(match &self.db.ast(item).kind {
            ItemKind::Fn(function) if function.label.owner.is_some() => (Class::Method, false),
            ItemKind::Fn(_) | ItemKind::Test(_) => (Class::Function, false),
            ItemKind::Struct(_) | ItemKind::Layout(_) | ItemKind::Registers(_) => {
                (Class::Struct, false)
            }
            ItemKind::Enum(_) => (Class::Enum, false),
            ItemKind::Interface(_) | ItemKind::Protocol(_) => (Class::Interface, false),
            ItemKind::Const(_) => (Class::Variable, true),
            ItemKind::Registry(_) | ItemKind::Contribute(_) => (Class::Variable, false),
            _ => return None,
        })
    }
}
