//! The semantic checker: the rules of language 1 beyond syntax, applied to the items of the
//! workspace's packages under each target they are checked for. Every fact it computes is a query
//! over one item under one target, memoized for the run.

mod api;
mod approval;
mod author;
mod authority;
mod body;
mod changes;
mod compatible;
mod component;
mod conform;
mod context;
pub(crate) mod db;
mod decl;
mod digest;
mod docs;
mod edits;
mod elaborate;
mod entail;
mod expand;
mod failure;
mod flow;
mod gate;
pub(crate) mod generation;
mod head;
mod identity;
mod interface;
mod kept;
mod kernel;
mod kinds;
pub(crate) mod ledger;
pub(crate) mod lowlevel;
mod measure;
pub(crate) mod migrate;
mod newer;
mod package;
mod preserved;
mod program;
pub(crate) mod props;
pub(crate) mod protocol;
mod published;
mod reading;
mod references;
mod registry;
mod releases;
pub(crate) mod requires;
mod resolve;
pub(crate) mod reversibility;
mod rewrite;
pub(crate) mod root;
mod runs;
pub(crate) mod schema;
pub(crate) mod seal;
mod shape;
pub(crate) mod sig;
pub(crate) mod snapshot;
pub(crate) mod spec;
mod supplied;
mod synthetic;
pub(crate) mod testing;
mod threshold;
pub(crate) mod tir;
mod tombstone;
mod traps;
pub(crate) mod ty;
mod units;
mod unknown;
mod untrusted;
mod unused;

use std::borrow::Cow;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

pub use approval::{approvals, authority_summaries, is_unapproved};
pub use changes::{Change, changes, semantic_conflicts};
pub use compatible::{compatible, encode_class, schema_records};
pub use context::{Context, Green, context_gaps, contexts, greens, greens_after};
use db::{Db, ItemId, ModuleId, PackageId};
pub use edits::{identity, moved, renamed};
pub use elaborate::EnsuresClause;
pub use gate::gate;
pub use measure::{Discharge, Measured, measure};
pub use preserved::not_preserved;
pub use published::summaries as publish_summaries;
pub use reading::{api, contributed, declared, expanded, explained, holes, ledgered, shown};
pub use references::Relocated;
pub use releases::{holds_releases, release_check, release_errors};
pub use root::{nir_streams, package_files as package_sources};
use sig::{GenericDef, Sig};
pub use snapshot::{Snapshots, Unsealed};
pub use supplied::stated;
pub use traps::closure_demands_notrap;

use crate::cache::Cache;
use crate::diagnostic::{Code, Diagnostic, Fix, Offer, Reason, Role};
use crate::hash::Hash;
use crate::machine::api::Entry as ApiEntry;
use crate::scope::Scope;
use crate::syntax::ast::{self, Tok};
use crate::workspace::{Authority, Closure, Package, Target, Workspace};

pub struct Checker<'a> {
    pub db: Db<'a>,
    shapes: RefCell<HashMap<ItemId, Rc<Vec<GenericDef>>>>,
    sigs: RefCell<HashMap<ItemId, Rc<Sig>>>,
    found: RefCell<Vec<Found>>,
    /// The item whose own work is being computed, if any.
    traced: Cell<Option<ItemId>>,
    /// The package being checked.
    package: Cell<Option<PackageId>>,
    /// The impl lines, by the type they target, once indexed.
    impls: RefCell<Option<conform::ImplIndex>>,
    /// While positive, typing reports nothing: a probe of what a construct is.
    muted: Cell<u32>,
    /// While a probe of an item's typing listens, that item and the diagnostics found on it.
    heard: RefCell<Option<(ItemId, Vec<Diagnostic>)>>,
    /// The generic parameters of the body being checked, with the bounds that hold in it.
    scope: RefCell<Vec<GenericDef>>,
    /// The item whose body or clauses are being typed.
    within: Cell<Option<ItemId>>,
    /// The properties of struct and enum definitions, by whether a consumer sees them.
    definitions: RefCell<HashMap<(ItemId, bool), props::Props>>,
    deciding: RefCell<props::Deciding>,
    /// The typed bodies of function items, from their checks or typed for their digests.
    bodies: RefCell<HashMap<ItemId, Rc<tir::TBody>>>,
    /// The functions each function calls directly or supplies as function arguments, in order;
    /// none for an item without a typed body.
    body_dependencies: RefCell<HashMap<ItemId, Option<Rc<Vec<ItemId>>>>>,
    /// The impl lines each function implements a method of, once found.
    implemented: RefCell<HashMap<ItemId, Vec<ItemId>>>,
    digests: RefCell<digest::Digests>,
    body_uses: RefCell<untrusted::uses::Cache>,
    cycles: RefCell<digest::Cycles>,
    /// The authorities each module performs directly, as far as checked.
    performed: RefCell<HashSet<(ModuleId, Authority)>>,
    /// The items of each module in the values namespace by folded name, once gathered.
    module_values: RefCell<HashMap<ModuleId, Rc<HashMap<String, ItemId>>>>,
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
    /// The workspace package, by index, that the check is narrowed to: the one package it checks
    /// whole, its closure's other packages judged item by item as it reads them; none when it
    /// checks each package its closure checks.
    narrowed: Option<usize>,
    /// The NIR units elaborated, none for an item without one.
    units: RefCell<HashMap<ItemId, Option<Rc<crate::nir::Unit>>>>,
    /// The values const evaluation computed, none for one that ends without a value.
    const_values: RefCell<HashMap<ItemId, Option<Rc<crate::interp::ConstValue>>>>,
    /// The consts and contributions being evaluated, innermost last.
    evaluating: RefCell<Vec<ItemId>>,
    /// The tombstones whose replacements are being found, innermost last.
    replacing: RefCell<Vec<ItemId>>,
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
    unused: RefCell<Vec<Found>>,
    /// The diagnostics found on the items of quiet packages, which no command reports.
    quiet: RefCell<Vec<Diagnostic>>,
    /// The fields of structs the bodies checked read.
    fields_read: RefCell<HashSet<(ItemId, String)>>,
    /// The functions named after a type, by member name, once gathered.
    member_functions: OnceCell<HashMap<String, Vec<ItemId>>>,
    /// The items each item names, once computed.
    named: RefCell<HashMap<ItemId, Rc<BTreeSet<ItemId>>>>,
    /// The `label: value` of each construction and struct pattern the bodies checked write, by
    /// the struct and the field it names.
    field_labels: RefCell<HashMap<(ItemId, String), Vec<unused::FieldLabel>>>,
    /// The proof cache of the running user, opened at the first query decided; none without a
    /// usable one.
    pub(crate) proof_cache: OnceCell<Option<Cache>>,
    /// The entries of the local store of the running user, once read; none without a usable one.
    store_entries: OnceCell<Option<Vec<crate::store::Entry>>>,
    /// The exported items of the release each package's history head names, once read from the
    /// local store; none for a head the store does not hold.
    head_exports: RefCell<HashMap<PackageId, Option<Rc<Exported>>>>,
    /// While the prover is compared with the proof cache, the obligation hashes whose held
    /// result differs from the procedure's.
    pub(crate) compared: RefCell<Option<Vec<Hash>>>,
    /// What the build cache keeps of the check and gives back to it.
    kept: kept::Kept,
}

/// A diagnostic found, with the item whose own work found it; none for a rule relating items.
#[derive(Clone)]
pub(crate) struct Found {
    pub owner: Option<ItemId>,
    pub diagnostic: Diagnostic,
}

/// The exported items of a release, by canonical path.
type Exported = BTreeMap<String, ApiEntry>;

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
            traced: Cell::new(None),
            package: Cell::new(None),
            impls: RefCell::new(None),
            muted: Cell::new(0),
            heard: RefCell::new(None),
            scope: RefCell::new(Vec::new()),
            within: Cell::new(None),
            definitions: RefCell::new(HashMap::new()),
            deciding: RefCell::new(props::Deciding::default()),
            bodies: RefCell::new(HashMap::new()),
            body_dependencies: RefCell::new(HashMap::new()),
            implemented: RefCell::new(HashMap::new()),
            digests: RefCell::new(digest::Digests::default()),
            body_uses: RefCell::new(untrusted::uses::Cache::default()),
            cycles: RefCell::new(digest::Cycles::default()),
            performed: RefCell::new(HashSet::new()),
            module_values: RefCell::new(HashMap::new()),
            holes: RefCell::new(Vec::new()),
            members_named: RefCell::new(Vec::new()),
            misnamed: RefCell::new(Vec::new()),
            method_bodies: RefCell::new(HashMap::new()),
            typed_predicates: RefCell::new(HashMap::new()),
            typed_wheres: RefCell::new(HashMap::new()),
            rejections: RefCell::new(HashSet::new()),
            judged_all: Cell::new(false),
            judged: RefCell::new(HashSet::new()),
            narrowed: None,
            units: RefCell::new(HashMap::new()),
            const_values: RefCell::new(HashMap::new()),
            evaluating: RefCell::new(Vec::new()),
            replacing: RefCell::new(Vec::new()),
            needs: RefCell::new(std::collections::BTreeMap::new()),
            cyclic: RefCell::new(HashSet::new()),
            obligations: RefCell::new(HashMap::new()),
            may_traps: RefCell::new(HashMap::new()),
            instantiations: RefCell::new(Vec::new()),
            unused: RefCell::new(Vec::new()),
            quiet: RefCell::new(Vec::new()),
            fields_read: RefCell::new(HashSet::new()),
            member_functions: OnceCell::new(),
            named: RefCell::new(HashMap::new()),
            field_labels: RefCell::new(HashMap::new()),
            proof_cache: OnceCell::new(),
            store_entries: OnceCell::new(),
            head_exports: RefCell::new(HashMap::new()),
            compared: RefCell::new(None),
            kept: kept::Kept::default(),
        }
    }

    /// Computes a query about `subject`, or about no item: its reads recorded as that subject's,
    /// and the subject's own work.
    pub fn about<T>(&self, subject: Option<ItemId>, compute: impl FnOnce() -> T) -> T {
        self.db.about(subject, || match subject {
            Some(item) => self.traced_as(item, compute),
            None => compute(),
        })
    }

    /// Computes part of an item's own work, as the item whose own work is traced.
    fn traced_as<T>(&self, item: ItemId, compute: impl FnOnce() -> T) -> T {
        let outer = self.traced.replace(Some(item));
        let computed = compute();
        self.traced.set(outer);
        computed
    }

    /// Appends a diagnostic to those found, owned by the item whose own work reports it; one
    /// whose owner's outcome the build cache gives is dropped.
    fn push_found(&self, diagnostic: Diagnostic) {
        let owner = self.traced.get();
        if !self.is_restored(owner) {
            self.found.borrow_mut().push(Found { owner, diagnostic });
        }
    }

    /// Records a diagnostic of unused code as `push_found` records a found one.
    fn push_unused(&self, diagnostic: Diagnostic) {
        let owner = self.traced.get();
        if !self.is_restored(owner) {
            self.unused.borrow_mut().push(Found { owner, diagnostic });
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
        self.report_record(item, token, code, Some(expected), Some(found), Vec::new());
    }

    /// Reports a diagnostic with its because-chain.
    pub fn report_because(&self, item: ItemId, token: Tok, code: Code, because: Vec<Reason>) {
        self.report_record(item, token, code, None, None, because);
    }

    /// Reports a diagnostic with its expected and found texts, when given, and its because-chain.
    pub fn report_record(
        &self,
        item: ItemId,
        token: Tok,
        code: Code,
        expected: Option<String>,
        found: Option<String>,
        because: Vec<Reason>,
    ) {
        self.report_diagnostic(
            item,
            Diagnostic {
                expected,
                found,
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
        self.report_fixed_because(item, construct, code, fixes, offer, Vec::new());
    }

    /// Reports a diagnostic on the construct of an item's tokens `first..=last`, with the fixes
    /// its rule names and its because-chain.
    pub fn report_fixed_because(
        &self,
        item: ItemId,
        construct: (Tok, Tok),
        code: Code,
        fixes: Vec<Fix>,
        offer: Offer,
        because: Vec<Reason>,
    ) {
        self.report_diagnostic(
            item,
            Diagnostic {
                fixes,
                offer,
                because,
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

    /// Runs a muted probe of an item's typing, returning its result and the diagnostics it finds
    /// on that item.
    fn heard<T>(&self, item: ItemId, probe: impl FnOnce() -> T) -> (T, Vec<Diagnostic>) {
        let outer = self.heard.replace(Some((item, Vec::new())));
        self.muted.set(self.muted.get() + 1);
        let result = probe();
        self.muted.set(self.muted.get() - 1);
        let heard = self.heard.replace(outer).map(|(_, heard)| heard);
        (result, heard.unwrap_or_default())
    }

    /// The entries of the local store of the running user; none without a usable one.
    fn store_entries(&self) -> Option<&[crate::store::Entry]> {
        self.store_entries
            .get_or_init(|| {
                let cache = Cache::open().ok()?.ok()?;
                crate::store::Store::new(cache.store()).entries().ok()?.ok()
            })
            .as_deref()
    }

    /// Runs a probe whose diagnostics are withheld, returning its result and those diagnostics.
    fn withheld<T>(&self, probe: impl FnOnce() -> T) -> (T, Vec<Diagnostic>) {
        let before = self.found.borrow().len();
        let result = probe();
        let withheld = self.found.borrow_mut().split_off(before);
        (
            result,
            withheld.into_iter().map(|found| found.diagnostic).collect(),
        )
    }

    /// Whether a diagnostic is reported on the construct starting at one of an item's tokens.
    fn reported_at(&self, item: ItemId, token: Tok) -> bool {
        let data = self.db.item(item);
        let start = data.file.syntax.tokens[token as usize].start;
        self.found.borrow().iter().any(|Found { diagnostic, .. }| {
            diagnostic.path.as_deref() == Some(data.file.path.as_str())
                && diagnostic.span.is_some_and(|(at, _)| at == start)
        })
    }

    fn report_diagnostic(&self, item: ItemId, diagnostic: Diagnostic) {
        if let Some(diagnostic) = self.on_item(item, diagnostic) {
            match self.db.is_reported(item) {
                true => self.push_found(diagnostic),
                false => self.quiet.borrow_mut().push(diagnostic),
            }
        }
    }

    /// Records a diagnostic of unused code on an item of a package declaring the checker's
    /// target, reported once found under every target checking its file.
    fn report_unused(&self, item: ItemId, diagnostic: Diagnostic) {
        let declared = check_targets(self.db.package(self.db.package_of(item)).source);
        if let Some(diagnostic) = self.on_item(item, diagnostic)
            && self.db.is_reported(item)
            && declared.contains(&self.db.target)
        {
            self.push_unused(diagnostic);
        }
    }

    /// A diagnostic located on an item's file and item; none for the carried std's items and in
    /// probes, which hear those on the item they listen to.
    fn on_item(&self, item: ItemId, mut diagnostic: Diagnostic) -> Option<Diagnostic> {
        if self.db.is_carried(item) {
            return None;
        }
        if self.muted.get() > 0 {
            if let Some((listening, heard)) = &mut *self.heard.borrow_mut()
                && *listening == item
            {
                heard.push(diagnostic);
            }
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
        for (bound, token) in &problems.bounds {
            self.check_arguments(item, *token, bound.interface, &bound.args);
        }
        for (construct, tombstone) in &problems.renamed {
            self.report_renamed_path(item, *construct, *tombstone);
        }
    }

    /// Reports a diagnostic on a line of a workspace file outside every item.
    pub fn report_file(&self, path: &str, line: u32, code: Code) {
        self.report_file_fixed(path, line, code, Vec::new());
    }

    /// Reports a diagnostic on a line of a file with the fixes its rule names.
    pub fn report_file_fixed(&self, path: &str, line: u32, code: Code, fixes: Vec<Fix>) {
        self.push_found(Diagnostic {
            fixes,
            ..Diagnostic::at(code, path, line)
        });
    }
}

/// The diagnostics check time reports on lines of a scope, those of loading, lexing and parsing
/// included, by path, line and code, each once, each located on its reported construct's bytes.
pub fn reported(workspace: &Workspace, scope: &Scope) -> Vec<Diagnostic> {
    reported_by(workspace, &checked_scope(workspace, scope), scope)
}

/// The checkers of what checking a scope runs, each under its target, once checked: of the
/// package the scope lies in, checked alone, or of every closure for a scope holding several.
/// Each checker is checked from the build cache's snapshot when that gives what a full check
/// gives.
pub fn checked_scope<'a>(workspace: &'a Workspace, scope: &Scope) -> Vec<(Target, Checker<'a>)> {
    let std = crate::stdlib::packages();
    scope_closures(workspace, scope)
        .into_iter()
        .map(|(closure, target, narrowed)| {
            let checker = kept::kept_checked(workspace, &closure, std, target, narrowed);
            (target, checker)
        })
        .collect()
}

/// The checker of a check under a target whose closure checks each workspace package of
/// `packages`, by index, when the check is narrowed to none or to the one package `packages`
/// holds.
pub fn serving<'c, 'a>(
    checkers: &'c [(Target, Checker<'a>)],
    target: Target,
    packages: &[usize],
) -> Option<&'c Checker<'a>> {
    checkers
        .iter()
        .filter(|(held, _)| *held == target)
        .map(|(_, checker)| checker)
        .find(|checker| {
            checker
                .narrowed
                .is_none_or(|narrowed| packages == [narrowed])
                && packages.iter().all(|package| checker.checks(*package))
        })
}

impl Checker<'_> {
    /// Whether the checker's closure checks the workspace package at an index.
    fn checks(&self, index: usize) -> bool {
        self.db
            .packages
            .iter()
            .any(|data| !data.quiet && data.member == Some(crate::workspace::Member::Source(index)))
    }
}

/// The diagnostics `reported` gives on lines of a scope, from the checkers of a check of it.
pub fn reported_by(
    workspace: &Workspace,
    checkers: &[(Target, Checker)],
    scope: &Scope,
) -> Vec<Diagnostic> {
    let mut found: Vec<Diagnostic> = workspace.diagnostics().collect();
    found.extend(root::pending_syncs(workspace, scope));
    found.extend(diagnostics(checkers));
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
    sort_diagnostics(&mut found);
    found
}

/// Sorts diagnostics by path, line and code, keeping each once whatever bytes it is located on.
pub fn sort_diagnostics(found: &mut Vec<Diagnostic>) {
    found.sort_by(|left, right| {
        (&left.path, left.line, left.code.name()).cmp(&(&right.path, right.line, right.code.name()))
    });
    found.dedup_by(|later, earlier| {
        Diagnostic {
            span: earlier.span,
            ..later.clone()
        } == *earlier
    });
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
            ast::ItemKind::Interface(_) | ast::ItemKind::Protocol(_) => self.check_proofs(item),
            _ => {}
        }
    }

    /// Whether the checker reports an error on a line of an item. Before every package is
    /// checked, and for an item of a package the check leaves out, the item is judged alone.
    pub fn rejected(&self, item: ItemId) -> bool {
        if !self.judged_all.get() || self.leaves_out(self.db.package_of(item)) {
            self.judge_alone(item);
        }
        self.rejections.borrow().contains(&item)
    }

    /// Whether the check leaves out a package it reports on: one it is not checked under the
    /// checker's target, or another than the one the check is narrowed to.
    fn leaves_out(&self, package: PackageId) -> bool {
        let data = self.db.package(package);
        let narrowed = self.narrowed.map(crate::workspace::Member::Source);
        !data.carried
            && !data.quiet
            && (!check_targets(data.source).contains(&self.db.target)
                || narrowed.is_some_and(|narrowed| data.member != Some(narrowed)))
    }

    /// Checks an item unless it is checked, judging it alone: rejected when its syntax errors or
    /// those its check reports lie on its lines.
    pub(crate) fn judge_alone(&self, item: ItemId) {
        if self.judged.borrow_mut().insert(item) {
            let reported = self.found.borrow().len();
            let quieted = self.quiet.borrow().len();
            let (muted, package) = (self.muted.replace(0), self.package.get());
            self.package.set(Some(self.db.package_of(item)));
            self.about(Some(item), || self.check_item(item));
            self.muted.set(muted);
            self.package.set(package);
            let data = self.db.item(item);
            let lines = data
                .file
                .syntax
                .item_lines(&data.file.syntax.items[data.index]);
            let error = |line: u32| lines.contains(&line);
            let on_item = |diagnostic: &Diagnostic| {
                diagnostic.code.class() == crate::diagnostic::Class::Error
                    && diagnostic.path.as_deref() == Some(data.file.path.as_str())
                    && diagnostic.line.is_some_and(error)
            };
            let failed = self.found.borrow()[reported..]
                .iter()
                .any(|found| on_item(&found.diagnostic))
                || self.quiet.borrow()[quieted..].iter().any(on_item)
                || data.file.syntax.diagnostics.iter().any(|diagnostic| {
                    diagnostic.code.class() == crate::diagnostic::Class::Error
                        && error(diagnostic.line)
                });
            if failed {
                self.rejections.borrow_mut().insert(item);
            }
            self.kept.judged_alone(item, failed);
        }
    }

    /// Records the items holding a line that an error is reported on, whose functions have no
    /// unit.
    fn reject_items(&self) {
        let workspace = self.db.workspace;
        self.judged_all.set(true);
        let mut errors: HashSet<(String, u32)> = HashSet::new();
        let found = self.found.borrow();
        let quiet = self.quiet.borrow();
        let reported = found
            .iter()
            .map(|found| &found.diagnostic)
            .chain(quiet.iter())
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

/// The packages a checker checks under its target: the workspace packages its closure checks
/// that declare the target, the one it is narrowed to alone, by workspace index, with their ids
/// in its database.
fn checked_packages(checker: &Checker) -> Vec<(usize, PackageId)> {
    let target = checker.db.target;
    checker
        .db
        .packages
        .iter()
        .enumerate()
        .filter(|(_, data)| {
            !data.carried && !data.quiet && check_targets(data.source).contains(&target)
        })
        .filter_map(|(id, data)| match data.member? {
            crate::workspace::Member::Source(index) => Some((index, PackageId(id as u32))),
            crate::workspace::Member::Release(_) => None,
        })
        .filter(|(index, _)| checker.narrowed.is_none_or(|narrowed| narrowed == *index))
        .collect()
}

/// The targets the packages a closure checks are checked under, in order.
fn closure_targets(workspace: &Workspace, closure: &Closure) -> Vec<Target> {
    let mut targets: Vec<Target> = closure
        .checked
        .iter()
        .flat_map(|index| check_targets(&workspace.packages[*index]))
        .collect();
    targets.sort();
    targets.dedup();
    targets
}

/// The checker of a closure under a target, once it has checked each package the closure checks
/// under it.
pub fn checked<'a>(
    workspace: &'a Workspace,
    closure: &Closure,
    std: &'a [Arc<Package>],
    target: Target,
) -> Checker<'a> {
    let checker = unchecked(workspace, closure, std, target);
    checker.check_all();
    checker
}

impl Checker<'_> {
    /// Checks each package the checker checks, then the rules relating its items.
    fn check_all(&self) {
        for (_, package) in checked_packages(self) {
            self.package.set(Some(package));
            self.check_package(package);
        }
        self.fix_misnamed();
        self.reject_items();
        for (_, package) in checked_packages(self) {
            for &module in &self.db.package(package).modules {
                for &item in &self.db.module(module).items {
                    if !self.is_restored(Some(item)) {
                        self.traced_as(item, || self.check_elaborated(item));
                    }
                }
            }
            self.check_tables(package);
            self.check_approvals(package);
        }
        self.judge_build_reaches();
        self.check_wheres();
        self.check_const_cycles();
    }
}

/// The checker of a closure under a target before it checks anything.
pub fn unchecked<'a>(
    workspace: &'a Workspace,
    closure: &Closure,
    std: &'a [Arc<Package>],
    target: Target,
) -> Checker<'a> {
    let mut db = Db::build(workspace, closure, std, target);
    db.link();
    Checker::new(db)
}

/// The obligation hashes, under each target the workspace's packages are checked for, whose
/// result the proof cache holds and the procedure does not give.
pub fn proof_cache_mismatches(workspace: &Workspace) -> Vec<String> {
    let std = crate::stdlib::packages();
    let mut mismatches = Vec::new();
    for (_, checker) in checkers(workspace, std) {
        *checker.compared.borrow_mut() = Some(Vec::new());
        checker.check_all();
        let found = checker.compared.borrow_mut().take().unwrap_or_default();
        mismatches.extend(found.iter().map(Hash::full));
    }
    mismatches
}

/// A checked checker of each closure under each target its packages are checked for.
fn checked_closures<'a>(
    workspace: &'a Workspace,
    std: &'a [Arc<Package>],
) -> Vec<(Target, Checker<'a>)> {
    workspace
        .closures
        .iter()
        .flat_map(|closure| {
            closure_targets(workspace, closure)
                .into_iter()
                .map(move |target| (target, checked(workspace, closure, std, target)))
        })
        .collect()
}

/// Unchecked checkers of what checking a scope runs: for a scope lying in one package, of the
/// part of the closure checking it that the package reaches, under each target it declares,
/// narrowed to it; otherwise of each closure under each target its packages are checked for.
fn scope_checkers<'a>(
    workspace: &'a Workspace,
    std: &'a [Arc<Package>],
    scope: &Scope,
) -> Vec<(Target, Checker<'a>)> {
    scope_closures(workspace, scope)
        .into_iter()
        .map(|(closure, target, narrowed)| {
            let mut checker = unchecked(workspace, &closure, std, target);
            checker.narrowed = narrowed;
            (target, checker)
        })
        .collect()
}

/// What checking a scope runs: each closure under each target its packages are checked for, or
/// for a scope lying in one package, the part of the closure checking it that it reaches under
/// each target it declares, narrowed to it.
fn scope_closures<'w>(
    workspace: &'w Workspace,
    scope: &Scope,
) -> Vec<(Cow<'w, Closure>, Target, Option<usize>)> {
    let Some(index) = scope.package_index(workspace) else {
        return workspace
            .closures
            .iter()
            .flat_map(|closure| {
                closure_targets(workspace, closure)
                    .into_iter()
                    .map(move |target| (Cow::Borrowed(closure), target, None))
            })
            .collect();
    };
    let package = &workspace.packages[index];
    let Some(closure) = workspace.closure_of(package) else {
        return Vec::new();
    };
    let reached = closure.reached_from(workspace, index);
    let declared = check_targets(package);
    closure_targets(workspace, closure)
        .into_iter()
        .filter(|target| declared.contains(target))
        .map(|target| (Cow::Owned(reached.clone()), target, Some(index)))
        .collect()
}

/// An unchecked checker of each closure under each target its packages are checked for.
fn checkers<'a>(workspace: &'a Workspace, std: &'a [Arc<Package>]) -> Vec<(Target, Checker<'a>)> {
    workspace
        .closures
        .iter()
        .flat_map(|closure| {
            closure_targets(workspace, closure)
                .into_iter()
                .map(move |target| (target, unchecked(workspace, closure, std, target)))
        })
        .collect()
}

/// The diagnostics checked checkers found, those of unused code once found under every target
/// checking their file.
fn diagnostics(checkers: &[(Target, Checker)]) -> Vec<Diagnostic> {
    let mut found = Vec::new();
    let mut unused: Vec<(Diagnostic, usize)> = Vec::new();
    let mut checking: HashMap<String, usize> = HashMap::new();
    for (_, checker) in checkers {
        for (_, package) in checked_packages(checker) {
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
        for Found { diagnostic, .. } in checker.unused.borrow().iter() {
            if !emitted.contains(diagnostic) {
                emitted.push(diagnostic.clone());
            }
        }
        for diagnostic in emitted {
            match unused.iter_mut().find(|(known, _)| *known == diagnostic) {
                Some((_, count)) => *count += 1,
                None => unused.push((diagnostic, 1)),
            }
        }
        found.extend(
            checker
                .found
                .borrow()
                .iter()
                .map(|found| found.diagnostic.clone()),
        );
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

/// Unchecked checkers of a workspace package's closure under each target it is checked for.
fn package_checkers<'a>(
    workspace: &'a Workspace,
    std: &'a [Arc<Package>],
    package: &Package,
) -> Vec<(Target, Checker<'a>)> {
    let Some(closure) = workspace.closure_of(package) else {
        return Vec::new();
    };
    check_targets(package)
        .into_iter()
        .map(|target| (target, unchecked(workspace, closure, std, target)))
        .collect()
}

/// The API snapshots `nan api update` computes for a workspace package, confirming the public
/// docs of the items `confirmed` names by canonical path.
pub fn package_snapshots(
    workspace: &Workspace,
    package: &Package,
    confirmed: &[String],
) -> Snapshots {
    let std = crate::stdlib::packages();
    let checkers = package_checkers(workspace, std, package);
    let views: Vec<(Target, &Checker)> =
        checkers.iter().map(|(target, ck)| (*target, ck)).collect();
    let items = snapshot::Items::of(&views, package);
    computed_snapshots(workspace, package, &items, confirmed)
}

/// The snapshots `nan api update` computes for a package of a workspace, from its items across
/// the checkers of its targets, its pending weakenings judged by entailment.
fn computed_snapshots(
    workspace: &Workspace,
    package: &Package,
    items: &snapshot::Items,
    confirmed: &[String],
) -> Snapshots {
    let mut computed = snapshot::snapshots(items, package, confirmed);
    if let Some(index) = workspace
        .packages
        .iter()
        .position(|held| std::ptr::eq(&**held, package))
    {
        entail::refine(workspace, index, items, &mut computed);
    }
    computed
}

/// The seal `principal` writes on a package's item at `path`, signed by the key of `seed` when
/// given, in a VCS workspace meeting the author rule, or why `nan seal` writes nothing.
pub fn sealed(
    workspace: &Workspace,
    package: &Package,
    path: &str,
    principal: &str,
    seed: Option<&[u8; 32]>,
) -> Result<snapshot::Sealed, snapshot::Unsealed> {
    let std = crate::stdlib::packages();
    let checkers = package_checkers(workspace, std, package);
    let views: Vec<(Target, &Checker)> =
        checkers.iter().map(|(target, ck)| (*target, ck)).collect();
    let name = package.name().unwrap_or_default();
    let items = snapshot::Items::of(&views, package);
    let computed = computed_snapshots(workspace, package, &items, &[]);
    let sealed = snapshot::sealed(&items, package, computed, path, principal, seed)?;
    let shared = author::History::open(workspace).and_then(|history| {
        history.would_share(name, path, &sealed.seal, sealed.accepted.as_ref())
    });
    match shared {
        Some(author) => Err(snapshot::Unsealed::SameAuthor(author)),
        None => Ok(sealed),
    }
}

/// Whether the workspace item of a canonical path has public docs.
pub fn has_public_docs(workspace: &Workspace, path: &str) -> bool {
    let std = crate::stdlib::packages();
    checkers(workspace, std).iter().any(|(_, checker)| {
        (0..checker.db.items.len())
            .map(|index| ItemId(index as u32))
            .filter(|item| checker.db.is_reported(*item))
            .any(|item| {
                checker.canonical_path(item).as_deref() == Some(path)
                    && checker.consumers_see(item)
                    && !checker.public_docs(item).is_empty()
            })
    })
}

/// The errors checking a closure finds under each target its packages are checked for: on the
/// packages it checks, and on each of its other members, checked whole, with the release checks'
/// stale schema trees.
pub fn closure_errors(workspace: &Workspace, closure: &Closure) -> Vec<Diagnostic> {
    let std = crate::stdlib::packages();
    let mut found = Vec::new();
    for target in closure_targets(workspace, closure) {
        let checker = checked(workspace, closure, std, target);
        let quiet: Vec<PackageId> = (0..checker.db.packages.len())
            .map(|index| PackageId(index as u32))
            .filter(|package| checker.db.package(*package).quiet)
            .collect();
        for package in &quiet {
            checker.check_whole(*package);
        }
        for package in quiet {
            let because = checker.stale_schemas(package);
            let stale = Diagnostic {
                because,
                ..Diagnostic::new(Code::ArtefactRecheckFailed)
            };
            if !stale.because.is_empty() && !found.contains(&stale) {
                found.push(stale);
            }
        }
        found.extend(
            checker
                .found
                .into_inner()
                .into_iter()
                .map(|found| found.diagnostic)
                .chain(checker.quiet.into_inner())
                .filter(|diagnostic| diagnostic.code.class() == crate::diagnostic::Class::Error),
        );
    }
    found
}

/// The tombstones of a workspace package that keep an `Encode` key: those retiring a field or
/// variant of a type deriving `Encode`, by file and first line.
pub fn encode_key_tombstones(workspace: &Workspace, package: &Package) -> Vec<(String, u32)> {
    let std = crate::stdlib::packages();
    let Some(closure) = workspace.closure_of(package) else {
        return Vec::new();
    };
    let Some(target) = check_targets(package).into_iter().next() else {
        return Vec::new();
    };
    let checker = unchecked(workspace, closure, std, target);
    let Some(id) = checker.package_named(package.name().unwrap_or_default()) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for &module in &checker.db.package(id).modules {
        for &item in &checker.db.module(module).items {
            let ast::ItemKind::Retired(retired) = &checker.db.ast(item).kind else {
                continue;
            };
            if checker.keeps_encode_key(module, retired) {
                let data = checker.db.item(item);
                let first = *data
                    .syntax()
                    .item_lines(&data.syntax().items[data.index])
                    .start();
                found.push((data.file.path.clone(), first));
            }
        }
    }
    found
}

#[cfg(test)]
mod tests;
