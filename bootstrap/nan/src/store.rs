//! The local store: the user's releases in `~/.nan/store`, one file per entry named by its
//! publication index, each written whole under a new name and renamed into place under an
//! exclusive lock on `store/write`, and never removed or rewritten. An entry holds its release's
//! artefact with the source tree its attestation hashes.

use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;

use crate::artefact::{self, read_bytes, read_uvar};
use crate::cache::{Exposed, StoreDir};
use crate::encoding::{Record, bytes, list, uvar};
use crate::hash::{Hash, blake3};

/// A release class, in the order identical, additive, breaking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    Identical,
    Additive,
    Breaking,
}

impl Class {
    pub fn word(self) -> &'static str {
        match self {
            Class::Identical => "identical",
            Class::Additive => "additive",
            Class::Breaking => "breaking",
        }
    }

    fn from_position(position: u128) -> Option<Class> {
        [Class::Identical, Class::Additive, Class::Breaking]
            .get(usize::try_from(position).ok()?)
            .copied()
    }
}

/// The publish summary of a named release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub class: Class,
    pub encode: Class,
    /// Each exported item's canonical path, class and behaviour epoch, by path.
    pub items: Vec<(String, Class, u64)>,
    /// Each epoch bump's item and reason.
    pub bumps: Vec<(String, String)>,
    /// The entries of the release's authority summary as a lock writes them, from which those
    /// gained and lost against another release follow.
    pub authority: Vec<String>,
    /// For each exported function and target, the capability methods it reaches and its trap
    /// sites, each a trap kind, file path and line.
    pub functions: Vec<FunctionSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionSummary {
    pub path: String,
    pub target: String,
    pub methods: Vec<String>,
    pub traps: Vec<(String, String, u32)>,
}

impl Summary {
    /// The behaviour epoch of an exported item, 0 for one it does not list.
    pub fn epoch(&self, path: &str) -> u64 {
        self.items
            .iter()
            .find(|(item, _, _)| item == path)
            .map_or(0, |(_, _, epoch)| *epoch)
    }
}

/// An entry of the local store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub index: u64,
    pub package: String,
    /// The store name of a named release; none for a path release.
    pub name: Option<String>,
    pub parent: Option<Hash>,
    pub summary: Option<Summary>,
    pub artefact: Vec<u8>,
    /// The package's source and machine files the artefact's attestation hashes, by path within
    /// the package.
    pub sources: Vec<(String, Vec<u8>)>,
}

impl Entry {
    /// The code root hash of its release.
    pub fn root(&self) -> Hash {
        artefact::decode(&self.artefact)
            .map(|decoded| decoded.root_hash())
            .unwrap_or(Hash([0; 33]))
    }

    fn encoded(&self) -> Vec<u8> {
        let mut record = Record::default();
        let text = |value: &str| {
            let mut out = Vec::new();
            bytes(&mut out, value.as_bytes());
            out
        };
        let mut index = Vec::new();
        uvar(&mut index, u128::from(self.index));
        record.field(1, index);
        record.field(2, text(&self.package));
        if let Some(name) = &self.name {
            record.field(3, text(name));
        }
        if let Some(parent) = &self.parent {
            let mut out = Vec::new();
            bytes(&mut out, &parent.0);
            record.field(4, out);
        }
        if let Some(summary) = &self.summary {
            record.field(5, summary.encoded());
        }
        let mut out = Vec::new();
        bytes(&mut out, &self.artefact);
        record.field(6, out);
        let mut sources = Vec::new();
        list(&mut sources, self.sources.iter(), |out, (path, content)| {
            bytes(out, path.as_bytes());
            bytes(out, content);
        });
        record.field(7, sources);
        record.finish()
    }

    fn decoded(encoded: &[u8]) -> Option<Entry> {
        let fields = Fields::read(encoded)?;
        let text = |field: Option<&[u8]>| -> Option<String> {
            let (value, rest) = read_bytes(field?)?;
            rest.is_empty()
                .then(|| String::from_utf8(value.to_vec()).ok())?
        };
        let hash = |field: Option<&[u8]>| -> Option<Hash> {
            let (value, _) = read_bytes(field?)?;
            Some(Hash(value.try_into().ok()?))
        };
        let (index, _) = read_uvar(fields.get(1)?)?;
        let mut sources = Vec::new();
        let (count, mut rest) = read_uvar(fields.get(7)?)?;
        for _ in 0..count {
            let (path, after) = read_bytes(rest)?;
            let (content, after) = read_bytes(after)?;
            sources.push((String::from_utf8(path.to_vec()).ok()?, content.to_vec()));
            rest = after;
        }
        Some(Entry {
            index: u64::try_from(index).ok()?,
            package: text(fields.get(2))?,
            name: fields.get(3).and_then(|field| text(Some(field))),
            parent: fields.get(4).and_then(|field| hash(Some(field))),
            summary: fields.get(5).and_then(Summary::decoded),
            artefact: read_bytes(fields.get(6)?)?.0.to_vec(),
            sources,
        })
    }
}

/// The fields of an encoded record, by key.
struct Fields<'a>(Vec<(u128, &'a [u8])>);

impl<'a> Fields<'a> {
    fn read(encoded: &'a [u8]) -> Option<Fields<'a>> {
        let (count, mut rest) = read_uvar(encoded)?;
        let mut fields = Vec::new();
        for _ in 0..count {
            let (key, after) = read_uvar(rest)?;
            let (content, after) = read_bytes(after)?;
            fields.push((key, content));
            rest = after;
        }
        Some(Fields(fields))
    }

    fn get(&self, key: u128) -> Option<&'a [u8]> {
        self.0
            .iter()
            .find(|(known, _)| *known == key)
            .map(|(_, content)| *content)
    }
}

impl Summary {
    fn encoded(&self) -> Vec<u8> {
        let mut record = Record::default();
        let text = |out: &mut Vec<u8>, value: &str| bytes(out, value.as_bytes());
        let class = |class: Class| {
            let mut out = Vec::new();
            uvar(&mut out, class as u128);
            out
        };
        record.field(1, class(self.class));
        record.field(2, class(self.encode));
        let mut items = Vec::new();
        list(
            &mut items,
            self.items.iter(),
            |out, (path, class, epoch)| {
                text(out, path);
                uvar(out, *class as u128);
                uvar(out, u128::from(*epoch));
            },
        );
        record.field(3, items);
        let mut bumps = Vec::new();
        list(&mut bumps, self.bumps.iter(), |out, (path, reason)| {
            text(out, path);
            text(out, reason);
        });
        record.field(4, bumps);
        let mut authority = Vec::new();
        list(&mut authority, self.authority.iter(), |out, entry| {
            text(out, entry)
        });
        record.field(5, authority);
        let mut functions = Vec::new();
        list(&mut functions, self.functions.iter(), |out, function| {
            text(out, &function.path);
            text(out, &function.target);
            list(out, function.methods.iter(), |out, method| {
                text(out, method)
            });
            list(out, function.traps.iter(), |out, (kind, path, line)| {
                text(out, kind);
                text(out, path);
                uvar(out, u128::from(*line));
            });
        });
        record.field(6, functions);
        let mut out = Vec::new();
        bytes(&mut out, &record.finish());
        out
    }

    fn decoded(field: &[u8]) -> Option<Summary> {
        let (encoded, _) = read_bytes(field)?;
        let fields = Fields::read(encoded)?;
        let class = |key: u128| Class::from_position(read_uvar(fields.get(key)?)?.0);
        let mut items = Vec::new();
        let (count, mut rest) = read_uvar(fields.get(3)?)?;
        for _ in 0..count {
            let (path, after) = read_text(rest)?;
            let (position, after) = read_uvar(after)?;
            let (epoch, after) = read_uvar(after)?;
            items.push((
                path,
                Class::from_position(position)?,
                u64::try_from(epoch).ok()?,
            ));
            rest = after;
        }
        let mut bumps = Vec::new();
        let (count, mut rest) = read_uvar(fields.get(4)?)?;
        for _ in 0..count {
            let (path, after) = read_text(rest)?;
            let (reason, after) = read_text(after)?;
            bumps.push((path, reason));
            rest = after;
        }
        let texts = |key: u128| -> Option<Vec<String>> {
            let (count, mut rest) = read_uvar(fields.get(key)?)?;
            let mut found = Vec::new();
            for _ in 0..count {
                let (value, after) = read_text(rest)?;
                found.push(value);
                rest = after;
            }
            Some(found)
        };
        let mut functions = Vec::new();
        let (count, mut rest) = read_uvar(fields.get(6)?)?;
        for _ in 0..count {
            let (path, after) = read_text(rest)?;
            let (target, after) = read_text(after)?;
            let (methods_count, mut after) = read_uvar(after)?;
            let mut methods = Vec::new();
            for _ in 0..methods_count {
                let (method, next) = read_text(after)?;
                methods.push(method);
                after = next;
            }
            let (traps_count, mut after) = read_uvar(after)?;
            let mut traps = Vec::new();
            for _ in 0..traps_count {
                let (kind, next) = read_text(after)?;
                let (path, next) = read_text(next)?;
                let (line, next) = read_uvar(next)?;
                traps.push((kind, path, u32::try_from(line).ok()?));
                after = next;
            }
            functions.push(FunctionSummary {
                path,
                target,
                methods,
                traps,
            });
            rest = after;
        }
        Some(Summary {
            class: class(1)?,
            encode: class(2)?,
            items,
            bumps,
            authority: texts(5)?,
            functions,
        })
    }
}

/// A text, and the bytes after it.
fn read_text(rest: &[u8]) -> Option<(String, &[u8])> {
    let (value, rest) = read_bytes(rest)?;
    Some((String::from_utf8(value.to_vec()).ok()?, rest))
}

/// The local store, read from the user directory.
pub struct Store {
    dir: StoreDir,
}

/// An entry to append: everything but its publication index.
pub struct Append {
    pub package: String,
    pub name: Option<String>,
    pub parent: Option<Hash>,
    pub summary: Option<Summary>,
    pub artefact: Vec<u8>,
    pub sources: Vec<(String, Vec<u8>)>,
}

impl Store {
    pub fn new(dir: StoreDir) -> Store {
        Store { dir }
    }

    /// The publication index and code root hash of each entry, in index order, from the names of
    /// the store's files; or the part of the store that is exposed.
    pub fn index(&self) -> io::Result<Result<Vec<(u64, Hash)>, Exposed>> {
        let Some(dir) = &self.dir.dir else {
            return Ok(Ok(Vec::new()));
        };
        if let Some(exposed) = self.dir.exposed(dir)? {
            return Ok(Err(exposed));
        }
        let names = match fs::read_dir(dir) {
            Ok(names) => names,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Ok(Vec::new())),
            Err(error) => return Err(error),
        };
        let mut found: Vec<(u64, Hash)> = Vec::new();
        for name in names {
            let name = name?.file_name().to_string_lossy().into_owned();
            let held = name
                .strip_suffix(".entry")
                .and_then(|stem| stem.split_once('-'))
                .and_then(|(index, root)| {
                    Some((index.parse().ok()?, crate::machine::hash_of(root)?))
                });
            found.extend(held);
        }
        found.sort();
        Ok(Ok(found))
    }

    /// The entry of a publication index, none when its file does not decode or its checksum
    /// does not match.
    pub fn read(&self, index: u64, root: &Hash) -> io::Result<Result<Option<Entry>, Exposed>> {
        let Some(dir) = &self.dir.dir else {
            return Ok(Ok(None));
        };
        let path = dir.join(entry_name(index, root));
        if let Some(exposed) = self.dir.exposed(&path)? {
            return Ok(Err(exposed));
        }
        let file = fs::read(&path)?;
        let entry = file
            .split_last_chunk::<32>()
            .filter(|(contents, digest)| blake3(contents) == **digest)
            .and_then(|(contents, _)| Entry::decoded(contents))
            .filter(|entry| entry.index == index && entry.root() == *root);
        Ok(Ok(entry))
    }

    /// The store's entries in order of publication index, those that do not read left out.
    pub fn entries(&self) -> io::Result<Result<Vec<Entry>, Exposed>> {
        let index = match self.index()? {
            Ok(index) => index,
            Err(exposed) => return Ok(Err(exposed)),
        };
        let mut entries = Vec::new();
        for (number, root) in index {
            match self.read(number, &root)? {
                Ok(entry) => entries.extend(entry),
                Err(exposed) => return Ok(Err(exposed)),
            }
        }
        Ok(Ok(entries))
    }

    /// Appends an entry under the next publication index, unless the store holds its release:
    /// under its store name for a named release, at all for a path release. Returns the entry
    /// holding the release.
    pub fn append(&self, append: Append) -> io::Result<Result<Entry, Exposed>> {
        let Some(dir) = &self.dir.dir else {
            return Err(io::Error::other(
                "the user database gives no home directory",
            ));
        };
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(dir.join("write"))?;
        let locked: File = lock;
        locked.lock()?;

        let entries = match self.entries()? {
            Ok(entries) => entries,
            Err(exposed) => return Ok(Err(exposed)),
        };
        let root = artefact::decode(&append.artefact)
            .map(|decoded| decoded.root_hash())
            .map_err(|_| io::Error::other("an artefact the toolchain wrote decodes"))?;
        let held = entries.iter().find(|entry| {
            entry.root() == root && (append.name.is_none() || entry.name == append.name)
        });
        if let Some(held) = held {
            return Ok(Ok(held.clone()));
        }
        let entry = Entry {
            index: entries.last().map_or(1, |entry| entry.index + 1),
            package: append.package,
            name: append.name,
            parent: append.parent,
            summary: append.summary,
            artefact: append.artefact,
            sources: append.sources,
        };
        let contents = entry.encoded();
        let fresh = dir.join(format!(".{}.{}", entry.index, std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&fresh)?;
        file.write_all(&contents)?;
        file.write_all(&blake3(&contents))?;
        file.sync_all()?;
        fs::rename(&fresh, dir.join(entry_name(entry.index, &root)))?;
        Ok(Ok(entry))
    }
}

/// The name of an entry's file: its publication index and its code root hash in full form.
fn entry_name(index: u64, root: &Hash) -> String {
    format!("{index}-{}.entry", root.full())
}

#[cfg(test)]
mod tests;
