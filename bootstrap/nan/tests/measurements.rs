//! The bounded measurements of the freeze that checking gives: the prover proves at least 90% of
//! the bounds and overflow obligations of the M4 scenario's packages and, separately, of the NaN
//! front end's workspace, M5's. Each runs on a copy of its workspace with the harnesses' home
//! directory and a local store of its own.

use std::fs;
use std::path::{Path, PathBuf};

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

/// Checks a copy of the workspace at `source`, given relative to the crate, and asserts that
/// the prover proves at least 90% of its bounds and overflow obligations.
fn discharged(source: &str, name: &str) {
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let root = tmp.join("measurements").join(name);
    copy_tree(&Path::new(env!("CARGO_MANIFEST_DIR")).join(source), &root)
        .expect("a copy of the workspace");
    fs::create_dir_all(tmp.join("home")).expect("the harnesses' home directory");
    nan::use_home(tmp.join("home"), Some(tmp.join("stores").join(name)));
    let discharge = nan::measure(&root)
        .expect("the workspace checked")
        .discharge;
    assert!(
        discharge.proved * 10 >= discharge.total * 9,
        "the prover proves {} of {} bounds and overflow obligations, below 90%; the others are\n{}",
        discharge.proved,
        discharge.total,
        discharge.unproved.join("\n")
    );
    fs::remove_dir_all(root).expect("the copy removed");
}

#[test]
fn m4_discharge() {
    discharged("tests/milestones/m4", "m4");
}

#[test]
fn m5_discharge() {
    discharged("../../toolchain", "m5");
}
