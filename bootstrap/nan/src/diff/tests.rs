use super::*;

fn texts(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|line| (*line).to_owned()).collect()
}

#[test]
fn texts_merge_along_the_least_common_subsequence() {
    assert_eq!(
        merged(&texts(&["a", "b", "c"]), &texts(&["a", "x", "c", "d"])),
        ["  a", "- b", "+ x", "  c", "+ d"]
    );
    assert_eq!(
        merged(&texts(&["a", "b"]), &texts(&["b", "a"])),
        ["+ b", "  a", "- b"]
    );
    assert_eq!(merged(&texts(&[]), &texts(&["a"])), ["+ a"]);
}
