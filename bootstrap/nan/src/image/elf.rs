//! Writes `x86_64-bare` images: static ELF64 executables whose segments load at their physical
//! addresses, with a `XEN_ELFNOTE_PHYS32_ENTRY` note naming the entry a PVH loader enters.

use super::{Access, Linked, PAGE};

/// The physical address at which an image's first segment loads.
pub const BASE: u64 = 0x0010_0000;

const HEADER: u64 = 64;
const PROGRAM_HEADER: u64 = 56;
const SECTION_HEADER: u16 = 64;

const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 62;
const EV_CURRENT: u32 = 1;
const PT_LOAD: u32 = 1;
const PT_NOTE: u32 = 4;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

/// The type of the note naming the 32-bit PVH entry, and its owner's name.
const XEN_ELFNOTE_PHYS32_ENTRY: u32 = 18;
const XEN: &[u8; 4] = b"Xen\0";

/// The note: the lengths of its name and description, its type, its name and the entry address
/// as 8 bytes.
fn note(entry: u64) -> Vec<u8> {
    let mut note = Vec::new();
    note.extend_from_slice(&(XEN.len() as u32).to_le_bytes());
    note.extend_from_slice(&8u32.to_le_bytes());
    note.extend_from_slice(&XEN_ELFNOTE_PHYS32_ENTRY.to_le_bytes());
    note.extend_from_slice(XEN);
    note.extend_from_slice(&entry.to_le_bytes());
    note
}

fn program_header(
    out: &mut Vec<u8>,
    kind: u32,
    flags: u32,
    offset: u64,
    (address, file, memory): (u64, u64, u64),
    align: u64,
) {
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(&address.to_le_bytes());
    out.extend_from_slice(&address.to_le_bytes());
    out.extend_from_slice(&file.to_le_bytes());
    out.extend_from_slice(&memory.to_le_bytes());
    out.extend_from_slice(&align.to_le_bytes());
}

/// The bytes of a static ELF64 executable loading the linked object's segments at their
/// addresses, entered at the address `entry`, which its note also names.
pub fn write(linked: &Linked, entry: u64) -> Vec<u8> {
    let count = linked.segments.len() as u64 + 1;
    let note_offset = HEADER + PROGRAM_HEADER * count;
    let note = note(entry);
    let mut out = Vec::new();
    out.extend_from_slice(b"\x7fELF\x02\x01\x01");
    out.resize(16, 0);
    out.extend_from_slice(&ET_EXEC.to_le_bytes());
    out.extend_from_slice(&EM_X86_64.to_le_bytes());
    out.extend_from_slice(&EV_CURRENT.to_le_bytes());
    out.extend_from_slice(&entry.to_le_bytes());
    out.extend_from_slice(&HEADER.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(HEADER as u16).to_le_bytes());
    out.extend_from_slice(&(PROGRAM_HEADER as u16).to_le_bytes());
    out.extend_from_slice(&(count as u16).to_le_bytes());
    out.extend_from_slice(&SECTION_HEADER.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    let mut offset = (note_offset + note.len() as u64).next_multiple_of(PAGE);
    let mut placed = Vec::new();
    for segment in &linked.segments {
        let flags = match segment.access {
            Access::Execute => PF_R | PF_X,
            Access::Read => PF_R,
            Access::Write => PF_R | PF_W,
        };
        let sizes = (segment.address, segment.bytes.len() as u64, segment.size);
        program_header(&mut out, PT_LOAD, flags, offset, sizes, PAGE);
        placed.push(offset);
        offset = (offset + segment.bytes.len() as u64).next_multiple_of(PAGE);
    }
    let length = note.len() as u64;
    program_header(&mut out, PT_NOTE, PF_R, note_offset, (0, length, 0), 4);
    out.extend_from_slice(&note);
    for (segment, at) in linked.segments.iter().zip(placed) {
        out.resize(at as usize, 0);
        out.extend_from_slice(&segment.bytes);
    }
    out
}
