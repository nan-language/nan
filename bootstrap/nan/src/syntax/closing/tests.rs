use super::*;

fn closed(prefix: &str) -> (String, Vec<(u32, String, bool)>) {
    let closed = close(prefix.as_bytes());
    let pieces = closed
        .pieces
        .iter()
        .map(|piece| (piece.line, piece.text.clone(), piece.hole))
        .collect();
    (String::from_utf8(closed.source).expect("UTF-8"), pieces)
}

#[test]
fn a_prefix_closes_with_the_fewest_tokens() {
    let (source, pieces) = closed(
        "//! A.\n\npub fn clamp_level(level: u32, ceiling: u32) -> u32 {\n    if level > ceiling {\n",
    );
    assert_eq!(
        pieces,
        vec![
            (5, "}".to_owned(), false),
            (6, "} clamp_level".to_owned(), false)
        ]
    );
    assert!(
        source.ends_with("    if level > ceiling {\n    }\n} clamp_level\n"),
        "{source}"
    );
}

#[test]
fn a_missing_expression_is_a_typed_hole_and_the_cut_token_is_dropped() {
    let (source, pieces) = closed("//! A.\n\npub fn level() -> u32 {\n    return 1 + lev");
    assert_eq!(pieces[0], (4, "todo \"hole\"".to_owned(), true));
    assert!(
        source.contains("    return 1 + todo \"hole\"\n} level\n"),
        "{source}"
    );
}
