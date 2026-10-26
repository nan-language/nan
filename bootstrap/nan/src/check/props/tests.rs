use crate::check::unchecked;
use crate::workspace::tests::Scratch;
use crate::workspace::{Target, Workspace};

const MANIFEST: &str =
    "//! Specification example.\n\npackage example {\n    language 1\n    targets any\n} example\n";

/// `Outer` holds a function value, which is not Send, and reaches itself through `Inner`.
const RECURSIVE: &str = "//! Specification example.\n\npub closed struct Outer {\n    pub call: fn(x: u32) -> u32\n    pub inner: mem::Box[Inner]\n} Outer\n\npub closed struct Inner {\n    pub outer: Option[mem::Box[Outer]]\n} Inner\n";

/// Whether each struct is Send, their properties decided in `order`.
fn sending(order: [&str; 2]) -> Vec<bool> {
    let scratch = Scratch::new(&[
        ("example/package.nan", MANIFEST),
        ("example/example.nan", RECURSIVE),
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
            .expect("the struct")
    };
    order
        .iter()
        .map(|path| checker.definition_props(item(path)).send)
        .collect()
}

#[test]
fn recursive_definitions_have_their_properties_whichever_is_decided_first() {
    assert_eq!(
        sending(["example::Outer", "example::Inner"]),
        [false, false]
    );
    assert_eq!(
        sending(["example::Inner", "example::Outer"]),
        [false, false]
    );
}
