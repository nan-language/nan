use super::*;
use crate::artefact::{Parts, artefact, attestation, code_root, seal_list, source_tree_hash};
use crate::workspace::tests::Scratch;

fn release(manifest: &str) -> Vec<u8> {
    let parts = Parts {
        language: 1,
        manifest: manifest.to_owned(),
        interface: Vec::new(),
        nir: Vec::new(),
        requires: Vec::new(),
        source_tombstones: Vec::new(),
        history_tombstones: Vec::new(),
        ledger: Vec::new(),
        lines: Vec::new(),
        schemas: Vec::new(),
    };
    artefact(
        &code_root(&parts),
        &attestation(
            Hash::of(b"toolchain"),
            source_tree_hash(&[]),
            &seal_list(&[]),
        ),
    )
}

fn append(name: Option<&str>, manifest: &str) -> Append {
    Append {
        package: "meter".to_owned(),
        name: name.map(str::to_owned),
        parent: None,
        summary: Some(Summary {
            class: Class::Additive,
            encode: Class::Identical,
            items: vec![("meter::Total.add".to_owned(), Class::Additive, 1)],
            bumps: vec![("meter::Total.add".to_owned(), "rounds".to_owned())],
            authority: vec!["alloc".to_owned()],
            functions: Vec::new(),
        }),
        artefact: release(manifest),
        sources: vec![("package.nan".to_owned(), manifest.as_bytes().to_vec())],
    }
}

#[test]
fn entries_append_once_per_release_and_read_back_in_index_order() {
    let scratch = Scratch::new(&[("store/other", "")]);
    let uid = std::os::unix::fs::MetadataExt::uid(&fs::metadata(&scratch.0).unwrap());
    let store = Store::new(StoreDir {
        dir: Some(scratch.0.join("store")),
        home: scratch.0.clone(),
        uid,
    });
    let appended = |name: Option<&str>, manifest: &str| {
        store.append(append(name, manifest)).unwrap().ok().unwrap()
    };
    let first = appended(Some("meter"), "a");
    let second = appended(Some("meter"), "b");
    assert_eq!((first.index, second.index), (1, 2));
    assert_eq!(appended(Some("meter"), "a"), first);
    assert_eq!(appended(None, "a"), first);
    let entries = store.entries().unwrap().ok().unwrap();
    assert_eq!(entries, vec![first, second]);
    assert_eq!(
        entries[0]
            .summary
            .as_ref()
            .unwrap()
            .epoch("meter::Total.add"),
        1
    );
}
