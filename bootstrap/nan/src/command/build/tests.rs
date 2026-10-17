use std::fs;

use crate::command::tests::run_in;
use crate::workspace::tests::Scratch;

const MANIFEST: &str = "//! Exits.\n\npackage exits {\n    language 1\n    targets x86_64-host\n    entry main\n} exits\n";

const SOURCE: &str = "//! Exits.\n\nfn main(take world: host::World) -> u8 fails task::Cancelled uses wait {\n    world.log.info(\"exiting\", value: 3u8)\n    return 3\n} main\n";

#[test]
fn stage_comparison_reports_each_package_whose_code_root_differs() {
    let scratch = Scratch::new(&[("exits/package.nan", MANIFEST), ("exits/exits.nan", SOURCE)]);
    let dir = &scratch.0;
    let built = run_in(dir, "build");
    assert_eq!(built.status, 0, "{}", built.output);
    fs::copy(
        dir.join(".nan/images/x86_64-host/exits"),
        dir.join("stage1"),
    )
    .expect("a copy");
    let same = run_in(dir, "build --compare-with stage1");
    assert_eq!(same.status, 0, "{}", same.output);
    let changed = SOURCE.replace("return 3", "return 4");
    fs::write(dir.join("exits/exits.nan"), changed).expect("a changed source");
    let differing = run_in(dir, "build --compare-with stage1").output;
    assert!(
        differing.contains("\nerror build.stage-mismatch exits/package.nan:3 exits\n"),
        "{differing}"
    );
    assert!(!differing.contains("\nwrote "), "{differing}");
}
