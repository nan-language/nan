use std::collections::BTreeSet;

use super::{Needed, approvals, unapproved};
use crate::diagnostic::Code;
use crate::machine::lock::{Entry, Lock};
use crate::workspace::Workspace;
use crate::workspace::tests::Scratch;

#[test]
fn an_epoch_the_lock_does_not_approve_is_unapproved() {
    let scratch = Scratch::new(&[
        (
            "app/package.nan",
            "//! A package.\n\npackage app {\n    language 1\n    targets any\n} app\n",
        ),
        ("app/app.nan", "//! Apps.\n"),
    ]);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let needed = Needed {
        pairs: BTreeSet::new(),
        epochs: BTreeSet::from([("meter::mean".to_owned(), 1)]),
    };
    let mut lock = Lock::default();
    let reported = |lock: &Lock| -> Vec<Code> {
        (unapproved(&workspace.packages[0], &needed, lock).iter())
            .map(|diagnostic| diagnostic.code)
            .collect()
    };
    assert_eq!(reported(&lock), [Code::DepEpochUnapproved]);
    lock.epochs.push(("meter::mean".to_owned(), 1));
    assert!(reported(&lock).is_empty());
}

#[test]
fn a_lock_approves_the_escapes_of_each_target_a_dependency_declares() {
    let scratch = Scratch::new(&[
        (
            "procinfo/package.nan",
            "//! Process ids.\n\npackage procinfo {\n    language 1\n    targets x86_64-linux, x86_64-windows\n    allow trusted in os\n} procinfo\n",
        ),
        (
            "procinfo/procinfo.nan",
            "//! The process id.\n\npub fn process_id() -> u32 {\n    return os::process_id()\n} process_id\n",
        ),
        (
            "procinfo/os@linux/os.nan",
            "//! From Linux.\n\nforeign fn getpid() -> i32 limits notrap, terminates, noalloc convention \"sysv\"\n\npub fn process_id() -> u32 {\n    return getpid().to_bits()\n} process_id\n",
        ),
        (
            "procinfo/os@windows/os.nan",
            "//! From Windows.\n\nforeign fn GetCurrentProcessId() -> u32 limits notrap, terminates, noalloc convention \"win64\"\n\npub fn process_id() -> u32 {\n    return GetCurrentProcessId()\n} process_id\n",
        ),
        (
            "app/package.nan",
            "//! Logs its process id.\n\npackage app {\n    language 1\n    targets x86_64-linux\n    entry main\n    dep procinfo \"../procinfo\"\n    allow trusted in procinfo\n} app\n",
        ),
        (
            "app/app.nan",
            "//! Entry.\n\nfn main(take world: host::World) fails task::Cancelled uses wait {\n    world.log.info(\"process\", value: procinfo::process_id())\n} main\n",
        ),
    ]);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let app = workspace
        .packages
        .iter()
        .find(|package| package.name() == Some("app"))
        .expect("the app package");
    let (lock, left) = approvals(&workspace, app, Lock::default(), true);
    assert!(left.is_empty());
    for function in ["getpid", "GetCurrentProcessId"] {
        let escape = Entry::Escape("foreign", format!("procinfo::os::{function}"));
        assert!(lock.approved.contains(&("procinfo".to_owned(), escape)));
    }
}
