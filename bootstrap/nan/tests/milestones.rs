//! The milestone harness: runs `nan test` in each scenario workspace of `tests/milestones`,
//! which passes when it reports no diagnostic and every test it runs passes.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Copies a directory tree into another, made anew.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.exists() {
        fs::remove_dir_all(to)?;
    }
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        match entry.file_type()?.is_dir() {
            true => copy_tree(&entry.path(), &target)?,
            false => drop(fs::copy(entry.path(), target)?),
        }
    }
    Ok(())
}

fn scenario(milestone: &str) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/milestones")
        .join(milestone);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("milestones")
        .join(milestone);
    copy_tree(&source, &root).expect("a copy of the scenario");
    let test = Command::new(env!("CARGO_BIN_EXE_nan"))
        .arg("test")
        .current_dir(&root)
        .output()
        .expect("a run of nan test");
    let output = String::from_utf8_lossy(&test.stdout);
    let verdict = output.lines().next().unwrap_or_default();
    let passed = verdict.starts_with("ok test .: errors 0, incomplete 0, tests ")
        && verdict.ends_with(", failed 0")
        && !verdict.contains("tests 0,");
    assert!(
        passed,
        "nan test in the {milestone} scenario reports\n{output}"
    );
    fs::remove_dir_all(&root).expect("the scenario's copy removed");
}

#[test]
fn m2() {
    scenario("m2");
}

#[test]
fn m6() {
    scenario("m6");
}
