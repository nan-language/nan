//! The semantic checker: the rules of language 1 beyond syntax, applied to the items of the
//! workspace's packages under each target they are checked for. Every fact it computes is a query
//! over one item under one target, memoized for the run.

mod api;
mod authority;
mod body;
mod conform;
mod context;
mod db;
mod decl;
mod digest;
mod edits;
mod expand;
mod failure;
mod flow;
mod head;
mod identity;
mod interface;
mod kernel;
mod lowlevel;
mod package;
mod props;
mod reading;
mod references;
mod registry;
mod resolve;
mod rewrite;
mod shape;
mod sig;
mod threshold;
mod tir;
mod ty;
mod unknown;
mod untrusted;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

pub use context::{Context, Green, context_gaps, contexts, greens};
use db::{Db, ItemId, ModuleId, PackageId};
pub use edits::{identity, moved, renamed};
pub use reading::{api, contributed, declared, expanded, explained, holes, shown};
pub use references::Relocated;
use sig::{GenericDef, Sig};

use crate::diagnostic::{Code, Diagnostic, Fix, Offer, Reason, Role};
use crate::scope::Scope;
use crate::syntax::ast::Tok;
use crate::workspace::{Authority, Package, Target, Workspace};

pub struct Checker<'a> {
    pub db: Db<'a>,
    shapes: RefCell<HashMap<ItemId, Rc<Vec<GenericDef>>>>,
    sigs: RefCell<HashMap<ItemId, Rc<Sig>>>,
    found: RefCell<Vec<Diagnostic>>,
    /// The package being checked.
    package: Cell<Option<PackageId>>,
    /// The impl lines, by the type they target, once indexed.
    impls: RefCell<Option<conform::ImplIndex>>,
    /// While positive, typing reports nothing: a probe of what a construct is.
    muted: Cell<u32>,
    /// The generic parameters of the body being checked, with the bounds that hold in it.
    scope: RefCell<Vec<GenericDef>>,
    /// The properties of struct and enum definitions, by whether a consumer sees them; none while
    /// one is being decided.
    definitions: RefCell<HashMap<(ItemId, bool), Option<props::Props>>>,
    /// The items each checked body names.
    refs: RefCell<HashMap<ItemId, std::collections::BTreeSet<ItemId>>>,
    /// The typed bodies of function items, from their checks or typed for their digests.
    bodies: RefCell<HashMap<ItemId, Rc<tir::TBody>>>,
    digests: RefCell<digest::Digests>,
    /// The authorities each module performs directly, as far as checked.
    performed: RefCell<HashSet<(ModuleId, Authority)>>,
    /// The typed holes of the bodies checked.
    holes: RefCell<Vec<Hole>>,
    /// The members the bodies checked name, each with the item and token writing its name.
    members_named: RefCell<Vec<(ItemId, Tok, Member)>>,
    /// The declared names reported as breaking their convention, with that convention.
    misnamed: RefCell<Vec<(ItemId, Tok, decl::Case)>>,
}

/// A member a name written in a body names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Member {
    /// A function named after a type.
    Function(ItemId),
    /// A method of an interface, by its name.
    Method(ItemId, String),
    Field(ItemId, String),
    Variant(ItemId, String),
}

/// A typed `todo` expression: where it stands, the type expected there, and the bindings in
/// scope there of that type.
pub struct Hole {
    pub item: ItemId,
    pub token: Tok,
    pub expected: ty::Ty,
    pub bindings: Vec<String>,
}

impl<'a> Checker<'a> {
    fn new(db: Db<'a>) -> Checker<'a> {
        Checker {
            db,
            shapes: RefCell::new(HashMap::new()),
            sigs: RefCell::new(HashMap::new()),
            found: RefCell::new(Vec::new()),
            package: Cell::new(None),
            impls: RefCell::new(None),
            muted: Cell::new(0),
            scope: RefCell::new(Vec::new()),
            definitions: RefCell::new(HashMap::new()),
            refs: RefCell::new(HashMap::new()),
            bodies: RefCell::new(HashMap::new()),
            digests: RefCell::new(digest::Digests::default()),
            performed: RefCell::new(HashSet::new()),
            holes: RefCell::new(Vec::new()),
            members_named: RefCell::new(Vec::new()),
            misnamed: RefCell::new(Vec::new()),
        }
    }

    pub fn current_package(&self) -> Option<PackageId> {
        self.package.get()
    }

    /// Reports a diagnostic on a line of an item's file; std's items report nothing.
    pub fn report_line(&self, item: ItemId, line: u32, code: Code) {
        self.report_diagnostic(item, Diagnostic::at(code, "", line));
    }

    /// Reports a diagnostic on one of an item's tokens.
    pub fn report(&self, item: ItemId, token: Tok, code: Code) {
        self.report_diagnostic(item, self.on(item, (token, token), code));
    }

    /// Reports a diagnostic with its expected and found texts.
    pub fn report_expected(
        &self,
        item: ItemId,
        token: Tok,
        code: Code,
        expected: String,
        found: String,
    ) {
        self.report_diagnostic(
            item,
            Diagnostic {
                expected: Some(expected),
                found: Some(found),
                ..self.on(item, (token, token), code)
            },
        );
    }

    /// Reports a diagnostic with its because-chain.
    pub fn report_because(&self, item: ItemId, token: Tok, code: Code, because: Vec<Reason>) {
        self.report_diagnostic(
            item,
            Diagnostic {
                because,
                ..self.on(item, (token, token), code)
            },
        );
    }

    /// Reports a diagnostic on the construct of an item's tokens `first..=last`, with the fixes
    /// its rule names.
    pub fn report_fixed(
        &self,
        item: ItemId,
        construct: (Tok, Tok),
        code: Code,
        fixes: Vec<Fix>,
        offer: Offer,
    ) {
        self.report_diagnostic(
            item,
            Diagnostic {
                fixes,
                offer,
                ..self.on(item, construct, code)
            },
        );
    }

    /// A diagnostic on the construct of an item's tokens `first..=last`.
    fn on(&self, item: ItemId, (first, last): (Tok, Tok), code: Code) -> Diagnostic {
        let data = self.db.item(item);
        let tokens = &data.file.syntax.tokens;
        Diagnostic {
            span: Some((tokens[first as usize].start, tokens[last as usize].end)),
            ..Diagnostic::at(code, "", data.line(first))
        }
    }

    /// A reason lying on the line of one of an item's tokens.
    pub fn reason(&self, item: ItemId, token: Tok, role: Role, subject: String) -> Reason {
        let data = self.db.item(item);
        Reason {
            role,
            subject,
            at: Some((data.file.path.clone(), data.line(token))),
        }
    }

    fn report_diagnostic(&self, item: ItemId, mut diagnostic: Diagnostic) {
        if self.db.is_std(item) || self.muted.get() > 0 {
            return;
        }
        diagnostic.path = Some(self.db.item(item).file.path.clone());
        diagnostic.item = self.canonical_path(item);
        self.found.borrow_mut().push(diagnostic);
    }

    /// Reports what resolving an item's signature or type expressions found.
    pub fn report_problems(&self, item: ItemId, problems: &sig::Problems) {
        for problem in &problems.found {
            self.report_problem(item, problem.token, problem.code);
        }
        for (ty, token) in &problems.instantiations {
            self.check_instantiation(item, ty, *token);
        }
    }

    /// Reports a diagnostic on a line of a workspace file outside every item.
    pub fn report_file(&self, path: &str, line: u32, code: Code) {
        self.report_file_fixed(path, line, code, Vec::new());
    }

    /// Reports a diagnostic on a line of a file with the fixes its rule names.
    pub fn report_file_fixed(&self, path: &str, line: u32, code: Code, fixes: Vec<Fix>) {
        self.found.borrow_mut().push(Diagnostic {
            fixes,
            ..Diagnostic::at(code, path, line)
        });
    }
}

/// The diagnostics check time reports on lines of a scope, those of loading, lexing and parsing
/// included, by path, line and code, each once, each located on its reported construct's bytes.
pub fn reported(workspace: &Workspace, scope: &Scope) -> Vec<Diagnostic> {
    let mut found: Vec<Diagnostic> = workspace.diagnostics().collect();
    found.extend(diagnostics(workspace));
    for (_, file) in workspace.files() {
        found.extend(file.syntax.diagnostics.iter().map(|diagnostic| {
            Diagnostic {
                expected: diagnostic.expected.clone(),
                found: diagnostic.found.clone(),
                span: diagnostic.span,
                failing: diagnostic.failing,
                fixes: diagnostic
                    .fixes
                    .iter()
                    .map(|fix| {
                        Fix(fix
                            .iter()
                            .map(|replacement| (file.path.clone(), replacement.clone()))
                            .collect())
                    })
                    .collect(),
                offer: diagnostic.offer,
                ..Diagnostic::at(diagnostic.code, &file.path, diagnostic.line)
            }
        }));
    }
    found.retain(|diagnostic| {
        diagnostic
            .path
            .as_deref()
            .is_none_or(|path| scope.holds(workspace, path, diagnostic.line))
    });
    for diagnostic in &mut found {
        if diagnostic.span.is_some() {
            continue;
        }
        let (Some(path), Some(line)) = (&diagnostic.path, diagnostic.line) else {
            continue;
        };
        if let Some((_, file)) = workspace.file(path) {
            let line = &file.syntax.lines[line as usize - 1];
            diagnostic.span = Some((line.start, line.end));
        }
    }
    found.sort_by(|left, right| {
        (&left.path, left.line, left.code.name()).cmp(&(&right.path, right.line, right.code.name()))
    });
    found.dedup_by(|later, earlier| {
        Diagnostic {
            span: earlier.span,
            ..later.clone()
        } == *earlier
    });
    found
}

/// The targets a package is checked under: `any` alone, or each concrete target it declares.
fn check_targets(package: &Package) -> Vec<Target> {
    let Some(manifest) = package
        .manifest
        .as_ref()
        .filter(|manifest| manifest.supported)
    else {
        return Vec::new();
    };
    if manifest.targets.is_empty() || manifest.targets.contains(&Target::Any) {
        return vec![Target::Any];
    }
    manifest.targets.clone()
}

/// The targets the workspace's packages are checked under, in order.
pub fn targets(workspace: &Workspace) -> Vec<Target> {
    let mut targets: Vec<Target> = workspace.packages.iter().flat_map(check_targets).collect();
    targets.sort();
    targets.dedup();
    targets
}

/// The workspace's packages checked under a target, with their ids in its database.
fn checked_packages<'w>(
    checker: &Checker<'w>,
    workspace: &'w Workspace,
    target: Target,
) -> Vec<(usize, PackageId)> {
    workspace
        .packages
        .iter()
        .enumerate()
        .filter(|(_, package)| check_targets(package).contains(&target))
        .filter_map(|(index, package)| {
            let id = checker
                .db
                .packages
                .iter()
                .position(|data| std::ptr::eq(data.source, package))?;
            Some((index, PackageId(id as u32)))
        })
        .collect()
}

/// The checker of a workspace under a target, once it has checked each package checked under
/// it.
pub fn checked<'a>(workspace: &'a Workspace, std: &'a [Package], target: Target) -> Checker<'a> {
    let mut db = Db::build(workspace, std, target);
    db.link();
    let checker = Checker::new(db);
    for (_, package) in checked_packages(&checker, workspace, target) {
        checker.package.set(Some(package));
        checker.check_package(package);
    }
    checker.fix_misnamed();
    checker
}

/// The checker of a workspace under a target before it checks anything.
pub fn unchecked<'a>(workspace: &'a Workspace, std: &'a [Package], target: Target) -> Checker<'a> {
    let mut db = Db::build(workspace, std, target);
    db.link();
    Checker::new(db)
}

/// An unchecked checker of the workspace under each target its packages are checked for.
fn checkers<'a>(workspace: &'a Workspace, std: &'a [Package]) -> Vec<(Target, Checker<'a>)> {
    targets(workspace)
        .into_iter()
        .map(|target| (target, unchecked(workspace, std, target)))
        .collect()
}

/// The diagnostics of the workspace's items, under each target their packages are checked for,
/// and of the manifest grants nothing uses under any of them.
pub fn diagnostics(workspace: &Workspace) -> Vec<Diagnostic> {
    let std = crate::stdlib::packages();
    let mut found = Vec::new();
    let mut unused: HashMap<(usize, u32, String), usize> = HashMap::new();
    for target in targets(workspace) {
        let checker = checked(workspace, &std, target);
        for (index, package) in checked_packages(&checker, workspace, target) {
            for (line, name) in checker.unused_grants(package) {
                *unused.entry((index, line, name)).or_default() += 1;
            }
        }
        found.extend(checker.found.into_inner());
    }
    for ((index, line, _), count) in unused {
        let package = &workspace.packages[index];
        if count == check_targets(package).len() {
            found.push(Diagnostic::at(
                Code::UnusedGrant,
                &package.files[0].path,
                line,
            ));
        }
    }
    found
}
