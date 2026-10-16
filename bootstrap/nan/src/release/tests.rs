use std::collections::BTreeMap;

use super::*;

fn exported(path: &str, text: &str) -> BTreeMap<String, Entry> {
    let entry = Entry {
        path: path.to_owned(),
        text: vec![text.to_owned()],
        interface: Some(Hash::of(text.as_bytes())),
        ..Entry::default()
    };
    BTreeMap::from([(path.to_owned(), entry)])
}

#[test]
fn a_removed_item_is_additive_only_while_a_preserves_tombstone_forwards_it() {
    let parent = exported("app::percent", "pub fn percent(raw: u32) -> u32");
    let renamed = exported("app::ratio", "pub fn ratio(raw: u32) -> u32");
    let changed = exported("app::ratio", "pub fn ratio(raw: u64) -> u32");
    let epochs = |_: &str| (0, 0);
    let class = |release: &BTreeMap<String, Entry>, forward: Option<Forward>| {
        let forwards = forward
            .map(|forward| BTreeMap::from([("app::percent".to_owned(), forward)]))
            .unwrap_or_default();
        release_class(&classes(&parent, release, &epochs, &forwards), true)
    };
    let path = || Some(Forward::Path("app::ratio".to_owned()));
    assert_eq!(class(&renamed, None), Class::Breaking);
    assert_eq!(class(&renamed, path()), Class::Additive);
    assert_eq!(class(&changed, path()), Class::Breaking);
    assert_eq!(class(&changed, Some(Forward::Head)), Class::Additive);
}
