//! Targets and programs: the targets each dependency covers, dependencies on programs, the one
//! concrete target a program declares and the head of its entry function.

use std::collections::HashSet;

use super::body::Clauses;
use super::db::{ItemId, ModuleId, PackageId, TypeKey};
use super::props::Property;
use super::registry::Contribution;
use super::ty::{Scalar, Ty};
use super::{Checker, check_targets, checked_packages, seal};
use crate::diagnostic::Code;
use crate::syntax::ast::{ItemKind, Mode, Visibility};
use crate::workspace::{Kind, Target};

impl Checker<'_> {
    /// The rules of a package's `targets`, `entry` and `dep` directives: a program declares one
    /// concrete target and an entry function `main` of its root module, and each dependency in
    /// the workspace covers every target the package declares and is no program, unless its
    /// `dep` directive lacks a grant its package needs; a failed `targets` directive declares
    /// unknown targets, which satisfy the rules of targets.
    pub fn check_program(&self, package: PackageId) {
        let data = self.db.package(package);
        let Some(manifest) = &data.source.manifest else {
            return;
        };
        let path = &data.source.files[0].path;
        let concrete = matches!(&manifest.targets[..], [target] if *target != Target::Any);
        if let Some((_, line)) = &manifest.entry
            && manifest.targets_line.is_some()
            && !manifest.targets_failed
            && !concrete
        {
            self.report_file(path, *line, Code::TargetProgramTargets);
        }
        if let Some((names, line)) = &manifest.entry
            && let Some(module) = data.root
            && !self.db.module(module).abandoned
            && let Some(code) = self.entry_problem(module, names)
        {
            self.report_file(path, *line, code);
        }
        for dep in &manifest.deps {
            let Some(Some(dependency)) = data.deps.get(&dep.alias) else {
                continue;
            };
            let Some(declared) = &self.db.package(*dependency).source.manifest else {
                continue;
            };
            if declared.targets_line.is_none() || !self.ungranted(package, &dep.alias).is_empty() {
                continue;
            }
            let covers = |target: &Target| {
                declared.targets.contains(target) || declared.targets.contains(&Target::Any)
            };
            let failed = manifest.targets_failed || declared.targets_failed;
            if !failed && !manifest.targets.iter().all(covers) {
                self.report_file(path, dep.line, Code::TargetDependencyUncovered);
            } else if declared.entry.is_some() {
                self.report_file(path, dep.line, Code::TargetDepProgram);
            }
        }
    }

    /// Judges under the checker's concrete target each item of a package declaring `any` within
    /// the reach of the final build of each program it checks, from the program's entry items and
    /// the admitted contributions to platform registries.
    pub fn judge_build_reaches(&self) {
        let mut packages = (0..self.db.packages.len()).map(|index| PackageId(index as u32));
        if self.db.target == Target::Any || !packages.any(|package| self.is_shared(package)) {
            return;
        }
        for (_, program) in checked_packages(self) {
            let data = self.db.package(program);
            let entry = data
                .source
                .manifest
                .as_ref()
                .and_then(|manifest| manifest.entry.as_ref());
            let (Some((names, _)), Some(module)) = (entry, data.root) else {
                continue;
            };
            let admitted: Vec<Contribution> = self
                .contributions(program)
                .into_iter()
                .filter(|contribution| contribution.admitted)
                .collect();
            let roots = names
                .iter()
                .filter_map(|name| self.db.lookup(module, name))
                .chain(
                    admitted
                        .iter()
                        .filter(|contribution| self.is_platform_contribution(contribution.item))
                        .map(|contribution| contribution.item),
                )
                .collect();
            self.judge_reach(roots, &admitted);
        }
    }

    /// Judges each item of a package declaring `any` that `roots` reach: each item a reached
    /// item's unit names, the impl lines of each reached type and the `admitted` contributions to
    /// each reached registry, an item of the carried std or of a quiet package adding only
    /// itself.
    fn judge_reach(&self, roots: Vec<ItemId>, admitted: &[Contribution]) {
        let mut pending = roots;
        let mut reach: HashSet<ItemId> = HashSet::new();
        while let Some(item) = pending.pop() {
            if !reach.insert(item) || !self.db.is_reported(item) {
                continue;
            }
            if self.is_shared(self.db.package_of(item)) {
                self.judge_alone(item);
            }
            if self.rejected(item) {
                continue;
            }
            match self.db.declaration(item).kind {
                ItemKind::Struct(_) | ItemKind::Enum(_) | ItemKind::Layout(_) => {
                    pending.extend(self.impl_lines(TypeKey::Item(item)));
                }
                ItemKind::Impl(_) => pending.extend(self.names(item).iter().copied()),
                ItemKind::Registry(_) => pending.extend(
                    admitted
                        .iter()
                        .filter(|contribution| contribution.registry == item)
                        .map(|contribution| contribution.item),
                ),
                _ => {}
            }
            if let Some(unit) = self.unit(item) {
                pending.extend(seal::named_items(&unit));
            }
        }
    }

    /// Whether a package of the checker's closure that it reports on declares `any`.
    fn is_shared(&self, package: PackageId) -> bool {
        let data = self.db.package(package);
        !data.carried && !data.quiet && check_targets(data.source) == [Target::Any]
    }

    /// The rule an `entry` directive listing `names` of a root module breaks: `main` alone names
    /// a function item `main`, and other names, on a nanos target, each name a component entry.
    fn entry_problem(&self, module: ModuleId, names: &[String]) -> Option<Code> {
        let item = |name: &str| self.db.lookup(module, name);
        if let [main] = names
            && main == "main"
        {
            let found =
                item(main).is_some_and(|item| matches!(self.db.ast(item).kind, ItemKind::Fn(_)));
            return (!found).then_some(Code::TargetEntryMissing);
        }
        let mut problem = None;
        for name in names {
            let state = item(name).filter(|state| {
                matches!(
                    self.db.ast(*state).kind,
                    ItemKind::Struct(_) | ItemKind::Enum(_)
                )
            });
            match state {
                Some(state) if self.db.target.is_nanos() => {
                    if !self.is_component_entry(state) {
                        problem = Some(Code::TargetComponentEntry);
                    }
                }
                _ => return Some(Code::TargetEntryMissing),
            }
        }
        problem
    }

    /// Whether a type item is a component entry: a state without generic parameters of a
    /// component serving a protocol, whose Env is not linear and implements `nanos::Environment`.
    fn is_component_entry(&self, state: ItemId) -> bool {
        if !self.sig(state).generics.is_empty() {
            return false;
        }
        let Some((env, message)) = self.component_of(&Ty::item(state, Vec::new())) else {
            return false;
        };
        let served = matches!(message, Ty::Item(protocol, _) if self.is_protocol(protocol));
        let built = self
            .db
            .std_item("nanos", "Environment")
            .is_some_and(|environment| self.implements(&env, environment));
        served && built && !self.has(&env, Property::Linear)
    }

    /// The head of a program's entry function: one of its target's forms, quoted as expected,
    /// unless a rule of heads reports it.
    pub fn check_entry_head(&self, item: ItemId) {
        let ast = self.db.ast(item);
        let ItemKind::Fn(function) = &ast.kind else {
            return;
        };
        let Some(sig) = self.fn_sig(item) else {
            return;
        };
        if sig.has_error() || self.reported_at(item, ast.token) {
            return;
        }
        let Some(kind) = self.db.target.kind() else {
            return;
        };
        const HOST_FORMS: &str = "fn main(take P: host::World) fails E uses wait or fn main(take P: \
                                  host::World) -> u8 fails E uses wait";
        let forms = match kind {
            Kind::Host => HOST_FORMS.to_owned(),
            Kind::Uefi => "fn main(take P: boot::Machine) -> Never".to_owned(),
            Kind::Bare => "fn main(take P: boot::Kernel) -> Never".to_owned(),
            Kind::Nanos => format!(
                "fn main(take P: nanos::World, take I: In) -> Out fails E uses wait, In and Out \
                 implementing Encode, or {HOST_FORMS}"
            ),
        };
        let taken = |index: usize, grant: &str| {
            sig.params.get(index).is_some_and(|param| {
                param.mode == Mode::Take && !param.is_self && self.is_std(&param.ty, grant)
            })
        };
        let failure = |fails: &Ty| {
            let rendered_enum = matches!(fails, Ty::Item(failure, _)
                if matches!(self.db.ast(*failure).kind, ItemKind::Enum(_)))
                && self
                    .db
                    .core_item("Render")
                    .is_some_and(|render| self.implements(fails, render));
            rendered_enum || self.is_std(fails, "task::Cancelled")
        };
        let waits = sig.fails.as_ref().is_some_and(failure) && sig.wait;
        let host = sig.params.len() == 1
            && taken(0, "host::World")
            && matches!(sig.result, Ty::Unit | Ty::Scalar(Scalar::U8))
            && waits;
        let encode = |ty: &Ty| {
            self.db
                .core_item("Encode")
                .is_some_and(|encode| self.implements(ty, encode))
        };
        let job = || {
            matches!(&sig.params[..], [_, input] if input.mode == Mode::Take && encode(&input.ty))
                && taken(0, "nanos::World")
                && encode(&sig.result)
                && waits
        };
        let rest = match kind {
            Kind::Host => host,
            Kind::Nanos => host || job(),
            Kind::Uefi => machine(&sig, taken(0, "boot::Machine")),
            Kind::Bare => machine(&sig, taken(0, "boot::Kernel")),
        };
        let shaped = ast.visibility == Visibility::Private
            && function.modifier.is_none()
            && function.head.generics.is_empty()
            && rest;
        if !shaped {
            let last = Clauses::of(&function.head, false).last;
            let found = self.written(item, ast.token, last);
            self.report_expected(item, ast.token, Code::TargetEntryHead, forms, found);
        }
    }
}

/// Whether an entry head has a machine target's form: one taken parameter, the root grant, and
/// the result `Never`, neither failing nor waiting.
fn machine(sig: &super::sig::FnSig, grant: bool) -> bool {
    grant && sig.params.len() == 1 && sig.result == Ty::Never && sig.fails.is_none() && !sig.wait
}
