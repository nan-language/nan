//! The references of an item or member across the workspace: its declaration and the labels,
//! paths, member names and doc references writing its name, which renaming rewrites.

use std::collections::{BTreeSet, HashMap};
use std::ops::RangeInclusive;

use super::db::{ItemId, ModuleId, TypeKey};
use super::resolve::Named;
use super::sig::SigKind;
use super::{Checker, Member};
use crate::format::bindings::bindings;
use crate::syntax::ast::{self, ItemKind};
use crate::syntax::file::SourceFile;
use crate::syntax::grammar::{Punct, Rule};
use crate::syntax::token::TokenKind;
use crate::syntax::tree::{Child, NodeId, Tree};
use crate::workspace::File;

/// The methods a `for` statement calls on what it iterates and on the cursor it obtains.
const CURSOR_METHODS: [&str; 4] = ["cursor", "cursor_mut", "into_cursor", "next"];

/// What an edit renames: an item, or a field, variant or interface method by its owner and name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Item(ItemId),
    Field(ItemId, String),
    Variant(ItemId, String),
    Method(ItemId, String),
}

/// A name written in a workspace file: the file's path and the name's bytes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Span {
    pub path: String,
    pub start: u32,
    pub end: u32,
}

impl Checker<'_> {
    /// What lines of a workspace file declare: the item whose head they hold, or the field,
    /// variant or interface method they hold the name of.
    pub fn target_at(&self, path: &str, lines: &RangeInclusive<u32>) -> Option<Target> {
        let db = &self.db;
        let item = (0..db.items.len())
            .map(|index| ItemId(index as u32))
            .find(|item| {
                let data = db.item(*item);
                db.is_reported(*item)
                    && data.file.path == path
                    && data
                        .syntax()
                        .item_lines(&data.syntax().items[data.index])
                        .contains(lines.start())
            })?;
        let data = db.item(item);
        if lines.contains(&data.head_line()) {
            return Some(Target::Item(item));
        }
        let on = |name: &ast::Name| lines.contains(&data.line(name.token));
        match &data.ast.kind {
            ItemKind::Struct(structure) => structure
                .fields()
                .find(|field| on(&field.name))
                .map(|field| Target::Field(item, field.name.text.clone())),
            ItemKind::Enum(enumeration) => enumeration
                .variants
                .iter()
                .find(|variant| on(&variant.name))
                .map(|variant| Target::Variant(item, variant.name.text.clone())),
            ItemKind::Interface(interface) => interface
                .methods
                .iter()
                .find(|method| on(&method.name))
                .map(|method| Target::Method(item, method.name.text.clone())),
            _ => None,
        }
    }

    /// The spans writing a target's name: its declaration, the labels naming it, and each path,
    /// member name and doc reference of the workspace naming it. Renaming an
    /// interface method renames the functions implementing it too.
    pub fn name_spans(&self, target: &Target) -> BTreeSet<Span> {
        let mut spans = BTreeSet::new();
        let mut targets = vec![target.clone()];
        if let Target::Method(interface, name) = target {
            targets.extend(
                self.implementing(*interface, name)
                    .into_iter()
                    .map(Target::Item),
            );
        }
        for target in &targets {
            for holder in self.holders() {
                self.declarations(&holder, target, &mut spans);
                self.paths(&holder, target, &mut spans);
                self.doc_references(&holder, target, &mut spans);
            }
            for (item, token, member) in self.members_named.borrow().iter() {
                let names = match (target, member) {
                    (Target::Item(function), Member::Function(named)) => function == named,
                    (Target::Field(owner, field), Member::Field(named, name))
                    | (Target::Variant(owner, field), Member::Variant(named, name))
                    | (Target::Method(owner, field), Member::Method(named, name)) => {
                        owner == named && field == name
                    }
                    _ => false,
                };
                if names {
                    spans.insert(self.span(*item, *token));
                }
            }
        }
        spans
    }

    /// The functions implementing an interface method: those named after each impl line's target
    /// with the method's name, in the impl line's module.
    fn implementing(&self, interface: ItemId, name: &str) -> Vec<ItemId> {
        let mut found = Vec::new();
        for index in 0..self.db.items.len() {
            let line = ItemId(index as u32);
            let SigKind::Impl {
                target: Some(key),
                interface: Some(bound),
                ..
            } = &self.sig(line).kind
            else {
                continue;
            };
            if bound.interface == interface {
                found.extend(
                    self.db.members(*key, name).iter().filter(|function| {
                        self.db.module_of(**function) == self.db.module_of(line)
                    }),
                );
            }
        }
        found
    }

    fn span(&self, item: ItemId, token: ast::Tok) -> Span {
        let data = self.db.item(item);
        let token = &data.syntax().tokens[token as usize];
        Span {
            path: data.file.path.clone(),
            start: token.start,
            end: token.end,
        }
    }

    /// An item with the module its paths are read in.
    fn holder(&self, item: ItemId) -> Holder<'_> {
        let data = self.db.item(item);
        Holder {
            item: Some(item),
            file: data.file,
            tree: data.syntax().items[data.index]
                .tree
                .as_ref()
                .expect("a checked item has a tree"),
            module: data.module,
        }
    }

    /// The parsed items of the workspace's packages, with the module their paths are read in.
    fn holders(&self) -> Vec<Holder<'_>> {
        let db = &self.db;
        let mut holders: Vec<Holder> = (0..db.items.len())
            .map(|index| ItemId(index as u32))
            .filter(|item| db.is_reported(*item))
            .map(|item| self.holder(item))
            .collect();
        for package in db
            .packages
            .iter()
            .filter(|package| !package.carried && !package.quiet)
        {
            let (Some(root), Some(file)) = (package.root, package.source.files.first()) else {
                continue;
            };
            holders.extend(
                file.syntax
                    .items
                    .iter()
                    .filter(|item| !item.abandoned)
                    .filter_map(|item| item.tree.as_ref())
                    .map(|tree| Holder {
                        item: None,
                        file,
                        tree,
                        module: root,
                    }),
            );
        }
        holders
    }

    /// The functions of the workspace named after a type, by their member names.
    pub fn member_functions(&self) -> HashMap<&str, Vec<ItemId>> {
        let mut functions: HashMap<&str, Vec<ItemId>> = HashMap::new();
        for members in self.db.members.values() {
            for (name, named) in members {
                functions.entry(name.as_str()).or_default().extend(
                    named
                        .iter()
                        .filter(|function| self.db.is_reported(**function)),
                );
            }
        }
        functions
    }

    /// The items an item names: those its paths name, its test target aside; the functions
    /// `functions` lists under each member name it writes after `.` and each cursor method its
    /// `for` statements call; the type a function is named after; and the functions
    /// implementing the interface of an impl line.
    pub fn names(&self, item: ItemId, functions: &HashMap<&str, Vec<ItemId>>) -> BTreeSet<ItemId> {
        let holder = self.holder(item);
        let written = holder.written();
        let mut named: BTreeSet<ItemId> = written
            .paths
            .iter()
            .filter_map(|path| match self.db.resolve(holder.module, path) {
                Ok(Named::Item(named)) => Some(named),
                _ => None,
            })
            .collect();
        let cursors = CURSOR_METHODS.iter().filter(|_| written.iterates).copied();
        for member in written.members.iter().map(String::as_str).chain(cursors) {
            named.extend(functions.get(member).into_iter().flatten());
        }
        if let Some(TypeKey::Item(owner)) = self.db.owners.get(&item) {
            named.insert(*owner);
        }
        if let SigKind::Impl {
            interface: Some(bound),
            ..
        } = &self.sig(item).kind
            && let SigKind::Interface { methods } = &self.sig(bound.interface).kind
        {
            named.extend(
                methods
                    .iter()
                    .filter_map(|method| self.implementing_function(item, &method.name)),
            );
        }
        named
    }

    /// The paths of an item that may name an item, outside labels, bindings, a tombstone's old
    /// form and a test's target.
    pub fn written_paths(&self, item: ItemId) -> Vec<ast::Path> {
        self.holder(item).written().paths
    }

    /// The declared names of an item, and the labels of its functions, naming a target.
    fn declarations(&self, holder: &Holder, target: &Target, spans: &mut BTreeSet<Span>) {
        let Some(item) = holder.item else {
            return;
        };
        let ast = self.db.ast(item);
        let mut add = |token: ast::Tok| {
            spans.insert(self.span(item, token));
        };
        match (target, &ast.kind) {
            (Target::Item(named), _) if *named == item => {
                if let Some(name) = super::decl::declared_name(ast) {
                    add(name.token);
                }
                for label in holder.labels() {
                    add(label.last().copied().expect("a label holds a name"));
                }
            }
            (Target::Item(named), ItemKind::Fn(function))
                if function.label.owner.is_some()
                    && self.db.owners.get(&item) == Some(&TypeKey::Item(*named)) =>
            {
                for label in holder.labels() {
                    if let [.., owner, _, _] = label[..] {
                        add(owner);
                    }
                }
            }
            (Target::Field(owner, name), ItemKind::Struct(structure)) if *owner == item => {
                for field in structure.fields().filter(|field| field.name.text == *name) {
                    add(field.name.token);
                }
                for path in holder.single_paths(name) {
                    add(path);
                }
            }
            (Target::Variant(owner, name), ItemKind::Enum(enumeration)) if *owner == item => {
                for variant in enumeration
                    .variants
                    .iter()
                    .filter(|variant| variant.name.text == *name)
                {
                    add(variant.name.token);
                }
            }
            (Target::Method(owner, name), ItemKind::Interface(interface)) if *owner == item => {
                for method in interface
                    .methods
                    .iter()
                    .filter(|method| method.name.text == *name)
                {
                    add(method.name.token);
                }
            }
            _ => {}
        }
    }

    /// The paths of an item naming a target item, by their last segments, and the members of
    /// test targets naming a target function.
    fn paths(&self, holder: &Holder, target: &Target, spans: &mut BTreeSet<Span>) {
        let Target::Item(target) = target else {
            return;
        };
        let syntax = &holder.file.syntax;
        for (path, member) in holder.paths() {
            let named = self.db.resolve(holder.module, &path);
            let last = path.last().token;
            let token = match (named, member) {
                (Ok(Named::Item(item)), None) if item == *target => last,
                (Ok(Named::Item(owner)), Some(member))
                    if self
                        .db
                        .members(TypeKey::Item(owner), &member.text)
                        .contains(target) =>
                {
                    member.token
                }
                _ => continue,
            };
            let token = &syntax.tokens[token as usize];
            spans.insert(Span {
                path: holder.file.path.clone(),
                start: token.start,
                end: token.end,
            });
        }
    }

    /// The doc references of an item's doc comments naming a target, by the name they write it
    /// with.
    fn doc_references(&self, holder: &Holder, target: &Target, spans: &mut BTreeSet<Span>) {
        for reference in holder.doc_references() {
            let Ok(Named::Item(item)) = self.db.resolve(holder.module, &reference.path) else {
                continue;
            };
            let path_named = matches!(target, Target::Item(named) if *named == item);
            let member_named = match (target, &reference.member) {
                (Target::Item(named), Some(member)) => self
                    .db
                    .members(TypeKey::Item(item), &member.1)
                    .contains(named),
                (
                    Target::Field(owner, name)
                    | Target::Variant(owner, name)
                    | Target::Method(owner, name),
                    Some(member),
                ) => *owner == item && *name == member.1,
                _ => false,
            };
            let (start, end) = if path_named {
                reference.last
            } else if let (true, Some((at, member))) = (member_named, &reference.member) {
                (*at, at + member.len() as u32)
            } else {
                continue;
            };
            spans.insert(Span {
                path: holder.file.path.clone(),
                start,
                end,
            });
        }
    }
}

impl Checker<'_> {
    /// Gives each `name.case` diagnostic its exact fix: the declaration and its uses in its package
    /// renamed to the name its convention writes, when that name follows the convention.
    pub fn fix_misnamed(&self) {
        let misnamed = self.misnamed.take();
        for (item, token, case) in misnamed {
            let data = self.db.item(item);
            let new = super::decl::converted(data.text(token), case);
            if !super::decl::follows(&new, case) {
                continue;
            }
            let Some(replacements) = self.misnamed_replacements(item, token, &new) else {
                continue;
            };
            let start = data.syntax().tokens[token as usize].start;
            for diagnostic in self.found.borrow_mut().iter_mut() {
                if diagnostic.code == crate::diagnostic::Code::NameCase
                    && diagnostic.path.as_deref() == Some(data.file.path.as_str())
                    && diagnostic.span.is_some_and(|(at, _)| at == start)
                {
                    diagnostic.fixes = vec![crate::diagnostic::Fix(replacements.clone())];
                }
            }
        }
    }

    /// The replacements renaming the name an item declares at `token` to `new` in its package: an
    /// item, field, variant or interface method with its uses, a generic parameter or a local with
    /// those of its item, a punned argument keeping its label; none for another name.
    fn misnamed_replacements(
        &self,
        item: ItemId,
        token: ast::Tok,
        new: &str,
    ) -> Option<Vec<(String, crate::diagnostic::Replacement)>> {
        let data = self.db.item(item);
        let syntax = data.syntax();
        let old = data.text(token);
        let replacement = |span: Span, text: String| {
            (
                span.path,
                crate::diagnostic::Replacement {
                    start: span.start,
                    end: span.end,
                    text,
                },
            )
        };
        let declares = |name: &ast::Name| name.token == token;
        let target = match &data.ast.kind {
            _ if super::decl::declared_name(data.ast).is_some_and(declares) => {
                Some(Target::Item(item))
            }
            ItemKind::Struct(structure) => structure
                .fields()
                .find(|field| declares(&field.name))
                .map(|field| Target::Field(item, field.name.text.clone())),
            ItemKind::Enum(enumeration) => enumeration
                .variants
                .iter()
                .find(|variant| declares(&variant.name))
                .map(|variant| Target::Variant(item, variant.name.text.clone())),
            ItemKind::Interface(interface) => interface
                .methods
                .iter()
                .find(|method| declares(&method.name))
                .map(|method| Target::Method(item, method.name.text.clone())),
            _ => None,
        };
        if let Some(target) = target {
            let package = self.db.package_of(item);
            let files: Vec<&str> = self
                .db
                .package(package)
                .source
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect();
            return Some(
                self.name_spans(&target)
                    .into_iter()
                    .filter(|span| files.contains(&span.path.as_str()))
                    .map(|span| replacement(span, new.to_owned()))
                    .collect(),
            );
        }
        let tree = syntax.items[data.index].tree.as_ref()?;
        let at = |token: u32| self.span(item, token);
        let generic = tree.descendants(tree.root()).into_iter().any(|node| {
            matches!(tree.rule(node), Rule::GenericParam | Rule::ImplParam)
                && tree.child_tokens(node).next() == Some(token)
        });
        if generic {
            let holder = Holder {
                item: Some(item),
                file: data.file,
                tree,
                module: data.module,
            };
            let mut tokens = holder.single_paths(old);
            tokens.push(token);
            return Some(
                tokens
                    .into_iter()
                    .map(|token| replacement(at(token), new.to_owned()))
                    .collect(),
            );
        }
        let bound = bindings(syntax, tree);
        if bound.of.get(&token) != Some(&token) {
            return None;
        }
        let punned = crate::format::punning::punning(syntax, tree, &bound);
        let mut replacements = Vec::new();
        for (used, _) in bound.of.iter().filter(|(_, first)| **first == token) {
            replacements.push(replacement(at(*used), new.to_owned()));
            if let Some((first, _)) = punned.values.iter().find(|(_, name)| *name == used) {
                let start = syntax.tokens[*first as usize].start;
                replacements.push((
                    data.file.path.clone(),
                    crate::diagnostic::Replacement {
                        start,
                        end: start,
                        text: format!("{old}: "),
                    },
                ));
            }
        }
        replacements.sort_by_key(|(_, replacement)| (replacement.start, replacement.end));
        Some(replacements)
    }
}

/// A move's edit, and where each item it moves lands.
pub struct Move {
    pub fix: crate::diagnostic::Fix,
    pub relocated: Vec<Relocated>,
}

/// The lines of a moved item: the path and first line it leaves and lands at, and their number.
pub struct Relocated {
    pub from: (String, u32),
    pub to: (String, u32),
    pub lines: u32,
}

impl Checker<'_> {
    /// The edit moving an item, with the functions named after it and the impl lines naming it in
    /// its module, to the end of the summary file of another module of its package; none when
    /// that move is invalid: of a member, of a function named after a type, or into a missing
    /// module.
    pub fn moving(&self, target: &Target, module: &str) -> Option<Move> {
        let Target::Item(item) = target else {
            return None;
        };
        let db = &self.db;
        if db.owners.contains_key(item) {
            return None;
        }
        let from = db.module_of(*item);
        let package = db.package_of(*item);
        let to = db.module_named(package, module)?;
        let target = db.module(to);
        let (dir, _) = target.files.first()?.path.rsplit_once('/')?;
        let summary = format!("{dir}/{}.nan", target.name);
        let mut moved: Vec<ItemId> = vec![*item];
        for &other in &db.module(from).items {
            let named_after = db.owners.get(&other) == Some(&TypeKey::Item(*item));
            let naming = matches!(&self.sig(other).kind, SigKind::Impl { target, interface, .. }
                if *target == Some(TypeKey::Item(*item))
                    || interface.as_ref().is_some_and(|bound| bound.interface == *item));
            if named_after || naming {
                moved.push(other);
            }
        }
        let lands = |other: ItemId| {
            if moved.contains(&other) {
                to
            } else {
                db.module_of(other)
            }
        };
        let mut in_place: Vec<(String, crate::diagnostic::Replacement)> = Vec::new();
        let mut inside: Vec<(ItemId, u32, u32, String)> = Vec::new();
        let mut raised: BTreeSet<ItemId> = BTreeSet::new();
        for holder in self.holders() {
            let reader = holder.item.map_or(holder.module, lands);
            for (path, member) in holder.paths() {
                if member.is_some() {
                    continue;
                }
                let Ok(Named::Item(named)) = db.resolve(holder.module, &path) else {
                    continue;
                };
                let landed = lands(named);
                let holder_moves = holder.item.is_some_and(|item| moved.contains(&item));
                if db.package_of(named) != package || !(holder_moves || moved.contains(&named)) {
                    continue;
                }
                let spelt =
                    self.spelling(reader, landed, &db.ast(named).name().unwrap_or_default());
                if landed != reader && db.ast(named).visibility == ast::Visibility::Private {
                    raised.insert(named);
                }
                let syntax = holder.syntax();
                let first = syntax.tokens[path.segments[0].token as usize].start;
                let last = syntax.tokens[path.last().token as usize].end;
                if syntax.source[first as usize..last as usize] == *spelt.as_bytes() {
                    continue;
                }
                match holder.item.filter(|item| moved.contains(item)) {
                    Some(item) => inside.push((item, first, last, spelt)),
                    None => in_place.push((
                        holder.file.path.clone(),
                        crate::diagnostic::Replacement {
                            start: first,
                            end: last,
                            text: spelt,
                        },
                    )),
                }
            }
            for reference in holder.doc_references() {
                let Ok(Named::Item(named)) = db.resolve(holder.module, &reference.path) else {
                    continue;
                };
                let holder_moves = holder.item.is_some_and(|item| moved.contains(&item));
                if db.package_of(named) != package || !(holder_moves || moved.contains(&named)) {
                    continue;
                }
                let spelt = self.spelling(
                    reader,
                    lands(named),
                    &db.ast(named).name().unwrap_or_default(),
                );
                let (first, last) = reference.whole;
                match holder.item.filter(|item| moved.contains(item)) {
                    Some(item) => inside.push((item, first, last, spelt)),
                    None => in_place.push((
                        holder.file.path.clone(),
                        crate::diagnostic::Replacement {
                            start: first,
                            end: last,
                            text: spelt,
                        },
                    )),
                }
            }
        }
        for (holder, _, member) in self.members_named.borrow().iter() {
            if let Member::Function(function) = member
                && moved.contains(function)
                && lands(*holder) != to
                && db.ast(*function).visibility == ast::Visibility::Private
            {
                raised.insert(*function);
            }
        }
        let (_, summary_file) = db
            .packages
            .iter()
            .flat_map(|package| package.source.files.iter().map(move |file| (package, file)))
            .find(|(_, file)| file.path == summary)?;
        let mut landing = summary_file
            .syntax
            .source
            .iter()
            .filter(|byte| **byte == b'\n')
            .count() as u32
            + 2;
        let mut relocated = Vec::new();
        let mut appended = String::new();
        let mut moved_in_order = moved.clone();
        moved_in_order
            .sort_by_key(|item| (db.item(*item).file.path.clone(), db.item(*item).head_line()));
        for &item in &moved_in_order {
            let data = db.item(item);
            let syntax = data.syntax();
            let lines = syntax.item_lines(&syntax.items[data.index]);
            let start = syntax.lines[*lines.start() as usize - 1].start;
            let end = syntax
                .lines
                .get(*lines.end() as usize)
                .map_or(syntax.source.len() as u32, |next| next.start);
            let head = syntax.tokens[data.ast.token as usize].start;
            let mut edits: Vec<(u32, u32, String)> = inside
                .iter()
                .filter(|(holder, ..)| *holder == item)
                .map(|(_, first, last, spelt)| (*first, *last, spelt.clone()))
                .collect();
            if raised.contains(&item) {
                edits.push((head, head, "pkg ".to_owned()));
            }
            edits.sort();
            let mut text = String::new();
            let mut at = start;
            for (first, last, spelt) in edits {
                text.push_str(&String::from_utf8_lossy(
                    &syntax.source[at as usize..first as usize],
                ));
                text.push_str(&spelt);
                at = last;
            }
            text.push_str(&String::from_utf8_lossy(
                &syntax.source[at as usize..end as usize],
            ));
            appended.push('\n');
            appended.push_str(&text);
            let count = lines.end() - lines.start() + 1;
            relocated.push(Relocated {
                from: (data.file.path.clone(), *lines.start()),
                to: (summary.clone(), landing),
                lines: count,
            });
            landing += count + 1;
            let blank_after = syntax
                .lines
                .get(*lines.end() as usize)
                .is_some_and(|next| next.start == next.end);
            let removed_end = if blank_after {
                syntax
                    .lines
                    .get(*lines.end() as usize + 1)
                    .map_or(syntax.source.len() as u32, |next| next.start)
            } else {
                end
            };
            in_place.push((
                data.file.path.clone(),
                crate::diagnostic::Replacement {
                    start,
                    end: removed_end,
                    text: String::new(),
                },
            ));
        }
        for &item in raised.iter().filter(|item| !moved.contains(item)) {
            let data = db.item(item);
            let head = data.syntax().tokens[data.ast.token as usize].start;
            in_place.push((
                data.file.path.clone(),
                crate::diagnostic::Replacement {
                    start: head,
                    end: head,
                    text: "pkg ".to_owned(),
                },
            ));
        }
        let end = summary_file.syntax.source.len() as u32;
        in_place.push((
            summary,
            crate::diagnostic::Replacement {
                start: end,
                end,
                text: appended,
            },
        ));
        Some(Move {
            fix: crate::diagnostic::Fix(in_place),
            relocated,
        })
    }

    /// The path code in module `from` writes for an item of module `to` named `name`.
    fn spelling(&self, from: ModuleId, to: ModuleId, name: &str) -> String {
        if from == to {
            return name.to_owned();
        }
        let module = self.db.module(to);
        let package = &self.db.package(module.package).name;
        let segment = if module.root { package } else { &module.name };
        format!("{segment}::{name}")
    }
}

/// A parsed item of a workspace package, with the module its paths are read in.
struct Holder<'a> {
    item: Option<ItemId>,
    file: &'a File,
    tree: &'a Tree,
    module: ModuleId,
}

impl Holder<'_> {
    fn syntax(&self) -> &SourceFile {
        &self.file.syntax
    }

    /// The identifier tokens of each item label: a function's head and closing labels, or a
    /// type's closing label.
    fn labels(&self) -> Vec<Vec<ast::Tok>> {
        let tree = self.tree;
        tree.descendants(tree.root())
            .into_iter()
            .filter(|node| tree.rule(*node) == Rule::ItemLabel)
            .map(|label| {
                tree.child_tokens(label)
                    .filter(|token| {
                        self.syntax().tokens[*token as usize].kind == TokenKind::Ident
                            || self.syntax().tokens[*token as usize].kind
                                == TokenKind::Punct(Punct::Dot)
                    })
                    .collect()
            })
            .collect()
    }

    /// The doc references of the item's doc comments.
    fn doc_references(&self) -> Vec<DocReference> {
        let syntax = self.syntax();
        let Some((first, last)) = self.tree.span(self.tree.root()) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for token in first..=last {
            if syntax.tokens[token as usize].kind != TokenKind::Doc {
                continue;
            }
            let (start, text) = super::docs::line_text(syntax, token as usize);
            for (offset, reference) in super::docs::references(text) {
                let at = start + offset as u32;
                let (path, member) = match reference.rsplit_once('.') {
                    Some((path, member)) => (path, Some(member)),
                    None => (reference, None),
                };
                let last_segment = path.rfind("::").map_or(0, |at| at + 2) as u32;
                found.push(DocReference {
                    path: ast::Path {
                        segments: path
                            .split("::")
                            .map(|segment| ast::Name {
                                text: segment.to_owned(),
                                token: 0,
                            })
                            .collect(),
                    },
                    whole: (at, at + path.len() as u32),
                    last: (at + last_segment, at + path.len() as u32),
                    member: member.map(|member| (at + path.len() as u32 + 1, member.to_owned())),
                });
            }
        }
        found
    }

    /// The tokens of the item's one-segment paths written `name` that name no binding.
    fn single_paths(&self, name: &str) -> Vec<ast::Tok> {
        let bound = bindings(self.syntax(), self.tree);
        self.tree
            .descendants(self.tree.root())
            .into_iter()
            .filter(|node| self.tree.rule(*node) == Rule::Path)
            .filter_map(|node| match self.tree.children(node) {
                [Child::Token(token)]
                    if self.syntax().text(*token as usize) == name
                        && !bound.of.contains_key(token) =>
                {
                    Some(*token)
                }
                _ => None,
            })
            .collect()
    }

    /// The paths of the item that may name an item, each with the member a test target names
    /// through it: not a label, a binding or a tombstone's old form.
    fn paths(&self) -> Vec<(ast::Path, Option<ast::Name>)> {
        let written = self.written();
        let mut found: Vec<(ast::Path, Option<ast::Name>)> =
            written.paths.into_iter().map(|path| (path, None)).collect();
        if let Some((path, member)) = written.target {
            found.push((path.clone(), None));
            found.push((path, member));
        }
        found
    }

    /// What the item writes that may name an item or member.
    fn written(&self) -> Written {
        let tree = self.tree;
        let syntax = self.syntax();
        let bound = bindings(syntax, tree);
        let name = |token: u32| ast::Name {
            text: syntax.text(token as usize).to_owned(),
            token,
        };
        let path = |node: NodeId| ast::Path {
            segments: tree
                .child_tokens(node)
                .filter(|token| syntax.tokens[*token as usize].kind == TokenKind::Ident)
                .map(name)
                .collect(),
        };
        let mut written = Written {
            paths: Vec::new(),
            members: Vec::new(),
            target: None,
            iterates: false,
        };
        let mut pending = vec![tree.root()];
        while let Some(node) = pending.pop() {
            let children: Vec<NodeId> = tree.child_nodes(node).collect();
            match tree.rule(node) {
                Rule::Arg | Rule::FieldPattern | Rule::PayloadField if children.len() == 2 => {
                    pending.push(children[1]);
                }
                Rule::RetiredOld | Rule::ItemLabel => {}
                Rule::TestTarget => {
                    let member = tree
                        .child_tokens(node)
                        .filter(|token| syntax.tokens[*token as usize].kind == TokenKind::Ident)
                        .last()
                        .map(name);
                    written.target = tree.find(node, Rule::Path).map(|node| (path(node), member));
                }
                Rule::Path => {
                    let found = path(node);
                    let local = found.segments.len() == 1
                        && bound.of.contains_key(&found.segments[0].token);
                    if !found.segments.is_empty() && !local {
                        written.paths.push(found);
                    }
                }
                rule => {
                    written.iterates |= rule == Rule::ForStmt;
                    let tokens: Vec<u32> = tree.child_tokens(node).collect();
                    for pair in tokens.windows(2) {
                        let kind = |token: u32| syntax.tokens[token as usize].kind;
                        if kind(pair[0]) == TokenKind::Punct(Punct::Dot)
                            && kind(pair[1]) == TokenKind::Ident
                        {
                            written
                                .members
                                .push(syntax.text(pair[1] as usize).to_owned());
                        }
                    }
                    pending.extend(children);
                }
            }
        }
        written
    }
}

/// What an item writes that may name an item or member: its paths outside labels, bindings, a
/// tombstone's old form and a test's target; the member names it writes after `.` outside
/// labels; its test target's path with the member it names through it; and whether a `for`
/// statement iterates.
struct Written {
    paths: Vec<ast::Path>,
    members: Vec<String>,
    target: Option<(ast::Path, Option<ast::Name>)>,
    iterates: bool,
}

/// A doc reference: its path, the bytes of the whole path and of its last segment, and the
/// member it names through that path with the offset of its name.
struct DocReference {
    path: ast::Path,
    whole: (u32, u32),
    last: (u32, u32),
    member: Option<(u32, String)>,
}
