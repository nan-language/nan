use super::*;
use crate::syntax::file::parse;

fn items(source: &str) -> Vec<Item> {
    let file = parse(source.as_bytes().to_vec());
    assert!(file.diagnostics.is_empty(), "{:?}", file.diagnostics);
    file.items
        .iter()
        .filter_map(|item| lower(&file, item.tree.as_ref()?))
        .collect()
}

fn returned(source: &str) -> Expr {
    let items = items(&format!("//! Test.\n\n{source}"));
    let ItemKind::Fn(item) = &items[0].kind else {
        panic!("a function");
    };
    let FnBody::Block(block) = &item.body else {
        panic!("a block body");
    };
    let StmtKind::Expr(Expr {
        kind: ExprKind::Return(Some(value)),
        ..
    }) = &block.stmts[0].kind
    else {
        panic!("a return");
    };
    (**value).clone()
}

fn shape(expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::Binary {
            op, left, right, ..
        } => format!("({} {op:?} {})", shape(left), shape(right)),
        ExprKind::Unary { op, operand } => format!("({op:?} {})", shape(operand)),
        ExprKind::Path { path, .. } => path.written(),
        ExprKind::Literal(_) => "lit".to_owned(),
        ExprKind::Paren(inner) => shape(inner),
        _ => "?".to_owned(),
    }
}

#[test]
fn operators_bind_by_their_precedence() {
    let cases = [
        ("a * b + c * a < b", "(((a Mul b) Add (c Mul a)) Lt b)"),
        (
            "a < b and b < c and not d",
            "(((a Lt b) And (b Lt c)) And (Not d))",
        ),
        ("a - b - c", "((a Sub b) Sub c)"),
        ("a & b & c", "((a BitAnd b) BitAnd c)"),
        ("a + b * c - d", "((a Add (b Mul c)) Sub d)"),
        ("(a or d) and -a < b", "((a Or d) And ((Neg a) Lt b))"),
    ];
    for (text, expected) in cases {
        let expr = returned(&format!("pub fn f() -> bool {{\n    return {text}\n}} f\n"));
        assert_eq!(shape(&expr), expected, "{text}");
    }
}
