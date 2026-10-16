//! Artefacts: the code root of a state of a package, whose hash identifies its release, followed
//! by its attestation file and the reserved sections.

use crate::diagnostic::Code;
use crate::encoding::{Record, bytes, list, uvar};
use crate::hash::Hash;

const MAGIC: [u8; 4] = [0x4E, 0x41, 0x52, 0x54];
/// The artefact format version of language 1.
const FORMAT: u128 = 1;

/// The parts of a code root, each list already in its order.
pub struct Parts {
    pub language: u64,
    /// The text of `package.nan` in canonical format.
    pub manifest: String,
    /// Each module's name with the text of its API snapshot, empty when it has none.
    pub interface: Vec<(String, String)>,
    pub nir: Vec<u8>,
    /// The encoded entries of the requires-set.
    pub requires: Vec<Vec<u8>>,
    /// The normalised forms of the tombstones in source.
    pub source_tombstones: Vec<String>,
    /// The normalised forms of the tombstones of the history.
    pub history_tombstones: Vec<String>,
    /// The encoded ledger records.
    pub ledger: Vec<Vec<u8>>,
    /// Each file holding the first line of a span, by path as spans write it, with each such
    /// line's number and text, in line order.
    pub lines: Vec<(String, Vec<(u32, String)>)>,
    /// Each component state's canonical path and schema hash.
    pub schemas: Vec<(String, Hash)>,
}

/// A list of elements sorted by the bytes of their encodings.
fn sorted_list(elements: Vec<Vec<u8>>) -> Vec<u8> {
    let mut elements = elements;
    elements.sort();
    let mut out = Vec::new();
    list(&mut out, elements.into_iter(), |out, element| {
        out.extend(element)
    });
    out
}

fn text(value: &str) -> Vec<u8> {
    let mut out = Vec::new();
    bytes(&mut out, value.as_bytes());
    out
}

/// The bytes of a code root.
pub fn code_root(parts: &Parts) -> Vec<u8> {
    let mut record = Record::default();
    let mut language = Vec::new();
    uvar(&mut language, u128::from(parts.language));
    record.field(1, language);
    record.field(2, text(&parts.manifest));
    record.field(
        3,
        sorted_list(
            parts
                .interface
                .iter()
                .map(|(module, snapshot)| [text(module), text(snapshot)].concat())
                .collect(),
        ),
    );
    let mut nir = Vec::new();
    bytes(&mut nir, &parts.nir);
    record.field(4, nir);
    record.field(5, sorted_list(parts.requires.clone()));
    record.field(
        6,
        sorted_list(
            parts
                .source_tombstones
                .iter()
                .map(|form| text(form))
                .collect(),
        ),
    );
    record.field(
        7,
        sorted_list(
            parts
                .history_tombstones
                .iter()
                .map(|form| text(form))
                .collect(),
        ),
    );
    record.field(8, sorted_list(parts.ledger.clone()));
    record.field(
        9,
        sorted_list(
            parts
                .lines
                .iter()
                .map(|(path, lines)| {
                    let mut out = text(path);
                    list(&mut out, lines.iter(), |out, (line, held)| {
                        uvar(out, u128::from(*line));
                        out.extend(text(held));
                    });
                    out
                })
                .collect(),
        ),
    );
    record.field(
        10,
        sorted_list(
            parts
                .schemas
                .iter()
                .map(|(path, hash)| {
                    let mut out = text(path);
                    bytes(&mut out, &hash.0);
                    out
                })
                .collect(),
        ),
    );
    record.finish()
}

/// A seal an attestation file records: principal, item, sealed item hash, closure hash and closure
/// entries, each entry a body flag, a canonical path and a hash.
pub struct AttestedSeal {
    pub principal: String,
    pub item: String,
    pub sealed: Hash,
    pub closure: Hash,
    pub entries: Vec<(bool, String, Hash)>,
}

/// The source tree hash of a package's files: the hash of the encoding of a list of pairs of
/// each file's path within the package and its content.
pub fn source_tree_hash(files: &[(String, Vec<u8>)]) -> Hash {
    let elements = files
        .iter()
        .map(|(path, content)| {
            let mut out = text(path);
            bytes(&mut out, content);
            out
        })
        .collect();
    Hash::of(&sorted_list(elements))
}

/// The bytes of an attestation file, `seals` being its seals as `seal_list` encodes them.
pub fn attestation(toolchain: Hash, source_tree: Hash, seals: &[u8]) -> Vec<u8> {
    let mut record = Record::default();
    record.field(1, hash_bytes(&toolchain));
    record.field(2, hash_bytes(&source_tree));
    record.field(3, seals.to_vec());
    record.finish()
}

fn hash_bytes(hash: &Hash) -> Vec<u8> {
    let mut out = Vec::new();
    bytes(&mut out, &hash.0);
    out
}

/// The encoding of the seals an attestation file records.
pub fn seal_list(seals: &[AttestedSeal]) -> Vec<u8> {
    let hash = hash_bytes;
    let seals = seals
        .iter()
        .map(|seal| {
            let mut fields = Record::default();
            fields.field(1, text(&seal.principal));
            fields.field(2, text(&seal.item));
            fields.field(3, hash(&seal.sealed));
            fields.field(4, hash(&seal.closure));
            fields.field(
                5,
                sorted_list(
                    seal.entries
                        .iter()
                        .map(|(body, path, entry)| {
                            let mut entry_record = Record::default();
                            let mut kind = Vec::new();
                            uvar(&mut kind, u128::from(*body));
                            entry_record.field(1, kind);
                            entry_record.field(2, text(path));
                            entry_record.field(3, hash(entry));
                            entry_record.finish()
                        })
                        .collect(),
                ),
            );
            fields.finish()
        })
        .collect();
    sorted_list(seals)
}

/// An artefact's bytes: the magic, the format version, the code root, the attestation file and
/// an empty reserved sections record.
pub fn artefact(code_root: &[u8], attestation: &[u8]) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    uvar(&mut out, FORMAT);
    bytes(&mut out, code_root);
    bytes(&mut out, attestation);
    out.extend(Record::default().finish());
    out
}

/// What an artefact decodes to: its code root.
pub struct Decoded {
    pub code_root: Vec<u8>,
}

impl Decoded {
    pub fn root_hash(&self) -> Hash {
        Hash::of(&self.code_root)
    }
}

/// Decodes an artefact, or the code of why it does not decode.
pub fn decode(artefact: &[u8]) -> Result<Decoded, Code> {
    let malformed = Code::ArtefactMalformed;
    let rest = artefact.strip_prefix(&MAGIC[..]).ok_or(malformed)?;
    let (format, rest) = read_uvar(rest).ok_or(malformed)?;
    if format != FORMAT {
        return Err(Code::ArtefactVersionUnsupported);
    }
    let (code_root, rest) = read_bytes(rest).ok_or(malformed)?;
    let (_, rest) = read_bytes(rest).ok_or(malformed)?;
    let (fields, rest) = read_uvar(rest).ok_or(malformed)?;
    if fields != 0 {
        return Err(Code::ArtefactReservedSection);
    }
    if !rest.is_empty() {
        return Err(malformed);
    }
    Ok(Decoded {
        code_root: code_root.to_vec(),
    })
}

/// A uvar without redundant trailing groups, and the bytes after it.
pub fn read_uvar(bytes: &[u8]) -> Option<(u128, &[u8])> {
    let mut value = 0u128;
    for (at, byte) in bytes.iter().enumerate() {
        if at >= 19 {
            return None;
        }
        value |= u128::from(byte & 0x7F) << (7 * at);
        if byte & 0x80 == 0 {
            let minimal = at == 0 || *byte != 0;
            return minimal.then_some((value, &bytes[at + 1..]));
        }
    }
    None
}

/// Bytes: a uvar length and the bytes, and the bytes after them.
pub fn read_bytes(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let (length, rest) = read_uvar(bytes)?;
    let length = usize::try_from(length).ok()?;
    (rest.len() >= length).then(|| rest.split_at(length))
}

#[cfg(test)]
mod tests;
