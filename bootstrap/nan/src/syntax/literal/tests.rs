use super::*;

#[test]
fn escapes_stand_for_the_characters_they_denote() {
    assert_eq!(
        text_value(r#""caf\u{E9} \"q\" \x41\t\\""#),
        "caf\u{E9} \"q\" A\t\\"
    );
    assert_eq!(text_value("\"\""), "");
}
