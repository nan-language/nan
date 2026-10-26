use crate::check::unchecked;
use crate::workspace::tests::Scratch;
use crate::workspace::{Target, Workspace};

const MANIFEST: &str =
    "//! Specification example.\n\npackage example {\n    language 1\n    targets any\n} example\n";

/// `grow` allocates and calls `relay`, which only calls `grow` back: one cycle.
const CYCLE: &str = "//! Specification example.\n\npub fn grow(mut log: mem::List[u32], count: u32) {\n    mut log.push(count)\n    if count > 0 {\n        relay(mut log, count: count - 1)\n    }\n} grow\n\npub fn relay(mut log: mem::List[u32], count: u32) {\n    if count > 0 {\n        grow(mut log, count: count - 1)\n    }\n} relay\n";

/// Whether each function of the cycle allocates, their digests computed in `order`.
fn allocating(order: [&str; 2]) -> Vec<bool> {
    let scratch = Scratch::new(&[
        ("example/package.nan", MANIFEST),
        ("example/example.nan", CYCLE),
    ]);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let checker = unchecked(
        &workspace,
        &workspace.closures[0],
        crate::stdlib::packages(),
        Target::Any,
    );
    let item = |path: &str| {
        (0..checker.db.items.len())
            .map(|index| crate::check::db::ItemId(index as u32))
            .find(|id| !checker.db.is_std(*id) && checker.db.canonical(*id) == path)
            .expect("the function")
    };
    order
        .iter()
        .map(|path| checker.digest(item(path)).alloc)
        .collect()
}

#[test]
fn every_member_of_a_cycle_has_its_facts_whichever_is_computed_first() {
    assert_eq!(
        allocating(["example::grow", "example::relay"]),
        [true, true]
    );
    assert_eq!(
        allocating(["example::relay", "example::grow"]),
        [true, true]
    );
}
