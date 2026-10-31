use super::api::{AcceptedTest, ClosureEntry, Doc, DocKey, Entry, Method, Seal, Snapshot};
use super::history::{History, Module};
use super::lock::{Entry as Approved, Lock, Locked};
use super::principals::Principals;
use super::{Invalid, escaped};
use crate::diagnostic::Code;
use crate::hash::Hash;

fn lock() -> Lock {
    Lock {
        snapshot: 2,
        deps: vec![
            Locked {
                package: "meter".to_owned(),
                alias: Some("meter".to_owned()),
                source: "../meter".to_owned(),
                root: Hash::of(b"meter"),
                signer: None,
                line: 0,
            },
            Locked {
                package: "ring".to_owned(),
                alias: None,
                source: "ring-store".to_owned(),
                root: Hash::of(b"ring"),
                signer: Some([3; 32]),
                line: 0,
            },
        ],
        approved: vec![
            ("meter".to_owned(), Approved::Alloc),
            (
                "meter".to_owned(),
                Approved::Escape("trusted", "meter::read".to_owned()),
            ),
        ],
        epochs: vec![("meter::Total.add".to_owned(), 1)],
    }
}

/// The lock's bytes with one line replaced and the check made to hold again.
fn edited(bytes: &[u8], line: usize, text: &str) -> Vec<u8> {
    let text_of = String::from_utf8(bytes.to_vec()).unwrap();
    let mut lines: Vec<&str> = text_of.lines().collect();
    lines[line - 1] = text;
    lines.pop();
    let mut body = lines.join("\n");
    body.push('\n');
    let check = Hash::of(body.as_bytes()).full();
    format!("{body}check \"{check}\"\n").into_bytes()
}

#[test]
fn a_lock_reads_back_as_written() {
    let mut written = lock();
    let bytes = written.bytes().unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(text.starts_with("nan lock 1\nsnapshot 2\ndep meter alias meter \"../meter\" "));
    let read = Lock::read(&bytes).unwrap();
    for dep in &mut written.deps {
        dep.line = read
            .deps
            .iter()
            .find(|d| d.package == dep.package)
            .unwrap()
            .line;
    }
    assert_eq!(read, written);
}

#[test]
fn a_lock_line_with_a_wrong_checksum_is_invalid_on_its_line() {
    let bytes = lock().bytes().unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    let line = text.lines().nth(2).unwrap();
    let at = line.rfind(" \"").unwrap();
    let forged = format!("{} {}", &line[..at], escaped(&Hash::of(b"forged").full()));
    let invalid = Lock::read(&edited(&bytes, 3, &forged)).unwrap_err();
    assert_eq!(invalid, Invalid::at(3));
}

#[test]
fn reserved_lock_fields_and_versions_are_their_own_errors() {
    let bytes = lock().bytes().unwrap();
    let reserved = Lock::read(&edited(&bytes, 4, "registry 1")).unwrap_err();
    assert_eq!(reserved.code, Code::FormatReservedField);
    assert_eq!(reserved.line, 4);
    let version = Lock::read(&edited(&bytes, 1, "nan lock 2")).unwrap_err();
    assert_eq!(version.code, Code::FormatMachineVersion);
}

#[test]
fn a_changed_byte_breaks_only_the_check() {
    let bytes = lock().bytes().unwrap();
    let text = String::from_utf8(bytes)
        .unwrap()
        .replace("snapshot 2", "snapshot 3");
    let invalid = Lock::read(text.as_bytes()).unwrap_err();
    assert_eq!(invalid, Invalid::at(text.lines().count() as u32));
}

#[test]
fn records_out_of_order_are_invalid() {
    let mut reordered = lock();
    reordered.deps.reverse();
    let bytes = reordered.bytes().unwrap();
    assert_eq!(Lock::read(&bytes).unwrap_err(), Invalid::at(4));
}

#[test]
fn a_history_reads_back_as_written() {
    let history = History {
        name: "meter".to_owned(),
        head: Some(Hash::of(b"head")),
        modules: vec![Module {
            name: "meter".to_owned(),
            schemas: vec![("Counter".to_owned(), Hash::of(b"schema"))],
            tombstones: vec![
                "retired Counter.bump(mut self) => mut self.record() changes \"saturates\""
                    .to_owned(),
            ],
        }],
    };
    let bytes = history.bytes().unwrap();
    assert_eq!(History::read(&bytes).unwrap(), history);
    let text = String::from_utf8(bytes).unwrap();
    let swapped = text.replacen("name", "head", 1);
    assert!(History::read(swapped.as_bytes()).is_err());
}

fn snapshot() -> Snapshot {
    let hash = Hash::of(b"interface");
    Snapshot {
        entries: vec![
            Entry {
                path: "example::Meter".to_owned(),
                text: vec!["pub open struct Meter {".to_owned(), "} Meter".to_owned()],
                promises: vec![
                    ("Copy".to_owned(), Vec::new()),
                    ("Send".to_owned(), vec!["T".to_owned()]),
                ],
                interface: Some(hash),
                docs: vec![Doc {
                    key: DocKey::Own,
                    bound: hash,
                    text: "A count that stops at its limit.".to_owned(),
                }],
                ..Entry::default()
            },
            Entry {
                path: "example::Meter.bump".to_owned(),
                text: vec!["pub fn Meter.bump(mut self)".to_owned()],
                interface: Some(hash),
                digests: vec![(
                    "x86_64-host".to_owned(),
                    "effects none | authority none | may-trap no | terminates".to_owned(),
                )],
                docs: vec![Doc {
                    key: DocKey::Member("count".to_owned()),
                    bound: hash,
                    text: "Line one\nline \"two\"".to_owned(),
                }],
                accepted: super::api::Accepted {
                    head: Some("pub fn Meter.bump(mut self)".to_owned()),
                    clauses: vec!["ensures self.value() >= old(self.value())".to_owned()],
                    tests: vec![AcceptedTest {
                        path: "example::bump_counts".to_owned(),
                        params: String::new(),
                        expects: vec![hash],
                    }],
                    methods: Vec::new(),
                },
                pending: vec![(hash, "requires".to_owned())],
                seals: vec![Seal {
                    principal: "ana@example.org".to_owned(),
                    item: hash,
                    closure: hash,
                    entries: [false, true]
                        .map(|body| ClosureEntry {
                            path: "hw::Ptr.read".to_owned(),
                            body,
                            hash,
                        })
                        .into_iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .into_iter()
                        .collect(),
                    signature: Some([7; 64]),
                }],
                ..Entry::default()
            },
            Entry {
                path: "example::Scaled".to_owned(),
                accepted: super::api::Accepted {
                    methods: vec![Method {
                        name: "scale".to_owned(),
                        head: "fn scale(self) -> u32".to_owned(),
                        clauses: vec!["ensures result > 0".to_owned()],
                        pending: vec![(hash, "ensures result > 0".to_owned())],
                    }],
                    ..super::api::Accepted::default()
                },
                ..Entry::default()
            },
        ],
    }
}

#[test]
fn an_api_snapshot_reads_back_as_written() {
    let snapshot = snapshot();
    let bytes = snapshot.bytes().unwrap();
    assert_eq!(Snapshot::read(&bytes).unwrap(), snapshot);
}

#[test]
fn an_api_record_out_of_its_entry_order_is_invalid() {
    let bytes = snapshot().bytes().unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    let interface = text
        .lines()
        .position(|line| line.starts_with("interface"))
        .unwrap();
    let promises = text.lines().nth(interface - 1).unwrap().to_owned();
    let swapped = edited(
        &edited(&bytes, interface, text.lines().nth(interface).unwrap()),
        interface + 1,
        &promises,
    );
    let invalid = Snapshot::read(&swapped).unwrap_err();
    assert_eq!(invalid, Invalid::at(interface as u32));
}

#[test]
fn a_principals_file_reads_back_as_written_and_keeps_its_principals_ordered() {
    let principals = Principals::default()
        .trusting("ben", [2; 32])
        .trusting("ana", [9; 32])
        .trusting("ana", [1; 32]);
    assert_eq!(
        principals.keys,
        vec![("ana".to_owned(), [1; 32]), ("ben".to_owned(), [2; 32])]
    );
    let bytes = principals.bytes().unwrap();
    assert_eq!(Principals::read(&bytes).unwrap(), principals);
    let text = String::from_utf8(bytes.clone()).unwrap();
    let ana = text.lines().nth(1).unwrap().to_owned();
    let swapped = edited(&edited(&bytes, 2, text.lines().nth(2).unwrap()), 3, &ana);
    assert_eq!(Principals::read(&swapped).unwrap_err(), Invalid::at(3));
    let unkeyed = edited(&bytes, 2, "principal \"ana\" \"ana\"");
    assert_eq!(Principals::read(&unkeyed).unwrap_err(), Invalid::at(2));
}

#[test]
fn a_file_without_records_is_not_written() {
    assert_eq!(Snapshot::default().bytes(), None);
}
