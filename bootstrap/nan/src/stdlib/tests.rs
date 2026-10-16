use super::*;
use crate::check::db::{ItemId, TypeKey};
use crate::check::ty::Ty;
use crate::interp::{End, Interpreter, Val};
use crate::nir::Role;
use crate::syntax::ast::{ClauseKind, FnModifier, ItemKind, Visibility};

/// The std packages as the workspace checked.
fn std_workspace() -> Workspace {
    Workspace::of_files(PathBuf::from("std"), &FILES)
}

#[test]
fn std_checks_without_diagnostics() {
    let workspace = std_workspace();
    let found: Vec<String> = crate::check::reported(&workspace, &crate::scope::Scope::Workspace)
        .iter()
        .map(|diagnostic| format!("{diagnostic:?}"))
        .collect();
    assert!(found.is_empty(), "{}", found.join("\n"));
}

/// The types of the values only machine targets give.
const MACHINE_VALUES: [&str; 11] = [
    "boot::Machine",
    "boot::Kernel",
    "boot::Services",
    "hw::Ring0",
    "hw::Region",
    "hw::PhysFrame",
    "hw::PageTableRoot",
    "hw::IrqContext",
    "hw::TrapFrame",
    "hw::SpinGuard",
    "hw::SavedContext",
];

/// The other functions no test or example on the host can call.
const HOST_UNCALLABLE: [&str; 5] = [
    "hw::Mmio.map",
    "hw::Ports.map",
    "hw::Idt.of_vectors",
    "hw::Anchor.with",
    "core::Untrusted.inspect",
];

#[test]
fn every_pub_std_function_has_an_example_or_a_test() {
    let workspace = std_workspace();
    let checker = crate::check::checked(
        &workspace,
        &workspace.closures[0],
        &[],
        crate::workspace::Target::Any,
    );
    let db = &checker.db;
    let items = (0..db.items.len()).map(|index| ItemId(index as u32));
    let tested: Vec<ItemId> = items
        .clone()
        .filter_map(|item| match &db.ast(item).kind {
            ItemKind::Test(test) => match checker.test_target(item, test) {
                Some(crate::check::testing::Target::Function(target)) => Some(target),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let missing: Vec<String> = items
        .filter(|item| {
            let ast = db.ast(*item);
            let ItemKind::Fn(function) = &ast.kind else {
                return false;
            };
            let example = function
                .clauses
                .iter()
                .any(|clause| matches!(clause.kind, ClauseKind::Example(_)) && !clause.disputed);
            let path = db.canonical(*item);
            let machine = |ty: ItemId| MACHINE_VALUES.contains(&db.canonical(ty).as_str());
            let owner = match db.owners.get(item) {
                Some(TypeKey::Item(owner)) => Some(*owner),
                _ => None,
            };
            let machine_value = owner.is_some_and(machine)
                || checker.fn_sig(*item).is_some_and(|sig| {
                    sig.params
                        .iter()
                        .any(|param| matches!(&param.ty, Ty::Item(ty, _) if machine(*ty)))
                });
            let landing = matches!(function.modifier, Some((FnModifier::Landing, _)));
            let callable = !machine_value && !landing && !HOST_UNCALLABLE.contains(&path.as_str());
            ast.visibility == Visibility::Pub && callable && !example && !tested.contains(item)
        })
        .map(|item| db.canonical(item))
        .collect();
    assert!(missing.is_empty(), "{}", missing.join("\n"));
}

#[test]
fn every_std_example_runs_to_true() {
    let workspace = std_workspace();
    let checker = crate::check::checked(
        &workspace,
        &workspace.closures[0],
        &[],
        crate::workspace::Target::Any,
    );
    let mut failing = Vec::new();
    for item in (0..checker.db.items.len()).map(|index| ItemId(index as u32)) {
        let Some(body) = checker.typed(item) else {
            continue;
        };
        for index in (0..body.examples.len()).filter(|index| !body.examples[*index].disputed) {
            let end =
                Interpreter::new(&checker).run(item, Role::Example, index as u32, Vec::new(), &[]);
            if !matches!(end, End::Value(Val::Bool(true))) {
                failing.push(format!("{} {index}: {end:?}", checker.db.canonical(item)));
            }
        }
    }
    assert!(failing.is_empty(), "{}", failing.join("\n"));
}
