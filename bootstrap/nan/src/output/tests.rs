use super::*;

fn output(records: usize) -> Output {
    let records = (0..records)
        .map(|index| Record {
            word: "error".to_owned(),
            fields: vec![format!("lex.tab a/a.nan:{index}"), "-".to_owned()],
            lines: vec!["quote".to_owned()],
        })
        .collect();
    Output {
        command: "check .".to_owned(),
        elision: "check .".to_owned(),
        verdict: Verdict::Failed,
        summary: vec![("errors", 2), ("incomplete", 0)],
        records,
    }
}

#[test]
fn records_lie_between_two_verdict_lines() {
    let text = output(1).render(&Format {
        json: false,
        budget: None,
        skip: 0,
    });
    assert_eq!(
        text,
        "failed check .: errors 2, incomplete 0\nerror lex.tab a/a.nan:0 -\n    quote\nfailed check .: errors 2, incomplete 0\n"
    );
}

#[test]
fn a_budget_elides_the_records_that_do_not_fit_and_names_the_command_showing_them() {
    let text = output(40).render(&Format {
        json: false,
        budget: Some(100),
        skip: 0,
    });
    assert!(text.len().div_ceil(4) <= 100);
    let elision = text
        .lines()
        .find(|line| line.starts_with("elided"))
        .expect("an elision line");
    let printed = text
        .lines()
        .filter(|line| line.starts_with("error"))
        .count();
    assert_eq!(
        elision,
        format!(
            "elided {} records: nan check . --skip {printed}",
            40 - printed
        )
    );
    let rest = output(40).render(&Format {
        json: false,
        budget: Some(100),
        skip: 39,
    });
    assert_eq!(
        rest.lines()
            .filter(|line| line.starts_with("error"))
            .count(),
        1
    );
    assert!(!rest.contains("elided"));
}

#[test]
fn json_lines_join_back_into_the_text_form() {
    let text = output(1).render(&Format {
        json: true,
        budget: None,
        skip: 0,
    });
    let verdict = "{\"command\":\"check .\",\"record\":\"verdict\",\"summary\":{\"errors\":2,\"incomplete\":0},\"verdict\":\"failed\"}";
    let record =
        "{\"fields\":[\"lex.tab a/a.nan:0\",\"-\"],\"lines\":[\"quote\"],\"record\":\"error\"}";
    assert_eq!(text, format!("{verdict}\n{record}\n{verdict}\n"));
}

#[test]
fn arguments_needing_it_are_written_as_text_literals() {
    assert_eq!(argument("example::Gauge"), "example::Gauge");
    assert_eq!(argument(""), "\"\"");
    assert_eq!(argument("a b\"\\"), "\"a b\\\"\\\\\"");
}

#[test]
fn output_holds_no_control_character_but_line_ends() {
    assert_eq!(printable(b"a\tb\rc\xFFd"), "a\\u{9}b\\u{D}c\u{FFFD}d");
}
