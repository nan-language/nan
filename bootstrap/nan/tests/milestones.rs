//! The milestone harness: in a copy of each scenario workspace of `tests/milestones`, runs the
//! commands its milestone states, then `nan verify`, which must accept the workspace after
//! running its tests; for M2, the obligation outcomes are also compared with the proof cache's.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

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

fn copied(milestone: &str) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/milestones")
        .join(milestone);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("milestones")
        .join(milestone);
    copy_tree(&source, &root).expect("a copy of the scenario");
    root
}

/// Runs `nan` in a directory and gives its output, asserting that its verdict is `ok`.
fn ok(dir: &Path, args: &[&str]) -> String {
    let run = Command::new(env!("CARGO_BIN_EXE_nan"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("a run of nan");
    let output = String::from_utf8_lossy(&run.stdout).into_owned();
    assert!(
        output.starts_with("ok "),
        "nan {} reports\n{output}",
        args.join(" ")
    );
    output
}

fn verified(root: &Path) {
    let output = ok(root, &["verify"]);
    assert!(
        !output
            .lines()
            .next()
            .unwrap_or_default()
            .contains("tests 0,"),
        "nan verify runs no test\n{output}"
    );
    fs::remove_dir_all(root).expect("the scenario's copy removed");
}

#[test]
fn m2() {
    let root = copied("m2");
    ok(&root, &["check"]);
    let mismatches = nan::proof_cache_mismatches(&root).expect("the scenario checked");
    assert!(
        mismatches.is_empty(),
        "the proof cache holds results the prover does not give for {mismatches:?}"
    );
    verified(&root);
}

/// The item the second release of the M3 scenario's dependency adds.
const ADDED: &str = "
/// Whether a raw reading is at full scale.
pub fn is_full(raw: u32) -> bool = raw >= 1_024
";

/// The code root a `nan publish` output reports.
fn published(output: &str) -> String {
    let line = output.lines().find(|line| line.starts_with("published "));
    let root = line.and_then(|line| line.split(' ').nth(2));
    root.expect("a published release").to_owned()
}

#[test]
fn m3() {
    let root = copied("m3");
    // The store name is this build's own, so that no release another build published under it
    // competes in resolution.
    let built = fs::metadata(env!("CARGO_BIN_EXE_nan"))
        .and_then(|metadata| metadata.modified())
        .expect("the binary's modification time");
    let built = built.duration_since(UNIX_EPOCH).unwrap_or_default();
    let name = format!("nan-milestones/m3/gauge-{:x}", built.as_nanos());
    let manifest = root.join("panel/package.nan");
    let text = fs::read_to_string(&manifest).expect("the panel manifest");
    let text = text.replace("\"nan-milestones/m3/gauge\"", &format!("\"{name}\""));
    fs::write(&manifest, text).expect("the panel manifest written");

    let first = published(&ok(
        &root,
        &["publish", "--local", "gauge", "--name", &name],
    ));
    let source = root.join("gauge/gauge.nan");
    let text = fs::read_to_string(&source).expect("the gauge source");
    fs::write(&source, text + ADDED).expect("the gauge source written");
    ok(&root, &["api", "update", "gauge"]);
    let second = published(&ok(&root, &["publish", "--local", "gauge"]));
    assert_ne!(first, second);
    let sync = ok(&root, &["dep", "sync", "panel"]);
    assert!(
        sync.lines()
            .any(|line| line.starts_with(&format!("release gauge - {second} "))),
        "nan dep sync locks another release than the second\n{sync}"
    );

    let versions = root.with_extension("merge");
    fs::create_dir_all(&versions).expect("the merge versions' directory");
    let file = root.join("panel/panel.nan");
    let base = fs::read_to_string(&file).expect("the panel source");
    let adding =
        |variant: &str| base.replacen("    Stop\n", &format!("    Stop\n    {variant}\n"), 1);
    let (base_path, theirs_path) = (versions.join("base.nan"), versions.join("theirs.nan"));
    fs::write(&base_path, &base).expect("the base version");
    fs::write(&theirs_path, adding("Resume")).expect("their version");
    fs::write(&file, adding("Pause")).expect("our version");
    let merged = ok(
        &root,
        &[
            "merge",
            base_path.to_str().expect("a UTF-8 path"),
            "panel/panel.nan",
            theirs_path.to_str().expect("a UTF-8 path"),
            "--path",
            "panel/panel.nan",
        ],
    );
    assert!(
        merged.contains(", conflicts 0,"),
        "nan merge reports\n{merged}"
    );
    let text = fs::read_to_string(&file).expect("the merged source");
    assert!(
        text.contains("    Stop\n    Pause\n    Resume\n"),
        "nan merge writes\n{text}"
    );
    fs::remove_dir_all(versions).expect("the merge versions removed");
    verified(&root);
}

#[test]
fn m6() {
    verified(&copied("m6"));
}
