use std::fs;

use crate::command::tests::{MANIFEST, records, run_in};
use crate::workspace::tests::Scratch;

const SHARE: &str = "//! Shares.\n\n/// A share of a total, in percent.\npub fn share(part: u32) -> u32\n    ensures result <= 100\n{\n    if part > 50 {\n        return 50\n    }\n    return part\n} share\n";

#[test]
fn an_ensures_the_current_spec_entails_is_kept_and_a_weaker_one_is_pending() {
    let scratch = Scratch::new(&[("app/package.nan", MANIFEST), ("app/app.nan", SHARE)]);
    let snapshot = scratch.0.join("app/.nan/api/app.nan");
    run_in(&scratch.0, "api update");
    let stronger = SHARE.replace("ensures result <= 100", "ensures result <= 50");
    fs::write(scratch.0.join("app/app.nan"), &stronger).expect("the stronger ensures");
    run_in(&scratch.0, "api update");
    let accepted = fs::read_to_string(&snapshot).expect("the snapshot");
    assert!(
        accepted.contains("\naccepted \"ensures result <= 50\"\n"),
        "{accepted}"
    );
    assert!(!accepted.contains("\npending "), "{accepted}");
    let weaker = stronger
        .replace("ensures result <= 50", "ensures result <= 60")
        .replace("return 50", "return 60");
    fs::write(scratch.0.join("app/app.nan"), weaker).expect("the weaker ensures");
    let run = run_in(&scratch.0, "api update");
    assert!(
        run.output
            .contains("\nincomplete api.weakening-pending app/app.nan:4 app::share\n"),
        "{}",
        run.output
    );
    let pending = fs::read_to_string(&snapshot).expect("the snapshot");
    assert!(pending.contains(" \"ensures result <= 50\"\n"), "{pending}");
}

#[test]
fn a_pending_weakening_prints_among_the_diagnostics_of_the_scope_by_line() {
    let scratch = Scratch::new(&[("app/package.nan", MANIFEST), ("app/app.nan", SHARE)]);
    run_in(&scratch.0, "api update");
    let weaker = SHARE.replace("ensures result <= 100", "ensures result <= 101");
    let source = format!("{weaker}\n/// Nothing.\npub fn none() -> u32 = nope\n");
    fs::write(scratch.0.join("app/app.nan"), source).expect("the weaker ensures");
    let run = run_in(&scratch.0, "api update");
    assert_eq!(
        records(&run.output),
        [
            "incomplete doc.stale app/app.nan:3",
            "incomplete api.weakening-pending app/app.nan:4",
            "error name.unknown app/app.nan:14"
        ]
    );
}
