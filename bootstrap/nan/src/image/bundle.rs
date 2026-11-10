//! The bundle writer: links the object of a bundle's lowering from address zero and writes the
//! bundle, each import bound to the index of the export its kernel's interface names; and the
//! image of a kernel's user-mode runtime, a bundle whose job is the runtime's start, recording
//! where it defines each export.

use super::{Access, Linked, link};
use crate::encoding::{Record, bytes, list, uvar};
use crate::lower::load::{Contributed, Interface};

/// The magic and format version of a bundle.
const MAGIC: &[u8; 4] = b"NBUN";
const VERSION: u128 = 1;

/// The bundle of a lowered object, with its job entry and the stack its task needs for a nanos
/// program's, or each component entry by name with the stack its tasks need, signed by the key
/// of a secret seed when one is given, or the names of the symbols it imports that the interface
/// does not export.
pub fn write(
    object: &[u8],
    contributed: &[Contributed],
    job: Option<u64>,
    components: &[(String, u64)],
    interface: &Interface,
    seed: Option<&[u8; 32]>,
) -> Result<Vec<u8>, Vec<String>> {
    let linked = link(object, 0, true);
    let index = |name: &str| interface.exports.iter().position(|export| export == name);
    let mut missing: Vec<String> = linked
        .imports
        .iter()
        .map(|(_, name)| name)
        .chain(contributed.iter().map(|entry| &entry.record))
        .filter(|name| index(name).is_none())
        .cloned()
        .collect();
    missing.sort();
    missing.dedup();
    if !missing.is_empty() {
        return Err(missing);
    }
    let mut out = MAGIC.to_vec();
    uvar(&mut out, VERSION);
    let parts = Parts {
        job: job.map(|stack| (crate::lower::JOB, stack)),
        components,
        runtime: None,
        lines: crate::lower::load::BUNDLE_LINES,
    };
    bytes(
        &mut out,
        &body(&linked, contributed, &parts, interface, &index),
    );
    let mut sections = Record::default();
    if let Some(seed) = seed {
        let mut signature = Record::default();
        signature.field(1, crate::sign::public_key(seed).to_vec());
        signature.field(2, crate::sign::sign(seed, &out).to_vec());
        sections.field(1, signature.finish());
    }
    out.extend(sections.finish());
    Ok(out)
}

/// The image of a kernel's user-mode runtime, unsigned, from the object of its lowering: a bundle
/// whose job is its start routine, with the stack that routine runs on, which records, for each
/// export of the interface, its offset plus one, or zero where the runtime defines none, and the
/// bytes of the boot area it runs over.
pub fn runtime(
    object: &[u8],
    interface: &Interface,
    start: &str,
    stack: u64,
    boot: u64,
) -> Vec<u8> {
    let linked = link(object, 0, false);
    let exports: Vec<u64> = interface
        .exports
        .iter()
        .map(|name| linked.symbols.get(name).map_or(0, |at| at + 1))
        .collect();
    let mut out = MAGIC.to_vec();
    uvar(&mut out, VERSION);
    let none = |_: &str| None;
    let parts = Parts {
        job: Some((start, stack)),
        components: &[],
        runtime: Some((&exports, boot)),
        lines: crate::lower::LINES,
    };
    bytes(&mut out, &body(&linked, &[], &parts, interface, &none));
    out.extend(Record::default().finish());
    out
}

/// The parts of a bundle's body beside its object's and its contributions: its job entry by
/// symbol with its task's stack, its component entries, for a runtime where it defines each
/// export and the bytes of its boot area, and the symbol of its line tables.
struct Parts<'a> {
    job: Option<(&'a str, u64)>,
    components: &'a [(String, u64)],
    runtime: Option<(&'a [u64], u64)>,
    lines: &'a str,
}

/// The body of a bundle: its interface hash, its image, the image's size in memory, its base
/// relocations, its imports, the offset of its line tables, its contributions, where its
/// read-only and writable data start, then its parts.
fn body(
    linked: &Linked,
    contributed: &[Contributed],
    parts: &Parts,
    interface: &Interface,
    index: &dyn Fn(&str) -> Option<usize>,
) -> Vec<u8> {
    let mut image = Vec::new();
    for segment in &linked.segments {
        image.resize(segment.address as usize, 0);
        image.extend_from_slice(&segment.bytes);
    }
    let relocations = &linked.absolute;
    let symbol = |name: &str| linked.symbol(name) as u128;
    let pair = |out: &mut Vec<u8>, (first, second): (u64, u64)| {
        uvar(out, u128::from(first));
        uvar(out, u128::from(second));
    };
    let mut record = Record::default();
    record.field(1, field(|out| out.extend_from_slice(&interface.hash)));
    record.field(2, image);
    record.field(3, field(|out| uvar(out, u128::from(linked.end()))));
    if !relocations.is_empty() {
        record.field(
            4,
            field(|out| {
                list(out, relocations.iter(), |out, at| {
                    uvar(out, u128::from(*at))
                })
            }),
        );
    }
    if !linked.imports.is_empty() {
        record.field(
            5,
            field(|out| {
                list(out, linked.imports.iter(), |out, (at, name)| {
                    uvar(out, u128::from(*at));
                    uvar(out, index(name).expect("an export") as u128);
                })
            }),
        );
    }
    record.field(6, field(|out| uvar(out, symbol(parts.lines))));
    if !contributed.is_empty() {
        record.field(
            7,
            field(|out| {
                list(out, contributed.iter(), |out, entry| {
                    let mut fields = Record::default();
                    let registry = index(&entry.record).expect("an export") as u128;
                    fields.field(1, field(|out| uvar(out, registry)));
                    fields.field(2, field(|out| uvar(out, symbol(&entry.entry))));
                    fields.field(3, field(|out| uvar(out, u128::from(entry.size))));
                    fields.field(4, field(|out| pair(out, entry.key)));
                    if !entry.unique.is_empty() {
                        fields.field(
                            5,
                            field(|out| {
                                list(out, entry.unique.iter(), |out, unique| pair(out, *unique))
                            }),
                        );
                    }
                    if !entry.gates.is_empty() {
                        fields.field(
                            6,
                            field(|out| {
                                list(out, entry.gates.iter(), |out, offset| {
                                    uvar(out, u128::from(*offset))
                                })
                            }),
                        );
                    }
                    out.extend(fields.finish());
                })
            }),
        );
    }
    let start = |access: Access| {
        linked
            .segments
            .iter()
            .find(|segment| segment.access >= access)
            .map_or(linked.end(), |segment| segment.address)
    };
    record.field(
        8,
        field(|out| pair(out, (start(Access::Read), start(Access::Write)))),
    );
    if let Some((symbol, stack)) = parts.job {
        let entry = linked.symbol(symbol);
        record.field(9, field(|out| pair(out, (entry, stack))));
    }
    if !parts.components.is_empty() {
        record.field(
            10,
            field(|out| {
                list(out, parts.components.iter(), |out, (name, stack)| {
                    bytes(out, name.as_bytes());
                    let entry = linked.symbol(&format!("{}{name}", crate::lower::COMPONENT));
                    pair(out, (entry, *stack));
                })
            }),
        );
    }
    if let Some((exports, boot)) = parts.runtime {
        record.field(
            11,
            field(|out| list(out, exports.iter(), |out, at| uvar(out, u128::from(*at)))),
        );
        record.field(12, field(|out| uvar(out, u128::from(boot))));
    }
    record.finish()
}

/// The content a writer gives.
fn field(write: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut out = Vec::new();
    write(&mut out);
    out
}
