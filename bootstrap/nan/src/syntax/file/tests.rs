use super::*;

/// The diagnostics of a file holding `body` after a summary line, by line and code.
fn codes(body: &str) -> Vec<(u32, &'static str)> {
    let file = parse(format!("//! Summary.\n{body}").into_bytes());
    file.diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.line, diagnostic.code.name()))
        .collect()
}

/// The one diagnostic of a file holding `body` after a summary line, with expected and found.
fn reported(body: &str) -> (u32, &'static str, Option<String>, Option<String>) {
    let file = parse(format!("//! Summary.\n{body}").into_bytes());
    let [diagnostic] = &file.diagnostics[..] else {
        panic!(
            "one diagnostic expected in {body:?}, found {:?}",
            file.diagnostics
        );
    };
    (
        diagnostic.line,
        diagnostic.code.name(),
        diagnostic.expected.clone(),
        diagnostic.found.clone(),
    )
}

#[test]
fn unexpected_tokens_list_what_the_grammar_accepts() {
    let (line, code, expected, found) =
        reported("pub fn larger(left: u32 right: u32) -> u32 = left\n");
    assert_eq!((line, code), (2, "syntax.unexpected-token"));
    assert_eq!(expected.as_deref(), Some("\")\", \",\", \"::\", \"[\""));
    assert_eq!(found.as_deref(), Some("IDENT"));
}

#[test]
fn delimiters_left_open_or_closed_wrongly_are_named() {
    let unclosed = reported("pub fn f(value: u32) -> u64 {\n    return u64.widen(value\n} f\n");
    assert_eq!(
        unclosed,
        (
            3,
            "syntax.unclosed-delimiter",
            Some("\")\"".to_owned()),
            Some("NL".to_owned())
        )
    );
    let mismatched = reported("pub fn f(value: u32) -> u64 {\n    return u64.widen(value]\n} f\n");
    assert_eq!(
        mismatched,
        (
            3,
            "syntax.mismatched-delimiter",
            Some("\")\"".to_owned()),
            Some("\"]\"".to_owned())
        )
    );
}

#[test]
fn foreign_idioms_give_their_matched_text_and_first_rewrite() {
    let cases = [
        ("    return a && b\n", "a and b", "a && b"),
        (
            "    return leading(values)?\n",
            "try leading(values)",
            "leading(values)?",
        ),
        ("    let mut x = 1\n", "var x", "let mut x"),
        (
            "    total := count * 2\n",
            "let total = count",
            "total := count",
        ),
        ("    return word as u8\n", "u8.widen(word)", "word as u8"),
        ("    return !ready\n", "not ready", "!ready"),
        ("    return f(&mut x)\n", "mut x", "&mut x"),
        ("    return a === b\n", "a == b", "a === b"),
        (
            "    return try fetch().await\n",
            "try await fetch()",
            "try fetch().await",
        ),
        ("    count++\n", "count += 1", "count++"),
        ("    return 0..n\n", "0..<n", "0..n"),
        ("    assert!(ready)\n", "expect ready", "assert!(ready)"),
    ];
    for (statement, expected, found) in cases {
        let body = format!("fn f() {{\n{statement}}} f\n");
        let (line, code, written, matched) = reported(&body);
        assert_eq!((line, code), (3, "syntax.foreign"), "{statement}");
        assert_eq!(
            (written.as_deref(), matched.as_deref()),
            (Some(expected), Some(found)),
            "{statement}"
        );
    }
}

#[test]
fn foreign_idioms_without_rewrites_report_as_unexpected_tokens_do() {
    let (_, code, expected, found) = reported("fn f() {\n    return *x\n} f\n");
    assert_eq!(code, "syntax.foreign");
    assert_eq!(found.as_deref(), Some("\"*\""));
    assert!(expected.is_some_and(|expected| expected.contains("IDENT")));
    let (_, code, expected, found) = reported("fn f(x: &T) {\n} f\n");
    assert_eq!(
        (code, expected.as_deref(), found.as_deref()),
        ("syntax.foreign", Some("x: T"), Some("x: &T"))
    );
    let (_, code, expected, found) = reported("fn f() -> () {\n} f\n");
    assert_eq!(
        (code, expected, found.as_deref()),
        ("syntax.foreign", None, Some("-> ()"))
    );
}

#[test]
fn column_zero_failures_name_indentation() {
    assert_eq!(
        codes("fn f() {\n    return 1\n    } f\n"),
        [(4, "item.indented")]
    );
    assert_eq!(
        codes("fn f() {\nreturn 1\n} f\n"),
        [(3, "item.stray-column-zero")]
    );
}

#[test]
fn item_rules_name_their_own_syntax_failures() {
    assert_eq!(codes("fn f()\n{\n} f\n"), [(3, "item.body-opener")]);
    assert_eq!(
        codes("fn f()\n    requires true {\n} f\n"),
        [(3, "item.body-opener")]
    );
    assert_eq!(
        codes("fn f()\n    requires true\n    {\n} f\n"),
        [(4, "item.body-opener")]
    );
    assert_eq!(
        codes("fn f() {\n    requires true\n} f\n"),
        [(3, "item.body-opener")]
    );
    assert_eq!(
        codes("fn f()\n\nconst X: u32 = 1\n"),
        [(2, "item.missing-body")]
    );
    assert_eq!(codes("foreign fn f()\n"), [(2, "item.foreign-form")]);
    assert_eq!(codes("fn f() {\n}\n"), [(3, "item.label-mismatch")]);
    assert_eq!(
        codes("pub fn f() -> bool = match x {\n"),
        [(2, "item.multiple-lines")]
    );
    assert_eq!(
        codes("const F: u32 = g(fn() -> u32 {\n    return 1\n})\n"),
        [(2, "item.multiple-lines")]
    );
}

#[test]
fn rejected_alternatives_report_their_first_token() {
    assert_eq!(
        codes("fn f(a: bool, b: bool, c: bool) -> bool {\n    return a and\n        b or c\n} f\n"),
        [(4, "expr.parentheses")]
    );
    assert_eq!(codes("const X = 1\n"), [(2, "item.const-untyped")]);
    assert_eq!(
        codes("fn f() uses time {\n} f\n"),
        [(2, "effect.uses-unknown")]
    );
    assert_eq!(
        codes("registers R {\n    bank: u32 at 0 read\n} R\n"),
        [(3, "register.bank-reserved")]
    );
}

#[test]
fn context_words_follow_the_lookahead_resolutions() {
    let valid = [
        "fn f(op: task fn() -> u32) -> task::Cancelled = task::Cancelled.make()\n",
        "fn f(values: Slice[u32]) -> bool = all value in values: value > 0\n",
        "fn f(all: u32) -> u32 = all\n",
        "fn f(values: Slice[u32]) -> u32 = values[0]\n",
        "fn f(op: fn() -> u32 limits pure, notrap, step: u32) -> u32 = step\n",
        "retired old_name => manual \"gone\"\n",
    ];
    for body in valid {
        assert_eq!(codes(body), [], "{body}");
    }
}

#[test]
fn an_abandoned_item_hides_nothing_after_it() {
    assert_eq!(
        codes("fn f() {\n    return )\n} f\n\npub struct Point {\n    x: u32\n} Point\n"),
        [(3, "syntax.unexpected-token"), (6, "item.openness-missing")]
    );
}

#[test]
fn doc_comments_documenting_nothing_are_plain_comments() {
    assert_eq!(
        codes("fn f() {\n    /// Not here.\n    return\n} f\n"),
        [(3, "doc.misplaced")]
    );
    assert_eq!(
        codes("fn f() {\n/// Not here either.\n    return\n} f\n"),
        [(3, "doc.misplaced")]
    );
    assert_eq!(
        codes("struct S {\n    x: u32\n    /// Documents nothing.\n} S\n"),
        [(4, "doc.misplaced")]
    );
    assert_eq!(
        codes("const X: u32 = 1\n\n/// Trailing.\n"),
        [(4, "doc.misplaced")]
    );
    assert_eq!(
        codes("//! A second summary.\nconst X: u32 = 1\n"),
        [(2, "doc.misplaced")]
    );
    assert_eq!(codes("/// Documented.\nconst X: u32 = 1\n"), []);
}

#[test]
fn every_file_starts_with_a_described_summary() {
    let summary = |source: &str| {
        parse(source.as_bytes().to_vec())
            .diagnostics
            .iter()
            .map(|d| (d.line, d.code.name()))
            .collect::<Vec<_>>()
    };
    assert_eq!(summary("const X: u32 = 1\n"), [(1, "doc.summary-missing")]);
    assert_eq!(
        summary("//!\nconst X: u32 = 1\n"),
        [(1, "doc.summary-missing")]
    );
    assert_eq!(summary("// A comment first.\n//! Summary.\n"), []);
}

#[test]
fn a_line_with_a_lexical_diagnostic_reports_nothing_else() {
    assert_eq!(codes("fn f() {\n    return )\t\n} f\n"), [(3, "lex.tab")]);
}

#[test]
fn a_conflict_block_is_reported_once_with_fixes_keeping_either_side() {
    let body = "<<<<<<< ours\nfn f() -> u32 = 2\n=======\nfn f() -> u32 = 3\n>>>>>>> theirs\n";
    assert_eq!(codes(body), [(2, "merge.unresolved")]);
    let file = parse(format!("//! Summary.\n{body}").into_bytes());
    let kept: Vec<&str> = file.diagnostics[0]
        .fixes
        .iter()
        .map(|fix| fix[0].text.as_str())
        .collect();
    assert_eq!(kept, ["fn f() -> u32 = 2\n", "fn f() -> u32 = 3\n"]);
    assert!(
        file.items
            .iter()
            .all(|item| item.abandoned || item.tree.is_none())
    );
}

#[test]
fn a_std_function_without_a_body_ends_before_the_docs_of_the_next_item() {
    let file = parse_std(
        b"//! Summary.\n\npub fn Store.len(self) -> usize\n    example true\n\n/// The next.\npub fn Store.next(self) -> usize\n\n/// The last.\npub closed struct Last {\n} Last\n".to_vec(),
    );
    assert!(file.diagnostics.is_empty(), "{:?}", file.diagnostics);
    assert_eq!(file.items.len(), 3);
}
