use std::fs;

use super::super::{Scope, Workspace, checked_scope, reported_by, scope_checkers};
use crate::workspace::tests::Scratch;

const MANIFEST: &str = "//! Sizes.\n\npackage app {\n    language 1\n    targets any\n} app\n";

const SOURCE: &str = "//! Sizes.\n\n/// The bytes of a frame.\npub fn frame() -> u64 {\n    return 4096\n} frame\n\n/// The bytes of some frames.\npub fn frames(count: u64) -> u64 {\n    return count * frame()\n} frames\n\n/// A count of pages.\npub fn pages(bytes: u64) -> u64 {\n    let unused = bytes\n    return bytes / 4096\n} pages\n";

#[test]
fn a_check_after_a_body_edit_checks_again_only_the_items_reaching_it() {
    let scratch = Scratch::new(&[("app/package.nan", MANIFEST), ("app/app.nan", SOURCE)]);
    let check = || crate::run(&["check".to_owned()], &scratch.0).output;
    check();
    let edited = SOURCE.replace("return 4096\n", "let page: u64 = 4096\n    return page\n");
    fs::write(scratch.0.join("app/app.nan"), &edited).expect("write");
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let checkers = checked_scope(&workspace, &Scope::Workspace);
    let [(_, checker)] = checkers.as_slice() else {
        panic!("one checker");
    };
    let mut restored: Vec<String> = (checker.kept.restored.borrow().iter())
        .filter_map(|item| checker.canonical_path(*item))
        .collect();
    restored.sort();
    assert_eq!(restored, ["app::frames", "app::pages"]);
    let whole = scope_checkers(&workspace, crate::stdlib::packages(), &Scope::Workspace);
    for (_, checker) in &whole {
        checker.check_all();
    }
    let reported = reported_by(&workspace, &whole, &Scope::Workspace);
    assert!(
        reported
            .iter()
            .any(|diagnostic| diagnostic.line == Some(16))
    );
    assert_eq!(
        reported_by(&workspace, &checkers, &Scope::Workspace),
        reported
    );
}
