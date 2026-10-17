//! Unused code under one target: the private items no reachable item names, the bindings no
//! expression uses, the fields no item reads, the dependencies no item names and the manifest
//! grants nothing uses.

use std::collections::{HashMap, HashSet};

use super::Checker;
use super::db::{ItemId, ModuleId, PackageId};
use super::props::Property;
use super::testing::Target;
use super::tir::{Local, LocalId, LocalKind, TExpr, TKind};
use crate::diagnostic::{Code, Diagnostic, Fix, Replacement};
use crate::syntax::ast::{FnModifier, ItemKind, StructMember, Tok, Visibility};
use crate::syntax::grammar::{Punct, Rule};
use crate::syntax::token::TokenKind;
use crate::syntax::tree::Tree;
use crate::workspace::Authority;

/// The constructs whose ends end the scopes of the bindings they hold.
const SCOPES: [Rule; 8] = [
    Rule::Block,
    Rule::LoopBlock,
    Rule::FnBody,
    Rule::MethodBody,
    Rule::ForStmt,
    Rule::MatchArm,
    Rule::ClosureBody,
    Rule::TrustedBody,
];

/// The derives whose methods read every field.
const READING: [&str; 5] = ["Eq", "Ord", "Hash", "Render", "Encode"];

/// A `let` or `var` statement binding one name without `else`: its first token, its
/// initializer's first and last tokens, and whether the initializer has type `Unit` or `Never`.
#[derive(Clone)]
pub struct Binding {
    pub statement: Tok,
    pub value: (Tok, Tok),
    pub unit: bool,
}

/// What typing a body records of the use of its bindings: the `let` and `var` statements
/// binding one name, the bindings expressions use and those statements assign, by the binding
/// outside closures, the `var` bindings of parts, assigning which writes the part, and the
/// bindings of the later alternatives of an arm by those of its first that they stand for.
#[derive(Clone, Default)]
pub struct Usage {
    pub bindings: HashMap<LocalId, Binding>,
    pub used: HashSet<LocalId>,
    pub assigned: HashSet<LocalId>,
    pub parts: HashSet<LocalId>,
    pub alternatives: HashMap<LocalId, Vec<LocalId>>,
}

/// A `label: value` naming a field in a construction or struct pattern of an item: its tokens,
/// and whether its value is a place, a literal or `_`.
pub struct FieldLabel {
    pub item: ItemId,
    pub first: Tok,
    pub last: Tok,
    pub simple: bool,
}

impl Checker<'_> {
    /// Records that a body reads a field, when it is a struct's.
    pub fn record_read(&self, owner: ItemId, field: &str) {
        if matches!(self.db.declaration(owner).kind, ItemKind::Struct(_)) {
            self.fields_read
                .borrow_mut()
                .insert((owner, field.to_owned()));
        }
    }

    /// Records a `label: value` naming a field, when it is a struct's.
    pub fn record_label(&self, owner: ItemId, field: &str, label: FieldLabel) {
        if matches!(self.db.declaration(owner).kind, ItemKind::Struct(_)) {
            self.field_labels
                .borrow_mut()
                .entry((owner, field.to_owned()))
                .or_default()
                .push(label);
        }
    }

    /// The unused code of a package: its private items no reachable item names, its fields no
    /// item reads, its dependencies no item names and its grants nothing uses.
    pub fn check_unused(&self, package: PackageId) {
        let functions = self.member_functions();
        for &module in &self.db.package(package).modules {
            self.check_unused_items(module, &functions);
            self.check_unused_fields(module);
        }
        let unused_grants = self.unused_grants(package);
        self.check_unused_dependencies(package, &unused_grants);
        let path = &self.db.package(package).source.files[0].path;
        for (line, _) in unused_grants {
            self.unused
                .borrow_mut()
                .push(Diagnostic::at(Code::UnusedGrant, path, line));
        }
    }

    /// The `dep` directives of a package whose alias no item names and no `allow contribute`
    /// directive lists as a name among the grants nothing uses, each with the fix deleting it.
    fn check_unused_dependencies(&self, package: PackageId, unused_grants: &[(u32, String)]) {
        let data = self.db.package(package);
        let Some(manifest) = &data.source.manifest else {
            return;
        };
        let contributing: HashSet<&str> = manifest
            .allows
            .iter()
            .filter(|allow| matches!(allow.authority, Authority::Contribute(_)))
            .flat_map(|allow| {
                allow
                    .names
                    .iter()
                    .filter(|name| !unused_grants.contains(&(allow.line, (*name).clone())))
            })
            .map(String::as_str)
            .collect();
        let mut named: HashSet<&str> = HashSet::new();
        for &module in &data.modules {
            for &item in &self.db.module(module).items {
                let data = self.db.item(item);
                let syntax = data.syntax();
                let span = &syntax.items[data.index];
                named.extend(
                    syntax
                        .path_starts(span.first..span.end)
                        .map(|index| syntax.text(index)),
                );
            }
        }
        let file = &data.source.files[0];
        for dep in &manifest.deps {
            let alias = dep.alias.as_str();
            if named.contains(alias) || contributing.contains(alias) {
                continue;
            }
            let mut tokens = file
                .syntax
                .tokens
                .iter()
                .filter(|token| token.line == dep.line && !token.kind.is_virtual());
            let span = tokens
                .next()
                .map(|first| (first.start, tokens.next_back().unwrap_or(first).end));
            let fixes = self.fix_dependency_deleted(package, alias);
            self.unused.borrow_mut().push(Diagnostic {
                span,
                fixes: fixes.into_iter().collect(),
                ..Diagnostic::at(Code::UnusedDependency, &file.path, dep.line)
            });
        }
    }

    /// The private items of a module that no reachable item names, each reported on its first
    /// line with the fix deleting it. Reachable are its items that are not private, its tests,
    /// impl lines, contributions and const asserts, its landing fns, the function its package's
    /// `entry` directive names, and each private item a reachable item names, a test naming its
    /// target; a tombstone is neither reachable nor unused.
    fn check_unused_items(&self, module: ModuleId, functions: &HashMap<&str, Vec<ItemId>>) {
        let items = &self.db.module(module).items;
        let private = |item: ItemId| {
            let ast = self.db.ast(item);
            let landing = matches!(&ast.kind, ItemKind::Fn(function)
                if function.modifier.is_some_and(|(modifier, _)| modifier == FnModifier::Landing));
            ast.visibility == Visibility::Private
                && matches!(
                    ast.kind,
                    ItemKind::Fn(_)
                        | ItemKind::Struct(_)
                        | ItemKind::Enum(_)
                        | ItemKind::Interface(_)
                        | ItemKind::Layout(_)
                        | ItemKind::Registers(_)
                        | ItemKind::Const(_)
                        | ItemKind::Registry(_)
                )
                && !landing
                && !self.names_entry(item)
        };
        let mut reached: HashSet<ItemId> = items
            .iter()
            .copied()
            .filter(|item| {
                !private(*item) && !matches!(self.db.ast(*item).kind, ItemKind::Retired(_))
            })
            .collect();
        let mut pending: Vec<ItemId> = reached.iter().copied().collect();
        while let Some(item) = pending.pop() {
            let mut names = self.names(item, functions);
            if let ItemKind::Test(test) = &self.db.ast(item).kind {
                names.extend(self.test_target(item, test).map(|target| match target {
                    Target::Function(function) => function,
                    Target::Derived(ty) => ty,
                }));
            }
            for named in names {
                if self.db.module_of(named) == module && private(named) && reached.insert(named) {
                    pending.push(named);
                }
            }
        }
        for &item in items {
            if !private(item) || reached.contains(&item) {
                continue;
            }
            let data = self.db.item(item);
            let syntax = data.syntax();
            let line = *syntax.item_lines(&syntax.items[data.index]).start();
            let fixes = vec![self.fix_deleting_item(item)];
            self.report_unused(
                item,
                Diagnostic {
                    fixes,
                    ..Diagnostic::at(Code::UnusedItem, "", line)
                },
            );
        }
    }

    /// The fields of a module's structs that no item of the package reads, but the `pub` fields
    /// of `pub` structs, each reported on its first line. A field is read by an access other
    /// than the target of `=`, a struct pattern giving it a pattern other than `_`, a predicate
    /// naming it, and the methods its struct's derives of `Eq`, `Ord`, `Hash`, `Render` and
    /// `Encode` write.
    fn check_unused_fields(&self, module: ModuleId) {
        for &item in &self.db.module(module).items {
            let ast = self.db.ast(item);
            let ItemKind::Struct(structure) = &ast.kind else {
                continue;
            };
            let derived = structure
                .derives
                .iter()
                .any(|entry| READING.contains(&entry.text.as_str()));
            if derived {
                continue;
            }
            let mut predicated: HashSet<String> = HashSet::new();
            for member in &structure.members {
                let condition = match member {
                    StructMember::Field(field) => field.where_clause.as_ref(),
                    StructMember::Invariant(condition) => Some(condition),
                };
                condition.into_iter().for_each(|condition| {
                    condition.visit_paths(&mut |path| {
                        if let [name] = &path.segments[..] {
                            predicated.insert(name.text.clone());
                        }
                    });
                });
            }
            for field in structure.fields() {
                let exempt =
                    field.visibility == Visibility::Pub && ast.visibility == Visibility::Pub;
                let key = (item, field.name.text.clone());
                if exempt
                    || predicated.contains(&field.name.text)
                    || self.fields_read.borrow().contains(&key)
                {
                    continue;
                }
                let syntax = self.db.item(item).syntax();
                let first = documented_from(syntax, field.token);
                let last = field
                    .where_clause
                    .as_ref()
                    .map_or(field.ty.last, |condition| condition.last);
                let fixes = self.fix_field_deleted(item, first, last, &field.name.text);
                let line = syntax.tokens[first as usize].line;
                let span = (
                    syntax.tokens[field.token as usize].start,
                    syntax.tokens[last as usize].end,
                );
                self.report_unused(
                    item,
                    Diagnostic {
                        span: Some(span),
                        fixes: fixes.into_iter().collect(),
                        ..Diagnostic::at(Code::UnusedField, "", line)
                    },
                );
            }
        }
    }

    /// The fix deleting the lines of a struct's field, its tokens `first..=last`, and its
    /// `label: value` in each construction and struct pattern; none when one of these values is
    /// not a place, a literal or `_`, or is its list's only element.
    fn fix_field_deleted(&self, item: ItemId, first: Tok, last: Tok, name: &str) -> Option<Fix> {
        let mut fix = self.fix_deleting_lines(item, first, last);
        let labels = self.field_labels.borrow();
        let mut seen = HashSet::new();
        for label in labels.get(&(item, name.to_owned())).into_iter().flatten() {
            if !seen.insert((label.item, label.first)) {
                continue;
            }
            if !label.simple {
                return None;
            }
            fix = fix.and(self.fix_element_deleted(label.item, label.first, label.last)?);
        }
        Some(fix)
    }

    /// The fix deleting a `dep` directive's line, its alias from each grant listing it, and each
    /// grant it leaves listing nothing.
    fn fix_dependency_deleted(&self, package: PackageId, alias: &str) -> Option<Fix> {
        let data = self.db.package(package);
        let manifest = data.source.manifest.as_ref()?;
        let file = &data.source.files[0];
        let syntax = &file.syntax;
        let line = |line: u32| {
            let start = syntax.lines[line as usize - 1].start;
            let end = syntax
                .lines
                .get(line as usize)
                .map_or(syntax.source.len() as u32, |next| next.start);
            (start, end)
        };
        let dep = manifest.deps.iter().find(|dep| dep.alias == alias)?;
        let mut deleted = vec![line(dep.line)];
        for allow in manifest
            .allows
            .iter()
            .filter(|allow| allow.names.iter().any(|name| name == alias))
        {
            if allow.names.len() == 1 {
                deleted.push(line(allow.line));
                continue;
            }
            let tokens: Vec<usize> = (0..syntax.tokens.len())
                .filter(|index| syntax.tokens[*index].line == allow.line)
                .collect();
            let comma = |at: usize| {
                tokens.get(at).is_some_and(|index| {
                    syntax.tokens[*index].kind == TokenKind::Punct(Punct::Comma)
                })
            };
            let at = tokens.iter().rposition(|index| {
                syntax.tokens[*index].kind == TokenKind::Ident && syntax.text(*index) == alias
            })?;
            let (start, end) = if comma(at.wrapping_sub(1)) {
                (
                    syntax.tokens[tokens[at - 1]].start,
                    syntax.tokens[tokens[at]].end,
                )
            } else {
                (
                    syntax.tokens[tokens[at]].start,
                    syntax.tokens[tokens[at + 2]].start,
                )
            };
            deleted.push((start, end));
        }
        Some(Fix(deleted
            .into_iter()
            .map(|(start, end)| {
                let replacement = Replacement {
                    start,
                    end,
                    text: String::new(),
                };
                (file.path.clone(), replacement)
            })
            .collect()))
    }

    /// The bindings of a typed body that no expression uses: `var` bindings, pattern bindings,
    /// those of an arm's alternatives as one, and names `for` patterns bind, whose types are not
    /// linear; none when the body holds a failed construct. The fix of a `let` or `var`
    /// statement binding one name writes `discard` in place of its binding, or nothing when its
    /// initializer has type `Unit` or `Never`, unless a statement assigns it; that of any other
    /// binding writes `_` in place of its name.
    pub fn check_unused_locals(
        &self,
        item: ItemId,
        locals: &[Local],
        usage: &Usage,
        roots: &[&TExpr],
    ) {
        if roots.iter().any(|root| holds_failure(root)) {
            return;
        }
        let alternative = |id: &LocalId| {
            usage
                .alternatives
                .values()
                .flatten()
                .any(|later| later == id)
        };
        for (index, local) in locals.iter().enumerate() {
            let id = LocalId(index as u32);
            let bound = matches!(
                local.kind,
                LocalKind::Var | LocalKind::Pattern | LocalKind::Element(_)
            );
            if !bound
                || usage.used.contains(&id)
                || alternative(&id)
                || local.ty.has_error()
                || self.has(&local.ty, Property::Linear)
            {
                continue;
            }
            let (construct, fixes) = match usage.bindings.get(&id) {
                Some(binding) => {
                    let fixes = (!usage.assigned.contains(&id)).then(|| {
                        let tokens = &self.db.item(item).syntax().tokens;
                        let kept = if binding.unit { "" } else { "discard " };
                        let (start, end) = (
                            tokens[binding.statement as usize].start,
                            tokens[binding.value.0 as usize].start,
                        );
                        self.fix_bytes(item, start, end, kept)
                    });
                    ((binding.statement, binding.value.1), fixes)
                }
                None => {
                    let alternatives = usage.alternatives.get(&id).into_iter().flatten();
                    let fix = alternatives
                        .map(|later| locals[later.0 as usize].token)
                        .fold(
                            self.fix_replacing(item, local.token, local.token, "_"),
                            |fix, token| fix.and(self.fix_replacing(item, token, token, "_")),
                        );
                    ((local.token, local.token), Some(fix))
                }
            };
            let data = self.db.item(item);
            let syntax = data.syntax();
            let scope = syntax.items[data.index]
                .tree
                .as_ref()
                .and_then(|tree| scope_end(tree, local.token));
            let judged = scope.map(|end| {
                (
                    syntax.tokens[construct.0 as usize].start,
                    syntax.tokens[end as usize].end,
                )
            });
            self.report_unused(
                item,
                Diagnostic {
                    judged,
                    fixes: fixes.into_iter().collect(),
                    ..self.on(item, construct, Code::UnusedLocal)
                },
            );
        }
    }
}

/// The token starting a member, its doc comment's first line included.
fn documented_from(syntax: &crate::syntax::file::SourceFile, token: Tok) -> Tok {
    let mut first = token as usize;
    while let Some(line_end) = syntax.previous(first)
        && syntax.tokens[line_end].kind == TokenKind::Nl
        && let Some(doc) = syntax.previous(line_end)
        && syntax.tokens[doc].kind == TokenKind::Doc
    {
        first = doc;
    }
    first as Tok
}

/// The last token of the innermost block, loop, arm or closure body of a tree holding a token:
/// where the scope of a binding there ends.
fn scope_end(tree: &Tree, token: Tok) -> Option<Tok> {
    tree.descendants(tree.root())
        .into_iter()
        .filter(|node| SCOPES.contains(&tree.rule(*node)))
        .filter_map(|node| tree.span(node))
        .filter(|(first, last)| (*first..=*last).contains(&token))
        .min_by_key(|(first, last)| last - first)
        .map(|(_, last)| last)
}

/// Whether a typed expression holds a construct that failed a rule.
fn holds_failure(expr: &TExpr) -> bool {
    let mut failed = matches!(expr.kind, TKind::Failed);
    expr.each_child(&mut |child| failed |= holds_failure(child));
    failed
}
