//! The milestone harness: in a copy of each scenario workspace of `tests/milestones`, runs the
//! commands its milestone states, then `nan verify`, which must accept the workspace after
//! running its tests; for M2, the obligation outcomes are also compared with the proof cache's.
//! Its runs of `nan` use the harnesses' home directory and a local store of their own, which each
//! scenario starts empty.

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

fn copied(milestone: &str) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/milestones")
        .join(milestone);
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let root = tmp.join("milestones").join(milestone);
    copy_tree(&source, &root).expect("a copy of the scenario");
    let store = tmp.join("stores").join(milestone);
    match fs::remove_dir_all(&store) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            panic!("the scenario's local store not emptied: {error}")
        }
        _ => {}
    }
    fs::create_dir_all(tmp.join("home")).expect("the harnesses' home directory");
    fs::create_dir_all(tmp.join("stores")).expect("the directory of local stores");
    nan::use_home(tmp.join("home"), Some(store));
    root
}

/// Runs `nan` in a directory and gives its output, asserting that its verdict is `ok`.
fn ok(dir: &Path, args: &[&str]) -> String {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let output = nan::run(&args, dir).output;
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
    let first = published(&ok(
        &root,
        &[
            "publish",
            "--local",
            "gauge",
            "--name",
            "nan-milestones/m3/gauge",
        ],
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
