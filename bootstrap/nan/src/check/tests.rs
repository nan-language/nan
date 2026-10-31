use super::*;
use crate::workspace::tests::Scratch;

/// A workspace of a package naming an unknown type beside an unrelated package.
fn probe_and_other() -> (Scratch, Workspace) {
    let manifest = |name: &str| {
        format!("//! A package.\n\npackage {name} {{\n    language 1\n    targets any\n}} {name}\n")
    };
    let scratch = Scratch::new(&[
        ("probe/package.nan", &manifest("probe")),
        (
            "probe/probe.nan",
            "//! A record.\n\npub closed struct Record {\n    pub size: Sise\n} Record\n",
        ),
        ("other/package.nan", &manifest("other")),
        (
            "other/other.nan",
            "//! Sizes.\n\npub closed struct Size {\n    pub bytes: u32\n} Size\n",
        ),
    ]);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    (scratch, workspace)
}

#[test]
fn a_check_of_one_package_reads_only_what_it_reaches() {
    let (_scratch, workspace) = probe_and_other();
    let probe = workspace
        .packages
        .iter()
        .position(|package| package.dir == "probe")
        .expect("the probe package");
    let scope = Scope::Package(probe);
    let checkers = scope_checkers(&workspace, crate::stdlib::packages(), &scope);
    assert!(!checkers.is_empty());
    for (_, checker) in &checkers {
        let names: Vec<&str> = checker
            .db
            .packages
            .iter()
            .filter(|package| !package.carried)
            .map(|package| package.name.as_str())
            .collect();
        assert_eq!(names, ["probe"]);
    }
    let whole: Vec<Diagnostic> = reported(&workspace, &Scope::Workspace)
        .into_iter()
        .filter(|diagnostic| {
            diagnostic
                .path
                .as_deref()
                .is_some_and(|path| scope.holds(&workspace, path, diagnostic.line))
        })
        .collect();
    let scoped = reported(&workspace, &scope);
    assert_eq!(
        scoped
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>(),
        [Code::NameUnknown]
    );
    assert_eq!(scoped, whole);
}
