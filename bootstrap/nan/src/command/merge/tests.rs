use crate::command::tests::run_in;
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
