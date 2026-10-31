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

/// A kernel moving floats, through calls, a const, a literal, a wire encoding and decoding and a
/// total order, and counting bits lowers on each machine architecture without a float or vector
/// type, and without `popcnt`, which AArch64 computes in its vector registers.
#[test]
fn machine_code_holds_no_float_or_vector_value() {
    let source = "//! Moves floats.\n\n\
        struct Point derives Copy, Encode {\n    x: f64\n    y: f32\n} Point\n\n\
        const ORIGIN: Point = Point(x: 1.5, y: 2.5)\n\n\
        fn pick(p: Point, other: Point, near: bool) -> Point {\n    if near {\n        return p\n    }\n    return other\n} pick\n\n\
        fn main(take k: boot::Kernel) -> Never limits stack 16_384 {\n\
        \x20   let alloc = k.alloc()\n\
        \x20   let a = pick(p: ORIGIN, other: Point(x: 3.0, y: 4.0), near: false)\n\
        \x20   var out = mem::Bytes.empty(alloc)\n\
        \x20   mem::wire::Codec[Point].encode(a, mut out)\n\
        \x20   match attempt mem::wire::Codec[Point].decode(Untrusted.of(out.view()), alloc) {\n\
        \x20       Ok(back) => {\n            discard back\n        }\n\
        \x20       Err(error) => {\n            discard error\n        }\n\
        \x20   }\n\
        \x20   let ordered = core::TotalF64(value: a.x).cmp(core::TotalF64(value: 2.0)) == core::Ordering.Less\n\
        \x20   discard ordered\n\
        \x20   discard 0xF0F0u64.count_ones()\n\
        \x20   loop {}\n\
        } main\n";
    for target in ["x86_64-bare", "aarch64-bare"] {
        let manifest = format!(
            "//! Floats moved by a kernel.\n\npackage fl {{\n    language 1\n    targets {target}\n    entry main\n}} fl\n"
        );
        let scratch = Scratch::new(&[("fl/package.nan", &manifest), ("fl/fl.nan", source)]);
        let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
        let texts = crate::final_build::lowered_functions(&workspace, 0);
        assert!(!texts.is_empty(), "{target}: the kernel lowers");
        for (name, text) in texts {
            let floating = text
                .split(|c: char| !c.is_ascii_alphanumeric())
                .find(|token| {
                    let vector = token.split_once('x').is_some_and(|(lane, count)| {
                        lane.len() > 1
                            && lane[1..].bytes().all(|b| b.is_ascii_digit())
                            && !count.is_empty()
                            && count.bytes().all(|b| b.is_ascii_digit())
                    });
                    matches!(*token, "f32" | "f64")
                        || (*token == "popcnt" && target.starts_with("aarch64"))
                        || vector
                });
            assert_eq!(
                floating, None,
                "{target}: {name} holds {floating:?}:\n{text}"
            );
        }
    }
}
