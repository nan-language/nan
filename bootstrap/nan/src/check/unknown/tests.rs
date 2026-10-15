use super::*;

#[test]
fn distances_count_folded_edits_and_vocabulary_synonyms() {
    assert_eq!(distance("check_sum", "checksum"), 0);
    assert_eq!(distance("scael", "scale"), 1);
    assert_eq!(distance("units::Scael", "units::Scale"), 1);
    assert_eq!(distance("iter", "cursor"), 0);
    assert_eq!(distance("make_new", "make_empty"), 0);
    assert_eq!(distance("count", "amount"), 2);
}
