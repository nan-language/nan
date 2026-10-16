use super::*;

fn parts() -> Parts {
    Parts {
        language: 1,
        manifest: "package meter {\n}\n".to_owned(),
        interface: vec![("meter".to_owned(), "nan api 1\n".to_owned())],
        nir: vec![0x4E, 0x49, 0x52, 0x00, 1, 0],
        requires: Vec::new(),
        source_tombstones: Vec::new(),
        history_tombstones: Vec::new(),
        ledger: Vec::new(),
        lines: vec![(
            "meter/meter.nan".to_owned(),
            vec![(3, "pub fn f() = 1".to_owned())],
        )],
        schemas: Vec::new(),
    }
}

#[test]
fn an_artefact_decodes_to_its_code_root_and_attestation() {
    let root = code_root(&parts());
    let attestation = attestation(
        Hash::of(b"toolchain"),
        source_tree_hash(&[]),
        &seal_list(&[]),
    );
    let decoded = decode(&artefact(&root, &attestation)).unwrap();
    assert_eq!(decoded.code_root, root);
    assert_eq!(decoded.root_hash(), Hash::of(&root));
}

#[test]
fn equal_parts_give_equal_code_roots_whatever_the_order_of_unordered_lists() {
    let mut reordered = parts();
    reordered.source_tombstones = vec!["b".to_owned(), "a".to_owned()];
    let mut ordered = parts();
    ordered.source_tombstones = vec!["a".to_owned(), "b".to_owned()];
    assert_eq!(code_root(&reordered), code_root(&ordered));
}

#[test]
fn a_reserved_section_or_another_format_is_its_own_error() {
    let mut reserved = artefact(&code_root(&parts()), &[]);
    let last = reserved.len() - 1;
    reserved[last] = 1;
    reserved.extend([1, 0]);
    assert_eq!(decode(&reserved).err(), Some(Code::ArtefactReservedSection));
    let mut future = artefact(&code_root(&parts()), &[]);
    future[4] = 2;
    assert_eq!(
        decode(&future).err(),
        Some(Code::ArtefactVersionUnsupported)
    );
    assert_eq!(decode(b"NARX").err(), Some(Code::ArtefactMalformed));
}
