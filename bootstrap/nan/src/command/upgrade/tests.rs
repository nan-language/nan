use std::fs;

use crate::command::tests::run_in;
use crate::workspace::tests::Scratch;

const GAUGE: &str = "//! Readings.\n\n/// A reading in percent.\npub fn percent(raw: u32) -> u32 = raw.saturating_mul(100) / 1_024\n";

#[test]
fn a_rename_published_behind_a_tombstone_forwards_until_nan_upgrade_rewrites_the_use() {
    let scratch = Scratch::new(&[]);
    let dir = &scratch.0;
    let name = format!(
        "nan-test/{}",
        dir.file_name().expect("a directory name").to_string_lossy()
    );
    let panel = format!(
        "//! Panels.\n\npackage panel {{\n    language 1\n    targets any\n    dep gauge \"{name}\"\n}} panel\n"
    );
    let files = [
        (
            "gauge/package.nan",
            "//! Readings.\n\npackage gauge {\n    language 1\n    targets any\n} gauge\n".to_owned(),
        ),
        ("gauge/gauge.nan", GAUGE.to_owned()),
        ("panel/package.nan", panel),
        (
            "panel/panel.nan",
            "//! Panels.\n\n/// The reading shown.\npub fn shown(raw: u32) -> u32 = gauge::percent(raw)\n"
                .to_owned(),
        ),
    ];
    for (path, text) in files {
        fs::create_dir_all(dir.join(path).parent().expect("a directory")).expect("a directory");
        fs::write(dir.join(path), text).expect("a file");
    }
    run_in(dir, "api update gauge");
    let first = run_in(dir, &format!("publish --local gauge --name {name}"));
    assert!(first.output.starts_with("ok publish"), "{}", first.output);
    run_in(dir, "dep sync panel");
    let renamed = GAUGE.replace("pub fn percent", "pub fn ratio")
        + "\nretired percent(raw: u32) -> u32 => ratio(raw) preserves\n";
    fs::write(dir.join("gauge/gauge.nan"), renamed).expect("the renamed source");
    run_in(dir, "api update gauge");
    let second = run_in(dir, "publish --local gauge");
    assert!(second.output.contains(" additive "), "{}", second.output);
    let upgraded = run_in(dir, "upgrade panel");
    assert!(
        upgraded
            .output
            .starts_with("ok upgrade panel: files 2, errors 0"),
        "{}",
        upgraded.output
    );
    let source = fs::read_to_string(dir.join("panel/panel.nan")).expect("the upgraded source");
    assert!(source.contains("= gauge::ratio(raw)"), "{source}");
    let unused = format!(
        "{}\nretired never => ratio preserves\n",
        fs::read_to_string(dir.join("gauge/gauge.nan")).expect("the published source")
    );
    fs::write(dir.join("gauge/gauge.nan"), unused).expect("an unused tombstone");
    let checked = run_in(dir, "check gauge");
    assert!(
        checked
            .output
            .contains("\nincomplete unused.tombstone gauge/gauge.nan:"),
        "{}",
        checked.output
    );
}
