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

#[test]
fn a_package_declaring_any_is_judged_within_the_reach_of_a_final_build() {
    let domain = |name: &str| {
        format!(
            "pub fn {name}(n: u64, exec: task::Executor, alloc: mem::Alloc) -> bool fails task::Cancelled uses wait {{\n    let outcome = try await sys::Domain.run_sized(fn() -> u64 {{\n        return n\n    }}, stack: 65_536, exec, alloc)\n    return outcome.is_ok()\n}} {name}\n"
        )
    };
    let tally = format!(
        "//! Runs counts in domains.\n\n{}\n{}",
        domain("reached"),
        domain("unreached")
    );
    let scratch = Scratch::new(&[
        (
            "tally/package.nan",
            "//! Tallies.\n\npackage tally {\n    language 1\n    targets any\n} tally\n",
        ),
        ("tally/tally.nan", &tally),
        (
            "counter/package.nan",
            "//! A kernel.\n\npackage counter {\n    language 1\n    targets x86_64-bare\n    entry main\n    dep tally \"../tally\"\n} counter\n",
        ),
        (
            "counter/counter.nan",
            "//! Counts once.\n\nfn main(take k: boot::Kernel) -> Never limits stack 16_384 {\n    let alloc = k.alloc()\n    take k.anchor(Unit, run: fn(exec: task::Executor) fails task::Cancelled uses wait limits stack 16_384 {\n        discard try await tally::reached(3, exec, alloc)\n        loop {\n            try await hw::Signal[Unit].empty().wait()\n        }\n    })\n} main\n",
        ),
    ]);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let missing: Vec<(Option<String>, Option<u32>)> = reported(&workspace, &Scope::Workspace)
        .into_iter()
        .filter(|diagnostic| diagnostic.code == Code::SuspendStackMissing)
        .map(|diagnostic| (diagnostic.path, diagnostic.line))
        .collect();
    assert_eq!(missing, [(Some("tally/tally.nan".to_owned()), Some(4))]);
}
