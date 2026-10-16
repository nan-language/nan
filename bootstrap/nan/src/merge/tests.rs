use super::merge;

fn merged(path: &str, base: &str, ours: &str, theirs: &str) -> (String, usize) {
    let merged = merge(
        path,
        Some(base.as_bytes()),
        Some(ours.as_bytes()),
        Some(theirs.as_bytes()),
    );
    (
        String::from_utf8(merged.content.unwrap_or_default()).unwrap(),
        merged.conflicts.len(),
    )
}

const SIGNAL: &str = "//! Signals.\n\npub closed enum Signal {\n    Start\n    Stop\n} Signal\n";

#[test]
fn variants_both_sides_add_merge_in_order() {
    let ours = SIGNAL.replace("    Stop\n", "    Stop\n    Pause\n");
    let theirs = SIGNAL.replace("    Stop\n", "    Stop\n    Resume\n");
    let (text, conflicts) = merged("app/app.nan", SIGNAL, &ours, &theirs);
    assert_eq!(conflicts, 0);
    assert_eq!(
        text,
        "//! Signals.\n\npub closed enum Signal {\n    Start\n    Stop\n    Pause\n    Resume\n} Signal\n"
    );
}

#[test]
fn items_changed_on_one_side_take_that_side_and_others_keep_ours_order() {
    let base = "//! Levels.\n\npub fn low() -> u8 = 1\n\npub fn high() -> u8 = 9\n";
    let ours = "//! Levels.\n\npub fn low() -> u8 = 2\n\npub fn high() -> u8 = 9\n\npub fn mid() -> u8 = 5\n";
    let theirs = "//! Levels.\n\npub fn top() -> u8 = 10\n\npub fn low() -> u8 = 1\n\npub fn high() -> u8 = 8\n";
    let (text, conflicts) = merged("app/app.nan", base, ours, theirs);
    assert_eq!(conflicts, 0);
    assert_eq!(
        text,
        "//! Levels.\n\npub fn top() -> u8 = 10\n\npub fn low() -> u8 = 2\n\npub fn high() -> u8 = 8\n\npub fn mid() -> u8 = 5\n"
    );
}

#[test]
fn an_item_both_sides_change_differently_is_a_conflict_block() {
    let base = "//! Levels.\n\npub fn low() -> u8 = 1\n";
    let ours = "//! Levels.\n\npub fn low() -> u8 = 2\n";
    let theirs = "//! Levels.\n\npub fn low() -> u8 = 3\n";
    let (text, conflicts) = merged("app/app.nan", base, ours, theirs);
    assert_eq!(conflicts, 1);
    assert_eq!(
        text,
        "//! Levels.\n\n<<<<<<< ours\npub fn low() -> u8 = 2\n=======\npub fn low() -> u8 = 3\n>>>>>>> theirs\n"
    );
}

#[test]
fn manifest_targets_both_sides_change_merge_as_sets() {
    let base = "//! App.\n\npackage app {\n    language 1\n    targets x86_64-host\n} app\n";
    let ours = base.replace("x86_64-host", "x86_64-bare, x86_64-host");
    let theirs = base.replace("x86_64-host", "x86_64-host, x86_64-uefi");
    let (text, conflicts) = merged("app/package.nan", base, &ours, &theirs);
    assert_eq!(conflicts, 0);
    assert!(
        text.contains("    targets x86_64-bare, x86_64-host, x86_64-uefi\n"),
        "{text}"
    );
}
