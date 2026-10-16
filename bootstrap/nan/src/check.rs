//! The semantic checker: the rules of language 1 beyond syntax, applied to the items of the
//! workspace's packages under each target they are checked for. Every fact it computes is a query
//! over one item under one target, memoized for the run.

mod api;
mod authority;
mod body;
mod component;
mod conform;
mod context;
pub(crate) mod db;
mod decl;
mod digest;
mod docs;
mod edits;
mod elaborate;
mod expand;
mod failure;
mod flow;
mod generation;
mod head;
mod identity;
mod interface;
mod kernel;
pub(crate) mod lowlevel;
mod package;
mod program;
pub(crate) mod props;
mod reading;
mod references;
mod registry;
mod resolve;
mod rewrite;
mod runs;
mod shape;
pub(crate) mod sig;
mod supplied;
pub(crate) mod testing;
mod threshold;
pub(crate) mod tir;
mod tombstone;
mod traps;
pub(crate) mod ty;
mod unknown;
mod untrusted;
mod unused;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

pub use context::{Context, Green, context_gaps, contexts, greens};
use db::{Db, ItemId, ModuleId, PackageId};
pub use edits::{identity, moved, renamed};
pub use elaborate::EnsuresClause;
pub use reading::{api, contributed, declared, expanded, explained, holes, shown};
pub use references::Relocated;
use sig::{GenericDef, Sig};
pub use supplied::stated;
pub use traps::closure_demands_notrap;

use crate::diagnostic::{Code, Diagnostic, Fix, Offer, Reason, Role};
use crate::scope::Scope;
use crate::syntax::ast::{self, Tok};
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
    /// The item whose body or clauses are being typed.
    within: Cell<Option<ItemId>>,
    /// The properties of struct and enum definitions, by whether a consumer sees them; none while
    /// one is being decided.
    definitions: RefCell<HashMap<(ItemId, bool), Option<props::Props>>>,
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
    /// The typed bodies of interface methods, default bodies and clauses, by interface and
    /// method index.
    method_bodies: RefCell<HashMap<(ItemId, usize), Rc<tir::TBody>>>,
    /// The typed predicates of structs.
    typed_predicates: RefCell<HashMap<ItemId, Rc<elaborate::TypedPredicates>>>,
    /// The typed `where` conditions of items' const parameters.
    typed_wheres: RefCell<HashMap<ItemId, Rc<elaborate::TypedWheres>>>,
    /// The items the check reports an error on, once every package is checked.
    rejections: RefCell<HashSet<ItemId>>,
    /// Whether the rejections of every item are known, once every package is checked; before,
    /// those of the items judged alone.
    judged_all: Cell<bool>,
    /// The items checked, each judged alone.
    judged: RefCell<HashSet<ItemId>>,
    /// The NIR units elaborated, none for an item without one.
    units: RefCell<HashMap<ItemId, Option<Rc<crate::nir::Unit>>>>,
    /// The values const evaluation computed, none for one that ends without a value.
    const_values: RefCell<HashMap<ItemId, Option<Rc<crate::interp::ConstValue>>>>,
    /// The consts and contributions being evaluated, innermost last.
    evaluating: RefCell<Vec<ItemId>>,
    /// The consts and contributions whose values each evaluation needs, by the one evaluated.
    needs: RefCell<std::collections::BTreeMap<ItemId, std::collections::BTreeSet<ItemId>>>,
    /// The consts and contributions whose evaluation needs their own values.
    cyclic: RefCell<HashSet<ItemId>>,
    /// The obligation sites of the units decided, with their statuses.
    pub(crate) obligations: RefCell<HashMap<ItemId, Rc<Vec<crate::prover::Obligated>>>>,
    /// The may-trap fact of the functions whose digests hold it.
    may_traps: RefCell<HashMap<ItemId, bool>>,
    /// The instantiations of items with const parameters that code writes, by the item writing
    /// each, its token, and the item instantiated with its arguments.
    instantiations: RefCell<Vec<(ItemId, Tok, ItemId, crate::check::ty::Args)>>,
    /// The diagnostics of unused code found under the target, reported once found under every
    /// target checking their file.
    unused: RefCell<Vec<Diagnostic>>,
    /// The fields of structs the bodies checked read.
    fields_read: RefCell<HashSet<(ItemId, String)>>,
    /// The `label: value` of each construction and struct pattern the bodies checked write, by
    /// the struct and the field it names.
    field_labels: RefCell<HashMap<(ItemId, String), Vec<unused::FieldLabel>>>,
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
            within: Cell::new(None),
            definitions: RefCell::new(HashMap::new()),
            bodies: RefCell::new(HashMap::new()),
            digests: RefCell::new(digest::Digests::default()),
            performed: RefCell::new(HashSet::new()),
            holes: RefCell::new(Vec::new()),
            members_named: RefCell::new(Vec::new()),
            misnamed: RefCell::new(Vec::new()),
            method_bodies: RefCell::new(HashMap::new()),
            typed_predicates: RefCell::new(HashMap::new()),
            typed_wheres: RefCell::new(HashMap::new()),
            rejections: RefCell::new(HashSet::new()),
            judged_all: Cell::new(false),
            judged: RefCell::new(HashSet::new()),
            units: RefCell::new(HashMap::new()),
            const_values: RefCell::new(HashMap::new()),
            evaluating: RefCell::new(Vec::new()),
            needs: RefCell::new(std::collections::BTreeMap::new()),
            cyclic: RefCell::new(HashSet::new()),
            obligations: RefCell::new(HashMap::new()),
            may_traps: RefCell::new(HashMap::new()),
            instantiations: RefCell::new(Vec::new()),
            unused: RefCell::new(Vec::new()),
            fields_read: RefCell::new(HashSet::new()),
            field_labels: RefCell::new(HashMap::new()),
        }
    }

    pub fn current_package(&self) -> Option<PackageId> {
        self.package.get()
    }

    /// Reports a diagnostic on a line of an item's file; the carried std's items report nothing.
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

    /// A diagnostic a final build reports on the construct of an item's tokens `first..=last`;
    /// none on the carried std's items.
    pub fn diagnostic_on(
        &self,
        item: ItemId,
        construct: (Tok, Tok),
        code: Code,
    ) -> Option<Diagnostic> {
        self.on_item(item, self.on(item, construct, code))
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

    /// Runs a probe whose diagnostics are withheld, returning its result and those diagnostics.
    fn withheld<T>(&self, probe: impl FnOnce() -> T) -> (T, Vec<Diagnostic>) {
        let before = self.found.borrow().len();
        let result = probe();
        let withheld = self.found.borrow_mut().split_off(before);
        (result, withheld)
    }

    /// Whether a diagnostic is reported on the construct starting at one of an item's tokens.
    fn reported_at(&self, item: ItemId, token: Tok) -> bool {
        let data = self.db.item(item);
        let start = data.file.syntax.tokens[token as usize].start;
        self.found.borrow().iter().any(|diagnostic| {
            diagnostic.path.as_deref() == Some(data.file.path.as_str())
                && diagnostic.span.is_some_and(|(at, _)| at == start)
        })
    }

    fn report_diagnostic(&self, item: ItemId, diagnostic: Diagnostic) {
        if let Some(diagnostic) = self.on_item(item, diagnostic) {
            self.found.borrow_mut().push(diagnostic);
        }
    }

    /// Records a diagnostic of unused code on an item, reported once found under every target
    /// checking its file.
    fn report_unused(&self, item: ItemId, diagnostic: Diagnostic) {
        if let Some(diagnostic) = self.on_item(item, diagnostic) {
            self.unused.borrow_mut().push(diagnostic);
        }
    }

    /// A diagnostic located on an item's file and item; none for the carried std's items and in
    /// probes.
    fn on_item(&self, item: ItemId, mut diagnostic: Diagnostic) -> Option<Diagnostic> {
        if self.db.is_carried(item) || self.muted.get() > 0 {
            return None;
        }
        diagnostic.path = Some(self.db.item(item).file.path.clone());
        diagnostic.item = self.canonical_path(item);
        Some(diagnostic)
    }

    /// Reports what resolving an item's signature or type expressions found.
    pub fn report_problems(&self, item: ItemId, problems: &sig::Problems) {
        for problem in &problems.found {
            self.report_problem(item, problem.token, problem.code);
        }
        for (token, expected, found) in &problems.mismatches {
            self.report_expected(
                item,
                *token,
                Code::TypeMismatch,
                expected.clone(),
                found.clone(),
            );
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

impl Checker<'_> {
    /// The rules of check time that read an accepted item's NIR: its interpreter runs.
    fn check_elaborated(&self, item: ItemId) {
        if self.unit(item).is_none() {
            return;
        }
        match &self.db.ast(item).kind {
            ast::ItemKind::Const(_)
            | ast::ItemKind::ConstAssert(_)
            | ast::ItemKind::Contribute(_) => {
                self.const_value(item);
                self.check_traps(item);
                self.check_platform_entry(item);
            }
            ast::ItemKind::Fn(_) => {
                self.run_examples(item);
                self.check_proofs(item);
                self.check_contracts(item);
                self.check_traps(item);
            }
            ast::ItemKind::Struct(_) => {
                self.check_proofs(item);
                self.check_predicates(item);
            }
            ast::ItemKind::Interface(_) => self.check_proofs(item),
            _ => {}
        }
    }

    /// Whether the checker reports an error on a line of an item. Before every package is
    /// checked, the item is judged alone.
    pub fn rejected(&self, item: ItemId) -> bool {
        if !self.judged_all.get() {
            self.judge_alone(item);
        }
        self.rejections.borrow().contains(&item)
    }

    /// Checks an item unless it is checked, judging it alone: rejected when its syntax errors or
    /// those its check reports lie on its lines.
    pub(crate) fn judge_alone(&self, item: ItemId) {
        if self.judged.borrow_mut().insert(item) {
            let reported = self.found.borrow().len();
            let (muted, package) = (self.muted.replace(0), self.package.get());
            self.package.set(Some(self.db.package_of(item)));
            self.db.about(Some(item), || self.check_item(item));
            self.muted.set(muted);
            self.package.set(package);
            let data = self.db.item(item);
            let lines = data
                .file
                .syntax
                .item_lines(&data.file.syntax.items[data.index]);
            let error = |line: u32| lines.contains(&line);
            let failed = self.found.borrow()[reported..].iter().any(|diagnostic| {
                diagnostic.code.class() == crate::diagnostic::Class::Error
                    && diagnostic.path.as_deref() == Some(data.file.path.as_str())
                    && diagnostic.line.is_some_and(error)
            }) || data.file.syntax.diagnostics.iter().any(|diagnostic| {
                diagnostic.code.class() == crate::diagnostic::Class::Error && error(diagnostic.line)
            });
            if failed {
                self.rejections.borrow_mut().insert(item);
            }
        }
    }

    /// Records the items holding a line that an error is reported on, whose functions have no
    /// unit.
    fn reject_items(&self, workspace: &Workspace) {
        self.judged_all.set(true);
        let mut errors: HashSet<(String, u32)> = HashSet::new();
        let found = self.found.borrow();
        let reported = found
            .iter()
            .chain(workspace.diagnostics().collect::<Vec<_>>().iter())
            .filter(|diagnostic| diagnostic.code.class() == crate::diagnostic::Class::Error)
            .filter_map(|diagnostic| Some((diagnostic.path.clone()?, diagnostic.line?)))
            .collect::<Vec<_>>();
        errors.extend(reported);
        for (_, file) in workspace.files() {
            for diagnostic in &file.syntax.diagnostics {
                if diagnostic.code.class() == crate::diagnostic::Class::Error {
                    errors.insert((file.path.clone(), diagnostic.line));
                }
            }
        }
        let mut rejected = self.rejections.borrow_mut();
        for (index, data) in self.db.items.iter().enumerate() {
            let syntax = &data.file.syntax;
            let span = &syntax.items[data.index];
            let first = syntax.tokens[span.first].line;
            let last = syntax.tokens[span.end.saturating_sub(1).max(span.first)].line;
            if (first..=last).any(|line| errors.contains(&(data.file.path.clone(), line))) {
                rejected.insert(ItemId(index as u32));
            }
        }
    }
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
    checker.reject_items(workspace);
    for (_, package) in checked_packages(&checker, workspace, target) {
        for &module in &checker.db.package(package).modules {
            for &item in &checker.db.module(module).items {
                checker.check_elaborated(item);
            }
        }
        checker.check_tables(package);
    }
    checker.check_wheres();
    checker.check_const_cycles();
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

/// The diagnostics of the workspace's items under each target their packages are checked for,
/// those of unused code once found under every target checking their file.
pub fn diagnostics(workspace: &Workspace) -> Vec<Diagnostic> {
    let std = crate::stdlib::packages();
    let mut found = Vec::new();
    let mut unused: Vec<(Diagnostic, usize)> = Vec::new();
    let mut checking: HashMap<String, usize> = HashMap::new();
    for target in targets(workspace) {
        let checker = checked(workspace, std, target);
        for (_, package) in checked_packages(&checker, workspace, target) {
            let data = checker.db.package(package);
            let modules = data.modules.iter().map(|module| checker.db.module(*module));
            let files = modules
                .flat_map(|module| &module.files)
                .map(|file| &file.path);
            for path in files.chain(data.source.files.first().map(|file| &file.path)) {
                *checking.entry(path.clone()).or_default() += 1;
            }
        }
        let mut emitted: Vec<Diagnostic> = Vec::new();
        for diagnostic in checker.unused.into_inner() {
            if !emitted.contains(&diagnostic) {
                emitted.push(diagnostic);
            }
        }
        for diagnostic in emitted {
            match unused.iter_mut().find(|(known, _)| *known == diagnostic) {
                Some((_, count)) => *count += 1,
                None => unused.push((diagnostic, 1)),
            }
        }
        found.extend(checker.found.into_inner());
    }
    found.extend(
        unused
            .into_iter()
            .filter(|(diagnostic, count)| {
                diagnostic
                    .path
                    .as_ref()
                    .is_some_and(|path| checking.get(path) == Some(count))
            })
            .map(|(diagnostic, _)| diagnostic),
    );
    found
}
