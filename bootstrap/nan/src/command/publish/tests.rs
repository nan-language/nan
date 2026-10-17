use std::fs;

use crate::command::tests::run_in;
use crate::workspace::tests::Scratch;

const FIRST: &str = "//! Modes.\n\n/// A mode.\npub open enum Mode derives Eq, Encode {\n    Idle\n    Busy\n} Mode\n\n/// The level of a mode.\npub fn Mode.level(self) -> u8 {\n    return match self {\n        Mode.Idle => 0\n        Mode.Busy => 2\n    }\n} Mode.level\n";

const SECOND: &str = "//! Modes.\n\n/// A mode.\npub open enum Mode derives Eq, Encode {\n    Idle\n    Busy\n    Draining\n} Mode\n\n/// The level of a mode.\npub fn Mode.level(self) -> u8 {\n    return match self {\n        Mode.Idle => 0\n        Mode.Busy => 2\n        Mode.Draining => 1\n    }\n} Mode.level\n\n/// Whether a mode is idle.\npub fn Mode.is_idle(self) -> bool = self == Mode.Idle\n";

/// The release and encode classes a publish prints.
fn classes(output: &str) -> String {
    let line = output.lines().find(|line| line.starts_with("published "));
    let fields: Vec<&str> = line.expect("a published release").split(' ').collect();
    fields[3..].join(" ")
}

#[test]
fn a_new_variant_of_an_open_enum_is_additive_and_a_removed_one_breaking() {
    let scratch = Scratch::new(&[(
        "modes/package.nan",
        "//! Modes.\n\npackage modes {\n    language 1\n    targets any\n} modes\n",
    )]);
    let dir = &scratch.0;
    let source = dir.join("modes/modes.nan");
    let name = format!(
        "nan-test/{}",
        dir.file_name().expect("a directory name").to_string_lossy()
    );
    fs::write(&source, FIRST).expect("the first release");
    run_in(dir, "api update");
    run_in(dir, &format!("publish --local modes --name {name}"));
    fs::write(&source, SECOND).expect("the second release");
    run_in(dir, "api update --confirm-docs modes::Mode");
    let second = run_in(dir, "publish --local modes");
    assert_eq!(
        classes(&second.output),
        "additive additive",
        "{}",
        second.output
    );
    let third = SECOND
        .replace("    Busy\n    Draining\n", "    Draining\n")
        .replace("        Mode.Busy => 2\n", "");
    fs::write(&source, third).expect("the third release");
    run_in(dir, "api update --confirm-docs modes::Mode");
    let third = run_in(dir, "publish --local modes");
    assert_eq!(
        classes(&third.output),
        "breaking breaking",
        "{}",
        third.output
    );
}
