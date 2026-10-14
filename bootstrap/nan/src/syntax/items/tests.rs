use crate::syntax::file::parse;

fn codes(body: &str) -> Vec<(u32, &'static str)> {
    let file = parse(format!("//! Summary.\n{body}").into_bytes());
    file.diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.line, diagnostic.code.name()))
        .collect()
}

#[test]
fn modifiers_belong_to_their_kinds_once_each() {
    assert_eq!(
        codes("closed fn f() = 1\n"),
        [(2, "item.modifier-misplaced")]
    );
    assert_eq!(
        codes("proj struct S {\n    x: u32\n} S\n"),
        [(2, "item.modifier-misplaced")]
    );
    assert_eq!(
        codes("pub closed open struct S {\n    x: u32\n} S\n"),
        [(2, "item.modifier-conflict")]
    );
    assert_eq!(
        codes("pub linear closed linear struct S {\n    x: u32\n} S\n"),
        [(2, "item.modifier-conflict")]
    );
    assert_eq!(
        codes("proj validator fn f(x: u32) -> u32 {\n    return x\n} f\n"),
        [(2, "item.modifier-conflict")]
    );
    assert_eq!(
        codes("interrupt fn f() = 1\n"),
        [(2, "item.modifier-misplaced")]
    );
    assert_eq!(
        codes("proj fn f(self) -> u32 = 1\n"),
        [(2, "item.self-misplaced")]
    );
}

#[test]
fn openness_is_stated_exactly_on_pub_types() {
    assert_eq!(
        codes("pub enum E {\n    A\n} E\n"),
        [(2, "item.openness-missing")]
    );
    assert_eq!(
        codes("pkg closed enum E {\n    A\n} E\n"),
        [(2, "item.openness-not-pub")]
    );
    assert_eq!(
        codes("open struct S {\n    x: u32\n} S\n"),
        [(2, "item.openness-not-pub")]
    );
}

#[test]
fn heads_end_in_their_kind_of_body() {
    assert_eq!(
        codes("fn f() convention \"win64\"\n"),
        [(2, "item.foreign-form")]
    );
    assert_eq!(codes("foreign fn f() {\n} f\n"), [(2, "item.foreign-form")]);
    assert_eq!(codes("foreign fn f() = 1\n"), [(2, "item.foreign-form")]);
    assert_eq!(codes("foreign fn f() convention \"win64\"\n"), []);
    assert_eq!(
        codes("fn f() -> u32 limits pure = 1\n"),
        [(2, "item.transparent-limits")]
    );
    assert_eq!(codes("fn f() -> Unit {\n} f\n"), [(2, "item.unit-result")]);
    assert_eq!(
        codes("interface I {\n    fn f(self) -> Unit\n} I\n"),
        [(3, "item.unit-result")]
    );
}

#[test]
fn self_stands_first_in_member_heads_only() {
    assert_eq!(
        codes("fn S.f(x: u32, self) = 1\n"),
        [(2, "item.self-misplaced")]
    );
    assert_eq!(codes("fn S.f(mut self, x: u32) = 1\n"), []);
    assert_eq!(
        codes("test t for f (self) {\n} t\n"),
        [(2, "item.self-misplaced")]
    );
    assert_eq!(codes("interface I {\n    fn f(self)\n} I\n"), []);
}

#[test]
fn closing_labels_repeat_the_name() {
    assert_eq!(
        codes("struct S {\n    x: u32\n} T\n"),
        [(4, "item.label-mismatch")]
    );
    assert_eq!(codes("fn S.f() {\n} f\n"), [(3, "item.label-mismatch")]);
    assert_eq!(
        codes("interface I {\n    fn f(self) {\n    } g\n} I\n"),
        [(4, "item.label-mismatch")]
    );
}

#[test]
fn clause_lines_follow_their_groups() {
    let ordered = "fn f(x: u32) -> u32\n    requires x > 0\n    todo \"pin\" ensures result > 0\n    example f(1) == 1\n    waive length \"why\"\n{\n    return x\n} f\n";
    assert_eq!(codes(ordered), []);
    let late = "fn f(x: u32) -> u32\n    ensures result > 0\n    todo \"pin\" requires x > 0\n{\n    return x\n} f\n";
    assert_eq!(codes(late), [(4, "item.clause-order")]);
}

#[test]
fn kinds_without_visibility_reject_it() {
    assert_eq!(
        codes("pub impl S: I\n"),
        [(2, "item.visibility-not-allowed")]
    );
    assert_eq!(
        codes("pkg const assert true\n"),
        [(2, "item.visibility-not-allowed")]
    );
}
