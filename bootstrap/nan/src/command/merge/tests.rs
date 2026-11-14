use crate::command::tests::{records, run_in};
use crate::workspace::tests::Scratch;

const MANIFEST: &str =
    "//! Example.\n\npackage example {\n    language 1\n    targets any\n} example\n";

#[test]
fn a_use_of_an_item_the_other_side_changed_is_a_semantic_conflict() {
    let base =
        "//! Example.\n\n/// Twice a value.\npub fn scale(x: u32) -> u32 = x.saturating_mul(2)\n";
    let ours = base.replace("(x: u32) -> u32", "(x: u64) -> u64");
    let theirs = format!(
        "{base}\n/// Four times a value.\npub fn quadruple(x: u32) -> u32 = scale(scale(x))\n"
    );
    let scratch = Scratch::new(&[
        ("base/example/package.nan", MANIFEST),
        ("base/example/example.nan", base),
        ("ours/example/package.nan", MANIFEST),
        ("ours/example/example.nan", &ours),
        ("theirs/example/package.nan", MANIFEST),
        ("theirs/example/example.nan", &theirs),
    ]);
    let run = run_in(&scratch.0, "merge base ours theirs");
    assert!(
        run.output
            .contains("\nerror merge.semantic-conflict example/example.nan:7 example::quadruple\n"),
        "{}",
        run.output
    );
    assert!(
        run.output
            .contains("\n    because note - - + pub fn scale(x: u64) -> u64")
    );
}

#[test]
fn a_workspace_merge_prints_its_conflicts_among_its_checks_by_path_and_line() {
    let limit = |value: u32| format!("//! Limits.\n\npub const LIMIT: u32 = {value}\n");
    let scale = "//! Example.\n\n/// Twice a value.\npub fn scale(x: u32) -> u32 = x * 2\n";
    let scratch = Scratch::new(&[
        ("base/example/package.nan", MANIFEST),
        ("base/example/example.nan", scale),
        ("base/example/z.nan", &limit(10)),
        ("ours/example/package.nan", MANIFEST),
        ("ours/example/example.nan", &scale.replace("x * 2", "true")),
        ("ours/example/z.nan", &limit(20)),
        ("theirs/example/package.nan", MANIFEST),
        ("theirs/example/example.nan", scale),
        ("theirs/example/z.nan", &limit(30)),
    ]);
    let run = run_in(&scratch.0, "merge base ours theirs");
    assert_eq!(
        records(&run.output),
        [
            "error type.mismatch example/example.nan:4",
            "error merge.conflict example/z.nan:3",
            "error merge.unresolved example/z.nan:3"
        ]
    );
}
