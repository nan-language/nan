use std::fs;

use crate::command::tests::{MANIFEST, run_in};
use crate::workspace::tests::Scratch;

fn read(scratch: &Scratch, path: &str) -> String {
    fs::read_to_string(scratch.0.join(path)).expect("read a file")
}

#[test]
fn replace_swaps_an_item_only_while_its_hash_is_expected() {
    let scratch = Scratch::new(&[
        ("app/package.nan", MANIFEST),
        ("app/app.nan", "//! Levels.\n\npub fn level() -> u32 = 1\n"),
        ("new.nan", "pub fn level() -> u32 = 2\n"),
    ]);
    let shown = run_in(&scratch.0, "show app::level").output;
    let hash = shown
        .lines()
        .nth(1)
        .and_then(|line| line.split(' ').nth(3))
        .expect("a hash");
    let line = format!("edit replace app::level --expect {hash} --from new.nan");
    let replaced = run_in(&scratch.0, &line);
    assert!(
        replaced
            .output
            .contains("\nreplaced app::level app/app.nan:3 "),
        "{}",
        replaced.output
    );
    assert_eq!(
        read(&scratch, "app/app.nan"),
        "//! Levels.\n\npub fn level() -> u32 = 2\n"
    );
    let stale = run_in(&scratch.0, &line);
    assert_eq!(stale.status, 1);
    assert!(
        stale
            .output
            .contains("\nerror edit.stale app/app.nan:3 app::level\n")
    );
}

#[test]
fn rename_rewrites_the_declaration_and_every_reference() {
    let scratch = Scratch::new(&[
        ("app/package.nan", MANIFEST),
        (
            "app/app.nan",
            "//! Gauges.\n\npub closed struct Gauge {\n    pub level: u32\n} Gauge\n\npub fn Gauge.empty() -> Gauge {\n    return Gauge(level: 0)\n} Gauge.empty\n",
        ),
        (
            "app/use.nan",
            "//! Reading.\n\n/// The [Gauge.level] of a gauge.\npub fn level_of(gauge: Gauge) -> u32 {\n    let level = gauge.level\n    return level\n} level_of\n",
        ),
    ]);
    let run = run_in(&scratch.0, "edit rename app::Gauge.level amount");
    assert_eq!(run.status, 0, "{}", run.output);
    assert_eq!(
        read(&scratch, "app/app.nan"),
        "//! Gauges.\n\npub closed struct Gauge {\n    pub amount: u32\n} Gauge\n\npub fn Gauge.empty() -> Gauge {\n    return Gauge(amount: 0)\n} Gauge.empty\n"
    );
    assert_eq!(
        read(&scratch, "app/use.nan"),
        "//! Reading.\n\n/// The [Gauge.amount] of a gauge.\npub fn level_of(gauge: Gauge) -> u32 {\n    let level = gauge.amount\n    return level\n} level_of\n"
    );
}

#[test]
fn move_respells_paths_and_raises_what_another_module_then_names() {
    let scratch = Scratch::new(&[
        ("app/package.nan", MANIFEST),
        (
            "app/app.nan",
            "//! Areas.\n\nstruct Scale {\n    pub ratio: u64\n} Scale\n\nfn Scale.of(ratio: u64) -> Scale {\n    return Scale(ratio: ratio)\n} Scale.of\n\npub fn doubled() -> u64 {\n    return Scale.of(2).ratio\n} doubled\n",
        ),
        (
            "app/units/units.nan",
            "//! Units.\n\npub const UNIT: u32 = 1\n",
        ),
    ]);
    let run = run_in(&scratch.0, "edit move app::Scale units");
    assert_eq!(run.status, 0, "{}", run.output);
    assert_eq!(
        read(&scratch, "app/app.nan"),
        "//! Areas.\n\npub fn doubled() -> u64 {\n    return units::Scale.of(2).ratio\n} doubled\n"
    );
    assert_eq!(
        read(&scratch, "app/units/units.nan"),
        "//! Units.\n\npub const UNIT: u32 = 1\n\npkg struct Scale {\n    pub ratio: u64\n} Scale\n\npkg fn Scale.of(ratio: u64) -> Scale {\n    return Scale(ratio: ratio)\n} Scale.of\n"
    );
    let invalid = run_in(&scratch.0, "edit move app::doubled nowhere");
    assert!(
        invalid
            .output
            .contains("\nerror edit.move-invalid app/app.nan:3 app::doubled\n")
    );
}
