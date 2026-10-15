use super::*;
use crate::syntax::file::{parse, parse_std};

fn format(source: &str) -> String {
    String::from_utf8(source_file(&parse(source.as_bytes().to_vec()), None)).expect("UTF-8 output")
}

/// Formats `source`, checks the result against `expected` and that formatting it changes nothing.
fn check(source: &str, expected: &str) {
    let formatted = format(source);
    assert_eq!(formatted, expected, "formatting\n{source}");
    assert_eq!(
        format(&formatted),
        formatted,
        "formatting again\n{formatted}"
    );
}

#[test]
fn tokens_are_spaced_and_numbers_grouped() {
    check(
        "//!Sums.\npub const MASK:u64=0xfeb00000\nconst BIG:u32=1000000\nconst SMALL:u32=1_0_0\nconst HALF:f64=1234.5678\nfn f(x:u32)->u32=-x+ ~x*foo(x)[0].y - ~ - -x\n",
        "//! Sums.\n\npub const MASK: u64 = 0xFEB0_0000\n\nconst BIG: u32 = 1_000_000\n\nconst SMALL: u32 = 100\n\nconst HALF: f64 = 1_234.5678\n\nfn f(x: u32) -> u32 = -x + ~x * foo(x)[0].y - ~- -x\n",
    );
    check(
        "//! T.\n\npackage app {\n    language 1\n    targets x86_64 - host\n} app\n",
        "//! T.\n\npackage app {\n    language 1\n    targets x86_64-host\n} app\n",
    );
}

#[test]
fn lines_take_the_depth_of_their_delimiters_and_units() {
    check(
        "//! D.\n\nfn f(x: u32) -> u32 {\n        if x > 1 {\n    return x +\n    1\n        }\n    return match x {\n    0 => 1\n        else => 2\n    }\n} f\n",
        "//! D.\n\nfn f(x: u32) -> u32 {\n    if x > 1 {\n        return x +\n            1\n    }\n    return match x {\n        0 => 1\n        else => 2\n    }\n} f\n",
    );
}

#[test]
fn blank_lines_separate_items_once() {
    check(
        "\n//! B.\n\n\n// About X.\nconst X: u32 = 1\n// About Y.\n\n\n\nconst Y: u32 = 2\nfn f() {\n\n    return\n\n} f\n\n\n",
        "//! B.\n\n// About X.\nconst X: u32 = 1\n\n// About Y.\n\nconst Y: u32 = 2\n\nfn f() {\n    return\n} f\n",
    );
}

#[test]
fn breakable_lists_are_flat_or_broken() {
    check(
        "//! L.\n\nfn f(a: u32, b: u32, c: u32, d: u32, e: u32) {\n} f\n\nfn g(\n    a: u32,\n    ) {\n    h(a,\n        b)\n    h(a, b,)\n} g\n",
        "//! L.\n\nfn f(\n    a: u32,\n    b: u32,\n    c: u32,\n    d: u32,\n    e: u32,\n    ) {\n} f\n\nfn g(a: u32) {\n    h(\n        a,\n        b,\n    )\n    h(a, b)\n} g\n",
    );
    check(
        "//! L.\n\nfn f( // the only one\n    a: u32) {\n} f\n",
        "//! L.\n\nfn f( // the only one\n    a: u32,\n    ) {\n} f\n",
    );
}

#[test]
fn other_lists_lie_on_one_line() {
    check(
        "//! O.\n\npub closed struct P[T: Copy,\n    U] derives Eq,\n    Copy {\n    pub x: T\n} P\n",
        "//! O.\n\npub closed struct P[T: Copy, U] derives Copy, Eq {\n    pub x: T\n} P\n",
    );
}

#[test]
fn braces_end_and_start_lines_except_in_trusted_code() {
    check(
        "//! T.\n\nfn f(x: u32) -> u32 {\n    if x > 0 { return 1\n    }\n    let y = trusted \"premise\" { g(x) }\n    match x {\n        0 => {}\n        else => return y\n    }\n    return y\n} f\n",
        "//! T.\n\nfn f(x: u32) -> u32 {\n    if x > 0 {\n        return 1\n    }\n    let y = trusted \"premise\" { g(x) }\n    match x {\n        0 => {\n        }\n        else => return y\n    }\n    return y\n} f\n",
    );
}

#[test]
fn canonical_order_sorts_modifiers_clauses_directives_limits_and_derives() {
    check(
        "//! C.\n\npub view linear open struct S derives Render, Hash, Copy, Custom {\n    x: u32\n} S\n\nfn f(x: u32) -> u32 limits stack 64, notrap, pure\n    waive length \"why\"\n    todo \"check\" example f(1) == 1\n    ensures result > 0\n    requires x > 0\n{\n    return x\n} f\n",
        "//! C.\n\npub open linear view struct S derives Copy, Hash, Render, Custom {\n    x: u32\n} S\n\nfn f(x: u32) -> u32 limits pure, notrap, stack 64\n    requires x > 0\n    ensures result > 0\n    todo \"check\" example f(1) == 1\n    waive length \"why\"\n{\n    return x\n} f\n",
    );
    check(
        "//! P.\n\npackage app {\n    deny b -> a\n    allow trusted in zeta, alpha\n    dep units \"../units\"\n    entry main\n    deny a -> c\n    targets x86_64-host, x86_64-bare\n    dep clock \"../clock\"\n    allow device in app\n    language 1\n} app\n",
        "//! P.\n\npackage app {\n    language 1\n    targets x86_64-bare, x86_64-host\n    entry main\n    dep clock \"../clock\"\n    dep units \"../units\"\n    allow device in app\n    allow trusted in alpha, zeta\n    deny a -> c\n    deny b -> a\n} app\n",
    );
}

#[test]
fn labels_naming_a_binding_in_scope_are_punned_in_calls() {
    check(
        "//! P.\n\nfn f(count: u32, items: Slice[u32]) -> u32 {\n    let total = g(count: count, items: items, total: total)\n    let pair = Pair(left: count, count: count)\n    var limit = 1\n    return h(count, limit: mut limit, LIMIT: LIMIT, other: count)\n} f\n",
        "//! P.\n\nfn f(count: u32, items: Slice[u32]) -> u32 {\n    let total = g(count: count, items, total: total)\n    let pair = Pair(left: count, count: count)\n    var limit = 1\n    return h(count, mut limit, LIMIT: LIMIT, other: count)\n} f\n",
    );
    check(
        "//! P.\n\nfn f(items: Slice[u32], x: u32) -> u32 {\n    for item in items {\n        g(x, item: item)\n    }\n    g(x, item: item)\n    if x > 0 {\n        let y = 1\n        g(x, y: y)\n    }\n    g(x, y: y)\n    return match x {\n        Some(inner) => g(x, inner: inner)\n        else => g(x, inner: inner)\n    }\n} f\n",
        "//! P.\n\nfn f(items: Slice[u32], x: u32) -> u32 {\n    for item in items {\n        g(x, item)\n    }\n    g(x, item: item)\n    if x > 0 {\n        let y = 1\n        g(x, y)\n    }\n    g(x, y: y)\n    return match x {\n        Some(inner) => g(x, inner)\n        else => g(x, inner: inner)\n    }\n} f\n",
    );
}

#[test]
fn clause_lines_of_a_body_less_std_function_take_the_depth_of_its_clauses() {
    let source = "//! S.\n\npub fn u32.f(self) -> u32 limits pure\n    example (~0u32).f() == 1\n    example(1u32).f() == 1\n";
    let formatted = source_file(&parse_std(source.as_bytes().to_vec()), None);
    assert_eq!(
        String::from_utf8(formatted).expect("UTF-8 output"),
        "//! S.\n\npub fn u32.f(self) -> u32 limits pure\n    example (~0u32).f() == 1\n    example (1u32).f() == 1\n",
    );
}

#[test]
fn comments_take_the_depth_of_the_line_they_precede() {
    check(
        "//! C.\n\nfn f(x: u32) -> u32\n    requires x > 0\n// before the body\n{\n    for y in x {\n        g(y)\n            //last\n    }\n    return x   //   done\n} f\n",
        "//! C.\n\nfn f(x: u32) -> u32\n    requires x > 0\n    // before the body\n{\n    for y in x {\n        g(y)\n        // last\n    }\n    return x //   done\n} f\n",
    );
}

#[test]
fn text_runs_keep_the_depth_of_their_first_line() {
    check(
        "//! R.\n\nconst USAGE: Str =\n        \\\\usage: nan\n    \\\\    check\n",
        "//! R.\n\nconst USAGE: Str =\n    \\\\usage: nan\n    \\\\    check\n",
    );
}

#[test]
fn damaged_lines_and_items_are_kept_as_written() {
    check(
        "//! K.\n\nfn f( {\n  x\n} f\nconst   Y: u32 = 2\n",
        "//! K.\n\nfn f( {\n  x\n} f\nconst Y: u32 = 2\n",
    );
    check(
        "//! K.\nconst  X: u32 = 1 \n",
        "//! K.\n\nconst  X: u32 = 1 \n",
    );
    let invalid = b"//! K.\nconst  X: Str = \"\xFF\"\n".to_vec();
    assert_eq!(source_file(&parse(invalid.clone()), None), invalid);
}

#[test]
fn an_item_scope_formats_that_item_alone() {
    let source = "//! S.\nconst  X: u32 = 1\nconst  Y: u32 = 2\n";
    let file = parse(source.as_bytes().to_vec());
    let formatted = String::from_utf8(source_file(&file, Some(3..=3))).expect("UTF-8 output");
    assert_eq!(formatted, "//! S.\nconst  X: u32 = 1\nconst Y: u32 = 2\n");
}
