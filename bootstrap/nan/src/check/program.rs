//! Targets and programs: the targets each dependency covers, dependencies on programs, the one
//! concrete target a program declares and the head of its entry function.

use super::Checker;
use super::body::Clauses;
use super::db::{ItemId, PackageId};
use super::ty::{Scalar, Ty};
use crate::diagnostic::Code;
use crate::syntax::ast::{ItemKind, Mode, Visibility};
use crate::workspace::{Kind, Target};

impl Checker<'_> {
    /// The rules of a package's `targets`, `entry` and `dep` directives: a program declares one
    /// concrete target and an entry function `main` of its root module, and each dependency in
    /// the workspace covers every target the package declares and is no program, unless its
    /// `dep` directive lacks a grant its package needs.
    pub fn check_program(&self, package: PackageId) {
        let data = self.db.package(package);
        let Some(manifest) = &data.source.manifest else {
            return;
        };
        let path = &data.source.files[0].path;
        let concrete = matches!(&manifest.targets[..], [target] if *target != Target::Any);
        if let Some((_, line)) = &manifest.entry
            && manifest.targets_line.is_some()
            && !concrete
        {
            self.report_file(path, *line, Code::TargetProgramTargets);
        }
        if let Some((name, line)) = &manifest.entry
            && let Some(module) = data.root
            && !self.db.module(module).abandoned
        {
            let main = self
                .db
                .lookup(module, "main")
                .is_some_and(|item| matches!(self.db.ast(item).kind, ItemKind::Fn(_)));
            if name != "main" || !main {
                self.report_file(path, *line, Code::TargetEntryMissing);
            }
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
            if !manifest.targets.iter().all(covers) {
                self.report_file(path, dep.line, Code::TargetDependencyUncovered);
            } else if declared.entry.is_some() {
                self.report_file(path, dep.line, Code::TargetDepProgram);
            }
        }
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
        let failed = sig.params.iter().any(|param| param.ty.has_error())
            || sig.result.has_error()
            || sig.fails.as_ref().is_some_and(Ty::has_error);
        if failed || self.reported_at(item, ast.token) {
            return;
        }
        let Some(kind) = self.db.target.kind() else {
            return;
        };
        let (grant, forms) = match kind {
            Kind::Host => (
                "host::World",
                "fn main(take P: host::World) fails E uses wait or fn main(take P: host::World) -> \
                 u8 fails E uses wait",
            ),
            Kind::Uefi => ("boot::Machine", "fn main(take P: boot::Machine) -> Never"),
            Kind::Bare => ("boot::Kernel", "fn main(take P: boot::Kernel) -> Never"),
        };
        let parameter = matches!(&sig.params[..], [param]
            if param.mode == Mode::Take && !param.is_self && self.is_std(&param.ty, grant));
        let failure = |fails: &Ty| {
            let rendered_enum = matches!(fails, Ty::Item(failure, _)
                if matches!(self.db.ast(*failure).kind, ItemKind::Enum(_)))
                && self
                    .db
                    .core_item("Render")
                    .is_some_and(|render| self.implements(fails, render));
            rendered_enum || self.is_std(fails, "task::Cancelled")
        };
        let rest = match kind {
            Kind::Host => {
                matches!(sig.result, Ty::Unit | Ty::Scalar(Scalar::U8))
                    && sig.fails.as_ref().is_some_and(failure)
                    && sig.wait
            }
            _ => sig.result == Ty::Never && sig.fails.is_none() && !sig.wait,
        };
        let shaped = ast.visibility == Visibility::Private
            && function.modifier.is_none()
            && function.head.generics.is_empty()
            && parameter
            && rest;
        if !shaped {
            let last = Clauses::of(&function.head, false).last;
            let found = self.written(item, ast.token, last);
            self.report_expected(
                item,
                ast.token,
                Code::TargetEntryHead,
                forms.to_owned(),
                found,
            );
        }
    }
}
