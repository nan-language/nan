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

#[test]
fn unused_code_is_reported_where_no_target_uses_it_with_its_fixes() {
    let scratch = Scratch::new(&[
        (
            "probe/package.nan",
            "//! Probes.\n\npackage probe {\n    language 1\n    targets x86_64-bare, x86_64-host\n    dep ring \"../ring\"\n} probe\n",
        ),
        (
            "probe/probe.nan",
            "//! Reserves.\n\nstruct Reading {\n    level: u32\n    sensor: u8\n} Reading\n\npub fn reserve(level: u32) -> u32 {\n    let doubled = level * 2\n    let reading = Reading(level: level, sensor: 0)\n    return reading.level + platform::stack()\n} reserve\n",
        ),
        (
            "probe/platform@bare/platform.nan",
            "//! Stacks.\n\npkg fn stack() -> u32 {\n    return bare_stack()\n} stack\n\nfn bare_stack() -> u32 {\n    return 16\n} bare_stack\n",
        ),
        (
            "probe/platform@host/platform.nan",
            "//! Stacks.\n\npkg fn stack() -> u32 {\n    return 64\n} stack\n",
        ),
        (
            "ring/package.nan",
            "//! Rings.\n\npackage ring {\n    language 1\n    targets any\n} ring\n",
        ),
        (
            "ring/ring.nan",
            "//! Rings.\n\npub const CAPACITY: usize = 64\n",
        ),
    ]);
    let run = run_in(&scratch.0, "check");
    assert!(
        run.output
            .starts_with("ok check .: errors 0, incomplete 3\n"),
        "{}",
        run.output
    );
    assert!(run.output.contains(
        "\nincomplete unused.dependency probe/package.nan:6 probe\n        dep ring \"../ring\"\n    found: dep ring \"../ring\"\n    fix 1 probe/package.nan\n    -     dep ring \"../ring\"\n"
    ));
    assert!(run.output.contains(
        "\n    fix 1 probe/probe.nan\n    -     sensor: u8\n    fix 1 probe/probe.nan\n    -     let reading = Reading(level: level, sensor: 0)\n    +     let reading = Reading(level: level)\n"
    ));
    assert!(
        run.output
            .contains("\n    -     let doubled = level * 2\n    +     discard level * 2\n")
    );
}

#[test]
fn generator_gaps_unfold_transparent_functions_and_find_unsatisfiable_bounds() {
    let scratch = Scratch::new(&[
        ("app/package.nan", MANIFEST),
        (
            "app/app.nan",
            "//! Spans.\n\npub fn ordered(low: u32, high: u32) -> bool = low <= high\n\npub closed struct Span derives Generate {\n    pub start: u32\n    pub end: u32 where ordered(start, high: end)\n} Span\n\npub fn trim(span: Span, by: u32) -> u32\n    requires by <= span.start and span.start < by\n{\n    return span.end - by\n} trim\n\ntest trim_keeps_end for trim (span: Span, by: u32) {\n    expect trim(span, by) <= span.end\n} trim_keeps_end\n",
        ),
    ]);
    let run = run_in(&scratch.0, "check");
    assert_eq!(
        run.output,
        "failed check .: errors 1, incomplete 1\nerror contract.vacuous app/app.nan:11 app::trim\n        requires by <= span.start and span.start < by\nincomplete test.generator-gap app/app.nan:16 app::trim_keeps_end\n    test trim_keeps_end for trim (span: Span, by: u32) {\nfailed check .: errors 1, incomplete 1\n"
    );
}

#[test]
fn a_rewrite_checks_with_the_try_its_outermost_call_needs() {
    let scratch = Scratch::new(&[
        ("app/package.nan", MANIFEST),
        (
            "app/app.nan",
            "//! Counters.\n\npub closed enum LoadError derives Render {\n    Missing\n} LoadError\n\npub closed struct Counter {\n    pub hits: u32\n} Counter\n\npub fn Counter.load(self) -> u32 fails LoadError {\n    if self.hits == 0 {\n        fail LoadError.Missing\n    }\n    return self.hits\n} Counter.load\n\nretired Counter.fetch(self) -> u32 fails LoadError => self.load() preserves\n\nretired Counter.grab(self) -> u32 => self.load() preserves\n",
        ),
    ]);
    let run = run_in(&scratch.0, "check");
    assert_eq!(
        run.output,
        "failed check .: errors 1, incomplete 0\nerror retired.bad-rewrite app/app.nan:20 -\n    retired Counter.grab(self) -> u32 => self.load() preserves\nfailed check .: errors 1, incomplete 0\n"
    );
}

#[test]
fn a_prefix_reports_no_binding_its_open_scope_may_still_use() {
    let scratch = Scratch::new(&[
        ("app/package.nan", MANIFEST),
        (
            "app/app.nan",
            "//! Loads.\n\npub fn busiest(loads: Slice[u32]) -> usize {\n    let count = loads.len()\n    return 0\n} busiest\n",
        ),
    ]);
    assert!(
        !run_in(&scratch.0, "check --prefix app/app.nan:4")
            .output
            .contains("unused.local")
    );
    assert!(
        run_in(&scratch.0, "check")
            .output
            .contains("\nincomplete unused.local app/app.nan:4 app::busiest\n")
    );
}

#[test]
fn example_runs_dispatch_calls_through_dyn_views() {
    let scratch = Scratch::new(&[
        ("app/package.nan", MANIFEST),
        (
            "app/app.nan",
            "//! Shapes.\n\npub interface Shape {\n    fn area(self) -> u32\n} Shape\n\npub closed struct Square {\n    pub side: u32\n} Square\n\nimpl Square: Shape\n\npub fn Square.area(self) -> u32 {\n    return self.side.saturating_mul(self.side)\n} Square.area\n\npub fn area_of(shape: Dyn[Shape]) -> u32\n    example area_of(Dyn[Shape].view(Square(side: 3))) == 10\n{\n    return shape.area()\n} area_of\n",
        ),
    ]);
    assert_eq!(
        run_in(&scratch.0, "check").output,
        "failed check .: errors 1, incomplete 0\nerror example.failed app/app.nan:18 app::area_of\n        example area_of(Dyn[Shape].view(Square(side: 3))) == 10\nfailed check .: errors 1, incomplete 0\n"
    );
}

#[test]
fn a_run_reaching_a_std_operation_the_interpreter_lacks_is_not_judged() {
    let scratch = Scratch::new(&[
        ("app/package.nan", MANIFEST),
        (
            "app/app.nan",
            "//! Text.\n\npub fn is_text(data: Slice[u8]) -> bool\n    example is_text(b\"abc\")\n{\n    return text::utf8::is_valid(data)\n} is_text\n",
        ),
    ]);
    assert_eq!(
        run_in(&scratch.0, "check").output,
        "ok check .: errors 0, incomplete 0\nok check .: errors 0, incomplete 0\n"
    );
}
