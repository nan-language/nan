use std::fs;

use crate::command::tests::{MANIFEST, run_in};
use crate::workspace::tests::Scratch;

#[test]
fn a_failing_check_shows_what_changed_since_the_agent_last_saw_green() {
    let scratch = Scratch::new(&[
        ("app/package.nan", MANIFEST),
        (
            "app/app.nan",
            "//! Scaling.\n\npub fn scale(value: u32) -> u32 = value\n\npub fn doubled(value: u32) -> u32 {\n    return scale(value)\n} doubled\n",
        ),
    ]);
    assert_eq!(run_in(&scratch.0, "check --agent alpha").status, 0);
    let source = fs::read_to_string(scratch.0.join("app/app.nan")).expect("read");
    fs::write(
        scratch.0.join("app/app.nan"),
        source.replace("scale(value: u32) -> u32", "scale(value: u64) -> u64"),
    )
    .expect("write");
    let run = run_in(&scratch.0, "check --agent alpha");
    assert!(
        run.output.contains(
            "\nchanged app::scale app/app.nan:3\n    - pub fn scale(value: u32) -> u32 = value\n    + pub fn scale(value: u64) -> u64 = value\n"
        ),
        "{}",
        run.output
    );
    assert!(
        !run_in(&scratch.0, "check --agent beta")
            .output
            .contains("changed")
    );
}

#[test]
fn naming_fixes_rename_uses_and_offer_candidates_or_stubs() {
    let scratch = Scratch::new(&[
        ("app/package.nan", MANIFEST),
        (
            "app/app.nan",
            "//! Counts.\n\npub fn byteCount() -> u32 = 1\n\npub fn total() -> u32 {\n    return byteCount() + bite_count()\n} total\n\npub fn checked() -> u32 {\n    return zqxv(total())\n} checked\n",
        ),
    ]);
    let run = run_in(&scratch.0, "check");
    assert!(
        run.output
            .contains("\n    + pub fn byte_count() -> u32 = 1\n"),
        "{}",
        run.output
    );
    assert!(run.output.contains("\n    because note - - byteCount\n"));
    assert!(run.output.contains(
        "\n    + fn zqxv(value: u32) -> u32 {\n    +     return todo \"zqxv\"\n    + } zqxv\n"
    ));
}
