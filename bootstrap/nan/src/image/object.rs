//! Reads the ELF64 relocatable object lowering writes: its sections, symbols and relocations.

/// A section of the object: its bytes, empty for a section without contents, and its size in
/// memory.
pub struct Section {
    pub kind: Kind,
    pub data: Vec<u8>,
    pub size: u64,
    pub align: u64,
    pub relocations: Vec<Relocation>,
}

/// Where a section goes in an image; `Other` sections are left out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Text,
    ReadOnly,
    Data,
    Zeroed,
    Other,
}

pub struct Symbol {
    pub name: String,
    /// The index of the section defining it, none for an undefined or absolute symbol.
    pub section: Option<usize>,
    pub value: u64,
}

/// A relocation of a section: where it applies, its type, the symbol it names and its addend.
pub struct Relocation {
    pub offset: u64,
    pub kind: u32,
    pub symbol: usize,
    pub addend: i64,
}

pub struct Object {
    pub sections: Vec<Section>,
    pub symbols: Vec<Symbol>,
}

const SHT_SYMTAB: u32 = 2;
const SHT_RELA: u32 = 4;
const SHT_NOBITS: u32 = 8;
const SHF_WRITE: u64 = 1;
const SHF_ALLOC: u64 = 2;
const SHF_EXECINSTR: u64 = 4;
const SHF_TLS: u64 = 0x400;
const SHN_UNDEF: u16 = 0;
const SHN_LORESERVE: u16 = 0xFF00;

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().expect("two bytes"))
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes"))
}

/// The NUL-terminated name at an offset of a string table.
fn name_at(strings: &[u8], at: usize) -> String {
    let end = strings[at..]
        .iter()
        .position(|byte| *byte == 0)
        .map_or(strings.len(), |length| at + length);
    String::from_utf8_lossy(&strings[at..end]).into_owned()
}

struct Header {
    kind: u32,
    flags: u64,
    offset: usize,
    size: usize,
    link: u32,
    info: u32,
    align: u64,
}

/// Reads an object lowering wrote; any other input is an invariant broken.
pub fn read(bytes: &[u8]) -> Object {
    assert!(
        bytes.starts_with(b"\x7fELF\x02\x01"),
        "lowering writes ELF64 little-endian objects"
    );
    let table = u64_at(bytes, 0x28) as usize;
    let count = u16_at(bytes, 0x3C) as usize;
    let headers: Vec<Header> = (0..count)
        .map(|index| {
            let at = table + index * 64;
            Header {
                kind: u32_at(bytes, at + 4),
                flags: u64_at(bytes, at + 8),
                offset: u64_at(bytes, at + 24) as usize,
                size: u64_at(bytes, at + 32) as usize,
                link: u32_at(bytes, at + 40),
                info: u32_at(bytes, at + 44),
                align: u64_at(bytes, at + 48),
            }
        })
        .collect();
    let contents = |header: &Header| &bytes[header.offset..header.offset + header.size];
    let mut sections: Vec<Section> = headers
        .iter()
        .map(|header| {
            let alloc = header.flags & SHF_ALLOC != 0;
            assert!(
                !alloc || header.flags & SHF_TLS == 0,
                "machine images have no thread-local data"
            );
            let kind = match (alloc, header.kind == SHT_NOBITS) {
                (false, _) => Kind::Other,
                (true, true) => Kind::Zeroed,
                _ if header.flags & SHF_EXECINSTR != 0 => Kind::Text,
                _ if header.flags & SHF_WRITE != 0 => Kind::Data,
                _ => Kind::ReadOnly,
            };
            let data = match kind {
                Kind::Zeroed | Kind::Other => Vec::new(),
                _ => contents(header).to_vec(),
            };
            Section {
                kind,
                data,
                size: header.size as u64,
                align: header.align.max(1),
                relocations: Vec::new(),
            }
        })
        .collect();
    let mut symbols = Vec::new();
    for header in headers.iter().filter(|header| header.kind == SHT_SYMTAB) {
        let strings = contents(&headers[header.link as usize]);
        for entry in contents(header).as_chunks::<24>().0 {
            let index = u16_at(entry, 6);
            symbols.push(Symbol {
                name: name_at(strings, u32_at(entry, 0) as usize),
                section: (index != SHN_UNDEF && index < SHN_LORESERVE).then_some(index as usize),
                value: u64_at(entry, 8),
            });
        }
    }
    for header in headers.iter().filter(|header| header.kind == SHT_RELA) {
        let target = header.info as usize;
        for entry in contents(header).as_chunks::<24>().0 {
            let info = u64_at(entry, 8);
            sections[target].relocations.push(Relocation {
                offset: u64_at(entry, 0),
                kind: info as u32,
                symbol: (info >> 32) as usize,
                addend: u64_at(entry, 16) as i64,
            });
        }
    }
    Object { sections, symbols }
}
