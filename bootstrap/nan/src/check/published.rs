//! What a publish summary records of a package under each target it declares: the capability
//! methods each exported function reaches with its trap sites, and the package's authority
//! summary.

use std::collections::BTreeSet;

use super::{check_targets, checked};
use crate::nir::CheckKind;
use crate::prover::{Obligation, Status};
use crate::store::FunctionSummary;
use crate::syntax::ast::{ItemKind, Visibility};
use crate::workspace::{Closure, Package, Workspace};

/// The trap kind of an obligation kept as a run-time check.
fn trap_kind(obligation: Obligation) -> &'static str {
    match obligation {
        Obligation::Check(CheckKind::Requires) => "trap.requires",
        Obligation::Check(CheckKind::Ensures) => "trap.ensures",
        Obligation::Check(CheckKind::Predicate) => "trap.predicate",
        Obligation::Check(_) => "trap.expect",
        Obligation::Overflow => "trap.overflow",
        Obligation::DivisionByZero => "trap.division-by-zero",
        Obligation::Shift => "trap.shift",
        Obligation::Conversion => "trap.conversion",
        Obligation::Bounds => "trap.bounds",
    }
}

/// The function summaries and authority summary of a closure's workspace package: per exported
/// function and target, the capability methods it reaches and its kept checks; and each entry of
/// its authority summary as a lock writes it.
pub fn summaries(
    workspace: &Workspace,
    closure: &Closure,
    package: &Package,
) -> (Vec<FunctionSummary>, Vec<String>) {
    let std = crate::stdlib::packages();
    let name = package.name().unwrap_or_default();
    let mut functions = Vec::new();
    let mut authority = BTreeSet::new();
    for target in check_targets(package) {
        let checker = checked(workspace, closure, std, target);
        let Some(id) = checker.package_named(name) else {
            continue;
        };
        authority.extend(
            checker
                .authority_summary(id)
                .iter()
                .map(|entry| entry.written()),
        );
        for &module in &checker.db.package(id).modules {
            for &item in &checker.db.module(module).items {
                let ast = checker.db.ast(item);
                if !matches!(ast.kind, ItemKind::Fn(_)) || ast.visibility != Visibility::Pub {
                    continue;
                }
                let data = checker.db.item(item);
                let mut methods: Vec<String> = checker
                    .methods_reached(item)
                    .into_iter()
                    .map(|method| checker.db.canonical(method))
                    .collect();
                methods.sort();
                methods.dedup();
                let traps = checker
                    .obligations(item)
                    .iter()
                    .filter(|site| site.status == Status::Checked && !site.role.is_contract())
                    .map(|site| {
                        (
                            trap_kind(site.obligation).to_owned(),
                            data.file.path.clone(),
                            data.line(site.span.first),
                        )
                    })
                    .collect();
                functions.push(FunctionSummary {
                    path: checker.canonical_path(item).unwrap_or_default(),
                    target: target.name().to_owned(),
                    methods,
                    traps,
                });
            }
        }
    }
    functions.sort_by(|left, right| (&left.path, &left.target).cmp(&(&right.path, &right.target)));
    (functions, authority.into_iter().collect())
}
