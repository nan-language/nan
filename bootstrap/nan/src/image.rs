//! The image writer: links the relocatable object of a Windows or machine target's lowering into
//! its image, a PE32+ console program on a Windows target, a PE32+ EFI application on a UEFI
//! target, a static ELF64 executable on a bare target, or a bundle. It lays the object's sections
//! out in segments, one per access kind, resolves its symbols and applies its relocations.

pub mod bundle;
mod elf;
mod object;
mod pe;

use std::collections::HashMap;

use crate::workspace::{self, Target};
use object::Kind;

/// The size of a page, to which every segment is aligned.
const PAGE: u64 = 4_096;

/// The data object of 16 bytes into which the image writer writes the image's first and last
/// address plus one.
pub const IMAGE_EXTENT: &str = "_NSimage";

/// The data object of `SEGMENT_TABLE_BYTES` bytes into which the image writer writes the image's
/// segment count, then for each of its segments, in address order, its absolute address, its size
/// and its access: bit 0 for writable and bit 1 for executable. An image has at most
/// `MOST_SEGMENTS` segments.
pub const SEGMENT_TABLE: &str = "_NSsegments";
pub const MOST_SEGMENTS: u64 = 4;
pub const SEGMENT_TABLE_BYTES: u64 = 8 + 24 * MOST_SEGMENTS;

/// The relocation header, a data object of `RELOCATION_HEADER_BYTES` bytes into which the image
/// writer of a bare image writes five words as linked: the header's address, the image's first
/// and last address plus one, the address of a table listing the address of each absolute address
/// its relocations wrote, and that table's entry count. The table lies in a read-only segment
/// after the others; none of the header's words is itself relocated.
pub const RELOCATION_HEADER: &str = "_NSrelocs";
pub const RELOCATION_HEADER_BYTES: usize = 40;

/// The image of a Windows or machine target linking an object lowering wrote, entered at the
/// symbol `entry`.
pub fn write(target: Target, object: &[u8], entry: &str) -> Vec<u8> {
    let arch = target.arch().expect("a concrete target's architecture");
    match target.kind() {
        Some(workspace::Kind::Windows) => {
            let linked = link(object, pe::base(pe::Program::Console), Imports::Table);
            pe::write(&linked, linked.symbol(entry), arch, pe::Program::Console)
        }
        Some(workspace::Kind::Uefi) => {
            let linked = link(object, pe::base(pe::Program::Efi), Imports::None);
            pe::write(&linked, linked.symbol(entry), arch, pe::Program::Efi)
        }
        Some(workspace::Kind::Bare) => {
            let linked = link(object, elf::base(arch), Imports::None);
            elf::write(&linked, linked.symbol(entry), arch)
        }
        _ => panic!("the image writer builds no {} image", target.name()),
    }
}

/// How a link treats the symbols an object leaves undefined.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Imports {
    /// None is left undefined.
    None,
    /// A bundle's: each absolute address of one is an import, and each call of one goes through
    /// an import stub after the object's code.
    Bundle,
    /// A Windows image's: each is an `IMPORT_PREFIX` symbol naming a word of the import address
    /// table, which the system's loader fills with the address of the function it names.
    Table,
}

/// The prefix of the symbol naming the import address table's word of a function a Windows image
/// imports, followed by the function's name for a function of `KERNEL32`, and otherwise by its
/// library's name, `!` and its name.
pub const IMPORT_PREFIX: &str = "__imp_";

/// The library a Windows image imports a function from when its symbol names none.
const KERNEL32: &str = "kernel32.dll";

/// What a segment's memory permits beside reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Access {
    Execute,
    Read,
    Write,
}

/// A segment of an image: its address, its bytes and its size in memory, the rest zeros.
struct Segment {
    address: u64,
    access: Access,
    bytes: Vec<u8>,
    size: u64,
}

/// An object linked at an address: its segments, the address of each of its symbols, the
/// address of each 8-byte absolute address its relocations wrote, and the address of each word
/// holding the address of a symbol it imports, with that symbol's name; a Windows image's words
/// are its import address table, the last segment, where each library's functions follow each
/// other in order and end with a zero word, and each name is the library's, `!` and the
/// function's.
struct Linked {
    segments: Vec<Segment>,
    symbols: HashMap<String, u64>,
    absolute: Vec<u64>,
    imports: Vec<(u64, String)>,
}

impl Linked {
    /// The address of a symbol the lowering defines.
    fn symbol(&self, name: &str) -> u64 {
        *self
            .symbols
            .get(name)
            .unwrap_or_else(|| panic!("the symbol {name} in the linked object"))
    }

    /// The end of the memory the segments occupy.
    fn end(&self) -> u64 {
        self.segments
            .iter()
            .map(|segment| segment.address + segment.size)
            .max()
            .unwrap_or(0)
    }

    /// Writes the relocation header and appends its table when the object defines the header,
    /// the table listing the image extent's words too.
    fn record_relocations(&mut self, base: u64) {
        let Some(header) = self.symbols.get(RELOCATION_HEADER).copied() else {
            return;
        };
        let mut places = self.absolute.clone();
        if let Some(extent) = self.symbols.get(IMAGE_EXTENT) {
            places.extend([*extent, *extent + 8]);
        }
        if let Some(table) = self.symbols.get(SEGMENT_TABLE) {
            let count = self.segments.len() as u64 + 1;
            places.extend((0..count).map(|index| table + 8 + 24 * index));
        }
        places.sort();
        let table = self.end().next_multiple_of(PAGE);
        if !places.is_empty() {
            self.segments.push(Segment {
                address: table,
                access: Access::Read,
                bytes: places
                    .iter()
                    .flat_map(|place| place.to_le_bytes())
                    .collect(),
                size: 8 * places.len() as u64,
            });
        }
        let words = [header, base, self.end(), table, places.len() as u64];
        self.write_words(header, &words);
    }

    /// Writes words at an address of a segment's bytes.
    fn write_words(&mut self, at: u64, words: &[u64]) {
        let segment = self
            .segments
            .iter_mut()
            .find(|segment| (segment.address..segment.address + segment.size).contains(&at))
            .expect("the segment of a recorded object");
        let offset = (at - segment.address) as usize;
        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        segment.bytes[offset..offset + bytes.len()].copy_from_slice(&bytes);
    }

    /// Writes the image's extent, from `base` to the end of its segments, into the data object
    /// `IMAGE_EXTENT` when the object defines one, as two absolute addresses.
    fn record_extent(&mut self, base: u64) {
        let Some(at) = self.symbols.get(IMAGE_EXTENT).copied() else {
            return;
        };
        let end = self.end();
        self.write_words(at, &[base, end]);
        self.absolute.extend([at, at + 8]);
    }

    /// Writes the image's segments into the data object `SEGMENT_TABLE` when the object defines
    /// one, each segment's address being absolute.
    fn record_segments(&mut self) {
        let Some(at) = self.symbols.get(SEGMENT_TABLE).copied() else {
            return;
        };
        assert!(
            self.segments.len() as u64 <= MOST_SEGMENTS,
            "an image of at most {MOST_SEGMENTS} segments"
        );
        let mut words = vec![self.segments.len() as u64];
        for segment in &self.segments {
            let access = match segment.access {
                Access::Execute => 2,
                Access::Read => 0,
                Access::Write => 1,
            };
            words.extend([segment.address, segment.size, access]);
        }
        self.write_words(at, &words);
        let places = (0..self.segments.len() as u64).map(|index| at + 8 + 24 * index);
        self.absolute.extend(places);
    }
}

/// The relocation types the objects lowering writes hold: the x86_64 ones and `A64_ABS64`, which
/// patch a byte range with an absolute or program-counter-relative address, and the other AArch64
/// ones, which patch an instruction's immediate field.
mod reloc {
    pub const X64_ABS64: u32 = 1;
    pub const X64_PC32: u32 = 2;
    pub const X64_PLT32: u32 = 4;
    pub const A64_ABS64: u32 = 257;
    pub const A64_ADR_PREL_PG_HI21: u32 = 275;
    pub const A64_ADD_ABS_LO12_NC: u32 = 277;
    pub const A64_CALL26: u32 = 283;
}

/// Patches the 32-bit AArch64 instruction at `at` in `bytes`, replacing the bits `field` covers
/// with `value`.
fn patch_a64(bytes: &mut [u8], at: usize, field: u32, value: u32) {
    let instruction = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"));
    let patched = (instruction & !field) | (value & field);
    bytes[at..at + 4].copy_from_slice(&patched.to_le_bytes());
}

/// The code a bundle calls an imported function through, in its text: an indirect jump through
/// the word that follows it, which holds the function's address.
const IMPORT_STUB: [u8; 8] = [0xff, 0x25, 0x02, 0x00, 0x00, 0x00, 0xcc, 0xcc];
const IMPORT_STUB_BYTES: u64 = 16;

/// Links an object with its first segment at `base`, treating its undefined symbols as `imports`
/// says.
fn link(object: &[u8], base: u64, imports: Imports) -> Linked {
    let object = object::read(object);
    let undefined = |symbol: &object::Symbol| symbol.section.is_none() && !symbol.name.is_empty();
    let mut called: Vec<String> = Vec::new();
    for relocation in object
        .sections
        .iter()
        .flat_map(|section| &section.relocations)
    {
        let symbol = &object.symbols[relocation.symbol];
        if undefined(symbol)
            && matches!(relocation.kind, reloc::X64_PC32 | reloc::X64_PLT32)
            && !called.contains(&symbol.name)
        {
            called.push(symbol.name.clone());
        }
    }
    assert!(
        imports == Imports::Bundle || called.is_empty(),
        "a call of an undefined symbol in an image"
    );
    let mut placed: Vec<Option<u64>> = vec![None; object.sections.len()];
    let mut segments: Vec<Segment> = Vec::new();
    let mut next = base;
    let mut stubs = 0;
    for (kind, access) in [
        (Kind::Text, Access::Execute),
        (Kind::ReadOnly, Access::Read),
        (Kind::Data, Access::Write),
    ] {
        let mut segment = Segment {
            address: next,
            access,
            bytes: Vec::new(),
            size: 0,
        };
        let kinds: &[Kind] = match kind {
            Kind::Data => &[Kind::Data, Kind::Zeroed],
            _ => &[kind],
        };
        for kind in kinds {
            for (index, section) in object.sections.iter().enumerate() {
                if section.kind != *kind {
                    continue;
                }
                let offset = segment.size.next_multiple_of(section.align);
                placed[index] = Some(segment.address + offset);
                segment.size = offset + section.size;
                if *kind != Kind::Zeroed {
                    segment.bytes.resize(offset as usize, 0);
                    segment.bytes.extend_from_slice(&section.data);
                }
            }
        }
        if kind == Kind::Text && !called.is_empty() {
            let offset = segment.size.next_multiple_of(IMPORT_STUB_BYTES);
            segment.bytes.resize(offset as usize, 0);
            for _ in &called {
                segment.bytes.extend_from_slice(&IMPORT_STUB);
                segment.bytes.extend_from_slice(&[0; 8]);
            }
            stubs = segment.address + offset;
            segment.size = segment.bytes.len() as u64;
        }
        if segment.size > 0 {
            next = (segment.address + segment.size).next_multiple_of(PAGE);
            segments.push(segment);
        }
    }
    let mut imported: Vec<(u64, String)> = called
        .iter()
        .enumerate()
        .map(|(index, name)| (stubs + index as u64 * IMPORT_STUB_BYTES + 8, name.clone()))
        .collect();
    let mut table: HashMap<String, u64> = HashMap::new();
    if imports == Imports::Table {
        let mut named: Vec<(&str, &str, &str)> = object
            .sections
            .iter()
            .flat_map(|section| &section.relocations)
            .map(|relocation| &object.symbols[relocation.symbol])
            .filter(|symbol| undefined(symbol))
            .map(|symbol| {
                let import = symbol
                    .name
                    .strip_prefix(IMPORT_PREFIX)
                    .unwrap_or_else(|| panic!("the import {} names no function", symbol.name));
                let (library, function) = import.split_once('!').unwrap_or((KERNEL32, import));
                (library, function, symbol.name.as_str())
            })
            .collect();
        named.sort();
        named.dedup();
        // Each library's words end with a zero word.
        let mut at = next;
        for (index, (library, function, symbol)) in named.iter().enumerate() {
            if index > 0 && named[index - 1].0 != *library {
                at += 8;
            }
            imported.push((at, format!("{library}!{function}")));
            table.insert((*symbol).to_owned(), at);
            at += 8;
        }
        let words = at + 8 - next;
        segments.push(Segment {
            address: next,
            access: Access::Write,
            bytes: vec![0; words as usize],
            size: words,
        });
    }
    let address_of = |symbol: &object::Symbol| -> u64 {
        let section = symbol
            .section
            .unwrap_or_else(|| panic!("the symbol {} defined in the object", symbol.name));
        placed[section].expect("a symbol of a placed section") + symbol.value
    };
    let symbols: HashMap<String, u64> = object
        .symbols
        .iter()
        .filter(|symbol| {
            !symbol.name.is_empty() && symbol.section.is_some_and(|index| placed[index].is_some())
        })
        .map(|symbol| (symbol.name.clone(), address_of(symbol)))
        .collect();
    let mut absolute = Vec::new();
    for (index, section) in object.sections.iter().enumerate() {
        let Some(start) = placed[index].filter(|_| !section.relocations.is_empty()) else {
            continue;
        };
        let segment = segments
            .iter_mut()
            .find(|segment| (segment.address..segment.address + segment.size).contains(&start))
            .expect("the segment of a placed section");
        for relocation in &section.relocations {
            let place = start + relocation.offset;
            let symbol = &object.symbols[relocation.symbol];
            let at = (place - segment.address) as usize;
            let target = match called.iter().position(|name| *name == symbol.name) {
                Some(index) if undefined(symbol) => {
                    (stubs + index as u64 * IMPORT_STUB_BYTES) as i64 + relocation.addend
                }
                _ if undefined(symbol) && imports == Imports::Table => {
                    table[&symbol.name] as i64 + relocation.addend
                }
                _ if undefined(symbol) => {
                    assert!(
                        imports == Imports::Bundle
                            && matches!(relocation.kind, reloc::X64_ABS64 | reloc::A64_ABS64),
                        "an absolute address of the undefined symbol {}",
                        symbol.name
                    );
                    imported.push((place, symbol.name.clone()));
                    continue;
                }
                _ => address_of(symbol) as i64 + relocation.addend,
            };
            let relative = target - place as i64;
            match relocation.kind {
                reloc::X64_ABS64 | reloc::A64_ABS64 => {
                    segment.bytes[at..at + 8].copy_from_slice(&target.to_le_bytes());
                    absolute.push(place);
                }
                reloc::X64_PC32 | reloc::X64_PLT32 => {
                    let value = i32::try_from(relative).expect("a relative address within 2 GiB");
                    segment.bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
                }
                // The branch's 26-bit field holds the instruction count to the target.
                reloc::A64_CALL26 => {
                    let count = relative >> 2;
                    assert!(
                        (-(1 << 25)..1 << 25).contains(&count),
                        "a branch target within 128 MiB"
                    );
                    patch_a64(&mut segment.bytes, at, 0x03FF_FFFF, count as u32);
                }
                // The address of the 4 KiB page the target lies in, relative to the page the
                // instruction lies in, in units of pages, held in the split immediate.
                reloc::A64_ADR_PREL_PG_HI21 => {
                    let pages = ((target & !0xFFF) - (place & !0xFFF) as i64) >> 12;
                    assert!(
                        (-(1 << 20)..1 << 20).contains(&pages),
                        "a page within 4 GiB"
                    );
                    let pages = pages as u32;
                    patch_a64(&mut segment.bytes, at, 0x6000_0000, pages << 29);
                    patch_a64(&mut segment.bytes, at, 0x00FF_FFE0, (pages >> 2) << 5);
                }
                // The target's low 12 bits, added to the page the previous instruction formed.
                reloc::A64_ADD_ABS_LO12_NC => {
                    let low = (target & 0xFFF) as u32;
                    patch_a64(&mut segment.bytes, at, 0x003F_FC00, low << 10);
                }
                other => panic!("relocation type {other} in a machine image"),
            }
        }
    }
    imported.sort();
    let mut linked = Linked {
        segments,
        symbols,
        absolute,
        imports: imported,
    };
    linked.record_relocations(base);
    linked.record_extent(base);
    linked.record_segments();
    linked.absolute.sort();
    linked
}

#[cfg(test)]
mod tests;
