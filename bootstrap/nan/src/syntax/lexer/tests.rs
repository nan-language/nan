use super::*;

/// The tokens of `source`, one logical line per row, spelt as grammar notation names them.
fn render(source: &[u8]) -> String {
    let mut out = String::new();
    for token in lex(source).tokens {
        let word = match token.kind {
            TokenKind::Keyword(keyword) => keyword.spelling().to_owned(),
            TokenKind::Punct(punct) => punct.spelling().to_owned(),
            TokenKind::Foreign(foreign) => foreign.spelling().to_owned(),
            TokenKind::LexError => "ERROR".to_owned(),
            kind => kind.term().expect("a terminal").written(),
        };
        out.push_str(&word);
        out.push(if token.kind == TokenKind::Nl {
            '\n'
        } else {
            ' '
        });
    }
    out.trim_end().to_owned()
}

fn diagnostics(source: &[u8]) -> Vec<(u32, &'static str)> {
    lex(source)
        .diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.line, diagnostic.code.name()))
        .collect()
}

#[test]
fn virtual_tokens_follow_logical_lines() {
    let source = b"/// Limits a sensor reading to the calibrated range.
pub fn clamp_reading(raw: u32) -> u32
    ensures result <= 1_000
{
    if raw > 1_000 {
        return 1_000
    }
    return raw
} clamp_reading
";
    let expected = "COL0 DOC NL
COL0 pub fn IDENT ( IDENT : IDENT ) -> IDENT NL
ensures IDENT <= INT NL
COL0 { if IDENT > INT { return INT NL
} NL
return IDENT NL
COL0 } IDENT NL
EOF";
    assert_eq!(render(source), expected);
}

#[test]
fn logical_lines_skip_blank_and_comment_lines_after_a_continuing_token() {
    assert_eq!(
        render(b"a +\n\n    // middle\n    b\nc\n"),
        "COL0 IDENT + IDENT NL\nCOL0 IDENT NL\nEOF"
    );
    assert_eq!(render(b"a and\n"), "COL0 IDENT and NL\nEOF");
    assert_eq!(render(b"f(x) // done\n"), "COL0 IDENT ( IDENT ) NL\nEOF");
}

#[test]
fn text_runs_join_consecutive_lines() {
    assert_eq!(
        render(b"x =\n    \\\\usage: nan\n    \\\\    check\n\n    \\\\new run\n"),
        "COL0 IDENT = TEXT_LINE TEXT_LINE NL\nTEXT_LINE NL\nEOF"
    );
}

#[test]
fn literals_and_numbers_lex_to_their_classes() {
    let source = "a b\"NAN\\x00\" 'x' b'\\n' \"caf\\u{E9}\" 0xFEB0_0000 0b1000_0001 255u8 0.5 0x1f32 0..<n a<<=b\n";
    assert_eq!(
        render(source.as_bytes()),
        "COL0 IDENT BYTES CHAR BYTE TEXT INT INT INT FLOAT INT INT ..< IDENT IDENT <<= IDENT NL\nEOF"
    );
}

#[test]
fn foreign_spellings_are_tokens() {
    assert_eq!(
        render(b"a && b || !c? ;\n"),
        "COL0 IDENT && IDENT || ! IDENT ? ; NL\nEOF"
    );
}

#[test]
fn each_line_reports_its_first_offending_character() {
    let cases: &[(&[u8], &str, &str)] = &[
        (b"  return x\n", "lex.indentation", "ERROR NL\nEOF"),
        (b"\tx\n", "lex.tab", "ERROR NL\nEOF"),
        (b"x\r\n", "lex.carriage-return", "COL0 IDENT ERROR NL\nEOF"),
        (
            b"x \n",
            "lex.trailing-whitespace",
            "COL0 IDENT ERROR NL\nEOF",
        ),
        (
            b"x = \xE2\x80\x8B\n",
            "lex.rejected-code-point",
            "COL0 IDENT = ERROR NL\nEOF",
        ),
        (
            b"x \xFF y\n",
            "lex.invalid-utf8",
            "COL0 IDENT ERROR NL\nEOF",
        ),
        (
            b"pub const RETRY_LIMIT: u8 = #7\n",
            "lex.unexpected-character",
            "COL0 pub const IDENT : IDENT = ERROR NL\nEOF",
        ),
        (
            b"x \\ y\n",
            "lex.unexpected-character",
            "COL0 IDENT ERROR NL\nEOF",
        ),
        (
            "MAX_\u{C9}TAGES\n".as_bytes(),
            "lex.non-ascii",
            "COL0 IDENT ERROR NL\nEOF",
        ),
        (b"0755\n", "lex.malformed-number", "ERROR NL\nEOF"),
        (
            b"x 255u7\n",
            "lex.malformed-number",
            "COL0 IDENT ERROR NL\nEOF",
        ),
        (b"1e3\n", "lex.malformed-number", "ERROR NL\nEOF"),
        (b"2f64\n", "lex.literal-suffix", "ERROR NL\nEOF"),
        (b"1.5u8\n", "lex.literal-suffix", "ERROR NL\nEOF"),
        (b"0b1f32\n", "lex.literal-suffix", "ERROR NL\nEOF"),
        (b"\"C:\\data\"\n", "lex.unknown-escape", "ERROR NL\nEOF"),
        (b"\"\\u{1234567}\"\n", "lex.unknown-escape", "ERROR NL\nEOF"),
        (b"b\"\\u{41}\"\n", "lex.unknown-escape", "ERROR NL\nEOF"),
        (b"'\\q'\n", "lex.unknown-escape", "ERROR NL\nEOF"),
        (
            b"\"caf\\xE9\"\n",
            "lex.escape-out-of-range",
            "ERROR NL\nEOF",
        ),
        (
            b"\"\\u{D800}\"\n",
            "lex.escape-out-of-range",
            "ERROR NL\nEOF",
        ),
        (
            "b\"caf\u{E9}\"\n".as_bytes(),
            "lex.non-ascii-byte",
            "ERROR NL\nEOF",
        ),
        (
            "b'\u{E9}'\n".as_bytes(),
            "lex.non-ascii-byte",
            "ERROR NL\nEOF",
        ),
        (b"'ok'\n", "lex.char-literal-size", "ERROR NL\nEOF"),
        (b"''\n", "lex.char-literal-size", "ERROR NL\nEOF"),
        (
            b"x \"unfinished\n",
            "lex.unterminated-literal",
            "COL0 IDENT ERROR NL\nEOF",
        ),
        (
            b"x /// doc\n",
            "lex.not-at-line-start",
            "COL0 IDENT ERROR NL\nEOF",
        ),
        (
            b"x \\\\run\n",
            "lex.not-at-line-start",
            "COL0 IDENT ERROR NL\nEOF",
        ),
        (
            b"x",
            "lex.missing-final-newline",
            "COL0 IDENT ERROR NL\nEOF",
        ),
        (b"", "lex.missing-final-newline", "ERROR NL\nEOF"),
    ];
    for (source, code, tokens) in cases {
        let text = String::from_utf8_lossy(source);
        assert_eq!(diagnostics(source), [(1, *code)], "diagnostics of {text:?}");
        assert_eq!(render(source), *tokens, "tokens of {text:?}");
    }
}

#[test]
fn valid_escapes_and_bytes_raise_nothing() {
    for source in [
        &b"b'\\xFF'\n"[..],
        b"'\\''\n",
        b"\"\\u{10FFFF}\"\n",
        b"\\\\a \"quote\" \\u{41}\n",
        b"// caf\xC3\xA9\n",
    ] {
        assert_eq!(
            diagnostics(source),
            [],
            "{}",
            String::from_utf8_lossy(source)
        );
    }
}

#[test]
fn rejected_code_points_are_errors_inside_comments_and_literals() {
    assert_eq!(
        diagnostics("x // a\u{200B}b\n".as_bytes()),
        [(1, "lex.rejected-code-point")]
    );
    assert_eq!(
        diagnostics("\"\u{FEFF}\"\n".as_bytes()),
        [(1, "lex.rejected-code-point")]
    );
}

#[test]
fn diagnostics_are_per_line() {
    assert_eq!(
        diagnostics(b"x\n  y\nz \n"),
        [(2, "lex.indentation"), (3, "lex.trailing-whitespace")]
    );
}
