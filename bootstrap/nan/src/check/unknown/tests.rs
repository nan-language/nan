use super::*;

#[test]
fn distances_count_folded_edits_and_vocabulary_synonyms() {
    assert_eq!(
        Ranker::new("check_sum").distance_within("checksum"),
        Some(0)
    );
    assert_eq!(Ranker::new("scael").distance_within("scale"), Some(1));
    assert_eq!(
        Ranker::new("units::Scael").distance_within("units::Scale"),
        Some(1)
    );
    assert_eq!(Ranker::new("iter").distance_within("cursor"), Some(0));
    assert_eq!(
        Ranker::new("make_new").distance_within("make_empty"),
        Some(0)
    );
    assert_eq!(Ranker::new("count").distance_within("amount"), Some(2));
}
