use std::os::unix::fs::PermissionsExt as _;

use super::*;
use crate::workspace::tests::Scratch;

#[test]
fn a_record_reads_back_until_damaged_or_exposed() {
    let scratch = Scratch::new(&[("cache/other", "")]);
    let cache = Cache {
        dir: Some(scratch.0.join("cache")),
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
