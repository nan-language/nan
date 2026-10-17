//! The image writer: links the relocatable object of a machine target's lowering into its image,
//! a PE32+ EFI application on `x86_64-uefi` or a static ELF64 executable entered by the PVH boot
//! protocol on `x86_64-bare`. It lays the object's sections out in segments, one per access
//! kind, resolves its symbols and applies its relocations.

mod elf;
mod object;
mod pe;

use std::collections::HashMap;

use crate::workspace::Target;
use object::Kind;

/// The size of a page, to which every segment is aligned.
const PAGE: u64 = 4_096;

/// The data object of 16 bytes into which the image writer writes the image's first and last
/// address plus one.
pub const IMAGE_EXTENT: &str = "_NSimage";

/// The image of a machine target linking an object lowering wrote, entered at the symbol `entry`.
pub fn write(target: Target, object: &[u8], entry: &str) -> Vec<u8> {
    match target {
        Target::Uefi => {
            let linked = link(object, pe::BASE);
            pe::write(&linked, linked.symbol(entry))
        }
        Target::Bare => {
            let linked = link(object, elf::BASE);
            elf::write(&linked, linked.symbol(entry))
        }
        Target::Host | Target::Any => panic!("the image writer builds no {} image", target.name()),
    }
}

/// What a segment's memory permits beside reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// An object linked at an address: its segments, the address of each of its symbols, and the
/// address and width in bytes of each absolute address its relocations wrote.
struct Linked {
    segments: Vec<Segment>,
    symbols: HashMap<String, u64>,
    absolute: Vec<(u64, usize)>,
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

    /// Writes the image's extent, from `base` to the end of its segments, into the data object
    /// `IMAGE_EXTENT` when the object defines one, as two absolute addresses.
    fn record_extent(&mut self, base: u64) {
        let Some(at) = self.symbols.get(IMAGE_EXTENT).copied() else {
            return;
        };
        let end = self.end();
        let segment = self
            .segments
            .iter_mut()
            .find(|segment| (segment.address..segment.address + segment.size).contains(&at))
            .expect("the segment of the image extent");
        let offset = (at - segment.address) as usize;
        segment.bytes[offset..offset + 8].copy_from_slice(&base.to_le_bytes());
        segment.bytes[offset + 8..offset + 16].copy_from_slice(&end.to_le_bytes());
        self.absolute.extend([(at, 8), (at + 8, 8)]);
    }
}

/// The relocation types the objects lowering writes hold.
mod reloc {
    pub const ABS64: u32 = 1;
    pub const PC32: u32 = 2;
    pub const PLT32: u32 = 4;
    pub const ABS32: u32 = 10;
}

/// Links an object with its first segment at `base`.
fn link(object: &[u8], base: u64) -> Linked {
    let object = object::read(object);
    let mut placed: Vec<Option<u64>> = vec![None; object.sections.len()];
    let mut segments: Vec<Segment> = Vec::new();
    let mut next = base;
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
        if segment.size > 0 {
            next = (segment.address + segment.size).next_multiple_of(PAGE);
            segments.push(segment);
        }
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
            let target = address_of(&object.symbols[relocation.symbol]) as i64 + relocation.addend;
            let at = (place - segment.address) as usize;
            let relative = target - place as i64;
            let bytes: Vec<u8> = match relocation.kind {
                reloc::ABS64 => target.to_le_bytes().to_vec(),
                reloc::ABS32 => u32::try_from(target)
                    .expect("an absolute address below 4 GiB")
                    .to_le_bytes()
                    .to_vec(),
                reloc::PC32 | reloc::PLT32 => i32::try_from(relative)
                    .expect("a relative address within 2 GiB")
                    .to_le_bytes()
                    .to_vec(),
                other => panic!("relocation type {other} in a machine image"),
            };
            if matches!(relocation.kind, reloc::ABS64 | reloc::ABS32) {
                absolute.push((place, bytes.len()));
            }
            segment.bytes[at..at + bytes.len()].copy_from_slice(&bytes);
        }
    }
    let mut linked = Linked {
        segments,
        symbols,
        absolute,
    };
    linked.record_extent(base);
    linked.absolute.sort();
    linked
}

#[cfg(test)]
mod tests;
