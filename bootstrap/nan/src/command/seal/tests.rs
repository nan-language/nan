use std::path::Path;
use std::process::Command;

use crate::command::tests::run_in;
use crate::workspace::tests::Scratch;

const MANIFEST: &str = "//! Process facts.\n\npackage procinfo {\n    language 1\n    targets x86_64-host\n    allow trusted in procinfo\n} procinfo\n";

const SOURCE: &str = "//! Reads process facts.\n\nforeign fn getpid() -> i32 limits notrap, terminates, noalloc convention \"sysv\"\n\npub fn process_id() -> i32 {\n    return getpid()\n} process_id\n";

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("a run of git")
        .status;
    assert!(status.success(), "git {args:?}");
}

#[test]
fn a_seal_needs_a_trust_escape_and_another_author_than_the_sealed_code() {
    let scratch = Scratch::new(&[
        ("procinfo/package.nan", MANIFEST),
        ("procinfo/procinfo.nan", SOURCE),
    ]);
    let dir = &scratch.0;
    run_in(dir, "api update");
    let verdict = |line: &str| {
        run_in(dir, line)
            .output
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned()
    };
    assert!(verdict("verify").starts_with("failed verify"));
    assert!(
        run_in(dir, "seal procinfo::process_id --as ana@example.org")
            .output
            .contains("\nerror seal.unneeded procinfo/procinfo.nan:5 ")
    );
    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.email", "Ana@Example.org"]);
    git(dir, &["config", "user.name", "Ana"]);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "procinfo"]);
    let same = run_in(dir, "seal procinfo::getpid --as ana-review");
    assert!(
        same.output
            .contains("\nincomplete seal.same-author procinfo/procinfo.nan:3 "),
        "{}",
        same.output
    );
    git(dir, &["config", "user.email", "ben@example.org"]);
    assert!(verdict("seal procinfo::getpid --as ben-review").starts_with("ok seal"));
    assert!(verdict("verify").starts_with("ok verify"));
}

#[test]
fn a_seal_accepts_the_first_requires_of_a_private_function() {
    let scratch = Scratch::new(&[
        (
            "levels/package.nan",
            "//! Levels.\n\npackage levels {\n    language 1\n    targets any\n} levels\n",
        ),
        (
            "levels/levels.nan",
            "//! Halves levels.\n\nfn halved(level: u32) -> u32\n    requires level > 0\n{\n    return level / 2\n} halved\n\npub fn level_of(level: u32) -> u32 {\n    if level == 0 {\n        return 0\n    }\n    return halved(level)\n} level_of\n",
        ),
    ]);
    let dir = &scratch.0;
    run_in(dir, "api update");
    let verdict = |line: &str| run_in(dir, line).output;
    assert!(verdict("verify").contains("\nincomplete api.weakening-pending levels/levels.nan:3 "));
    assert!(verdict("seal levels::halved --as ana-review").starts_with("ok seal"));
    assert!(verdict("verify").starts_with("ok verify"));
}
