use std::collections::HashMap;

use super::{Instance, Lowering, dumping};
use crate::check::checked;
use crate::check::db::ItemId;
use crate::check::ty::no_args;
use crate::nir::Role;
use crate::workspace::tests::Scratch;
use crate::workspace::{Target, Workspace};

/// A scratch workspace of one package `example` whose module holds `source`.
fn example(source: &str) -> Scratch {
    Scratch::new(&[
        (
            "example/package.nan",
            "//! Example.\n\npackage example {\n    language 1\n    targets any\n} example\n",
        ),
        ("example/example.nan", source),
    ])
}

fn item_named(checker: &crate::check::Checker, path: &str) -> ItemId {
    (0..checker.db.items.len())
        .map(|index| ItemId(index as u32))
        .find(|item| checker.db.canonical(*item) == path)
        .expect("the function")
}

#[test]
fn distinct_instances_with_one_symbol_name_collide() {
    let scratch = example("//! Scales.\n\npub fn scale(value: u64) -> u64 = value\n");
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let std = crate::stdlib::packages();
    let checker = checked(&workspace, &workspace.closures[0], std, Target::X64Host);
    let item = item_named(&checker, "example::scale");
    let mut lowering = Lowering::new(&checker, HashMap::new(), Vec::new(), None);
    lowering.instance(Instance::Nir {
        item,
        role: Role::Body,
        index: 0,
        args: no_args(),
    });
    lowering.instance(Instance::Supplied {
        item,
        args: no_args(),
    });
    assert_eq!(lowering.collisions.len(), 1);
}

/// A function computing only on Secret values, and each Secret member it calls, lowers to one
/// block without a branch or a division.
#[test]
fn secret_code_lowers_without_branches_or_divisions() {
    let scratch = example(
        "//! Ladder steps.\n\n\
         pub fn step(bit: core::Secret[u64], a: core::Secret[u64], b: core::Secret[u64]) -> core::Secret[u64] {\n\
         \x20   let swap = bit.bit_and(core::Secret.of(1)).eq(core::Secret.of(1))\n\
         \x20   let low = core::Secret[u32].truncate(a.wrapping_mul(b).bit_xor(a.mul_high(b)))\n\
         \x20   let sum = core::Secret[u64].widen(low).wrapping_add(a).wrapping_sub(b).rotate_left(13)\n\
         \x20   let smaller = sum.lt(b).bit_or(swap.bit_not()).ne(core::Secret.of(false))\n\
         \x20   return smaller.select(sum.bit_not(), if_false: b)\n\
         } step\n",
    );
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let std = crate::stdlib::packages();
    for target in [Target::X64Host, Target::Aarch64Host] {
        let checker = checked(&workspace, &workspace.closures[0], std, target);
        let item = item_named(&checker, "example::step");
        let mut lowering = Lowering::new(&checker, HashMap::new(), Vec::new(), None);
        let ((), texts) = dumping(|| {
            lowering.instance(Instance::Nir {
                item,
                role: Role::Body,
                index: 0,
                args: no_args(),
            });
            lowering.drain();
        });
        assert_eq!(texts.len(), 19, "the step and its eighteen Secret members");
        for (name, text) in texts {
            let blocks = text
                .lines()
                .filter(|line| line.trim_start().starts_with("block"))
                .count();
            assert_eq!(blocks, 1, "{name} has one block:\n{text}");
            for op in ["brif", "br_table", "div", "rem"] {
                assert!(!text.contains(op), "{name} holds no {op}:\n{text}");
            }
        }
    }
}
