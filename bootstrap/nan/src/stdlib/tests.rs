use super::*;
use crate::diagnostic::Code;

#[test]
fn every_std_declaration_parses() {
    for package in packages() {
        for file in &package.files {
            let failures: Vec<_> = file
                .syntax
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code != Code::DocMisplaced)
                .collect();
            assert!(failures.is_empty(), "{}: {failures:?}", file.path);
            assert!(file.syntax.items.iter().all(|item| !item.abandoned));
        }
    }
}

#[test]
fn families_restate_their_items_for_each_type() {
    let core = &modules()
        .into_iter()
        .find(|(path, _)| path == "core")
        .expect("core")
        .1;
    assert!(core.contains("pub fn u8.wrapping_add(self, other: u8) -> u8"));
    assert!(core.contains("example 0u8.leading_zeros() == 8"));
    assert!(core.contains("pub fn f32.total_cmp(self, other: f32) -> Ordering"));
    assert!(!core.contains("pub fn u8.wide_mul"));
}
