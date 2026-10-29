//! Writes UEFI-target images: PE32+ EFI applications with one section per segment and the base
//! relocations of every 64-bit absolute address, so the firmware may load them anywhere.

use super::{Access, Linked, PAGE};
use crate::workspace::Arch;

/// The address an image is linked for; its headers occupy its first page.
const IMAGE_BASE: u64 = 0x0040_0000;

/// The address of an image's first section, after its headers.
pub const BASE: u64 = IMAGE_BASE + PAGE;

const FILE_ALIGNMENT: u64 = 0x200;

/// The headers before the section table: the DOS header, whose field at `E_LFANEW` gives the
/// offset of the PE signature following it, the signature, the file header and the PE32+
/// optional header with its data directories.
const DOS_HEADER: u64 = 0x40;
const E_LFANEW: usize = 0x3C;
const SIGNATURE: &[u8; 4] = b"PE\0\0";
const FILE_HEADER: u64 = 20;
const OPTIONAL_HEADER: u64 = 240;
const HEADERS: u64 = DOS_HEADER + SIGNATURE.len() as u64 + FILE_HEADER + OPTIONAL_HEADER;
const SECTION_HEADER: u64 = 40;

const MACHINE_X86_64: u16 = 0x8664;
const MACHINE_AARCH64: u16 = 0xAA64;
const EXECUTABLE_LARGE_ADDRESS_AWARE: u16 = 0x0022;
const PE32_PLUS: u16 = 0x020B;
const SUBSYSTEM_EFI_APPLICATION: u16 = 10;
const DIRECTORIES: u32 = 16;
const BASE_RELOCATION_DIRECTORY: usize = 5;
const DIR64: u16 = 10;

const CODE: u32 = 0x0000_0020;
const INITIALIZED: u32 = 0x0000_0040;
const DISCARDABLE: u32 = 0x0200_0000;
const EXECUTE: u32 = 0x2000_0000;
const READ: u32 = 0x4000_0000;
const WRITE: u32 = 0x8000_0000;

/// A section of the image: its name, its RVA, its bytes, its size in memory and its flags.
struct Section {
    name: &'static [u8],
    rva: u64,
    bytes: Vec<u8>,
    size: u64,
    flags: u32,
}

/// The base relocation blocks of the 64-bit absolute addresses at the given RVAs: per page, its
/// RVA, the block's size and an entry per address, padded to 4 bytes.
pub fn base_relocations(rvas: &[u64]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < rvas.len() {
        let page = rvas[index] & !(PAGE - 1);
        let mut entries: Vec<u16> = Vec::new();
        while index < rvas.len() && rvas[index] & !(PAGE - 1) == page {
            entries.push(DIR64 << 12 | (rvas[index] & (PAGE - 1)) as u16);
            index += 1;
        }
        if entries.len() % 2 == 1 {
            entries.push(0);
        }
        out.extend_from_slice(&(page as u32).to_le_bytes());
        out.extend_from_slice(&(8 + 2 * entries.len() as u32).to_le_bytes());
        for entry in entries {
            out.extend_from_slice(&entry.to_le_bytes());
        }
    }
    out
}

/// The bytes of a PE32+ EFI application holding the linked object's segments, linked at
/// `IMAGE_BASE` and entered at the address `entry`.
pub fn write(linked: &Linked, entry: u64, arch: Arch) -> Vec<u8> {
    let mut sections: Vec<Section> = linked
        .segments
        .iter()
        .map(|segment| {
            let (name, flags): (&[u8], u32) = match segment.access {
                Access::Execute => (b".text", CODE | EXECUTE | READ),
                Access::Read => (b".rdata", INITIALIZED | READ),
                Access::Write => (b".data", INITIALIZED | READ | WRITE),
            };
            Section {
                name,
                rva: segment.address - IMAGE_BASE,
                bytes: segment.bytes.clone(),
                size: segment.size,
                flags,
            }
        })
        .collect();
    let relocations: Vec<u64> = linked
        .absolute
        .iter()
        .map(|&(address, width)| {
            assert!(
                width == 8,
                "a {}-bit absolute address at {address:#x} in a UEFI image, whose base \
                 relocations rebase only 64-bit ones",
                width * 8
            );
            address - IMAGE_BASE
        })
        .collect();
    let relocation_rva = (linked.end() - IMAGE_BASE).next_multiple_of(PAGE);
    let relocation_bytes = base_relocations(&relocations);
    let relocation_size = relocation_bytes.len() as u64;
    sections.push(Section {
        name: b".reloc",
        rva: relocation_rva,
        size: relocation_size.max(1),
        bytes: relocation_bytes,
        flags: INITIALIZED | DISCARDABLE | READ,
    });
    let image_size = (relocation_rva + relocation_size.max(1)).next_multiple_of(PAGE);
    let headers_size =
        (HEADERS + SECTION_HEADER * sections.len() as u64).next_multiple_of(FILE_ALIGNMENT);
    let sum = |flag: u32| -> u32 {
        sections
            .iter()
            .filter(|section| section.flags & flag != 0)
            .map(|section| section.size.next_multiple_of(FILE_ALIGNMENT) as u32)
            .sum()
    };
    let mut out = Vec::new();
    out.extend_from_slice(b"MZ");
    out.resize(E_LFANEW, 0);
    out.extend_from_slice(&(DOS_HEADER as u32).to_le_bytes());
    out.extend_from_slice(SIGNATURE);
    let machine = match arch {
        Arch::Amd64 => MACHINE_X86_64,
        Arch::Aarch64 => MACHINE_AARCH64,
    };
    out.extend_from_slice(&machine.to_le_bytes());
    out.extend_from_slice(&(sections.len() as u16).to_le_bytes());
    out.extend_from_slice(&[0; 12]);
    out.extend_from_slice(&(OPTIONAL_HEADER as u16).to_le_bytes());
    out.extend_from_slice(&EXECUTABLE_LARGE_ADDRESS_AWARE.to_le_bytes());
    out.extend_from_slice(&PE32_PLUS.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&sum(CODE).to_le_bytes());
    out.extend_from_slice(&sum(INITIALIZED).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&((entry - IMAGE_BASE) as u32).to_le_bytes());
    out.extend_from_slice(&(sections[0].rva as u32).to_le_bytes());
    out.extend_from_slice(&IMAGE_BASE.to_le_bytes());
    out.extend_from_slice(&(PAGE as u32).to_le_bytes());
    out.extend_from_slice(&(FILE_ALIGNMENT as u32).to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    out.extend_from_slice(&(image_size as u32).to_le_bytes());
    out.extend_from_slice(&(headers_size as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&SUBSYSTEM_EFI_APPLICATION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&[0; 32]);
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&DIRECTORIES.to_le_bytes());
    for directory in 0..DIRECTORIES as usize {
        let (rva, size) = match directory {
            BASE_RELOCATION_DIRECTORY => (relocation_rva as u32, relocation_size as u32),
            _ => (0, 0),
        };
        out.extend_from_slice(&rva.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
    }
    let mut raw = headers_size;
    let mut placed = Vec::new();
    for section in &sections {
        let mut name = [0u8; 8];
        name[..section.name.len()].copy_from_slice(section.name);
        let length = (section.bytes.len() as u64).next_multiple_of(FILE_ALIGNMENT);
        out.extend_from_slice(&name);
        out.extend_from_slice(&(section.size as u32).to_le_bytes());
        out.extend_from_slice(&(section.rva as u32).to_le_bytes());
        out.extend_from_slice(&(length as u32).to_le_bytes());
        out.extend_from_slice(&((if length > 0 { raw } else { 0 }) as u32).to_le_bytes());
        out.extend_from_slice(&[0; 12]);
        out.extend_from_slice(&section.flags.to_le_bytes());
        placed.push(raw);
        raw += length;
    }
    for (section, at) in sections.iter().zip(placed) {
        out.resize(at as usize, 0);
        out.extend_from_slice(&section.bytes);
    }
    out.resize(raw as usize, 0);
    out
}
