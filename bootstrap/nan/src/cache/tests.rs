use std::os::unix::fs::PermissionsExt as _;

use super::*;
use crate::workspace::tests::Scratch;

#[test]
fn a_record_reads_back_until_damaged_or_exposed() {
    let scratch = Scratch::new(&[("cache/other", "")]);
    let cache = Cache {
        dir: Some(scratch.0.join("cache")),
        store: None,
        keys: None,
        home: scratch.0.clone(),
        uid: running_user().expect("a user id"),
    };
    let read = |name: &str| match cache.record(name).expect("a readable record") {
        Ok(contents) => Ok(contents),
        Err(Exposed(path)) => Err(path),
    };
    cache.replace("pass", b"reach").expect("a written record");
    assert_eq!(read("pass"), Ok(Some(b"reach".to_vec())));
    assert_eq!(read("missing"), Ok(None));
    let path = scratch.0.join("cache/pass");
    let mut damaged = fs::read(&path).expect("the record's file");
    damaged[0] ^= 1;
    fs::write(&path, damaged).expect("a damaged record");
    assert_eq!(read("pass"), Ok(None));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o620)).expect("a group grant");
    assert_eq!(read("pass"), Err("~/cache/pass".to_owned()));
}

#[test]
fn a_secret_key_is_written_once_for_its_user_alone() {
    let scratch = Scratch::new(&[]);
    fs::create_dir_all(&scratch.0).expect("the home directory");
    let cache = Cache {
        dir: None,
        store: None,
        keys: Some(scratch.0.join("keys")),
        home: scratch.0.clone(),
        uid: running_user().expect("a user id"),
    };
    let written = |seed: &[u8; 32]| match cache.new_key("ana", seed).expect("a key written") {
        Ok(made) => made,
        Err(exposed) => panic!("{}", exposed.0),
    };
    let read = || match cache.secret_key("ana").expect("a key read") {
        Ok(seed) => seed,
        Err(exposed) => panic!("{}", exposed.0),
    };
    assert_eq!(read(), None);
    assert_eq!(written(&[1; 32]), (true, "~/keys/ana.key".to_owned()));
    assert!(!written(&[2; 32]).0);
    assert_eq!(read(), Some([1; 32]));
    let file = scratch.0.join("keys/ana.key");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).expect("the mode changed");
    assert!(matches!(cache.secret_key("ana"), Ok(Err(Exposed(path))) if path == "~/keys/ana.key"));
}
