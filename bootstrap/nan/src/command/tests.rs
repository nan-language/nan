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
fn fmt_writes_through_the_journal_and_names_each_file() {
    let scratch = Scratch::new(&[
        ("app/package.nan", MANIFEST),
        ("app/app.nan", "//! Root.\nconst  MAX: u32 = 1\n"),
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
        "//! Root.\n\nconst MAX: u32 = 1\n"
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
    let output = internal_error(&invocation, &line, &trace).render(&invocation.format);
    assert_eq!(
        output,
        "internal-error check app: records 1\ninternal internal.invariant - -\n    reproducer .nan/repro/2\ninternal-error check app: records 1\n"
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
