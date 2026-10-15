use super::*;
use crate::syntax::file::parse;

fn replacement(start: u32, end: u32, text: &str) -> (String, Replacement) {
    (
        "a/a.nan".to_owned(),
        Replacement {
            start,
            end,
            text: text.to_owned(),
        },
    )
}

#[test]
fn positions_move_with_the_replacements_before_them_and_vanish_inside_them() {
    let fix = Fix(vec![replacement(10, 12, "abcd"), replacement(4, 4, "xy")]);
    let shifts = Shift::of(&fix);
    let shift = &shifts["a/a.nan"];
    assert_eq!(shift.position(3), Some(3));
    assert_eq!(shift.position(4), Some(6));
    assert_eq!(shift.position(11), None);
    assert_eq!(shift.position(12), Some(16));
    assert_eq!(shift.replaced(), vec![(4, 6), (12, 16)]);
}

#[test]
fn a_construct_holds_the_texts_inserted_at_its_ends() {
    let fix = Fix(vec![replacement(4, 4, "take "), replacement(7, 7, "!")]);
    let shifts = Shift::of(&fix);
    assert_eq!(shifts["a/a.nan"].construct((4, 7)), (4, 13));
    assert_eq!(shifts["a/a.nan"].construct((8, 9)), (14, 15));
}

#[test]
fn an_edit_run_grows_until_its_text_occurs_once() {
    let file =
        parse(b"//! A.\n\nconst A: u32 = 1\n\nconst A: u32 = 1\nconst B: u32 = 2\n".to_vec());
    assert_eq!(unique(&file, (5, 5)), (3, 5));
    assert_eq!(unique(&file, (6, 6)), (6, 6));
    assert_eq!(unique(&file, (1, 1)), (1, 1));
    assert_eq!(merged(vec![(4, 5), (1, 2), (5, 6)]), vec![(1, 2), (4, 6)]);
}

#[test]
fn a_text_of_whole_lines_keeps_its_empty_lines() {
    assert_eq!(lines_of(b""), Vec::<String>::new());
    assert_eq!(lines_of(b"\n"), vec![String::new()]);
    assert_eq!(lines_of(b"a\n\nb\n"), vec!["a", "", "b"]);
}
