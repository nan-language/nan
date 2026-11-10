use std::collections::BTreeSet;

use super::*;

#[test]
fn choices_are_ll1_under_the_published_resolutions() {
    for (choice, alts) in CHOICES.iter().enumerate() {
        for (i, &left) in alts.iter().enumerate() {
            for &right in &alts[i + 1..] {
                let (left, right) = (ANALYSIS.seq(left), ANALYSIS.seq(right));
                let shared: Vec<String> = left
                    .ok
                    .terms()
                    .filter(|term| right.ok.contains(*term))
                    .map(Term::written)
                    .collect();
                assert!(
                    shared.is_empty(),
                    "choice {choice}: alternatives share {shared:?}"
                );
                let rejected: Vec<String> = left
                    .rejected
                    .terms()
                    .filter(|term| right.rejected.contains(*term))
                    .map(Term::written)
                    .collect();
                assert!(
                    rejected.is_empty(),
                    "choice {choice}: rejected alternatives share {rejected:?}"
                );
            }
        }
    }
}

#[test]
fn rejected_code_points_leave_printable_ascii_alone() {
    assert!(
        REJECTED
            .iter()
            .all(|&(low, high)| high < 0x20 || low >= 0x7F)
    );
}

/// The words between backquotes of the cells of `column` of the table rows starting `head`.
fn table_words(text: &str, head: &str, column: usize) -> BTreeSet<String> {
    text.lines()
        .filter(|line| line.starts_with(head))
        .flat_map(|line| {
            let cell = line.split('|').nth(column).unwrap_or_default();
            cell.split('`')
                .skip(1)
                .step_by(2)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn the_keyword_tables_list_the_context_and_reserved_words_of_the_grammar() {
    let grammar = include_str!("../../../../../docs/spec/41-grammar.md");
    let lexical = include_str!("../../../../../docs/spec/01-lexical.md");
    let context = table_words(grammar, "| context words |", 2);
    let reserved = table_words(grammar, "| reserved words |", 2);
    let classified = ["construction_expr =", "result_expr =", "old_expr ="]
        .iter()
        .flat_map(|rule| {
            let line = grammar.lines().find(|line| line.starts_with(rule));
            let line = line.expect("a classification rule");
            line.split('\'').skip(1).step_by(2).map(str::to_owned)
        });
    let generated: BTreeSet<String> = ContextWord::ALL
        .iter()
        .map(|word| word.spelling().to_owned())
        .chain(classified)
        .collect();
    assert_eq!(generated, &context | &reserved);
    let section = lexical
        .split("The context words of language 1 are exactly these:")
        .nth(1)
        .and_then(|rest| rest.split("\n\n").nth(1))
        .expect("the context word table");
    let listed = table_words(section, "| `", 1);
    assert_eq!(listed, context);
}
