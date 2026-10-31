//! What the reading commands print of the workspace: declarations, contributions, holes, items
//! with their hashes and api records, each listed once across the targets they are checked under,
//! and a function's explanation or a type's derived items.

use std::collections::HashSet;
use std::ops::RangeInclusive;

use super::db::{ItemId, ModuleId, PackageId, TypeKey};
use super::expand::DerivedItem;
use super::identity::Hashes;
use super::reversibility::Class;
use super::{
    Checker, check_targets, checked_closures, checked_packages, checkers, decl, ty, unchecked,
};
use crate::nir::{Builtin, Callable, Callee, CheckKind, Op, UnaryOp, Unit};
use crate::prover::{Obligated, Obligation, Status, Unproved};
use crate::scope::Scope;
use crate::syntax::ast::{ItemKind, Tok};
use crate::workspace::{Target, Workspace};

/// A hole as `nan holes` lists it: its location, expected type and producers.
pub struct Listed {
    pub path: String,
    pub line: u32,
    pub expected: String,
    pub producers: Vec<String>,
}

/// A declaration `nan find` lists: its canonical path, file and line, and whether a workspace
/// package declares it.
pub struct Declared {
    pub path: String,
    pub file: String,
    pub line: u32,
    /// The declaration's first line.
    pub text: String,
    pub workspace: bool,
}

/// The items and members of the workspace and its packages' dependency closures whose folded
/// name is `name` folded, under each target, each once: workspace ones first, then by file and
/// line.
pub fn declared(workspace: &Workspace, name: &str) -> Vec<Declared> {
    let std = crate::stdlib::packages();
    let wanted = decl::fold(name);
    let mut found: Vec<Declared> = Vec::new();
    for (_, checker) in checkers(workspace, std) {
        let db = &checker.db;
        for index in 0..db.items.len() {
            let item = ItemId(index as u32);
            let data = db.item(item);
            let mut add = |path: String, token: Tok| {
                let line = data.line(token);
                let entry = Declared {
                    path,
                    file: data.file.path.clone(),
                    line,
                    text: crate::output::printable(data.file.syntax.line_text(line)),
                    workspace: db.is_reported(item),
                };
                if !found.iter().any(|other| {
                    (&other.file, other.line, &other.path) == (&entry.file, entry.line, &entry.path)
                }) {
                    found.push(entry);
                }
            };
            let named = data.ast.name().unwrap_or_default();
            let member = named.rsplit('.').next().unwrap_or_default();
            if !member.is_empty() && decl::fold(member) == wanted {
                add(db.canonical(item), data.ast.token);
            }
            for (member, token) in checker.members_declared(item) {
                if decl::fold(&member) == wanted {
                    add(format!("{}.{member}", db.canonical(item)), token);
                }
            }
        }
    }
    found.sort_by(|left, right| {
        (!left.workspace, &left.file, left.line).cmp(&(!right.workspace, &right.file, right.line))
    });
    found
}

/// A contribution `nan find --contributors` lists: its registry's and its own canonical paths,
/// file and line, and whether the judging root admits it.
pub struct Contributed {
    pub registry: String,
    pub entry: String,
    pub file: String,
    pub line: u32,
    pub admitted: bool,
}

/// The contributions to the registry written `registry` in the dependency closure of the
/// workspace package `root`, judged with it as judging root, under each target it is checked
/// under, each once; none when `registry` names no registry from its root module.
pub fn contributed(workspace: &Workspace, root: usize, registry: &str) -> Option<Vec<Contributed>> {
    let std = crate::stdlib::packages();
    let mut found: Vec<Contributed> = Vec::new();
    let mut named = false;
    let closure = workspace.closure_of(&workspace.packages[root])?;
    for target in check_targets(&workspace.packages[root]) {
        let checker = unchecked(workspace, closure, std, target);
        let Some((_, package)) = checked_packages(&checker)
            .into_iter()
            .find(|(index, _)| *index == root)
        else {
            continue;
        };
        let Ok(wanted) = checker.granted_registry(package, registry) else {
            continue;
        };
        named = true;
        for contribution in checker.contributions(package) {
            if contribution.registry != wanted {
                continue;
            }
            let data = checker.db.item(contribution.item);
            let entry = Contributed {
                registry: checker.db.canonical(wanted),
                entry: checker.db.canonical(contribution.item),
                file: data.file.path.clone(),
                line: data.line(data.ast.token),
                admitted: contribution.admitted,
            };
            if !found
                .iter()
                .any(|other| (&other.file, other.line) == (&entry.file, entry.line))
            {
                found.push(entry);
            }
        }
    }
    found.sort_by(|left, right| (&left.file, left.line).cmp(&(&right.file, right.line)));
    named.then_some(found)
}

/// The holes of the workspace's items under each target their packages are checked for, each
/// once, by path and line: their expected types, and as producers the bindings in scope of
/// that type and the functions their module may name whose result is that type.
pub fn holes(workspace: &Workspace) -> Vec<Listed> {
    let std = crate::stdlib::packages();
    let mut listed: Vec<Listed> = Vec::new();
    for (_, checker) in checked_closures(workspace, std) {
        for hole in checker.holes.borrow().iter() {
            let data = checker.db.item(hole.item);
            let module = data.module;
            let mut producers = hole.bindings.clone();
            producers.extend(checker.producers(module, &hole.expected));
            let entry = Listed {
                path: data.file.path.clone(),
                line: data.line(hole.token),
                expected: checker.show(&hole.expected, module),
                producers,
            };
            if !listed
                .iter()
                .any(|other| (&other.path, other.line) == (&entry.path, entry.line))
            {
                listed.push(entry);
            }
        }
    }
    listed.sort_by(|left, right| (&left.path, left.line).cmp(&(&right.path, right.line)));
    listed
}

impl Checker<'_> {
    /// The functions a module may name whose result is a type, as it writes them, in byte
    /// order.
    fn producers(&self, module: ModuleId, ty: &ty::Ty) -> Vec<String> {
        let mut found: Vec<String> = (0..self.db.items.len())
            .map(|index| ItemId(index as u32))
            .filter(|&item| {
                matches!(self.db.ast(item).kind, ItemKind::Fn(_))
                    && self.db.visible(module, item)
                    && self
                        .fn_sig(item)
                        .is_some_and(|sig| sig.own_generics().is_empty() && sig.result == *ty)
            })
            .map(|item| self.item_written(item, module))
            .collect();
        found.sort();
        found.dedup();
        found
    }
}

/// A parsed item of a workspace package as `nan show` prints it.
pub struct Shown {
    /// Its canonical path, if it has one.
    pub path: Option<String>,
    pub file: String,
    /// The line of its head.
    pub line: u32,
    /// Its lines, doc comments included.
    pub lines: RangeInclusive<u32>,
    pub hashes: Hashes,
}

/// The parsed items of the workspace's packages whose heads lie in a scope, each once, by path and
/// line.
pub fn shown(workspace: &Workspace, scope: &Scope) -> Vec<Shown> {
    let std = crate::stdlib::packages();
    let mut shown: Vec<Shown> = Vec::new();
    let mut listed: HashSet<(String, u32)> = HashSet::new();
    for (_, checker) in checkers(workspace, std) {
        let db = &checker.db;
        let mut found: Vec<(&crate::workspace::File, usize, Option<ItemId>, PackageId)> =
            Vec::new();
        for (index, package) in db.packages.iter().enumerate() {
            if !package.carried && !package.quiet {
                let package = PackageId(index as u32);
                let manifest = &db.package(package).source.files[0];
                let item = manifest
                    .syntax
                    .items
                    .iter()
                    .position(|item| item.tree.is_some() && !item.abandoned);
                found.extend(item.map(|item| (manifest, item, None, package)));
            }
        }
        for index in 0..db.items.len() {
            let item = ItemId(index as u32);
            if db.is_reported(item) {
                let data = db.item(item);
                found.push((data.file, data.index, Some(item), db.package_of(item)));
            }
        }
        for (file, index, item, package) in found {
            let syntax = &file.syntax.items[index];
            let lines = file.syntax.item_lines(syntax);
            let head = match item {
                Some(item) => db.item(item).head_line(),
                None => file.lowered()[index]
                    .as_ref()
                    .map_or(*lines.start(), |ast| {
                        file.syntax.tokens[ast.token as usize].line
                    }),
            };
            if !scope.holds(workspace, &file.path, Some(head))
                || !listed.insert((file.path.clone(), head))
            {
                continue;
            }
            let (path, hashes) = match item {
                Some(item) => (checker.canonical_path(item), checker.hashes(item)),
                None => (
                    Some(db.package(package).name.clone()),
                    checker.package_hashes(package).expect("a package item"),
                ),
            };
            shown.push(Shown {
                path,
                file: file.path.clone(),
                line: head,
                lines,
                hashes,
            });
        }
    }
    shown.sort_by(|left, right| (&left.file, left.line).cmp(&(&right.file, right.line)));
    shown
}

/// An item consumers see, as `nan show --api` prints it.
#[derive(Clone)]
pub struct Api {
    pub path: String,
    /// The name of its package.
    pub package: String,
    pub file: String,
    pub line: u32,
    pub lines: Vec<String>,
}

/// The items of a scope that consumers see, with the functions named after a type the scope
/// names, each once, by path and line; a function's record ends with its digest line under each
/// target its package declares, in byte order, each followed by its target when there are several.
pub fn api(workspace: &Workspace, scope: &Scope) -> Vec<Api> {
    let std = crate::stdlib::packages();
    let checkers = checkers(workspace, std);
    let mut found: Vec<Api> = Vec::new();
    let mut listed: HashSet<(String, u32)> = HashSet::new();
    for (_, checker) in &checkers {
        let db = &checker.db;
        let mut items: Vec<ItemId> = Vec::new();
        for index in 0..db.items.len() {
            let item = ItemId(index as u32);
            let data = db.item(item);
            if !db.is_reported(item)
                || !scope.holds(workspace, &data.file.path, Some(data.head_line()))
            {
                continue;
            }
            items.push(item);
            let is_type = matches!(
                data.ast.kind,
                ItemKind::Struct(_) | ItemKind::Enum(_) | ItemKind::Layout(_)
            );
            if matches!(scope, Scope::Lines(..)) && is_type {
                items.extend(
                    db.members
                        .get(&TypeKey::Item(item))
                        .into_iter()
                        .flat_map(|members| members.values().flatten()),
                );
            }
        }
        for item in items {
            let data = db.item(item);
            if !checker.consumers_see(item)
                || !listed.insert((data.file.path.clone(), data.head_line()))
            {
                continue;
            }
            let mut lines = checker.api_lines(item, true, None);
            lines.extend(digest_lines(&checkers, checker, item));
            found.push(Api {
                path: checker.canonical_path(item).unwrap_or_default(),
                package: db.package(db.package_of(item)).name.clone(),
                file: data.file.path.clone(),
                line: data.head_line(),
                lines,
            });
        }
    }
    found.sort_by(|left, right| (&left.file, left.line).cmp(&(&right.file, right.line)));
    found
}

/// A function's digest lines under each target its package declares, in byte order, each
/// followed by its target when there are several; none for another item.
pub(super) fn digest_lines(
    checkers: &[(Target, Checker)],
    checker: &Checker,
    item: ItemId,
) -> Vec<String> {
    let db = &checker.db;
    let data = db.item(item);
    if !matches!(data.ast.kind, ItemKind::Fn(_)) {
        return Vec::new();
    }
    let mut declared = check_targets(db.package(db.package_of(item)).source);
    declared.sort_by_key(|target| target.name());
    let mut lines = Vec::new();
    for target in &declared {
        let Some((_, other)) = checkers.iter().find(|(checked, _)| checked == target) else {
            continue;
        };
        let Some(same) = other.db.same(data) else {
            continue;
        };
        let suffix = if declared.len() > 1 {
            format!(" ({})", target.name())
        } else {
            String::new()
        };
        lines.push(format!("digest: {}{suffix}", other.digest_text(same)));
    }
    lines
}

/// What `nan explain` prints of a function: its digest lines, each fact a line of its text raises
/// directly or through a call, and the methods of primitive capabilities it reaches.
pub struct Explained {
    pub digests: Vec<String>,
    /// Each fact, a class as written, with the file and line raising it, by line.
    pub facts: Vec<(String, String, u32)>,
    pub methods: Vec<String>,
    pub obligations: Vec<Explanation>,
}

/// An obligation of an explained function: its file and line, its kind, its status or, without
/// a trap kind, its outcome, and the labelled values of a refuted one's counterexample.
pub struct Explanation {
    pub path: String,
    pub line: u32,
    pub kind: String,
    pub status: String,
    pub counterexample: Vec<String>,
}

/// What `nan explain` prints of the function whose head lies in a scope, under each target its
/// package declares; none for another item.
pub fn explained(workspace: &Workspace, scope: &Scope) -> Option<Explained> {
    let std = crate::stdlib::packages();
    let checkers = checked_closures(workspace, std);
    for (_, checker) in &checkers {
        let db = &checker.db;
        let item = (0..db.items.len())
            .map(|index| ItemId(index as u32))
            .find(|item| {
                let data = db.item(*item);
                db.is_reported(*item)
                    && scope.holds(workspace, &data.file.path, Some(data.head_line()))
            });
        let Some(item) = item else {
            continue;
        };
        let data = db.item(item);
        let body = checker.typed_body(item)?;
        let mut facts: Vec<(String, String, u32)> = Vec::new();
        for site in checker.text_sites(item, &body) {
            let digest = &site.facts;
            let class = digest.class.text();
            let raised = [
                ("wait", digest.wait),
                ("alloc", digest.alloc),
                ("device", digest.device),
                ("priv", digest.privileged),
                ("sync", digest.sync),
                ("float", digest.float),
                (class.as_str(), digest.class != Class::Traceless),
            ];
            for (fact, holds) in raised {
                let found = (
                    fact.to_owned(),
                    data.file.path.clone(),
                    data.line(site.token),
                );
                if holds && !facts.contains(&found) {
                    facts.push(found);
                }
            }
        }
        let order = |fact: &str| {
            ["wait", "alloc", "device", "priv", "sync", "float"]
                .iter()
                .position(|known| *known == fact)
                .unwrap_or(6)
        };
        facts.sort_by_key(|(fact, _, line)| (*line, order(fact)));
        let mut methods: Vec<String> = checker
            .methods_reached(item)
            .into_iter()
            .map(|method| db.canonical(method))
            .collect();
        methods.sort();
        methods.dedup();
        let obligations = checker.explanations(item);
        return Some(Explained {
            digests: digest_lines(&checkers, checker, item),
            facts,
            methods,
            obligations,
        });
    }
    None
}

/// The ledger entries of a scope, by path and line: of a package, its ledger; of the lines of an
/// item, the item's.
pub fn ledgered(workspace: &Workspace, scope: &Scope) -> Vec<super::ledger::Entry> {
    let std = crate::stdlib::packages();
    for (_, checker) in checkers(workspace, std) {
        let db = &checker.db;
        if let Scope::Package(index) = scope {
            let package = &workspace.packages[*index];
            let id = db
                .packages
                .iter()
                .position(|data| std::ptr::eq(data.source, &**package) && !data.quiet);
            if let Some(id) = id {
                return checker.package_ledger(PackageId(id as u32));
            }
            continue;
        }
        let item = (0..db.items.len())
            .map(|index| ItemId(index as u32))
            .find(|item| {
                let data = db.item(*item);
                db.is_reported(*item)
                    && scope.holds(workspace, &data.file.path, Some(data.head_line()))
            });
        if let Some(item) = item {
            return checker.ledger(item);
        }
    }
    Vec::new()
}

impl Checker<'_> {
    /// The obligations of a function, in site order, each as `nan explain` prints it.
    fn explanations(&self, item: ItemId) -> Vec<Explanation> {
        let Some(unit) = self.unit(item) else {
            return Vec::new();
        };
        let data = self.db.item(item);
        self.obligations(item)
            .iter()
            .map(|site| {
                let trap_kind = !site.role.is_contract()
                    && site.obligation != Obligation::Check(CheckKind::Decreases);
                let (status, counterexample) = match (&site.unproved, trap_kind) {
                    (_, true) => (status_word(site.status).to_owned(), Vec::new()),
                    (None, false) => ("proved".to_owned(), Vec::new()),
                    (Some(Unproved::Refuted(values)), false) => {
                        ("refuted".to_owned(), values.clone())
                    }
                    (Some(Unproved::Unknown(_)), false) => ("unknown".to_owned(), Vec::new()),
                };
                Explanation {
                    path: data.file.path.clone(),
                    line: data.line(site.span.first),
                    kind: obligation_kind(&unit, site),
                    status,
                    counterexample,
                }
            })
            .collect()
    }
}

fn status_word(status: Status) -> &'static str {
    match status {
        Status::Proven => "proven",
        Status::Assumed => "assumed",
        Status::Checked => "checked",
        Status::Open => "open",
    }
}

/// An obligation's kind as `nan explain` names it: its row's name with `-` for spaces, or for
/// an arithmetic, shift, division or conversion obligation the operation's word.
pub(super) fn obligation_kind(unit: &Unit, site: &Obligated) -> String {
    if site.role.is_contract() {
        return "well-definedness".to_owned();
    }
    let operation = || {
        let function = unit.function(site.role, site.index)?;
        let block = function.block(site.block);
        Some(match &block.insts.get(site.position)?.op {
            Op::Binary(op, ..) => op.word(),
            Op::Unary(UnaryOp::Neg, _) => "neg",
            Op::Call(call) => match &call.callee {
                Callee::Fn(fn_ref) => match &fn_ref.callable {
                    Callable::Builtin(Builtin::Conversion(conversion, _)) => conversion.word(),
                    _ => return None,
                },
                _ => return None,
            },
            _ => return None,
        })
    };
    match site.obligation {
        Obligation::Check(CheckKind::Invariant) => "loop-invariant".to_owned(),
        Obligation::Check(kind) => kind.word().to_owned(),
        Obligation::Bounds => "index-bound".to_owned(),
        _ => operation().unwrap_or("layout-field").to_owned(),
    }
}

/// The items the derives of the struct or enum whose head lies in a scope write; none when the
/// scope names no struct or enum.
pub fn expanded(workspace: &Workspace, scope: &Scope) -> Option<Vec<DerivedItem>> {
    let std = crate::stdlib::packages();
    for (_, checker) in checkers(workspace, std) {
        let db = &checker.db;
        let item = (0..db.items.len())
            .map(|index| ItemId(index as u32))
            .find(|item| {
                let data = db.item(*item);
                db.is_reported(*item)
                    && scope.holds(workspace, &data.file.path, Some(data.head_line()))
            });
        let Some(item) = item else {
            continue;
        };
        return matches!(db.ast(item).kind, ItemKind::Struct(_) | ItemKind::Enum(_))
            .then(|| checker.derived(item));
    }
    None
}
