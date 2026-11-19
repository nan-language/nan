use std::fs;

use super::*;
use crate::machine::passes::{Pass, Passes};
use crate::workspace::tests::Scratch;
use crate::workspace::{Arch, Kind};

#[test]
fn nanos_test_images_are_bundles_running_alone() {
    let scratch = Scratch::new(&[
        (
            "halves/package.nan",
            "//! Halves.\n\npackage halves {\n    language 1\n    targets x86_64-nanos\n} halves\n",
        ),
        (
            "halves/halves.nan",
            "//! Halves.\n\npub fn half(value: u64) -> u64 = value / 2\n\ntest halves_four for half {\n    expect half(4) == 2\n} halves_four\n",
        ),
    ]);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let std = crate::stdlib::packages();
    let checker = check::checked(&workspace, &workspace.closures[0], std, Target::X64Nanos);
    let tests = select(&checker, &workspace, &Scope::Workspace);
    let package = checker.db.package_of(tests[0].item);
    let built = build(&checker, &workspace, package, &tests).expect("a test image");
    let Ok(Image::Host(path)) = built else {
        panic!("no test image");
    };
    let bundle = fs::read(path).expect("the test image");
    assert_eq!(&bundle[..4], b"NBUN");
}

#[test]
fn a_current_pass_stands_for_the_runs_of_a_target_the_machine_cannot_run() {
    let other = match Kind::running() {
        Kind::Linux => Kind::Macos,
        _ => Kind::Linux,
    };
    let target = Target::of(Arch::running(), other);
    let manifest = format!(
        "//! Halves.\n\npackage halves {{\n    language 1\n    targets {}\n}} halves\n",
        target.name()
    );
    let scratch = Scratch::new(&[
        ("halves/package.nan", &manifest),
        (
            "halves/halves.nan",
            "//! Halves.\n\npub fn half(value: u64) -> u64 = value / 2\n\ntest halves_four for half {\n    expect half(4) == 2\n} halves_four\n",
        ),
    ]);
    let failed = || {
        let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
        let Ok(Ok(cache)) = Cache::open() else {
            panic!("no build cache");
        };
        let Ok(Ok(report)) = run(&workspace, &Scope::Workspace, &Runs::All, &cache, None) else {
            panic!("no runs");
        };
        assert_eq!(report.tests, 1);
        report.failures.len()
    };
    assert_eq!(failed(), 1);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let std = crate::stdlib::packages();
    let checker = check::checked(&workspace, &workspace.closures[0], std, target);
    let test = select(&checker, &workspace, &Scope::Workspace)[0].item;
    let current = Hash::of(&reach(&checker, &ItemHashes::default(), test));
    fs::create_dir_all(scratch.0.join("halves/.nan")).expect("a machine directory");
    for (reach, failures) in [(current, 0), (Hash::of(b"stale"), 1)] {
        let pass = Pass {
            test_path: checker.db.canonical(test),
            target: target.name().to_owned(),
            principal: "tester".to_owned(),
            reach,
            signature: None,
        };
        let file = Passes::default()
            .recording(pass)
            .bytes()
            .expect("a passes file");
        fs::write(scratch.0.join("halves/.nan/passes.nan"), file).expect("the passes file");
        assert_eq!(failed(), failures);
    }
}
