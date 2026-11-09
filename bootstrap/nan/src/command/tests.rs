use super::*;
use crate::workspace::tests::Scratch;

pub(super) const MANIFEST: &str =
    "//! A package.\n\npackage app {\n    language 1\n    targets any\n} app\n";

fn args(line: &str) -> Vec<String> {
    line.split(' ').map(str::to_owned).collect()
}

pub(super) fn run_in(dir: &Path, line: &str) -> Run {
    run(&args(line), dir)
}

#[test]
fn an_omitted_scope_is_the_package_holding_the_current_directory_or_the_workspace() {
    let scratch = Scratch::new(&[
        ("app/package.nan", MANIFEST),
        ("app/app.nan", "//! Root.\n\nconst X = 1\n"),
    ]);
    let inside = run_in(&scratch.0.join("app"), "check");
    assert_eq!(inside.status, 1);
    assert_eq!(
        inside.output,
        "failed check app: errors 1, incomplete 0\nerror item.const-untyped app/app.nan:3 app::X\n    const X = 1\nfailed check app: errors 1, incomplete 0\n"
    );
    assert!(
        run_in(&scratch.0, "check")
            .output
            .starts_with("failed check .: errors 1")
    );
}

#[test]
fn a_scope_keeps_the_diagnostics_reported_on_its_lines() {
    let scratch = Scratch::new(&[
        ("app/package.nan", MANIFEST),
        ("app/app.nan", "//! Root.\n\nconst X = 1\n\nconst Y = 2\n"),
        ("app/units/units.nan", "//! Units.\n\nconst Z = 3\n"),
    ]);
    let count = |scope: &str| {
        run_in(&scratch.0, &format!("check {scope}"))
            .output
            .lines()
            .filter(|line| line.starts_with("error"))
            .count()
    };
    assert_eq!(count("."), 3);
    assert_eq!(count("app"), 3);
    assert_eq!(count("app::units"), 1);
    assert_eq!(count("app::Y"), 1);
    assert_eq!(count("app/app.nan"), 2);
    assert_eq!(count("app/app.nan:5"), 1);
    let inside = run_in(&scratch.0.join("app"), "check units::Z").output;
    assert!(
        inside.contains("error item.const-untyped app/units/units.nan:3 units::Z"),
        "{inside}"
    );
}

#[test]
fn several_packages_write_canonical_paths() {
    let scratch = Scratch::new(&[
        ("app/package.nan", MANIFEST),
        ("app/units/units.nan", "//! Units.\n\nconst Z = 3\n"),
        ("lib/package.nan", &MANIFEST.replace("app", "lib")),
    ]);
    assert!(
        run_in(&scratch.0, "check")
            .output
            .contains(" app::units::Z\n")
    );
}

#[test]
fn command_lines_outside_the_synopsis_are_tool_usage() {
    let scratch = Scratch::new(&[("app/package.nan", MANIFEST)]);
    for line in [
        "frobnicate",
        "check --affected",
        "check a b",
        "check --budget 99",
        "check --skip",
        "test app --replay sc/x86_64-host/app::halves/0/aaaaaaaaaaaaaaa",
        "test --affected --replay sc/x86_64-host/app::halves/0/aaaaaaaaaaaaaaa",
        "check app --prefix app/app.nan",
        "check --prefix app/app.nan --radius",
        "check --prefix app/app.nan --agent other",
    ] {
        let run = run_in(&scratch.0, line);
        assert_eq!(run.status, 2, "{line}");
        assert!(
            run.output.contains("\nerror tool.usage - -\n"),
            "{line}: {}",
            run.output
        );
    }
    assert_eq!(
        run_in(&scratch.0, "frobnicate").output,
        "invalid frobnicate: records 1\nerror tool.usage - -\ninvalid frobnicate: records 1\n"
    );
}

#[test]
fn a_directory_without_packages_is_no_workspace_and_stays_untouched() {
    let scratch = Scratch::new(&[("notes/readme.txt", "")]);
    let run = run_in(&scratch.0, "check");
    assert_eq!(
        (run.status, run.output.as_str()),
        (
            2,
            "invalid check .: errors 1, incomplete 0\nerror tool.no-workspace - -\ninvalid check .: errors 1, incomplete 0\n"
        )
    );
    assert!(!scratch.0.join(".nan").exists());
}

#[test]
fn a_scope_naming_nothing_is_unknown() {
    let scratch = Scratch::new(&[("app/package.nan", MANIFEST)]);
    let run = run_in(&scratch.0, "check app::Missing");
    assert_eq!(
        (run.status, run.output.lines().nth(1)),
        (2, Some("error tool.unknown-scope - -"))
    );
}

#[test]
fn a_stray_file_is_reported_on_its_first_line() {
    let scratch = Scratch::new(&[("app/package.nan", MANIFEST), ("stray.nan", "//! Stray.\n")]);
    let run = run_in(&scratch.0, "check");
    assert_eq!(
        run.output.lines().skip(1).take(2).collect::<Vec<_>>(),
        ["error manifest.missing stray.nan:1 -", "    //! Stray."]
    );
}

#[test]
fn fmt_writes_through_the_journal_and_names_each_file() {
    let scratch = Scratch::new(&[
        ("app/package.nan", MANIFEST),
        ("app/app.nan", "//! Root.\npub const  MAX: u32 = 1\n"),
    ]);
    let run = run_in(&scratch.0, "fmt");
    assert_eq!(
        (run.status, run.output.as_str()),
        (
            0,
            "ok fmt .: files 1, errors 0, incomplete 0\nwrote app/app.nan\nok fmt .: files 1, errors 0, incomplete 0\n"
        )
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("app/app.nan")).expect("read the file"),
        "//! Root.\n\npub const MAX: u32 = 1\n"
    );
    assert!(!scratch.0.join(".nan/journal").exists());
}

#[test]
fn an_internal_error_writes_a_reproducer() {
    let scratch = Scratch::new(&[("app/package.nan", MANIFEST), (".nan/repro/1/command", "")]);
    let line = args("check app");
    let invocation = read(&line).expect("a check command line");
    let trace = Trace {
        root: Some(scratch.0.clone()),
        scope: Some("app".to_owned()),
        read: vec!["app/package.nan".to_owned()],
    };
    let output = internal_error(
        &invocation,
        &line,
        &trace,
        "panic at nan/src/check.rs:7\nno type",
    )
    .render(&invocation.format);
    assert_eq!(
        output,
        "internal-error check app: records 1\ninternal internal.invariant - -\n    invariant panic at nan/src/check.rs:7\n        no type\n    reproducer .nan/repro/2\ninternal-error check app: records 1\n"
    );
    let reproducer = scratch.0.join(".nan/repro/2");
    assert_eq!(
        fs::read_to_string(reproducer.join("command")).expect("read the command"),
        "check\napp\n"
    );
    assert_eq!(
        fs::read_to_string(reproducer.join("toolchain")).expect("read the hashes"),
        format!("{}\n", toolchain::hash(1).full())
    );
    assert_eq!(
        fs::read_to_string(reproducer.join("app/package.nan")).expect("read the copy"),
        MANIFEST
    );
}

#[test]
fn a_panic_is_described_by_its_location_and_message() {
    let line = line!() + 1;
    let panicked = caught(|| panic!("an item {} has no type", 3)).expect_err("a panic");
    assert_eq!(
        panicked,
        format!("panic at nan/src/command/tests.rs:{line}\nan item 3 has no type")
    );
    assert_eq!(
        shown_path("/home/u/.cargo/registry/src/index/cranelift-codegen-1.0/src/isa/x64.rs"),
        "cranelift-codegen-1.0/src/isa/x64.rs"
    );
}

const HALVES: &str = "//! Root.\n\npub fn half(value: u32) -> u32 {\n    return value / 2\n} half\n\ntest halves for half {\n    expect half(4) == 2\n} halves\n";

#[test]
fn affected_tests_are_those_without_a_pass_of_their_reach() {
    let scratch = Scratch::new(&[("app/package.nan", MANIFEST), ("app/app.nan", HALVES)]);
    let verdict = |line: &str| {
        let run = run_in(&scratch.0, line);
        run.output
            .lines()
            .next()
            .expect("a verdict line")
            .to_owned()
    };
    assert_eq!(
        verdict("test"),
        "ok test .: errors 0, incomplete 0, tests 1, failed 0"
    );
    let unchanged = "ok test . --affected: errors 0, incomplete 0, tests 0, failed 0";
    assert_eq!(verdict("test --affected"), unchanged);
    fs::write(
        scratch.0.join("app/app.nan"),
        HALVES.replace("/ 2", "/ 2 + 0"),
    )
    .expect("an edit");
    let changed = "ok test . --affected: errors 0, incomplete 0, tests 1, failed 0";
    assert_eq!(verdict("test --affected"), changed);
}

#[test]
fn a_replay_token_reruns_its_run() {
    let failing = HALVES.replace("half(4) == 2", "half(5) == 3");
    let scratch = Scratch::new(&[("app/package.nan", MANIFEST), ("app/app.nan", &failing)]);
    let output = run_in(&scratch.0, "test").output;
    let token = output
        .lines()
        .find_map(|line| line.strip_prefix("    replay "))
        .expect("a replay token");
    let replayed = run_in(&scratch.0, &format!("test --replay {token}"));
    let verdict =
        format!("failed test --replay {token}: errors 0, incomplete 0, tests 1, failed 1\n");
    assert!(replayed.output.starts_with(&verdict), "{}", replayed.output);
    assert!(
        replayed
            .output
            .contains("\nfail app::halves app/app.nan:7 x86_64-host trap.expect\n")
    );
    for wrong in [
        token.replacen("sc/", "hb/", 1),
        token.replacen("/0/", "/00/", 1),
        token.replacen("halves", "thirds", 1),
        token.replacen("x86_64-host", "x86_64-uefi", 1),
        format!("{token}a"),
    ] {
        let run = run_in(&scratch.0, &format!("test --replay {wrong}"));
        assert!(
            run.output.contains("\nerror test.replay-token - -\n"),
            "{wrong}: {}",
            run.output
        );
    }
}

#[test]
fn a_test_head_names_a_function_and_takes_generated_values() {
    let tests = "\npub const LIMIT: u32 = 4\n\ntest limit for LIMIT {\n    expect LIMIT == 4\n} limit\n\ntest any_half for half (mut value: u32) {\n    expect half(value) <= value\n} any_half\n\ntest view_half for half (values: Slice[u32]) {\n    expect half(values.len().truncate()) <= 4\n} view_half\n\nstruct Pair derives Copy {\n    left: u64\n} Pair\n\nimpl Pair: Generate\n\ntest unknown_function for halve {\n    expect half(2) == 1\n} unknown_function\n\ntest unknown_owner for Pear.left {\n    expect half(2) == 1\n} unknown_owner\n\ntest unknown_member for Pair.right {\n    expect half(2) == 1\n} unknown_member\n";
    let source = format!("{HALVES}{tests}");
    let scratch = Scratch::new(&[("app/package.nan", MANIFEST), ("app/app.nan", &source)]);
    let output = run_in(&scratch.0, "check").output;
    for record in [
        "error test.target-kind app/app.nan:13 app::limit",
        "error test.parameter-form app/app.nan:17 app::any_half",
        "error test.parameter-type app/app.nan:21 app::view_half",
        "error test.generate-manual-impl app/app.nan:29 app::impl[app::Pair: core::Generate]",
        "error name.unknown app/app.nan:31 app::unknown_function",
        "error name.unknown app/app.nan:35 app::unknown_owner",
        "error name.unknown app/app.nan:39 app::unknown_member",
    ] {
        assert!(
            output.contains(&format!("\n{record}\n")),
            "{record}: {output}"
        );
    }
}

#[test]
fn a_changed_path_dependency_runs_the_affected_tests_of_a_sync_and_refuses_runs_once_broken() {
    let dial = |notch: &str| {
        format!(
            "//! Dials.\n\n/// The notch.\npub fn notch() -> u32 {{\n    return {notch}\n}} notch\n"
        )
    };
    let scratch = Scratch::new(&[
        (
            "dial/package.nan",
            "//! Dials.\n\npackage dial {\n    language 1\n    targets any\n} dial\n",
        ),
        ("dial/dial.nan", &dial("7")),
        (
            "app/package.nan",
            "//! Apps.\n\npackage app {\n    language 1\n    targets any\n    dep dial \"../dial\"\n} app\n",
        ),
        (
            "app/app.nan",
            "//! Apps.\n\n/// The notch.\npub fn notch() -> u32 {\n    return dial::notch()\n} notch\n\ntest seventh for notch {\n    expect notch() == 7\n} seventh\n",
        ),
    ]);
    let dir = &scratch.0;
    run_in(&dir.join("app"), "dep sync");
    fs::write(dir.join("dial/dial.nan"), dial("8")).expect("a changed dial");
    let synced = run_in(&dir.join("app"), "dep sync").output;
    assert!(
        synced.contains("\nerror dep.unresolved app/app.nan:8 app::seventh\n"),
        "{synced}"
    );
    fs::write(dir.join("dial/dial.nan"), dial("true")).expect("a broken dial");
    let tested = run_in(dir, "test app").output;
    assert!(
        tested.contains("\nerror type.mismatch dial/dial.nan:5 dial::notch\n"),
        "{tested}"
    );
}

#[test]
fn a_test_reaching_a_rejected_item_reports_its_errors_and_runs_nothing() {
    let broken = HALVES.replace("value / 2", "value / true");
    let scratch = Scratch::new(&[("app/package.nan", MANIFEST), ("app/app.nan", &broken)]);
    let run = run_in(&scratch.0, "test app::halves");
    assert!(
        run.output.starts_with(
            "failed test app::halves: errors 1, incomplete 0, tests 0, failed 0\nerror type.mismatch app/app.nan:4 app::half\n"
        ),
        "{}",
        run.output
    );
}
