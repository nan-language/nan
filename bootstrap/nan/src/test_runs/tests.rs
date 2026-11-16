use std::fs;

use super::*;
use crate::workspace::tests::Scratch;

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
