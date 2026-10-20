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
    run_in(&scratch.0, "dep sync probe");
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
fn a_generic_function_passes_its_values_to_the_function_values_it_calls() {
    let scratch = Scratch::new(&[
        ("app/package.nan", MANIFEST),
        (
            "app/app.nan",
            "//! Checks.\n\npub fn holds[T: Any](value: T, check: fn(value: T) -> bool) -> bool\n    example holds(3u8, check: fn(value: u8) => value == 4)\n{\n    return check(value)\n} holds\n",
        ),
    ]);
    assert_eq!(
        run_in(&scratch.0, "check").output,
        "failed check .: errors 1, incomplete 0\nerror example.failed app/app.nan:4 app::holds\n        example holds(3u8, check: fn(value: u8) => value == 4)\nfailed check .: errors 1, incomplete 0\n"
    );
}

#[test]
fn a_doc_bound_to_an_older_interface_hash_is_stale_until_confirmed() {
    let source = "//! Frames.\n\n/// Frames a window of `slots` slots holds, at 8 frames per slot.\npub fn window_frames(slots: u32) -> u32 {\n    return slots.saturating_mul(8)\n} window_frames\n";
    let scratch = Scratch::new(&[("app/package.nan", MANIFEST), ("app/app.nan", source)]);
    run_in(&scratch.0, "api update");
    let widened = source
        .replace("(slots: u32)", "(slots: u32, per_slot: u32)")
        .replace("saturating_mul(8)", "saturating_mul(per_slot)");
    fs::write(scratch.0.join("app/app.nan"), widened).expect("the widened source");
    let run = run_in(&scratch.0, "check");
    assert!(
        run.output
            .contains("\nincomplete doc.stale app/app.nan:3 app::window_frames\n"),
        "{}",
        run.output
    );
    run_in(&scratch.0, "api update --confirm-docs app::window_frames");
    let run = run_in(&scratch.0, "check");
    assert!(
        run.output
            .starts_with("ok check .: errors 0, incomplete 0\n"),
        "{}",
        run.output
    );
}

#[test]
fn a_tombstone_retiring_a_path_of_another_package_is_foreign() {
    let scratch = Scratch::new(&[
        ("app/package.nan", MANIFEST),
        (
            "app/app.nan",
            "//! Totals.\n\npub fn total() -> u32 = 1\n\nretired core::sum => total preserves\n",
        ),
    ]);
    let run = run_in(&scratch.0, "check");
    assert!(
        run.output
            .contains("\nerror retired.foreign-path app/app.nan:5 "),
        "{}",
        run.output
    );
}

#[test]
fn a_path_only_a_newer_release_names_reports_that_release() {
    let package = |name: &str, deps: &str| {
        format!(
            "//! The {name} package.\n\npackage {name} {{\n    language 1\n    targets any\n{deps}}} {name}\n"
        )
    };
    let scratch = Scratch::new(&[
        ("meter/package.nan", &package("meter", "")),
        (
            "meter/meter.nan",
            "//! Meters.\n\n/// The level.\npub fn level() -> u32 = 1\n",
        ),
        ("gauge/package.nan", &package("gauge", "")),
        (
            "gauge/gauge.nan",
            "//! Gauges.\n\n/// The reading.\npub fn reading() -> u32 = 2\n",
        ),
    ]);
    let dir = &scratch.0;
    let name = format!(
        "nan-test/{}/gauge",
        dir.file_name().expect("a directory name").to_string_lossy()
    );
    let deps = format!("    dep meter \"../meter\"\n    dep gauge \"{name}\"\n");
    fs::create_dir_all(dir.join("app")).expect("the app directory");
    fs::write(dir.join("app/package.nan"), package("app", &deps)).expect("the app manifest");
    let app = "//! Apps.\n\npub fn level() -> u32 = meter::level()\n\npub fn reading() -> u32 = gauge::reading()\n";
    fs::write(dir.join("app/app.nan"), app).expect("the app source");
    run_in(dir, "api update gauge");
    run_in(dir, &format!("publish --local gauge --name {name}"));
    run_in(&dir.join("app"), "dep sync");
    for (path, added) in [
        (
            "meter/meter.nan",
            "\n/// Resets.\npub fn reset() -> u32 = 0\n",
        ),
        ("gauge/gauge.nan", "\n/// Zero.\npub fn zero() -> u32 = 0\n"),
    ] {
        let text = fs::read_to_string(dir.join(path)).expect("a source");
        fs::write(dir.join(path), text + added).expect("a newer source");
    }
    run_in(dir, "api update gauge");
    let published = run_in(dir, "publish --local gauge").output;
    let line = published
        .lines()
        .find(|line| line.starts_with("published "));
    let root = line
        .and_then(|line| line.split(' ').nth(2))
        .expect("a published release");
    let uses = "\npub fn reset() -> u32 = meter::reset()\n\npub fn zero() -> u32 = gauge::zero()\n";
    fs::write(dir.join("app/app.nan"), format!("{app}{uses}")).expect("the app's new uses");
    let output = run_in(dir, "check app").output;
    for (line, item, release) in [
        (7, "reset", "../meter".to_owned()),
        (9, "zero", format!("{name} {root}")),
    ] {
        let reported = format!("\nerror name.newer-release app/app.nan:{line} app::{item}\n");
        assert!(output.contains(&reported), "{output}");
        assert!(
            output.contains(&format!("\n    because note - - {release}\n")),
            "{output}"
        );
    }
    assert!(
        output.contains("\n    because command - - nan dep sync\n"),
        "{output}"
    );
}

#[test]
fn uses_of_renamed_forms_forward_and_changed_ones_are_moved() {
    let scratch = Scratch::new(&[
        ("app/package.nan", MANIFEST),
        (
            "app/app.nan",
            "//! Renames.\n\npub open enum Mode {\n    Fast\n    Slow\n} Mode\n\nretired Mode.Quick => Mode.Fast preserves\n\npub const CEILING: u32 = 10\n\nretired OLD_CEILING => CEILING preserves\n\npub closed struct Gauge {\n    pub level: u32\n} Gauge\n\nretired Meter => Gauge preserves\n\nretired Gauge.amount => Gauge.level preserves\n\nretired Gauge.total => Gauge.level changes \"counts once\"\n\npub fn quick() -> Mode = Mode.Quick\n\npub fn top() -> u32 = OLD_CEILING\n\npub fn made() -> Gauge = Meter(amount: 1)\n\npub fn amount(gauge: Gauge) -> u32 = gauge.amount\n\npub fn is_quick(mode: Mode) -> bool {\n    match mode {\n        Mode.Quick => return true\n        Mode.Slow => return false\n    }\n} is_quick\n\npub fn total(gauge: Gauge) -> u32 = gauge.total\n",
        ),
    ]);
    let output = run_in(&scratch.0, "check").output;
    let reported: Vec<&str> = output
        .lines()
        .filter(|line| line.starts_with("error ") || line.starts_with("incomplete "))
        .collect();
    assert_eq!(
        reported,
        [
            "incomplete retired.forwarded-use app/app.nan:24 app::quick",
            "incomplete retired.forwarded-use app/app.nan:26 app::top",
            "incomplete retired.forwarded-use app/app.nan:28 app::made",
            "incomplete retired.forwarded-use app/app.nan:28 app::made",
            "incomplete retired.forwarded-use app/app.nan:30 app::amount",
            "incomplete retired.forwarded-use app/app.nan:34 app::is_quick",
            "error name.moved app/app.nan:39 app::total",
        ],
        "{output}"
    );
    assert!(
        output.contains("\n    + pub fn made() -> Gauge = Gauge(amount: 1)\n"),
        "{output}"
    );
    assert!(
        output.contains("\n    + pub fn made() -> Gauge = Meter(level: 1)\n"),
        "{output}"
    );
    assert!(
        output.contains("\n    because declared app/app.nan:22 - Gauge.total\n"),
        "{output}"
    );
}

#[test]
fn a_store_entry_holding_another_release_than_the_lock_names_is_a_root_mismatch() {
    let scratch = Scratch::new(&[
        (
            "gauge/package.nan",
            "//! Gauges.\n\npackage gauge {\n    language 1\n    targets any\n} gauge\n",
        ),
        (
            "gauge/gauge.nan",
            "//! Gauges.\n\n/// The reading.\npub fn reading() -> u32 = 2\n",
        ),
    ]);
    let dir = &scratch.0;
    let name = format!(
        "nan-test/{}/gauge",
        dir.file_name().expect("a directory name").to_string_lossy()
    );
    let manifest = format!(
        "//! Apps.\n\npackage app {{\n    language 1\n    targets any\n    dep gauge \"{name}\"\n}} app\n"
    );
    fs::create_dir_all(dir.join("app")).expect("the app directory");
    fs::write(dir.join("app/package.nan"), manifest).expect("the app manifest");
    let app = "//! Apps.\n\npub fn reading() -> u32 = gauge::reading()\n";
    fs::write(dir.join("app/app.nan"), app).expect("the app source");
    let published = |line: &str| {
        run_in(dir, "api update gauge");
        let output = run_in(dir, line).output;
        let line = output.lines().find(|line| line.starts_with("published "));
        let root = line
            .and_then(|line| line.split(' ').nth(2))
            .expect("a release");
        root[..14].to_owned()
    };
    let first = published(&format!("publish --local gauge --name {name}"));
    run_in(&dir.join("app"), "dep sync");
    let source = fs::read_to_string(dir.join("gauge/gauge.nan")).expect("the gauge source");
    let added = "\n/// Zero.\npub fn zero() -> u32 = 0\n";
    fs::write(dir.join("gauge/gauge.nan"), source + added).expect("a newer gauge");
    let second = published("publish --local gauge");
    let store = std::env::temp_dir().join("nan-test-home/.nan/store");
    let entry = |root: &str| {
        fs::read_dir(&store)
            .expect("the store")
            .map(|entry| {
                entry
                    .expect("a store entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .find(|file| {
                file.split_once('-')
                    .is_some_and(|(_, held)| held.starts_with(root))
            })
            .expect("an entry of the release")
    };
    let (held, other) = (entry(&first), entry(&second));
    let index = other.split_once('-').expect("an index").0;
    let renamed = format!("{index}-{}", held.split_once('-').expect("a root").1);
    fs::remove_file(store.join(&held)).expect("the first release removed");
    fs::rename(store.join(&other), store.join(renamed)).expect("the second named as the first");
    let output = run_in(dir, "check app").output;
    assert!(
        output.contains("\nerror artefact.root-mismatch app/package.nan:6 app\n"),
        "{output}"
    );
}

#[test]
fn a_syntax_error_in_a_target_variant_is_reported_as_such() {
    let scratch = Scratch::new(&[
        (
            "app/package.nan",
            "//! A package.\n\npackage app {\n    language 1\n    targets x86_64-bare, x86_64-host\n} app\n",
        ),
        ("app/app.nan", "//! Root.\n"),
        (
            "app/platform@bare/platform.nan",
            "//! Platform.\n\nfn broken() {\n} broken\n",
        ),
        (
            "app/platform@host/platform.nan",
            "//! Platform.\n\nfn broken() {\n    let k: fn() uses other = t\n} broken\n",
        ),
    ]);
    let run = run_in(&scratch.0, "check");
    assert!(
        run.output
            .contains("\nerror effect.uses-unknown app/platform@host/platform.nan:4 "),
        "{}",
        run.output
    );
}
