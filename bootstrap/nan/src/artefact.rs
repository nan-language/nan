//! Artefacts: the code root of a state of a package, whose hash identifies its release, followed
//! by its attestation file and its sections: none, or the publisher's signature, the builder's
//! certificate and the provenance.

use crate::diagnostic::Code;
use crate::encoding::{Record, bytes, list, uvar};
use crate::hash::Hash;
use crate::machine::principals::Principals;
use crate::sign;

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
    /// The canonical path of each type whose schema tree the code root records, with the
    /// tree's `mem::wire` encoding.
    pub schemas: Vec<(String, Vec<u8>)>,
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
                .map(|(path, tree)| {
                    let mut out = text(path);
                    bytes(&mut out, tree);
                    out
                })
                .collect(),
        ),
    );
    record.finish()
}

/// A seal an attestation file records: principal, item, sealed item hash, closure hash and closure
/// entries, each entry a body flag, a canonical path and a hash; for a signed seal of a principal
/// the package's principals file gives a key, its signature and that key.
pub struct AttestedSeal {
    pub principal: String,
    pub item: String,
    pub sealed: Hash,
    pub closure: Hash,
    pub entries: Vec<(bool, String, Hash)>,
    pub signed: Option<([u8; 64], [u8; 32])>,
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
    held(&hash.0)
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
            if let Some((signature, key)) = &seal.signed {
                fields.field(6, held(signature));
                fields.field(7, held(key));
            }
            fields.finish()
        })
        .collect();
    sorted_list(seals)
}

fn held(value: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    bytes(&mut out, value);
    out
}

/// An artefact's bytes: the magic, the format version, the code root, the attestation file and
/// an empty sections record.
pub fn artefact(code_root: &[u8], attestation: &[u8]) -> Vec<u8> {
    assembled(code_root, attestation, Record::default())
}

fn assembled(code_root: &[u8], attestation: &[u8], sections: Record) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    uvar(&mut out, FORMAT);
    bytes(&mut out, code_root);
    bytes(&mut out, attestation);
    out.extend(sections.finish());
    out
}

/// What `nan publish --as` signs a release with: the principal publishing and building it, the
/// seed of its key, the toolchain hash, the package and code root of each package of the checked
/// closure, the author principals and the package's lock.
pub struct Publication<'a> {
    pub principal: &'a str,
    pub seed: &'a [u8; 32],
    pub toolchain: Hash,
    pub closure: Vec<(String, Hash)>,
    pub authors: Vec<String>,
    pub lock: Vec<u8>,
}

/// An artefact without sections given the signature, certificate and provenance sections of a
/// publication.
pub fn published(artefact: &[u8], publication: &Publication) -> Vec<u8> {
    let decoded = decode(artefact).expect("an artefact the toolchain wrote decodes");
    let key = sign::public_key(publication.seed);
    let mut provenance = Record::default();
    provenance.field(
        1,
        sorted_list(
            publication
                .authors
                .iter()
                .map(|author| text(author))
                .collect(),
        ),
    );
    provenance.field(2, held(&publication.lock));
    let provenance = provenance.finish();
    let mut certified = Record::default();
    certified.field(1, text(publication.principal));
    certified.field(2, held(&key));
    certified.field(3, hash_bytes(&publication.toolchain));
    certified.field(
        4,
        sorted_list(
            publication
                .closure
                .iter()
                .map(|(package, root)| [text(package), hash_bytes(root)].concat())
                .collect(),
        ),
    );
    let mut certificate = certified.clone();
    let attested = certificate_message(certified);
    certificate.field(5, held(&sign::sign(publication.seed, &attested)));
    let message = artefact_message(
        publication.principal,
        &key,
        [&decoded.code_root, &decoded.attestation, &provenance],
    );
    let mut signature = Record::default();
    signature.field(1, text(publication.principal));
    signature.field(2, held(&key));
    signature.field(3, held(&sign::sign(publication.seed, &message)));
    let mut sections = Record::default();
    sections.field(1, signature.finish());
    sections.field(4, certificate.finish());
    sections.field(5, provenance);
    assembled(&decoded.code_root, &decoded.attestation, sections)
}

/// The bytes the publisher signs: `nan artefact 1`, its principal, the key form of its public key,
/// then the full forms of the hashes of the code root, the attestation file and the provenance
/// section, each followed by LF.
fn artefact_message(principal: &str, key: &[u8; 32], parts: [&[u8]; 3]) -> Vec<u8> {
    let mut message =
        format!("nan artefact 1\n{principal}\n{}\n", sign::key_form(key)).into_bytes();
    for part in parts {
        message.extend(Hash::of(part).full().as_bytes());
        message.push(b'\n');
    }
    message
}

/// The bytes the builder signs: `nan certificate 1` and LF, then the encoding of the record of
/// the certificate's fields 1 to 4.
fn certificate_message(fields: Record) -> Vec<u8> {
    [&b"nan certificate 1\n"[..], &fields.finish()].concat()
}

/// What an artefact decodes to: its code root, its attestation file and its sections, when it
/// holds them.
pub struct Decoded {
    pub code_root: Vec<u8>,
    pub attestation: Vec<u8>,
    sections: Option<Sections>,
}

/// The fields of a record, each a key and its content, keys strictly increasing.
type Fields = Vec<(u64, Vec<u8>)>;

/// The sections of an artefact: the fields of the signature and certificate sections, and the
/// provenance section's encoding.
struct Sections {
    signature: Fields,
    certificate: Fields,
    provenance: Vec<u8>,
}

impl Decoded {
    pub fn root_hash(&self) -> Hash {
        Hash::of(&self.code_root)
    }

    /// Whether the artefact is accepted as the release a lock line pins, whose publisher key is
    /// `pinned`, by a package trusting `principals`: every signature it holds verifies, it is
    /// signed by the pinned key or unsigned when none is pinned, its publisher is given its key
    /// by `principals`, and each signed seal's principal is given its key or none.
    pub fn accepted(&self, pinned: Option<&[u8; 32]>, principals: &Principals) -> bool {
        let Some((seals, publisher)) = self.signers() else {
            return false;
        };
        let trusted = publisher
            .iter()
            .all(|signer| principals.key(&signer.principal) == Some(&signer.key));
        let anchored = seals.iter().all(|signer| {
            principals
                .key(&signer.principal)
                .is_none_or(|key| *key == signer.key)
        });
        publisher.as_ref().map(|signer| &signer.key) == pinned && trusted && anchored
    }

    /// The public key of the artefact's publisher, when it holds sections.
    pub fn publisher_key(&self) -> Option<[u8; 32]> {
        Signer::of(&self.sections.as_ref()?.signature, 3).map(|signer| signer.key)
    }

    /// The signers of the artefact's signed seals and its publisher, when every signature it
    /// holds verifies: each signed seal's over its seal and, when it holds sections, the
    /// publisher's over its code root, attestation file and provenance, and the builder's, the
    /// same principal with the same key, over its certificate, which lists the artefact's own
    /// code root.
    fn signers(&self) -> Option<(Vec<Signer>, Option<Signer>)> {
        let seals: Vec<Signer> = attested_seals(&self.attestation)?
            .into_iter()
            .map(|(message, signer)| signer.verifies(&message).then_some(signer))
            .collect::<Option<_>>()?;
        let Some(sections) = &self.sections else {
            return Some((seals, None));
        };
        let publisher = Signer::of(&sections.signature, 3)?;
        let builder = Signer::of(&sections.certificate, 5)?;
        let message = artefact_message(
            &publisher.principal,
            &publisher.key,
            [&self.code_root, &self.attestation, &sections.provenance],
        );
        let mut certified = Record::default();
        for (key, content) in sections.certificate.iter().filter(|(key, _)| *key < 5) {
            certified.field(*key, content.clone());
        }
        let names_root = field(&sections.certificate, 4)
            .and_then(read_pairs)
            .is_some_and(|pairs| pairs.iter().any(|(_, root)| *root == self.root_hash()));
        let accepted = names_root
            && builder.principal == publisher.principal
            && builder.key == publisher.key
            && publisher.verifies(&message)
            && builder.verifies(&certificate_message(certified));
        accepted.then_some((seals, Some(publisher)))
    }
}

/// A principal, its public key and a signature by it.
struct Signer {
    principal: String,
    key: [u8; 32],
    signature: [u8; 64],
}

impl Signer {
    /// The principal in field 1, the key in field 2 and the signature in field `signature` of a
    /// section.
    fn of(fields: &[(u64, Vec<u8>)], signature: u64) -> Option<Signer> {
        Some(Signer {
            principal: held_text(field(fields, 1)?)?,
            key: held_bytes(field(fields, 2)?)?,
            signature: held_bytes(field(fields, signature)?)?,
        })
    }

    fn verifies(&self, message: &[u8]) -> bool {
        sign::verify(&self.key, message, &self.signature)
    }
}

fn field(fields: &[(u64, Vec<u8>)], key: u64) -> Option<&[u8]> {
    fields
        .iter()
        .find(|(held, _)| *held == key)
        .map(|(_, content)| content.as_slice())
}

/// The bytes of a field holding exactly `N` bytes.
fn held_bytes<const N: usize>(content: &[u8]) -> Option<[u8; N]> {
    let (value, rest) = read_bytes(content)?;
    rest.is_empty().then(|| value.try_into().ok()).flatten()
}

fn held_text(content: &[u8]) -> Option<String> {
    let (value, rest) = read_bytes(content)?;
    rest.is_empty()
        .then(|| String::from_utf8(value.to_vec()).ok())
        .flatten()
}

/// The pairs of a text and a hash a list holds.
fn read_pairs(content: &[u8]) -> Option<Vec<(String, Hash)>> {
    let (count, mut rest) = read_uvar(content)?;
    let mut pairs = Vec::new();
    for _ in 0..count {
        let (name, after) = read_bytes(rest)?;
        let (hash, after) = read_bytes(after)?;
        pairs.push((
            String::from_utf8(name.to_vec()).ok()?,
            Hash(hash.try_into().ok()?),
        ));
        rest = after;
    }
    rest.is_empty().then_some(pairs)
}

/// The message and signer of each signed seal an attestation file records; none when one holds
/// only one of the signature and the key.
fn attested_seals(attestation: &[u8]) -> Option<Vec<(Vec<u8>, Signer)>> {
    let fields = owned_record(attestation)?;
    let (count, mut rest) = read_uvar(field(&fields, 3)?)?;
    let mut signed = Vec::new();
    for _ in 0..count {
        let (seal, after) = read_record(rest)?;
        rest = after;
        match (field(&seal, 6), field(&seal, 7)) {
            (None, None) => {}
            (Some(signature), Some(key)) => {
                let principal = held_text(field(&seal, 1)?)?;
                let message = sign::seal_message(
                    &principal,
                    &held_text(field(&seal, 2)?)?,
                    Hash(held_bytes(field(&seal, 3)?)?),
                    Hash(held_bytes(field(&seal, 4)?)?),
                );
                let signer = Signer {
                    principal,
                    key: held_bytes(key)?,
                    signature: held_bytes(signature)?,
                };
                signed.push((message, signer));
            }
            _ => return None,
        }
    }
    rest.is_empty().then_some(signed)
}

/// The fields of the record `encoded` holds whole, each a key and its content.
fn owned_record(encoded: &[u8]) -> Option<Fields> {
    let (fields, rest) = read_record(encoded)?;
    rest.is_empty().then_some(fields)
}

/// The fields of a record and the bytes after it.
fn read_record(encoded: &[u8]) -> Option<(Fields, &[u8])> {
    let (count, mut rest) = read_uvar(encoded)?;
    let mut fields = Fields::new();
    for _ in 0..count {
        let (key, after) = read_uvar(rest)?;
        let (content, after) = read_bytes(after)?;
        let key = u64::try_from(key).ok()?;
        if fields.last().is_some_and(|(last, _)| *last >= key) {
            return None;
        }
        fields.push((key, content.to_vec()));
        rest = after;
    }
    Some((fields, rest))
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
    let (attestation, rest) = read_bytes(rest).ok_or(malformed)?;
    let fields = owned_record(rest).ok_or(malformed)?;
    if fields.iter().any(|(key, _)| matches!(key, 2 | 3)) {
        return Err(Code::ArtefactReservedSection);
    }
    let sections = match &fields[..] {
        [] => None,
        [(1, signature), (4, certificate), (5, provenance)] => Some(Sections {
            signature: owned_record(signature).ok_or(malformed)?,
            certificate: owned_record(certificate).ok_or(malformed)?,
            provenance: provenance.clone(),
        }),
        _ => return Err(malformed),
    };
    Ok(Decoded {
        code_root: code_root.to_vec(),
        attestation: attestation.to_vec(),
        sections,
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
