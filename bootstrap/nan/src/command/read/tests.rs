use std::fs;

use crate::command::tests::{MANIFEST, run_in};
use crate::workspace::tests::Scratch;

const GAUGE: &str = "//! Gauges.\n\npub const STEP: u32 = 5\n\npub closed struct Gauge {\n    pub level: u32\n} Gauge\n\npub fn Gauge.reset(mut self) {\n    let next = STEP\n    self.level = next\n} Gauge.reset\n";

/// The item and interface hashes `nan show` prints for an item.
fn hashes(scratch: &Scratch, item: &str) -> (String, String) {
    let output = run_in(&scratch.0, &format!("show {item}")).output;
    let header = output.lines().nth(1).expect("an item record");
    let fields: Vec<&str> = header.split(' ').collect();
    (fields[3].to_owned(), fields[4].to_owned())
}

#[test]
fn show_hashes_change_with_what_each_covers() {
    let scratch = Scratch::new(&[("app/package.nan", MANIFEST), ("app/app.nan", GAUGE)]);
    let shown = run_in(&scratch.0, "show app").output;
    let headers: Vec<&str> = shown
        .lines()
        .filter(|line| !line.starts_with(' '))
        .collect();
    assert_eq!(headers[1], "module app");
    assert!(headers[2].starts_with("item app::STEP app/app.nan:3 "));
    assert!(
        headers[2].ends_with(" -"),
        "a const has no interface hash yet"
    );
    let (item, interface) = hashes(&scratch, "app::Gauge.reset");
    assert_eq!((item.len(), interface.len()), (18, 18));
    let read = run_in(&scratch.0, "read app/app.nan:11").output;
    assert!(
        read.contains("\nitem app::Gauge.reset app/app.nan:9 "),
        "{read}"
    );
    let edit = |text: &str| fs::write(scratch.0.join("app/app.nan"), text).expect("write");
    edit(&GAUGE.replace("next", "chosen"));
    assert_eq!(
        hashes(&scratch, "app::Gauge.reset"),
        (item.clone(), interface.clone())
    );
    edit(&GAUGE.replace("= STEP\n", "= STEP + 1\n"));
    let (changed, same) = hashes(&scratch, "app::Gauge.reset");
    assert_ne!(changed, item);
    assert_eq!(same, interface);
}

#[test]
fn a_context_pack_holds_the_closure_set() {
    let scratch = Scratch::new(&[("app/package.nan", MANIFEST), ("app/app.nan", GAUGE)]);
    let run = run_in(&scratch.0, "context app::Gauge.reset");
    assert_eq!(
        run.output,
        "ok context app::Gauge.reset: entries 5, radius 103\nitem app::Gauge.reset app/app.nan:9\n    pub fn Gauge.reset(mut self) {\n        let next = STEP\n        self.level = next\n    } Gauge.reset\nmanifest app\n    language 1\n    targets any\nnames app\n    Gauge, Gauge.reset, STEP\napi app::Gauge app/app.nan:5\n    pub closed struct Gauge {\n        pub level: u32\n    } Gauge\n    properties: Send, Inert, AnyBits\napi app::STEP app/app.nan:3\n    pub const STEP: u32 = 5\nok context app::Gauge.reset: entries 5, radius 103\n"
    );
    let line = run_in(&scratch.0, "context app/app.nan:11").output;
    let records: Vec<&str> = line
        .lines()
        .filter(|line| line.starts_with("api "))
        .collect();
    assert_eq!(records, ["api app::Gauge app/app.nan:5"], "{line}");
    let radii = run_in(&scratch.0, "check --radius").output;
    let records: Vec<&str> = radii
        .lines()
        .filter(|line| line.starts_with("radius"))
        .collect();
    assert_eq!(records[0], "radius app::Gauge.reset 103");
}

#[test]
fn api_records_show_what_consumers_see() {
    let scratch = Scratch::new(&[
        ("app/package.nan", MANIFEST),
        (
            "app/app.nan",
            "//! Levels.\n\n/// Clamps a level.\npub fn clamp_level(level: u32) -> u32 limits notrap\n    ensures result <= 100\n{\n    if level > 100 {\n        return 100\n    }\n    return level\n} clamp_level\n\nfn hidden() -> u32 = 1\n",
        ),
    ]);
    assert_eq!(
        run_in(&scratch.0, "show app --api").output,
        "ok show app --api: records 1\napi app::clamp_level app/app.nan:4\n    /// Clamps a level.\n    pub fn clamp_level(level: u32) -> u32 limits notrap\n        ensures result <= 100 [checked]\n    digest: effects none | authority none | may-trap yes | terminates\nok show app --api: records 1\n"
    );
}

#[test]
fn derived_items_expand_and_functions_explain_their_facts() {
    let scratch = Scratch::new(&[
        ("app/package.nan", MANIFEST),
        (
            "app/app.nan",
            "//! Kinds.\n\npub closed enum Kind derives Copy, Eq {\n    Data\n    Ack\n} Kind\n\npub fn total(values: Slice[f64]) -> f64 {\n    var sum: f64 = 0.0\n    for value in values {\n        sum += value\n    }\n    return sum\n} total\n",
        ),
    ]);
    let expanded = run_in(&scratch.0, "expand app::Kind").output;
    assert!(expanded.starts_with(
        "ok expand app::Kind: records 2\nitem app::impl[app::Kind: core::Eq] derived "
    ));
    assert!(expanded.contains("\n    impl Kind: Eq\n"));
    assert!(expanded.contains(
        "\n    pub fn Kind.eq(self, other: Kind) -> bool limits pure, notrap, terminates {\n"
    ));
    let explained = run_in(&scratch.0, "explain app::total").output;
    assert!(
        explained.contains("\nfact float app/app.nan:11\n            sum += value\n"),
        "{explained}"
    );
    assert!(explained.contains("\nradius "));
}
