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
        protocols: Vec::new(),
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
    reserved.extend([2, 0]);
    assert_eq!(decode(&reserved).err(), Some(Code::ArtefactReservedSection));
    let mut future = artefact(&code_root(&parts()), &[]);
    future[4] = 2;
    assert_eq!(
        decode(&future).err(),
        Some(Code::ArtefactVersionUnsupported)
    );
    assert_eq!(decode(b"NARX").err(), Some(Code::ArtefactMalformed));
}

fn signed_seal() -> AttestedSeal {
    let seed = [5; 32];
    let (sealed, closure) = (Hash::of(b"item"), Hash::of(b"closure"));
    let message = sign::seal_message("ana", "meter::f", sealed, closure);
    AttestedSeal {
        principal: "ana".to_owned(),
        item: "meter::f".to_owned(),
        sealed,
        closure,
        entries: Vec::new(),
        signed: Some((sign::sign(&seed, &message), sign::public_key(&seed))),
    }
}

fn publication(seed: &[u8; 32]) -> Publication<'_> {
    Publication {
        principal: "ben",
        seed,
        toolchain: Hash::of(b"toolchain"),
        closure: vec![("meter".to_owned(), Hash::of(&code_root(&parts())))],
        authors: vec!["cy".to_owned()],
        lock: b"nan lock 1\n".to_vec(),
    }
}

/// Whether bytes decode to an artefact a package trusting `trusted` accepts as the release a lock
/// line pins with `pinned`.
fn accepted(artefact: &[u8], pinned: Option<[u8; 32]>, trusted: &[(&str, [u8; 32])]) -> bool {
    let principals = trusted
        .iter()
        .fold(Principals::default(), |held, (principal, key)| {
            held.trusting(principal, *key)
        });
    decode(artefact)
        .unwrap()
        .accepted(pinned.as_ref(), &principals)
}

#[test]
fn a_published_artefact_is_accepted_until_a_signed_byte_changes() {
    let root = code_root(&parts());
    let attested = attestation(
        Hash::of(b"toolchain"),
        source_tree_hash(&[]),
        &seal_list(&[signed_seal()]),
    );
    let unsigned = artefact(&root, &attested);
    let ben = sign::public_key(&[3; 32]);
    assert!(accepted(&unsigned, None, &[]));
    let signed = published(&unsigned, &publication(&[3; 32]));
    assert_eq!(decode(&signed).unwrap().code_root, root);
    assert!(accepted(&signed, Some(ben), &[("ben", ben)]));
    let author = signed
        .windows(2)
        .position(|window| window == b"cy")
        .expect("the author in the provenance");
    let mut forged = signed.clone();
    forged[author] = b'd';
    assert!(!accepted(&forged, Some(ben), &[("ben", ben)]));
    let mut other = publication(&[3; 32]);
    other.closure = vec![("meter".to_owned(), Hash::of(b"another root"))];
    assert!(!accepted(
        &published(&unsigned, &other),
        Some(ben),
        &[("ben", ben)]
    ));
    let mut resealed = signed_seal();
    resealed.closure = Hash::of(b"another closure");
    let attested = attestation(
        Hash::of(b"toolchain"),
        source_tree_hash(&[]),
        &seal_list(&[resealed]),
    );
    assert!(!accepted(&artefact(&root, &attested), None, &[]));
}

#[test]
fn an_artefact_is_accepted_only_from_the_pinned_and_trusted_publisher() {
    let unsigned = artefact(
        &code_root(&parts()),
        &attestation(
            Hash::of(b"toolchain"),
            source_tree_hash(&[]),
            &seal_list(&[signed_seal()]),
        ),
    );
    let (ben, impostor) = (sign::public_key(&[3; 32]), sign::public_key(&[9; 32]));
    let signed = published(&unsigned, &publication(&[3; 32]));
    let resigned = published(&unsigned, &publication(&[9; 32]));
    let trusting_ben = [("ben", ben)];
    assert!(!accepted(&resigned, Some(ben), &trusting_ben));
    assert!(!accepted(&resigned, Some(impostor), &trusting_ben));
    assert!(!accepted(&unsigned, Some(ben), &trusting_ben));
    assert!(!accepted(&signed, None, &trusting_ben));
    assert!(!accepted(&signed, Some(ben), &[]));
    let ana = sign::public_key(&[5; 32]);
    assert!(accepted(&signed, Some(ben), &[("ana", ana), ("ben", ben)]));
    assert!(!accepted(
        &signed,
        Some(ben),
        &[("ana", impostor), ("ben", ben)]
    ));
}

#[test]
fn an_artefact_holds_all_three_sections_or_none() {
    let signed = published(
        &artefact(&code_root(&parts()), &[0]),
        &publication(&[3; 32]),
    );
    let decoded = decode(&signed).unwrap();
    let mut partial = Record::default();
    partial.field(1, decoded.sections.as_ref().map(|_| vec![0]).unwrap());
    let mut lone = MAGIC.to_vec();
    uvar(&mut lone, FORMAT);
    bytes(&mut lone, &decoded.code_root);
    bytes(&mut lone, &decoded.attestation);
    lone.extend(partial.finish());
    assert_eq!(decode(&lone).err(), Some(Code::ArtefactMalformed));
}
