use crate::check::unchecked;
use crate::workspace::tests::Scratch;
use crate::workspace::{Target, Workspace};

const MANIFEST: &str =
    "//! Specification example.\n\npackage example {\n    language 1\n    targets any\n} example\n";

/// A documented const whose module's API snapshot holds it, so that checking it reads its
/// interface hash.
const SNAPSHOT: &str = "nan api 1\nitem \"example::MAX_RETRIES\"\ntext \"pub const MAX_RETRIES: u32 = 3\"\ninterface \"aekzejgsh5wrk4t4x3o2xffuu5n4hxqufc3cl5he32tg5ykieallmlk\"\ndoc \"aekzejgsh5wrk4t4x3o2xffuu5n4hxqufc3cl5he32tg5ykieallmlk\" \"Retries before a link counts as down.\"\ncheck \"ahrrb2ljwfnqls2khmgnsiulzm3c7vevb4pqij6hxywgznkkoxen62d\"\n";

const LOCK: &str =
    "nan lock 1\nsnapshot 0\ncheck \"agztj4xwgwq2vtzhnldtxaoj2jjl7wddvnzt3fyng3t3k5oms4pf2dl\"\n";

#[test]
fn a_const_hashed_before_its_check_has_an_interface_hash() {
    let scratch = Scratch::new(&[
        ("example/package.nan", MANIFEST),
        (
            "example/example.nan",
            "//! Specification example.\n\n/// Retries before a link counts as down.\npub const MAX_RETRIES: u32 = 3\n",
        ),
        ("example/.nan/lock.nan", LOCK),
        ("example/.nan/api/example.nan", SNAPSHOT),
    ]);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let checker = unchecked(
        &workspace,
        &workspace.closures[0],
        crate::stdlib::packages(),
        Target::Any,
    );
    let item = (0..checker.db.items.len())
        .map(|index| crate::check::db::ItemId(index as u32))
        .find(|id| !checker.db.is_std(*id) && checker.db.canonical(*id) == "example::MAX_RETRIES")
        .expect("the const");
    assert!(checker.hashes(item).interface.is_some());
}
