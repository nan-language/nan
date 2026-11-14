//! What a name written at a byte of a workspace file names, for the language server: its
//! declaration, the names writing it across the workspace, the text describing it, what
//! implements it, and the calls of a function.

use std::collections::{BTreeMap, BTreeSet};

use super::db::{ItemId, TypeKey};
use super::references::{Holder, Span, Target};
use super::resolve::Named as Resolved;
use super::sig::SigKind;
use super::tir::{Callee, LocalKind, TBody, TExpr, TKind};
use super::{Checker, Member, check_targets, checkers, decl, unchecked};
use crate::format::bindings::bindings;
use crate::syntax::ast::{self, ItemKind, Tok};
use crate::syntax::file::{ItemSyntax, SourceFile};
use crate::syntax::grammar::Keyword;
use crate::syntax::token::TokenKind;
use crate::workspace::Workspace;

/// What a name names: an item or member, or a local of an item by the token first binding it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Named {
    Target(Target),
    Local(ItemId, Tok),
}

/// A function as a call hierarchy shows it: its name, whether it is named after a type, and
/// the bytes of its item and of its name.
pub struct Function {
    pub name: String,
    pub member: bool,
    pub item: Span,
    pub selection: Span,
}

/// The declaration of what the name at a byte of a workspace file names, as the span of its
/// name; none when it names nothing or is declared outside the workspace's files.
pub fn definition(workspace: &Workspace, path: &str, offset: u32) -> Option<Span> {
    let checker = file_checker(workspace, path)?;
    let (named, _) = checker.named_at(path, offset)?;
    checker
        .declared_span(&named)
        .filter(|span| workspace.file(&span.path).is_some())
}

/// The spans writing the name of what the name at a byte of a workspace file names: for an item
/// or member, those of the workspace; for a local, those of its item; its declaration's only
/// when `declaration` holds.
pub fn references(workspace: &Workspace, path: &str, offset: u32, declaration: bool) -> Vec<Span> {
    let mut spans = BTreeSet::new();
    for (_, checker) in checkers(workspace, crate::stdlib::packages()) {
        let Some((named, _)) = checker.named_at(path, offset) else {
            continue;
        };
        let found = match &named {
            Named::Local(item, binding) => checker.local_spans(*item, *binding),
            Named::Target(target) => {
                checker.type_reported_bodies();
                checker.name_spans(target)
            }
        };
        let declared = checker.declared_span(&named);
        spans.extend(
            found
                .into_iter()
                .filter(|span| declaration || Some(span) != declared.as_ref()),
        );
    }
    spans.into_iter().collect()
}

/// What the name at a byte of a workspace file names, described: an item's `api` record lines
/// with a function's digest line, a member's type's, or a local's binding with its type; with
/// the span of the name.
pub fn hover(workspace: &Workspace, path: &str, offset: u32) -> Option<(Span, Vec<String>)> {
    let checker = file_checker(workspace, path)?;
    let (named, span) = checker.named_at(path, offset)?;
    let lines = checker.described(&named);
    (!lines.is_empty()).then_some((span, lines))
}

/// What implements what the name at a byte of a workspace file names: the impl lines naming an
/// interface or a type, or the functions implementing an interface method; those in the
/// workspace's files, by position.
pub fn implementations(workspace: &Workspace, path: &str, offset: u32) -> Vec<Span> {
    let mut spans = BTreeSet::new();
    for (_, checker) in checkers(workspace, crate::stdlib::packages()) {
        if let Some((Named::Target(target), _)) = checker.named_at(path, offset) {
            spans.extend(checker.implementing_spans(&target));
        }
    }
    spans
        .into_iter()
        .filter(|span| workspace.file(&span.path).is_some())
        .collect()
}

/// The function the name at a byte of a workspace file names.
pub fn function_at(workspace: &Workspace, path: &str, offset: u32) -> Option<Function> {
    let checker = file_checker(workspace, path)?;
    let function = checker.function_named(path, offset)?;
    checker.function(function)
}

/// The functions of the workspace calling the function the name at a byte of a workspace file
/// names, by position, each with the spans of its calls.
pub fn callers(workspace: &Workspace, path: &str, offset: u32) -> Vec<(Function, Vec<Span>)> {
    let mut found: BTreeMap<Span, (Function, BTreeSet<Span>)> = BTreeMap::new();
    for (_, checker) in checkers(workspace, crate::stdlib::packages()) {
        let Some(function) = checker.function_named(path, offset) else {
            continue;
        };
        checker.type_reported_bodies();
        for index in 0..checker.db.items.len() {
            let caller = ItemId(index as u32);
            if !checker.db.is_reported(caller) || checker.db.in_history(caller) {
                continue;
            }
            let calls: Vec<Span> = checker
                .calls_in(caller)
                .into_iter()
                .filter(|(callee, _)| *callee == function)
                .map(|(_, call)| call)
                .collect();
            let Some(shown) = checker.function(caller).filter(|_| !calls.is_empty()) else {
                continue;
            };
            let entry = found
                .entry(shown.item.clone())
                .or_insert_with(|| (shown, BTreeSet::new()));
            entry.1.extend(calls);
        }
    }
    found
        .into_values()
        .map(|(function, calls)| (function, calls.into_iter().collect()))
        .collect()
}

/// The functions declared in the workspace's files that the function the name at a byte of a
/// workspace file names calls, by position, each with the spans of the calls.
pub fn callees(workspace: &Workspace, path: &str, offset: u32) -> Vec<(Function, Vec<Span>)> {
    let Some(checker) = file_checker(workspace, path) else {
        return Vec::new();
    };
    let Some(function) = checker.function_named(path, offset) else {
        return Vec::new();
    };
    let mut found: BTreeMap<Span, (Function, Vec<Span>)> = BTreeMap::new();
    for (callee, call) in checker.calls_in(function) {
        let Some(shown) = checker.function(callee) else {
            continue;
        };
        if workspace.file(&shown.item.path).is_none() {
            continue;
        }
        found
            .entry(shown.item.clone())
            .or_insert_with(|| (shown, Vec::new()))
            .1
            .push(call);
    }
    found.into_values().collect()
}

/// The checker of the closure checking the package holding a workspace file, under the first
/// target that package is checked under, before it checks anything.
pub(super) fn file_checker<'a>(workspace: &'a Workspace, path: &str) -> Option<Checker<'a>> {
    let (package, _) = workspace.file(path)?;
    let closure = workspace.closure_of(package)?;
    let target = *check_targets(package).first()?;
    Some(unchecked(
        workspace,
        closure,
        crate::stdlib::packages(),
        target,
    ))
}

/// The token of an item's syntax at a byte that may write a name: an identifier, `self` or a
/// doc comment holding it, or else one ending at it.
pub(super) fn name_token(syntax: &SourceFile, item: &ItemSyntax, offset: u32) -> Option<Tok> {
    let naming = |token: usize| {
        matches!(
            syntax.tokens[token].kind,
            TokenKind::Ident | TokenKind::Doc | TokenKind::Keyword(Keyword::SelfValue)
        )
    };
    let holding = (item.first..item.end).find(|&token| {
        let at = &syntax.tokens[token];
        at.start <= offset && offset < at.end
    });
    let ending = (item.first..item.end).find(|&token| syntax.tokens[token].end == offset);
    holding
        .filter(|token| naming(*token))
        .or(ending.filter(|token| naming(*token)))
        .map(|token| token as Tok)
}

/// Visits each expression of a typed body: its code's and its clauses'.
pub(super) fn each_expr<'e>(body: &'e TBody, visit: &mut dyn FnMut(&'e TExpr)) {
    let mut pending: Vec<&TExpr> = body.root.iter().collect();
    pending.extend(body.clauses.iter().map(|clause| &clause.expr));
    while let Some(expr) = pending.pop() {
        visit(expr);
        expr.each_child(&mut |child| pending.push(child));
    }
}

impl Checker<'_> {
    /// The item of a workspace file whose lines hold a byte.
    pub(super) fn item_holding(&self, path: &str, offset: u32) -> Option<ItemId> {
        (0..self.db.items.len())
            .map(|index| ItemId(index as u32))
            .find(|item| {
                let data = self.db.item(*item);
                let syntax = data.syntax();
                let lines = syntax.item_lines(&syntax.items[data.index]);
                data.file.path == path && {
                    let start = syntax.lines[*lines.start() as usize - 1].start;
                    let end = syntax.lines[*lines.end() as usize - 1].end;
                    (start..=end).contains(&offset)
                }
            })
    }

    /// What the name at a byte of a workspace file names, with the span of that name.
    pub(super) fn named_at(&self, path: &str, offset: u32) -> Option<(Named, Span)> {
        let item = self.item_holding(path, offset)?;
        let data = self.db.item(item);
        let syntax = data.syntax();
        let token = name_token(syntax, &syntax.items[data.index], offset)?;
        if syntax.tokens[token as usize].kind == TokenKind::Doc {
            return self.doc_named(item, path, offset);
        }
        Some((self.named(item, token)?, self.span(item, token)))
    }

    /// What a name an item writes at a token names.
    fn named(&self, item: ItemId, token: Tok) -> Option<Named> {
        let data = self.db.item(item);
        let ast = data.ast;
        if decl::declared_name(ast).is_some_and(|name| name.token == token) {
            return Some(Named::Target(Target::Item(item)));
        }
        if let Some(member) = self.member_declared(item, token) {
            return Some(Named::Target(member));
        }
        let holder = self.holder(item);
        for label in holder.labels() {
            if label.last() == Some(&token) {
                return Some(Named::Target(Target::Item(item)));
            }
            if label.len() == 3
                && label[0] == token
                && let Some(TypeKey::Item(owner)) = self.db.owners.get(&item)
            {
                return Some(Named::Target(Target::Item(*owner)));
            }
        }
        let bound = bindings(data.syntax(), holder.tree);
        if let Some(first) = bound.of.get(&token) {
            return Some(Named::Local(item, *first));
        }
        self.typed(item);
        let member = self
            .members_named
            .borrow()
            .iter()
            .find(|(holding, named, _)| *holding == item && *named == token)
            .map(|(_, _, member)| member.clone());
        if let Some(member) = member {
            return Some(Named::Target(match member {
                Member::Function(function) => Target::Item(function),
                Member::Method(interface, name) => Target::Method(interface, name),
                Member::Field(owner, name) => Target::Field(owner, name),
                Member::Variant(owner, name) => Target::Variant(owner, name),
            }));
        }
        self.path_named(&holder, token)
    }

    /// The field, variant or interface method an item declares at a token.
    fn member_declared(&self, item: ItemId, token: Tok) -> Option<Target> {
        match &self.db.ast(item).kind {
            ItemKind::Struct(structure) => structure
                .fields()
                .find(|field| field.name.token == token)
                .map(|field| Target::Field(item, field.name.text.clone())),
            ItemKind::Enum(enumeration) => enumeration
                .variants
                .iter()
                .find(|variant| variant.name.token == token)
                .map(|variant| Target::Variant(item, variant.name.text.clone())),
            ItemKind::Interface(interface) => interface
                .methods
                .iter()
                .find(|method| method.name.token == token)
                .map(|method| Target::Method(item, method.name.text.clone())),
            _ => None,
        }
    }

    /// What a path an item writes names at a token: the item its segments up to that token
    /// name, or the member a test target names through it.
    fn path_named(&self, holder: &Holder, token: Tok) -> Option<Named> {
        for (path, member) in holder.paths() {
            if let Some(member) = member.filter(|member| member.token == token) {
                let Ok(Resolved::Item(owner)) = self.db.resolve(holder.module, &path) else {
                    continue;
                };
                return self.member_named(owner, &member.text).map(Named::Target);
            }
            let Some(at) = path
                .segments
                .iter()
                .position(|segment| segment.token == token)
            else {
                continue;
            };
            let written = ast::Path {
                segments: path.segments[..=at].to_vec(),
            };
            match self.db.resolve(holder.module, &written) {
                Ok(Resolved::Item(found)) => return Some(Named::Target(Target::Item(found))),
                Ok(Resolved::Variant(owner, index)) => {
                    let ItemKind::Enum(enumeration) = &self.db.ast(owner).kind else {
                        continue;
                    };
                    let name = enumeration.variants.get(index)?.name.text.clone();
                    return Some(Named::Target(Target::Variant(owner, name)));
                }
                _ => {}
            }
        }
        None
    }

    /// What a doc reference of an item names at a byte of its doc comments.
    fn doc_named(&self, item: ItemId, path: &str, offset: u32) -> Option<(Named, Span)> {
        let holder = self.holder(item);
        for reference in holder.doc_references() {
            let span = |(start, end): (u32, u32)| Span {
                path: path.to_owned(),
                start,
                end,
            };
            let Ok(Resolved::Item(found)) = self.db.resolve(holder.module, &reference.path) else {
                continue;
            };
            if (reference.last.0..=reference.last.1).contains(&offset) {
                return Some((Named::Target(Target::Item(found)), span(reference.last)));
            }
            if let Some((at, member)) = &reference.member
                && (*at..=at + member.len() as u32).contains(&offset)
            {
                let named = self.member_named(found, member)?;
                return Some((Named::Target(named), span((*at, at + member.len() as u32))));
            }
        }
        None
    }

    /// The member of an item a name after `.` names: a field, variant or interface method it
    /// declares, or else a function named after it.
    fn member_named(&self, owner: ItemId, name: &str) -> Option<Target> {
        let declared = match &self.db.ast(owner).kind {
            ItemKind::Struct(structure) => structure
                .fields()
                .any(|field| field.name.text == name)
                .then(|| Target::Field(owner, name.to_owned())),
            ItemKind::Enum(enumeration) => enumeration
                .variants
                .iter()
                .any(|variant| variant.name.text == name)
                .then(|| Target::Variant(owner, name.to_owned())),
            ItemKind::Interface(interface) => interface
                .methods
                .iter()
                .any(|method| method.name.text == name)
                .then(|| Target::Method(owner, name.to_owned())),
            _ => None,
        };
        declared.or_else(|| {
            let functions = self.db.members(TypeKey::Item(owner), name);
            functions.first().map(|function| Target::Item(*function))
        })
    }

    /// The span of the name declaring what a name names.
    fn declared_span(&self, named: &Named) -> Option<Span> {
        let (item, token) = match named {
            Named::Local(item, binding) => (*item, *binding),
            Named::Target(Target::Item(item)) => {
                (*item, decl::declared_name(self.db.ast(*item))?.token)
            }
            Named::Target(
                Target::Field(owner, name)
                | Target::Variant(owner, name)
                | Target::Method(owner, name),
            ) => (*owner, self.member_token(*owner, name)?),
        };
        Some(self.span(item, token))
    }

    /// The token of the name of a field, variant or interface method an item declares.
    fn member_token(&self, owner: ItemId, name: &str) -> Option<Tok> {
        match &self.db.ast(owner).kind {
            ItemKind::Struct(structure) => structure
                .fields()
                .find(|field| field.name.text == name)
                .map(|field| field.name.token),
            ItemKind::Enum(enumeration) => enumeration
                .variants
                .iter()
                .find(|variant| variant.name.text == name)
                .map(|variant| variant.name.token),
            ItemKind::Interface(interface) => interface
                .methods
                .iter()
                .find(|method| method.name.text == name)
                .map(|method| method.name.token),
            _ => None,
        }
    }

    /// The spans of an item's names of the local first bound at `binding`.
    fn local_spans(&self, item: ItemId, binding: Tok) -> BTreeSet<Span> {
        let holder = self.holder(item);
        bindings(&holder.file.syntax, holder.tree)
            .of
            .iter()
            .filter(|(_, first)| **first == binding)
            .map(|(token, _)| self.span(item, *token))
            .collect()
    }

    /// Types every body of the packages the checker reports on, recording the members their
    /// names name.
    fn type_reported_bodies(&self) {
        for index in 0..self.db.items.len() {
            let item = ItemId(index as u32);
            if self.db.is_reported(item) && !self.db.in_history(item) {
                self.typed(item);
            }
        }
    }

    /// The lines describing what a name names.
    fn described(&self, named: &Named) -> Vec<String> {
        let item = match named {
            Named::Local(item, binding) => return self.local_described(*item, *binding),
            Named::Target(target) => target.item(),
        };
        let mut lines = self.api_lines(item, true, None);
        if matches!(named, Named::Target(Target::Item(_)))
            && matches!(self.db.ast(item).kind, ItemKind::Fn(_))
        {
            lines.push(format!("digest: {}", self.digest_text(item)));
        }
        lines
    }

    /// A local's binding as written, with its type: `let`, `var` or its mode, its name and type.
    fn local_described(&self, item: ItemId, binding: Tok) -> Vec<String> {
        let Some(body) = self.typed(item) else {
            return Vec::new();
        };
        let Some(local) = body.locals.iter().find(|local| local.token == binding) else {
            return Vec::new();
        };
        let word = match local.kind {
            LocalKind::Let => "let ",
            LocalKind::Var => "var ",
            LocalKind::Param(mode) | LocalKind::Element(mode) | LocalKind::Capture(mode) => {
                match mode {
                    ast::Mode::Mut => "mut ",
                    ast::Mode::Take => "take ",
                    _ => "",
                }
            }
            LocalKind::Pattern | LocalKind::Quantified => "",
        };
        let module = self.db.item(item).module;
        vec![format!(
            "{word}{}: {}",
            local.name,
            self.show(&local.ty, module)
        )]
    }

    /// The impl lines naming an interface or a type, as their items' spans, or the names of the
    /// functions implementing an interface method.
    fn implementing_spans(&self, target: &Target) -> Vec<Span> {
        match target {
            Target::Method(interface, name) => self
                .implementing(*interface, name)
                .into_iter()
                .filter_map(|function| self.declared_span(&Named::Target(Target::Item(function))))
                .collect(),
            Target::Item(named) => (0..self.db.items.len())
                .map(|index| ItemId(index as u32))
                .filter(|line| match &self.sig(*line).kind {
                    SigKind::Impl {
                        target, interface, ..
                    } => {
                        *target == Some(TypeKey::Item(*named))
                            || interface
                                .as_ref()
                                .is_some_and(|bound| bound.interface == *named)
                    }
                    _ => false,
                })
                .map(|line| self.item_span(line))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The bytes of an item's lines, without the last line's end.
    fn item_span(&self, item: ItemId) -> Span {
        let data = self.db.item(item);
        let syntax = data.syntax();
        let lines = syntax.item_lines(&syntax.items[data.index]);
        Span {
            path: data.file.path.clone(),
            start: syntax.lines[*lines.start() as usize - 1].start,
            end: syntax.lines[*lines.end() as usize - 1].end,
        }
    }

    /// The function item the name at a byte of a workspace file names.
    fn function_named(&self, path: &str, offset: u32) -> Option<ItemId> {
        match self.named_at(path, offset)? {
            (Named::Target(Target::Item(item)), _)
                if matches!(self.db.ast(item).kind, ItemKind::Fn(_)) =>
            {
                Some(item)
            }
            _ => None,
        }
    }

    /// A function item as a call hierarchy shows it.
    fn function(&self, item: ItemId) -> Option<Function> {
        let ast = self.db.ast(item);
        let ItemKind::Fn(function) = &ast.kind else {
            return None;
        };
        Some(Function {
            name: ast.name()?,
            member: function.label.owner.is_some(),
            item: self.item_span(item),
            selection: self.span(item, function.label.name.token),
        })
    }

    /// The function items an item's body calls, each with the span of the call.
    fn calls_in(&self, item: ItemId) -> Vec<(ItemId, Span)> {
        let Some(body) = self.typed(item) else {
            return Vec::new();
        };
        let mut calls = Vec::new();
        each_expr(&body, &mut |expr| {
            if let TKind::Call(call) = &expr.kind
                && let Callee::Function { item: callee } = &call.callee
            {
                let mut span = self.span(item, expr.token);
                span.end = self.span(item, expr.last).end;
                calls.push((*callee, span));
            }
        });
        calls.sort_by(|left, right| left.1.cmp(&right.1));
        calls
    }
}
