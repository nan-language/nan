use super::decoded;
use crate::syntax::file::parse;

/// A file with doc comments, keywords, punctuation, a foreign idiom and a syntax failure.
const SAMPLE: &str = "//! A.\n\n/// The sum.\npub fn sum(left: u32, right: u32) -> u32 {\n    return left && right\n} sum\n\npub struct Pair {\n    pub low: Array[u8, 2]\n} Pair\n\nfn broken( {\n} broken\n";

#[test]
fn a_stored_syntax_decodes_to_the_parse_it_stores() {
    let parsed = parse(SAMPLE.as_bytes().to_vec());
    assert!(!parsed.diagnostics.is_empty() && !parsed.items.is_empty());
    let stored = parsed.stored();
    let Ok(file) = decoded(&stored, SAMPLE.as_bytes().to_vec()) else {
        panic!("a stored syntax");
    };
    assert_eq!(file.stored(), stored);
    assert_eq!(file.tokens, parsed.tokens);
    assert_eq!(file.diagnostics, parsed.diagnostics);
    assert!(decoded(&stored[..stored.len() - 1], SAMPLE.as_bytes().to_vec()).is_err());
}
