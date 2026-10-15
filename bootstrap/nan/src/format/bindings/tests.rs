use super::*;
use crate::syntax::file::parse;

/// Each identifier of the item's bindings, by line and text, with the line and text of the
/// binding's first introduction; parameters marked `kept`.
fn named(source: &str) -> Vec<String> {
    let file = parse(source.as_bytes().to_vec());
    let item = file
        .items
        .iter()
        .find(|item| item.tree.is_some())
        .expect("an item");
    let found = bindings(&file, item.tree.as_ref().expect("a tree"));
    let at = |token: u32| {
        let token = token as usize;
        format!("{}:{}", file.tokens[token].line, file.text(token))
    };
    let mut named: Vec<(u32, String)> = found
        .of
        .iter()
        .map(|(token, first)| {
            let kept = if found.parameters.contains(first) {
                " kept"
            } else {
                ""
            };
            (*token, format!("{} -> {}{kept}", at(*token), at(*first)))
        })
        .collect();
    named.sort();
    named.into_iter().map(|(_, text)| text).collect()
}

#[test]
fn uses_name_the_binding_in_scope() {
    assert_eq!(
        named(
            "fn f(limit: u32) -> u32 {\n    let total = limit\n    loop as outer {\n        for item in values() {\n            break outer\n        }\n    }\n    let add = fn [mut total](step: u32) => total + step\n    return match total {\n        Some(x) or Other(x) if x > 0 => all i in 0..<x: i < limit\n        else => total\n    }\n} f\n"
        ),
        [
            "1:limit -> 1:limit kept",
            "2:total -> 2:total",
            "2:limit -> 1:limit kept",
            "3:outer -> 3:outer",
            "4:item -> 4:item",
            "5:outer -> 3:outer",
            "8:add -> 8:add",
            "8:total -> 2:total",
            "8:step -> 8:step",
            "8:total -> 2:total",
            "8:step -> 8:step",
            "9:total -> 2:total",
            "10:x -> 10:x",
            "10:x -> 10:x",
            "10:x -> 10:x",
            "10:i -> 10:i",
            "10:x -> 10:x",
            "10:i -> 10:i",
            "10:limit -> 1:limit kept",
            "11:total -> 2:total",
        ]
    );
}
