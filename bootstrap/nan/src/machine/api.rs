//! The API snapshot `.nan/api/<module>.nan` of a module: per item, what consumers see of it with
//! its public docs, its accepted spec and pending weakening, and its seals.

use super::{Cursor, Invalid, Kind, Record, escaped, read, write};
use crate::hash::Hash;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// In byte order of canonical path.
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    /// The lines of its interface text, for a `pub` item or impl line.
    pub text: Vec<String>,
    /// The promised properties, each with the generic parameters it rests on.
    pub promises: Vec<(String, Vec<String>)>,
    pub interface: Option<Hash>,
    /// Its digest line per declared target, in byte order of target.
    pub digests: Vec<(String, String)>,
    /// Its public docs and those of its members, in source order.
    pub docs: Vec<Doc>,
    pub accepted: Accepted,
    /// Its pending weakening: the item hash it is bound to and each weakened part, in byte order
    /// of part; an interface's lie with the methods holding them.
    pub pending: Vec<(Hash, String)>,
    /// In byte order of principal.
    pub seals: Vec<Seal>,
}

impl Entry {
    /// Whether its interface text declares a member named `member`: a field, variant or method.
    pub fn declares(&self, member: &str) -> bool {
        self.text.iter().skip(1).any(|line| {
            let line = line.trim();
            let line = line.strip_prefix("pub ").unwrap_or(line);
            let line = line.strip_prefix("fn ").unwrap_or(line);
            let name = line
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .next()
                .unwrap_or_default();
            name == member
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocKey {
    Own,
    Member(String),
    Invariant(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doc {
    pub key: DocKey,
    pub bound: Hash,
    pub text: String,
}

/// An accepted spec: a function's head, clauses and targeting tests; a struct's field and
/// invariant lines; an interface's methods.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Accepted {
    pub head: Option<String>,
    pub clauses: Vec<String>,
    /// In byte order of canonical path.
    pub tests: Vec<AcceptedTest>,
    /// In byte order of name.
    pub methods: Vec<Method>,
}

impl Accepted {
    pub fn is_empty(&self) -> bool {
        self.clauses.is_empty() && self.tests.is_empty() && self.methods.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedTest {
    pub path: String,
    /// Its parameter types joined by `, `.
    pub params: String,
    /// Its expect hashes in byte order, each as often as it occurs.
    pub expects: Vec<Hash>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Method {
    pub name: String,
    pub head: String,
    pub clauses: Vec<String>,
    pub pending: Vec<(Hash, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seal {
    pub principal: String,
    pub item: Hash,
    pub closure: Hash,
    /// In byte order of path, then kind, then hash.
    pub entries: Vec<ClosureEntry>,
    /// The principal's signature of the seal.
    pub signature: Option<[u8; 64]>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ClosureEntry {
    pub path: String,
    pub body: bool,
    pub hash: Hash,
}

impl ClosureEntry {
    pub fn kind(&self) -> &'static str {
        match self.body {
            true => "body",
            false => "interface",
        }
    }

    fn key(&self) -> (&str, &'static str, [u8; 33]) {
        (&self.path, self.kind(), self.hash.0)
    }
}

/// Closure entries order in byte order of path, then kind, then hash.
impl Ord for ClosureEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key().cmp(&other.key())
    }
}

impl PartialOrd for ClosureEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

fn pending_line((hash, part): &(Hash, String)) -> String {
    format!("pending {} {}", escaped(&hash.full()), escaped(part))
}

impl Entry {
    /// The entry's records, in their order.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![format!("item {}", escaped(&self.path))];
        lines.extend(
            self.text
                .iter()
                .map(|line| format!("text {}", escaped(line))),
        );
        if !self.promises.is_empty() {
            let promises: Vec<String> = self
                .promises
                .iter()
                .map(|(property, params)| match params.is_empty() {
                    true => property.clone(),
                    false => format!("{property}[{}]", params.join(", ")),
                })
                .collect();
            lines.push(format!("promises {}", promises.join(", ")));
        }
        if let Some(interface) = &self.interface {
            lines.push(format!("interface {}", escaped(&interface.full())));
        }
        lines.extend(
            self.digests
                .iter()
                .map(|(target, digest)| format!("digest {target} {}", escaped(digest))),
        );
        for doc in &self.docs {
            let key = match &doc.key {
                DocKey::Own => String::new(),
                DocKey::Member(name) => format!("{name} "),
                DocKey::Invariant(position) => format!("{position} "),
            };
            lines.push(format!(
                "doc {key}{} {}",
                escaped(&doc.bound.full()),
                escaped(&doc.text)
            ));
        }
        let accepted = &self.accepted;
        if let Some(head) = &accepted.head {
            lines.push(format!("head {}", escaped(head)));
        }
        lines.extend(
            accepted
                .clauses
                .iter()
                .map(|clause| format!("accepted {}", escaped(clause))),
        );
        for test in &accepted.tests {
            let mut line = format!("test {} {}", escaped(&test.path), escaped(&test.params));
            for expect in &test.expects {
                line.push(' ');
                line.push_str(&escaped(&expect.full()));
            }
            lines.push(line);
        }
        for method in &accepted.methods {
            lines.push(format!("method {}", method.name));
            lines.push(format!("head {}", escaped(&method.head)));
            lines.extend(
                method
                    .clauses
                    .iter()
                    .map(|clause| format!("accepted {}", escaped(clause))),
            );
            lines.extend(method.pending.iter().map(pending_line));
        }
        lines.extend(self.pending.iter().map(pending_line));
        for seal in &self.seals {
            lines.push(format!(
                "seal {} {} {}",
                escaped(&seal.principal),
                escaped(&seal.item.full()),
                escaped(&seal.closure.full())
            ));
            lines.extend(seal.entries.iter().map(|entry| {
                format!(
                    "closure {} {} {}",
                    entry.kind(),
                    escaped(&entry.path),
                    escaped(&entry.hash.full())
                )
            }));
            if let Some(signature) = &seal.signature {
                lines.push(format!(
                    "signature {}",
                    escaped(&crate::sign::signature_form(signature))
                ));
            }
        }
        lines
    }

    /// Whether its keyed records are in strictly increasing order of their keys.
    fn ordered(&self) -> bool {
        fn increasing<T: PartialOrd>(keys: &[T]) -> bool {
            keys.windows(2).all(|pair| pair[0] < pair[1])
        }
        let parts = |pending: &[(Hash, String)]| {
            increasing(&pending.iter().map(|(_, part)| part).collect::<Vec<_>>())
        };
        increasing(
            &self
                .digests
                .iter()
                .map(|(target, _)| target)
                .collect::<Vec<_>>(),
        ) && increasing(
            &self
                .accepted
                .tests
                .iter()
                .map(|test| &test.path)
                .collect::<Vec<_>>(),
        ) && increasing(
            &self
                .accepted
                .methods
                .iter()
                .map(|method| &method.name)
                .collect::<Vec<_>>(),
        ) && self
            .accepted
            .methods
            .iter()
            .all(|method| parts(&method.pending))
            && parts(&self.pending)
            && increasing(
                &self
                    .seals
                    .iter()
                    .map(|seal| &seal.principal)
                    .collect::<Vec<_>>(),
            )
            && self.seals.iter().all(|seal| increasing(&seal.entries))
            && self
                .accepted
                .tests
                .iter()
                .all(|test| test.expects.windows(2).all(|pair| pair[0].0 <= pair[1].0))
    }
}

impl Snapshot {
    pub fn entry(&self, path: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.path == path)
    }

    pub fn bytes(&self) -> Option<Vec<u8>> {
        let lines: Vec<String> = self.entries.iter().flat_map(Entry::lines).collect();
        write(Kind::Api, &lines)
    }

    pub fn read(bytes: &[u8]) -> Result<Snapshot, Invalid> {
        let parsed = read(Kind::Api, bytes)?;
        let mut snapshot = Snapshot::default();
        let mut records = parsed.records.iter().peekable();
        while let Some(record) = records.next() {
            let mut cursor = Cursor::new(record);
            let path = (cursor.takes("item"))
                .then(|| cursor.text())
                .flatten()
                .filter(|_| cursor.done())
                .ok_or(Invalid::at(record.line))?;
            let mut held = vec![record];
            while let Some(next) = records.next_if(|next| !next.text.starts_with("item ")) {
                held.push(next);
            }
            let entry = read_entry(path, &held[1..]).map_err(Invalid::at)?;
            let lines = entry.lines();
            if let Some(index) = (0..held.len()).find(|&at| lines.get(at) != Some(&held[at].text)) {
                return Err(Invalid::at(held[index].line));
            }
            if lines.len() != held.len() || !entry.ordered() {
                return Err(Invalid::at(record.line));
            }
            if snapshot
                .entries
                .last()
                .is_some_and(|last| last.path >= entry.path)
            {
                return Err(Invalid::at(record.line));
            }
            snapshot.entries.push(entry);
        }
        parsed.checked(snapshot)
    }
}

/// Reads an entry's records after its `item` record, or the line of the first that does not
/// read.
fn read_entry(path: &str, records: &[&Record]) -> Result<Entry, u32> {
    let mut entry = Entry {
        path: path.to_owned(),
        ..Entry::default()
    };
    let mut method: Option<usize> = None;
    for record in records {
        let fail = record.line;
        let mut cursor = Cursor::new(record);
        let word = cursor.word().ok_or(fail)?;
        match word {
            "text" => entry.text.push(cursor.text().ok_or(fail)?.to_owned()),
            "promises" => loop {
                let property = cursor.word().ok_or(fail)?.to_owned();
                let mut params = Vec::new();
                if cursor.punct("[") {
                    loop {
                        params.push(cursor.word().ok_or(fail)?.to_owned());
                        if !cursor.punct(",") {
                            break;
                        }
                    }
                    if !cursor.punct("]") {
                        return Err(fail);
                    }
                }
                entry.promises.push((property, params));
                if !cursor.punct(",") {
                    break;
                }
            },
            "interface" => entry.interface = Some(cursor.hash().ok_or(fail)?),
            "digest" => {
                let target = cursor.target().ok_or(fail)?;
                entry
                    .digests
                    .push((target, cursor.text().ok_or(fail)?.to_owned()));
            }
            "doc" => {
                let key = if let Some(name) = cursor.word() {
                    DocKey::Member(name.to_owned())
                } else if matches!(cursor.peek(), Some(super::Tok::Int(_))) {
                    DocKey::Invariant(cursor.int().ok_or(fail)?)
                } else {
                    DocKey::Own
                };
                let bound = cursor.hash().ok_or(fail)?;
                let text = cursor.text().ok_or(fail)?.to_owned();
                entry.docs.push(Doc { key, bound, text });
            }
            "head" => {
                let head = cursor.text().ok_or(fail)?.to_owned();
                match method {
                    Some(index) => entry.accepted.methods[index].head = head,
                    None => entry.accepted.head = Some(head),
                }
            }
            "accepted" => {
                let clause = cursor.text().ok_or(fail)?.to_owned();
                match method {
                    Some(index) => entry.accepted.methods[index].clauses.push(clause),
                    None => entry.accepted.clauses.push(clause),
                }
            }
            "test" => {
                let path = cursor.text().ok_or(fail)?.to_owned();
                let params = cursor.text().ok_or(fail)?.to_owned();
                let mut expects = Vec::new();
                while !cursor.done() {
                    expects.push(cursor.hash().ok_or(fail)?);
                }
                entry.accepted.tests.push(AcceptedTest {
                    path,
                    params,
                    expects,
                });
            }
            "method" => {
                let name = cursor.word().ok_or(fail)?.to_owned();
                entry.accepted.methods.push(Method {
                    name,
                    head: String::new(),
                    clauses: Vec::new(),
                    pending: Vec::new(),
                });
                method = Some(entry.accepted.methods.len() - 1);
            }
            "pending" => {
                let hash = cursor.hash().ok_or(fail)?;
                let part = cursor.text().ok_or(fail)?.to_owned();
                match method {
                    Some(index) => entry.accepted.methods[index].pending.push((hash, part)),
                    None => entry.pending.push((hash, part)),
                }
            }
            "seal" => {
                let principal = cursor.text().ok_or(fail)?.to_owned();
                let item = cursor.hash().ok_or(fail)?;
                let closure = cursor.hash().ok_or(fail)?;
                entry.seals.push(Seal {
                    principal,
                    item,
                    closure,
                    entries: Vec::new(),
                    signature: None,
                });
                method = None;
            }
            "closure" => {
                let body = match cursor.word().ok_or(fail)? {
                    "body" => true,
                    "interface" => false,
                    _ => return Err(fail),
                };
                let path = cursor.text().ok_or(fail)?.to_owned();
                let hash = cursor.hash().ok_or(fail)?;
                let seal = entry.seals.last_mut().ok_or(fail)?;
                if seal.signature.is_some() {
                    return Err(fail);
                }
                seal.entries.push(ClosureEntry { path, body, hash });
            }
            "signature" => {
                let signature = cursor
                    .text()
                    .and_then(crate::sign::read_signature)
                    .ok_or(fail)?;
                let seal = entry.seals.last_mut().ok_or(fail)?;
                if seal.signature.replace(signature).is_some() {
                    return Err(fail);
                }
            }
            _ => return Err(fail),
        }
        if !cursor.done() {
            return Err(fail);
        }
    }
    Ok(entry)
}
