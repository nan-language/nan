use super::*;

#[test]
fn doc_text_reads_code_spans_and_references_left_to_right_and_claims_across_lines() {
    assert_eq!(
        references("a `[b]` [c] [d`e] `f [g"),
        vec![(9, "c"), (13, "d`e")]
    );
    let lines = [
        (1, 0, "Frames, never"),
        (2, 0, "allocates; `may trap`, never: blocks"),
        (3, 0, "    ```nan"),
        (4, 0, "is pure"),
    ];
    assert_eq!(claim_lines(&lines), BTreeSet::from([1, 4]));
    assert_eq!(code_block(&lines), Some(3));
}
