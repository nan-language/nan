use std::collections::HashMap;

use super::{Instance, Lowering};
use crate::check::checked;
use crate::check::db::ItemId;
use crate::check::ty::no_args;
use crate::nir::Role;
use crate::workspace::tests::Scratch;
use crate::workspace::{Target, Workspace};

#[test]
fn distinct_instances_with_one_symbol_name_collide() {
    let scratch = Scratch::new(&[
        (
            "example/package.nan",
            "//! Scales.\n\npackage example {\n    language 1\n    targets any\n} example\n",
        ),
        (
            "example/example.nan",
            "//! Scales.\n\npub fn scale(value: u64) -> u64 = value\n",
        ),
    ]);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let std = crate::stdlib::packages();
    let checker = checked(&workspace, &workspace.closures[0], std, Target::X64Host);
    let item = (0..checker.db.items.len())
        .map(|index| ItemId(index as u32))
        .find(|item| checker.db.canonical(*item) == "example::scale")
        .expect("the function");
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
