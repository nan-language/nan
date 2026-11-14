//! Writes PE32+ images: the console programs of the Windows targets, which import functions of
//! the system's libraries, and the EFI applications of the UEFI targets, each with one section per segment
//! and the base relocations of every 64-bit absolute address, so the system or the firmware may
//! load it anywhere, then the object's unallocated sections.

use super::{Access, Linked, PAGE};
use crate::workspace::Arch;

/// What a PE image runs as: a Windows console program or an EFI application.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Program {
    Console,
    Efi,
}

impl Program {
    /// The address the image is linked for; its headers occupy its first page.
    fn image_base(self) -> u64 {
        match self {
            Program::Console => 0x0001_4000_0000,
            Program::Efi => 0x0040_0000,
        }
    }
}

/// The address of an image's first section, after its headers.
pub fn base(program: Program) -> u64 {
    program.image_base() + PAGE
}

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
const SUBSYSTEM_CONSOLE: u16 = 3;
const SUBSYSTEM_EFI_APPLICATION: u16 = 10;
/// The system version a console program needs, Windows Vista's, and its DLL characteristics: a
/// high-entropy and dynamic base, no execution of data and awareness of terminal servers.
const CONSOLE_VERSION: u16 = 6;
const CONSOLE_CHARACTERISTICS: u16 = 0x8160;
/// The stack and heap a console program's process reserves and commits at start.
const CONSOLE_RESERVE: u64 = 0x10_0000;
const CONSOLE_COMMIT: u64 = 0x1000;
const DIRECTORIES: u32 = 16;
const IMPORT_DIRECTORY: usize = 1;
const BASE_RELOCATION_DIRECTORY: usize = 5;
const IMPORT_ADDRESS_DIRECTORY: usize = 12;
/// The size of an import directory entry.
const IMPORT_ENTRY: u64 = 20;
const DIR64: u16 = 10;

const CODE: u32 = 0x0000_0020;
const INITIALIZED: u32 = 0x0000_0040;
const DISCARDABLE: u32 = 0x0200_0000;
const EXECUTE: u32 = 0x2000_0000;
const READ: u32 = 0x4000_0000;
const WRITE: u32 = 0x8000_0000;

/// The name of the image section holding an unallocated section of the object: its name without
/// the dots after its first character, which fits the 8 bytes of a section name.
fn section_name(name: &str) -> Vec<u8> {
    let spelt: String = name
        .char_indices()
        .filter(|(at, c)| *at == 0 || *c != '.')
        .map(|(_, c)| c)
        .collect();
    assert!(spelt.len() <= 8, "a section name of at most 8 bytes");
    spelt.into_bytes()
}

/// A section of the image: its name, its RVA, its bytes, its size in memory and its flags.
struct Section {
    name: Vec<u8>,
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

/// The import directory of a console program, laid after its import address table, its last
/// section, whose words `imports` gives in order with each function's library and name: an entry
/// per library and the terminating entry, the lookup tables, laid out as the address table is,
/// each function's hint and name, then each library's name. Each word of the address table is set
/// as the lookup table's is, to the address of the function's hint and name, which the loader
/// replaces with the function's. Gives the data directory entries of the import address table
/// and of the import directory.
fn import_directory(section: &mut Section, imports: &[(u64, &str, &str)]) -> [(u32, u32); 2] {
    let mut libraries: Vec<(&str, usize)> = Vec::new();
    for (_, library, _) in imports {
        match libraries.last_mut() {
            Some((last, count)) if last == library => *count += 1,
            _ => libraries.push((library, 1)),
        }
    }
    let table = section.rva;
    let size = 8 * (imports.len() + libraries.len()) as u64;
    let directory = table + size;
    let lookup = directory + IMPORT_ENTRY * (libraries.len() as u64 + 1);
    let strings = lookup + size;
    let mut names = Vec::new();
    let mut words = Vec::new();
    let mut functions = imports.iter();
    for (_, count) in &libraries {
        for (_, _, function) in functions.by_ref().take(*count) {
            words.extend_from_slice(&(strings + names.len() as u64).to_le_bytes());
            names.extend_from_slice(&[0, 0]);
            names.extend_from_slice(function.as_bytes());
            names.push(0);
            if names.len() % 2 == 1 {
                names.push(0);
            }
        }
        words.extend_from_slice(&0u64.to_le_bytes());
    }
    let mut bytes = words.clone();
    let mut first = 0;
    for (library, count) in &libraries {
        let name = strings + names.len() as u64;
        names.extend_from_slice(library.as_bytes());
        names.push(0);
        for word in [lookup + first, 0, 0, name, table + first] {
            bytes.extend_from_slice(&(word as u32).to_le_bytes());
        }
        first += 8 * (*count as u64 + 1);
    }
    bytes.extend_from_slice(&[0; IMPORT_ENTRY as usize]);
    bytes.extend_from_slice(&words);
    bytes.extend_from_slice(&names);
    section.size = bytes.len() as u64;
    section.bytes = bytes;
    [
        (table as u32, size as u32),
        (directory as u32, (lookup - directory) as u32),
    ]
}

/// The bytes of a PE32+ image of `program` holding the linked object's segments, linked at the
/// program's image base and entered at the address `entry`; a console program's last segment is
/// its import address table.
pub fn write(linked: &Linked, entry: u64, arch: Arch, program: Program) -> Vec<u8> {
    let image_base = program.image_base();
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
                name: name.to_vec(),
                rva: segment.address - image_base,
                bytes: segment.bytes.clone(),
                size: segment.size,
                flags,
            }
        })
        .collect();
    let mut imports = None;
    if program == Program::Console {
        let functions: Vec<(u64, &str, &str)> = linked
            .imports
            .iter()
            .map(|(address, name)| {
                let (library, function) = name.split_once('!').expect("a library's function");
                (address - image_base, library, function)
            })
            .collect();
        let section = sections
            .last_mut()
            .expect("a console program's import table");
        section.name = b".idata".to_vec();
        imports = Some(import_directory(section, &functions));
    }
    let relocations: Vec<u64> = linked
        .absolute
        .iter()
        .map(|address| address - image_base)
        .collect();
    let end = sections
        .iter()
        .map(|section| section.rva + section.size)
        .max()
        .unwrap_or(0);
    let relocation_rva = end.next_multiple_of(PAGE);
    let relocation_bytes = base_relocations(&relocations);
    let relocation_size = relocation_bytes.len() as u64;
    sections.push(Section {
        name: b".reloc".to_vec(),
        rva: relocation_rva,
        size: relocation_size.max(1),
        bytes: relocation_bytes,
        flags: INITIALIZED | DISCARDABLE | READ,
    });
    for (name, bytes) in &linked.unallocated {
        let previous = sections.last().expect("the relocation section");
        sections.push(Section {
            name: section_name(name),
            rva: (previous.rva + previous.size).next_multiple_of(PAGE),
            size: bytes.len() as u64,
            bytes: bytes.clone(),
            flags: INITIALIZED | DISCARDABLE | READ,
        });
    }
    let last = sections.last().expect("the relocation section");
    let image_size = (last.rva + last.size).next_multiple_of(PAGE);
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
    out.extend_from_slice(&((entry - image_base) as u32).to_le_bytes());
    out.extend_from_slice(&(sections[0].rva as u32).to_le_bytes());
    out.extend_from_slice(&image_base.to_le_bytes());
    out.extend_from_slice(&(PAGE as u32).to_le_bytes());
    out.extend_from_slice(&(FILE_ALIGNMENT as u32).to_le_bytes());
    let (version, subsystem, characteristics, reserve, commit) = match program {
        Program::Console => (
            CONSOLE_VERSION,
            SUBSYSTEM_CONSOLE,
            CONSOLE_CHARACTERISTICS,
            CONSOLE_RESERVE,
            CONSOLE_COMMIT,
        ),
        Program::Efi => (0, SUBSYSTEM_EFI_APPLICATION, 0, 0, 0),
    };
    for half in [version, 0, 0, 0, version, 0] {
        out.extend_from_slice(&half.to_le_bytes());
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(image_size as u32).to_le_bytes());
    out.extend_from_slice(&(headers_size as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&subsystem.to_le_bytes());
    out.extend_from_slice(&characteristics.to_le_bytes());
    for size in [reserve, commit, reserve, commit] {
        out.extend_from_slice(&size.to_le_bytes());
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&DIRECTORIES.to_le_bytes());
    for directory in 0..DIRECTORIES as usize {
        let (rva, size) = match (directory, imports) {
            (BASE_RELOCATION_DIRECTORY, _) => (relocation_rva as u32, relocation_size as u32),
            (IMPORT_ADDRESS_DIRECTORY, Some([table, _])) => table,
            (IMPORT_DIRECTORY, Some([_, entries])) => entries,
            _ => (0, 0),
        };
        out.extend_from_slice(&rva.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
    }
    let mut raw = headers_size;
    let mut placed = Vec::new();
    for section in &sections {
        let mut name = [0u8; 8];
        name[..section.name.len()].copy_from_slice(&section.name);
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
