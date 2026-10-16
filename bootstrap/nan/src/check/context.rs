//! Closure sets: the context packs `nan context` prints, recorded while checking items or lines,
//! the completeness test over them, and the last-green entries of items and of the closure sets
//! of lines.

use std::collections::HashSet;
use std::ops::RangeInclusive;

use super::db::{ItemId, TypeKey};
use super::reading::{Api, digest_lines};
use super::{Checker, check_targets, checkers, ty};
use crate::scope::Scope;
use crate::syntax::ast::{ItemKind, Tok};
use crate::syntax::grammar::Rule;
use crate::syntax::token::TokenKind;
use crate::workspace::{Target, Workspace};

/// An item's context pack, as `nan context` prints it.
pub struct Context {
    pub path: String,
    pub file: String,
    pub line: u32,
    pub lines: RangeInclusive<u32>,
    /// Its package's name and its closure set's manifest directives, in canonical order.
    pub package: String,
    pub directives: Vec<String>,
    /// Its module's path and the names of the module's items, in byte order.
    pub module: String,
    pub names: Vec<String>,
    /// The records of the items, members and expression types checking it reads.
    pub apis: Vec<Api>,
}

/// The context packs of the items whose heads lie in a scope, by path and line, each closure set
/// recorded while checking the item under the first target its package is checked under: of the
/// item, or of its line `line`.
pub fn contexts(workspace: &Workspace, scope: &Scope, line: Option<u32>) -> Vec<Context> {
    let std = crate::stdlib::packages();
    let checkers = checkers(workspace, std);
    let mut packs: Vec<Context> = Vec::new();
    let mut listed: HashSet<(String, u32)> = HashSet::new();
    for (target, checker) in &checkers {
        let db = &checker.db;
        db.record();
        for index in 0..db.items.len() {
            let item = ItemId(index as u32);
            let data = db.item(item);
            let first =
                check_targets(db.package(db.package_of(item)).source).first() == Some(target);
            if !db.is_reported(item)
                || !first
                || !scope.holds(workspace, &data.file.path, Some(data.head_line()))
                || !listed.insert((data.file.path.clone(), data.head_line()))
            {
                continue;
            }
            packs.push(checker.context(&checkers, item, line));
        }
    }
    packs.sort_by(|left, right| (&left.file, left.line).cmp(&(&right.file, right.line)));
    packs
}

impl Checker<'_> {
    /// The items an item's closure set holds beside it: those checking it reads and the types of
    /// its expressions, in order of first read. The database must be recording.
    fn closure(&self, item: ItemId) -> Vec<ItemId> {
        let db = &self.db;
        self.package.set(Some(db.package_of(item)));
        db.about(Some(item), || {
            self.check_item(item);
            self.check_elaborated(item);
        });
        let mut read = db.read_by(item);
        if let Some(body) = self.typed_body(item) {
            let mut types = Vec::new();
            for local in &body.locals {
                type_items(&local.ty, &mut types);
            }
            if let Some(root) = &body.root {
                expression_types(root, &|_| true, &mut types);
            }
            for found in types {
                if !read.contains(&found) {
                    read.push(found);
                }
            }
        }
        read.retain(|read| *read != item);
        read
    }

    /// The items the closure set of a line of an item holds beside the item: those checking the
    /// head, clause or statement starting at the nearest recorded line at or above it reads, with
    /// the item types of the expressions starting on that line.
    fn line_closure(&self, item: ItemId, line: u32) -> Vec<ItemId> {
        self.closure(item);
        let db = &self.db;
        let checked = db
            .lines_read(item)
            .into_iter()
            .filter(|checked| *checked <= line)
            .max()
            .unwrap_or(0);
        let mut read = db.read_on(item, checked);
        if let Some(body) = self.typed_body(item)
            && let Some(root) = &body.root
        {
            let data = db.item(item);
            let mut types = Vec::new();
            expression_types(root, &|token| data.line(token) == checked, &mut types);
            for found in types {
                if !read.contains(&found) {
                    read.push(found);
                }
            }
        }
        read.retain(|read| *read != item);
        read
    }

    /// An item's context pack, recording its closure set, or that of its line `line`, by checking
    /// it.
    fn context(&self, checkers: &[(Target, Checker)], item: ItemId, line: Option<u32>) -> Context {
        let db = &self.db;
        let data = db.item(item);
        let package = db.package_of(item);
        let core = db.core();
        let closure = match line {
            Some(line) => self.line_closure(item, line),
            None => self.closure(item),
        };
        let apis = closure
            .into_iter()
            .filter(|read| {
                let ast = db.declaration(*read);
                !matches!(ast.kind, ItemKind::Impl(_))
                    && !(db.package_of(*read) == core
                        && ast
                            .name()
                            .is_some_and(|name| super::resolve::is_core_vocabulary(&name)))
            })
            .map(|read| {
                let other = db.item(read);
                let mut lines = self.api_lines(read, false, Some(data.module));
                lines.extend(digest_lines(checkers, self, read));
                Api {
                    path: self
                        .canonical_path(read)
                        .or_else(|| self.own_path(read))
                        .unwrap_or_default(),
                    package: db.package(db.package_of(read)).name.clone(),
                    file: other.file.path.clone(),
                    line: other.head_line(),
                    lines,
                }
            })
            .collect();
        let mut names: Vec<String> = db
            .module(data.module)
            .items
            .iter()
            .filter_map(|other| db.declaration(*other).name())
            .collect();
        names.sort();
        names.dedup();
        let syntax = &data.file.syntax;
        Context {
            path: self.canonical_path(item).unwrap_or_default(),
            file: data.file.path.clone(),
            line: data.head_line(),
            lines: syntax.item_lines(&syntax.items[data.index]),
            package: db.package(package).name.clone(),
            directives: self.closure_directives(item),
            module: db.module_path(data.module),
            names,
            apis,
        }
    }

    /// The manifest directives of an item's closure set, in canonical order: its package's
    /// `language`, `targets` and `entry` directives, the `dep` directive of each alias it uses
    /// and the directives naming its module.
    fn closure_directives(&self, item: ItemId) -> Vec<String> {
        let data = self.db.item(item);
        let package = self.db.package(self.db.package_of(item));
        let module = self.db.module(data.module);
        let module = if module.root {
            &package.name
        } else {
            &module.name
        };
        let syntax = &data.file.syntax;
        let span = &syntax.items[data.index];
        let aliases: HashSet<&str> = syntax
            .path_starts(span.first..span.end)
            .map(|index| syntax.text(index))
            .collect();
        let manifest = &package.source.files[0].syntax;
        let Some(tree) = manifest
            .items
            .iter()
            .find_map(|item| item.tree.as_ref().filter(|_| !item.abandoned))
        else {
            return Vec::new();
        };
        let mut found: Vec<((u8, String), String)> = Vec::new();
        for node in tree.descendants(tree.root()) {
            if tree.rule(node) != Rule::Directive {
                continue;
            }
            let Some(inner) = tree.child_nodes(node).next() else {
                continue;
            };
            let words: Vec<&str> = tree
                .descendants(inner)
                .into_iter()
                .flat_map(|node| tree.child_tokens(node).collect::<Vec<_>>())
                .filter(|token| manifest.tokens[*token as usize].kind == TokenKind::Ident)
                .map(|token| manifest.text(token as usize))
                .collect();
            let key = match tree.rule(inner) {
                Rule::LanguageDirective => (0, String::new()),
                Rule::TargetsDirective => (1, String::new()),
                Rule::EntryDirective => (2, String::new()),
                Rule::DepDirective if words.get(1).is_some_and(|alias| aliases.contains(alias)) => {
                    (3, words[1].to_owned())
                }
                Rule::AllowDirective
                    if tree.find(inner, Rule::NameList).is_some_and(|names| {
                        tree.child_tokens(names)
                            .any(|token| manifest.text(token as usize) == module)
                    }) =>
                {
                    let (first, last) = tree
                        .span(tree.find(inner, Rule::Authority).expect("an authority"))
                        .expect("tokens");
                    let authority: Vec<&str> = (first..=last)
                        .map(|token| manifest.text(token as usize))
                        .collect();
                    (4, authority.join(" "))
                }
                Rule::DenyDirective if words[1..].contains(&module.as_str()) => {
                    (5, words.join(" "))
                }
                _ => continue,
            };
            let line = manifest.tokens
                [tree.span(inner).expect("a directive holds tokens").0 as usize]
                .line;
            let text = crate::output::printable(manifest.line_text(line));
            found.push((key, text.trim_start().to_owned()));
        }
        found.sort();
        found.into_iter().map(|(_, text)| text).collect()
    }
}

/// The items a type names: its own, its arguments' and those of its parts.
fn type_items(ty: &ty::Ty, found: &mut Vec<ItemId>) {
    match ty {
        ty::Ty::Item(item, args) => {
            found.push(*item);
            for arg in args.iter() {
                if let ty::Arg::Ty(arg) = arg {
                    type_items(arg, found);
                }
            }
        }
        ty::Ty::Fn(function) => {
            for param in &function.params {
                type_items(&param.ty, found);
            }
            type_items(&function.result, found);
            if let Some(fails) = &function.fails {
                type_items(fails, found);
            }
        }
        ty::Ty::Dyn(bound) => found.push(bound.interface),
        ty::Ty::Registers(item) => found.push(*item),
        _ => {}
    }
}

/// The items the types of an expression and of those it holds name, for those starting at a
/// token `starts` accepts.
fn expression_types(
    expr: &super::tir::TExpr,
    starts: &dyn Fn(Tok) -> bool,
    found: &mut Vec<ItemId>,
) {
    if starts(expr.token) {
        type_items(&expr.ty, found);
    }
    expr.each_child(&mut |child| expression_types(child, starts, found));
}

/// An item's last-green entry: its canonical path and location, its interface hash and its
/// `nan show --api` lines.
pub struct Green {
    pub path: String,
    pub file: String,
    pub line: u32,
    pub interface: Option<crate::hash::Hash>,
    pub api: Vec<String>,
}

/// The last-green entries of the workspace's items: of those whose heads lie in a scope, if any,
/// and of those in the closure sets of `lines`, each once, by path.
pub fn greens(workspace: &Workspace, scope: Option<&Scope>, lines: &[(String, u32)]) -> Vec<Green> {
    let std = crate::stdlib::packages();
    let checkers = checkers(workspace, std);
    let mut found: Vec<Green> = Vec::new();
    let mut listed: HashSet<(String, u32)> = HashSet::new();
    for (_, checker) in &checkers {
        let db = &checker.db;
        db.record();
        let mut items: Vec<ItemId> = Vec::new();
        for index in 0..db.items.len() {
            let item = ItemId(index as u32);
            let data = db.item(item);
            if !db.is_reported(item) {
                continue;
            }
            if scope.is_some_and(|scope| {
                scope.holds(workspace, &data.file.path, Some(data.head_line()))
            }) {
                items.push(item);
            }
            let held = lines.iter().filter(|(path, line)| {
                *path == data.file.path
                    && data
                        .syntax()
                        .item_lines(&data.syntax().items[data.index])
                        .contains(line)
            });
            for (_, line) in held {
                items.push(item);
                items.extend(
                    checker
                        .line_closure(item, *line)
                        .into_iter()
                        .filter(|read| db.is_reported(*read)),
                );
            }
        }
        for item in items {
            let data = db.item(item);
            if !listed.insert((data.file.path.clone(), data.head_line())) {
                continue;
            }
            let mut api = checker.api_lines(item, true, None);
            api.extend(digest_lines(&checkers, checker, item));
            found.push(Green {
                path: checker
                    .canonical_path(item)
                    .or_else(|| checker.own_path(item))
                    .unwrap_or_default(),
                file: data.file.path.clone(),
                line: data.head_line(),
                interface: checker.hashes(item).interface,
                api,
            });
        }
    }
    found.sort_by(|left, right| {
        (&left.path, &left.file, left.line).cmp(&(&right.path, &right.file, right.line))
    });
    found
}

/// The facts checking each item of the workspace reads that its context pack lacks: each item read
/// outside the core vocabulary with neither a record of its own nor, for an impl line, the record
/// of the type or interface showing it.
pub fn context_gaps(workspace: &Workspace) -> Vec<String> {
    let std = crate::stdlib::packages();
    let checkers = checkers(workspace, std);
    let mut gaps = Vec::new();
    for (target, checker) in &checkers {
        let db = &checker.db;
        db.record();
        for index in 0..db.items.len() {
            let item = ItemId(index as u32);
            let first =
                check_targets(db.package(db.package_of(item)).source).first() == Some(target);
            if !db.is_reported(item) || !first {
                continue;
            }
            let pack = checker.context(&checkers, item, None);
            let shown: HashSet<&str> = pack.apis.iter().map(|api| api.path.as_str()).collect();
            let core = db.core();
            for read in db.read_by(item) {
                let ast = db.declaration(read);
                let vocabulary = db.package_of(read) == core
                    && ast
                        .name()
                        .is_some_and(|name| super::resolve::is_core_vocabulary(&name));
                let path = checker
                    .canonical_path(read)
                    .or_else(|| checker.own_path(read))
                    .unwrap_or_default();
                let present = vocabulary
                    || read == item
                    || shown.contains(path.as_str())
                    || match &checker.sig(read).kind {
                        super::sig::SigKind::Impl {
                            target, interface, ..
                        } => {
                            let named = |named: ItemId| {
                                pack.apis.iter().any(|api| {
                                    api.path == checker.canonical_path(named).unwrap_or_default()
                                        && api.lines.iter().any(|line| line.starts_with("impl "))
                                })
                            };
                            matches!(target, Some(TypeKey::Item(target)) if named(*target))
                                || interface
                                    .as_ref()
                                    .is_some_and(|bound| named(bound.interface))
                        }
                        _ => false,
                    };
                if !present {
                    gaps.push(format!("{} reads {path}", pack.path));
                }
            }
        }
    }
    gaps
}
