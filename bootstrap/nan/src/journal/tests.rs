use super::*;
use crate::workspace::tests::Scratch;

#[test]
fn writes_replace_files_and_leave_no_journal() {
    let scratch = Scratch::new(&[(".nan/write", ""), ("a/a.nan", "old\n")]);
    write(&scratch.0, &[("a/a.nan".to_owned(), b"new\n".to_vec())])
        .expect("write through the journal");
    assert_eq!(
        fs::read_to_string(scratch.0.join("a/a.nan")).expect("read the file"),
        "new\n"
    );
    assert!(!journal_path(&scratch.0).exists());
}

#[test]
fn a_lock_performs_a_complete_journal_and_drops_an_incomplete_one() {
    let complete = Scratch::new(&[
        (".nan/journal", "write 4 a/a.nan\nnew\ncomplete\n"),
        ("a/a.nan", "old\n"),
    ]);
    drop(Lock::acquire(&complete.0, false).expect("lock the workspace"));
    assert_eq!(
        fs::read_to_string(complete.0.join("a/a.nan")).expect("read the file"),
        "new\n"
    );
    assert!(!journal_path(&complete.0).exists());

    let incomplete = Scratch::new(&[
        (".nan/journal", "write 4 a/a.nan\nne"),
        ("a/a.nan", "old\n"),
    ]);
    drop(Lock::acquire(&incomplete.0, true).expect("lock the workspace"));
    assert_eq!(
        fs::read_to_string(incomplete.0.join("a/a.nan")).expect("read the file"),
        "old\n"
    );
    assert!(!journal_path(&incomplete.0).exists());
}
