//! The lock `.nan/lock.nan`: the snapshot of the local store, the locked release of each package
//! of the dependency closure, and a final-link root's approvals.

use super::{Cursor, Invalid, Kind, Parsed, Record, escaped, read, write};
use crate::diagnostic::Code;
use crate::hash::Hash;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lock {
    pub snapshot: u64,
    /// The lines of the closure's packages, in byte order of package name.
    pub deps: Vec<Locked>,
    /// The approved pairs of the closure summary, in byte order of package, then entry.
    pub approved: Vec<(String, Entry)>,
    /// The approved epochs, in byte order of canonical path, then by epoch.
    pub epochs: Vec<(String, u64)>,
}

/// The line of one package of the closure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locked {
    pub package: String,
    /// Its alias, for a direct dependency.
    pub alias: Option<String>,
    /// Its source as its `dep` directive writes it, a path relative to the lock's package.
    pub source: String,
    /// The code root hash of its locked release.
    pub root: Hash,
    /// The record's line in the file read, 0 for one not read.
    pub line: u32,
}

/// An entry of an authority summary, as a lock's `approved` record writes it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Entry {
    Capability(String),
    Alloc,
    Device,
    Priv,
    Validator(String),
    Escape(&'static str, String),
}

/// The ledger kinds of trust escapes, as `escape` entries write them.
pub const ESCAPE_KINDS: [&str; 7] = [
    "trusted",
    "assume",
    "foreign",
    "landing",
    "validator",
    "decoder",
    "grant",
];

impl Entry {
    pub fn written(&self) -> String {
        match self {
            Entry::Capability(path) => format!("capability {}", escaped(path)),
            Entry::Alloc => "alloc".to_owned(),
            Entry::Device => "device".to_owned(),
            Entry::Priv => "priv".to_owned(),
            Entry::Validator(path) => format!("validator {}", escaped(path)),
            Entry::Escape(kind, path) => format!("escape {kind} {}", escaped(path)),
        }
    }

    fn read(cursor: &mut Cursor) -> Option<Entry> {
        Some(match cursor.word()? {
            "capability" => Entry::Capability(cursor.text()?.to_owned()),
            "alloc" => Entry::Alloc,
            "device" => Entry::Device,
            "priv" => Entry::Priv,
            "validator" => Entry::Validator(cursor.text()?.to_owned()),
            "escape" => {
                let word = cursor.word()?;
                let kind = ESCAPE_KINDS.into_iter().find(|kind| *kind == word)?;
                Entry::Escape(kind, cursor.text()?.to_owned())
            }
            _ => return None,
        })
    }
}

impl Locked {
    /// The record without its checksum.
    fn unchecked(&self) -> String {
        let alias = self
            .alias
            .as_ref()
            .map(|alias| format!(" alias {alias}"))
            .unwrap_or_default();
        format!(
            "dep {}{alias} {} {}",
            self.package,
            escaped(&self.source),
            escaped(&self.root.full())
        )
    }

    pub fn record(&self) -> String {
        let unchecked = self.unchecked();
        let checksum = Hash::of(unchecked.as_bytes()).full();
        format!("{unchecked} {}", escaped(&checksum))
    }
}

impl Lock {
    /// The file's bytes; none for a lock holding no record, which is not written.
    pub fn bytes(&self) -> Option<Vec<u8>> {
        let mut lines = vec![format!("snapshot {}", self.snapshot)];
        lines.extend(self.deps.iter().map(Locked::record));
        lines.extend(
            self.approved
                .iter()
                .map(|(package, entry)| format!("approved {package} {}", entry.written())),
        );
        lines.extend(
            self.epochs
                .iter()
                .map(|(path, epoch)| format!("epoch {} {epoch}", escaped(path))),
        );
        write(Kind::Lock, &lines)
    }

    /// Reads a lock, or why it is not valid.
    pub fn read(bytes: &[u8]) -> Result<Lock, Invalid> {
        let parsed = read(Kind::Lock, bytes)?;
        let lock = Lock::records(&parsed)?;
        parsed.checked(lock)
    }

    fn records(parsed: &Parsed) -> Result<Lock, Invalid> {
        let mut lock = Lock::default();
        let mut stage = 0;
        for (index, record) in parsed.records.iter().enumerate() {
            let invalid = Invalid::at(record.line);
            let mut cursor = Cursor::new(record);
            let word = cursor.word().ok_or(invalid)?;
            if matches!(word, "registry" | "floor") {
                return Err(Invalid {
                    code: Code::FormatReservedField,
                    line: record.line,
                });
            }
            let next = match word {
                "snapshot" if index == 0 => {
                    lock.snapshot = cursor.int().ok_or(invalid)?;
                    0
                }
                "dep" if index > 0 => {
                    lock.deps
                        .push(read_dep(record, &mut cursor).ok_or(invalid)?);
                    1
                }
                "approved" if index > 0 => {
                    let package = cursor.word().ok_or(invalid)?.to_owned();
                    let entry = Entry::read(&mut cursor).ok_or(invalid)?;
                    lock.approved.push((package, entry));
                    2
                }
                "epoch" if index > 0 => {
                    let path = cursor.text().ok_or(invalid)?.to_owned();
                    let epoch = cursor.int().ok_or(invalid)?;
                    lock.epochs.push((path, epoch));
                    3
                }
                _ => return Err(invalid),
            };
            let canonical = match next {
                0 => format!("snapshot {}", lock.snapshot),
                1 => lock.deps.last().map(Locked::record).unwrap_or_default(),
                2 => lock
                    .approved
                    .last()
                    .map(|(package, entry)| format!("approved {package} {}", entry.written()))
                    .unwrap_or_default(),
                _ => lock
                    .epochs
                    .last()
                    .map(|(path, epoch)| format!("epoch {} {epoch}", escaped(path)))
                    .unwrap_or_default(),
            };
            if !cursor.done() || next < stage || canonical != record.text || !lock.ordered() {
                return Err(invalid);
            }
            stage = next;
        }
        if parsed.records.is_empty() {
            return Err(Invalid::at(1));
        }
        Ok(lock)
    }

    /// Whether each kind of record is in its order, without two equal keys.
    fn ordered(&self) -> bool {
        let increasing = |keys: Vec<(&str, String)>| keys.windows(2).all(|pair| pair[0] < pair[1]);
        increasing(
            self.deps
                .iter()
                .map(|dep| (dep.package.as_str(), String::new()))
                .collect(),
        ) && increasing(
            self.approved
                .iter()
                .map(|(package, entry)| (package.as_str(), entry.written()))
                .collect(),
        ) && increasing(
            self.epochs
                .iter()
                .map(|(path, epoch)| (path.as_str(), format!("{epoch:020}")))
                .collect(),
        )
    }
}

fn read_dep(record: &Record, cursor: &mut Cursor) -> Option<Locked> {
    let package = cursor.word()?.to_owned();
    let alias = match cursor.takes("alias") {
        true => Some(cursor.word()?.to_owned()),
        false => None,
    };
    let source = cursor.text()?.to_owned();
    let root = cursor.hash()?;
    let checksum = cursor.hash()?;
    let locked = Locked {
        package,
        alias,
        source,
        root,
        line: record.line,
    };
    (Hash::of(locked.unchecked().as_bytes()) == checksum).then_some(locked)
}
