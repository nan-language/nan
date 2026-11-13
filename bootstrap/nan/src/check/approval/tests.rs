use std::collections::BTreeSet;

use super::{Needed, unapproved};
use crate::diagnostic::Code;
use crate::machine::lock::Lock;
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
