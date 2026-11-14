//! Writes Mach-O images: the executables of the macOS targets, position-independent and linked
//! against `libSystem`, which dyld loads and enters at the program entry seam. Each segment of the
//! linked object is a segment of the image with one section, the object's unallocated sections
//! follow in the read-only segment `__NAN`, and `__LINKEDIT` holds what dyld reads: the rebasing
//! of each absolute address, the binding of each imported function's word and the image's ad-hoc
//! code signature.

use super::{Access, Linked};
use crate::workspace::Arch;

/// The size of a page, to which every segment is aligned in memory and in the file: AArch64
/// macOS maps 16 KiB pages.
pub const PAGE: u64 = 0x4000;

/// The address of the image's header, the start of `__TEXT`, above the 4 GiB `__PAGEZERO` every
/// 64-bit executable reserves.
const IMAGE_BASE: u64 = 0x1_0000_0000;

/// The address of the image's first section, after the page holding its headers.
pub fn base() -> u64 {
    IMAGE_BASE + PAGE
}

const MH_MAGIC_64: u32 = 0xFEED_FACF;
const CPU_TYPE_X86_64: u32 = 0x0100_0007;
const CPU_TYPE_ARM64: u32 = 0x0100_000C;
const CPU_SUBTYPE_X86_64_ALL: u32 = 3;
const CPU_SUBTYPE_ARM64_ALL: u32 = 0;
const MH_EXECUTE: u32 = 2;
/// Linked by dyld, two-level namespace binding and position-independent.
const MH_FLAGS: u32 = 0x4 | 0x80 | 0x20_0000;
const HEADER: u64 = 32;

const LC_SEGMENT_64: u32 = 0x19;
const LC_SYMTAB: u32 = 0x2;
const LC_DYSYMTAB: u32 = 0xB;
const LC_LOAD_DYLIB: u32 = 0xC;
const LC_LOAD_DYLINKER: u32 = 0xE;
const LC_UUID: u32 = 0x1B;
const LC_CODE_SIGNATURE: u32 = 0x1D;
const LC_BUILD_VERSION: u32 = 0x32;
const LC_DYLD_INFO_ONLY: u32 = 0x8000_0022;
const LC_MAIN: u32 = 0x8000_0028;

const SEGMENT_COMMAND: u64 = 72;
const SECTION: u64 = 80;

/// The protections of a segment's memory.
const READ: u32 = 1;
const WRITE: u32 = 2;
const EXECUTE: u32 = 4;

/// The flag of a segment dyld makes read-only once it has rebased and bound it.
const SG_READ_ONLY: u32 = 0x10;

/// The types and attributes of sections: code, data, and data the system zero-fills.
const S_REGULAR: u32 = 0;
const S_ZEROFILL: u32 = 1;
const S_CODE: u32 = 0x8000_0400;

/// The alignment of the section starting a segment, as a power of two: the cache line functions
/// start at.
const SEGMENT_ALIGN: u32 = 6;

const DYLINKER: &str = "/usr/lib/dyld";
const LIBSYSTEM: &str = "/usr/lib/libSystem.B.dylib";
/// The versions of `libSystem` the image needs, 1.0.0, and of macOS, 15.0, which the image
/// names its minimum and the SDK it was built for.
const LIBSYSTEM_VERSION: u32 = 0x0001_0000;
const MACOS_VERSION: u32 = 0x000F_0000;
const PLATFORM_MACOS: u32 = 1;

/// The opcodes of rebasing and binding information.
const REBASE_TYPE_POINTER: u8 = 0x11;
const REBASE_SEGMENT_AND_OFFSET: u8 = 0x20;
const REBASE_ADD_ADDRESS: u8 = 0x30;
const REBASE_ONCE: u8 = 0x51;
const BIND_LIBSYSTEM: u8 = 0x11;
const BIND_TYPE_POINTER: u8 = 0x51;
const BIND_SYMBOL: u8 = 0x40;
const BIND_SEGMENT_AND_OFFSET: u8 = 0x70;
const BIND_ONCE: u8 = 0x90;
const DONE: u8 = 0;

/// The symbol every executable exports, naming its header.
const HEADER_SYMBOL: &str = "__mh_execute_header";
const N_SECT_EXTERNAL: u8 = 0x0F;
const REFERENCED_DYNAMICALLY: u16 = 0x10;

/// The code signature: an embedded signature holding one code directory, of version 0x20400,
/// ad-hoc and linker-signed, hashing each 4 KiB page of the file before it with SHA-256, the
/// executable segment that of a main binary. Its fields are big-endian.
const CSMAGIC_EMBEDDED_SIGNATURE: u32 = 0xFADE_0CC0;
const CSMAGIC_CODEDIRECTORY: u32 = 0xFADE_0C02;
const CODE_DIRECTORY_VERSION: u32 = 0x2_0400;
const CS_ADHOC_LINKER_SIGNED: u32 = 0x2_0002;
const CS_HASHTYPE_SHA256: u8 = 2;
const CS_EXECSEG_MAIN_BINARY: u64 = 1;
const SIGNATURE_PAGE_BITS: u8 = 12;
const SUPER_BLOB: u64 = 12;
const BLOB_INDEX: u64 = 8;
const CODE_DIRECTORY: u64 = 88;

/// A segment of the image: its name, address and size in memory, its offset and size in the
/// file, its protection, its flags and its sections.
struct Segment {
    name: &'static str,
    address: u64,
    size: u64,
    offset: u64,
    file_size: u64,
    protection: u32,
    flags: u32,
    sections: Vec<Section>,
}

/// A section of a segment: its name, address and size, its offset in the file, zero for one the
/// system zero-fills, its alignment as a power of two, and its type.
struct Section {
    name: String,
    address: u64,
    size: u64,
    offset: u64,
    align: u32,
    kind: u32,
}

/// The bytes of the Mach-O image named `name` holding the linked object's segments, linked from
/// `base()` and entered at the address `entry`; its imports are words of its read-only segment,
/// which dyld binds to the functions of `libSystem` they name.
pub fn write(linked: &Linked, entry: u64, arch: Arch, name: &str) -> Vec<u8> {
    let mut segments = vec![Segment {
        name: "__PAGEZERO",
        address: 0,
        size: IMAGE_BASE,
        offset: 0,
        file_size: 0,
        protection: 0,
        flags: 0,
        sections: Vec::new(),
    }];
    let mut contents: Vec<(u64, &[u8])> = Vec::new();
    let mut file_end = 0;
    for segment in &linked.segments {
        let start = match segment.access {
            Access::Execute => IMAGE_BASE,
            _ => segment.address,
        };
        let size = (segment.address + segment.size - start).next_multiple_of(PAGE);
        let initialized = segment.address + segment.bytes.len() as u64 - start;
        let file_size = initialized.next_multiple_of(PAGE).min(size);
        let offset = file_end;
        file_end = offset + file_size;
        let section = |name: &str, kind| Section {
            name: name.to_owned(),
            address: segment.address,
            size: segment.bytes.len() as u64,
            offset: offset + segment.address - start,
            align: SEGMENT_ALIGN,
            kind,
        };
        let (name, protection, flags, mut sections) = match segment.access {
            Access::Execute => ("__TEXT", READ | EXECUTE, 0, vec![section("__text", S_CODE)]),
            Access::Read => (
                "__DATA_CONST",
                READ | WRITE,
                SG_READ_ONLY,
                vec![section("__const", S_REGULAR)],
            ),
            Access::Write => (
                "__DATA",
                READ | WRITE,
                0,
                vec![section("__data", S_REGULAR)],
            ),
        };
        if segment.size > segment.bytes.len() as u64 {
            sections.push(Section {
                name: "__bss".to_owned(),
                address: segment.address + segment.bytes.len() as u64,
                size: segment.size - segment.bytes.len() as u64,
                offset: 0,
                align: 0,
                kind: S_ZEROFILL,
            });
        }
        contents.push((offset + segment.address - start, segment.bytes.as_slice()));
        segments.push(Segment {
            name,
            address: start,
            size,
            offset,
            file_size,
            protection,
            flags,
            sections,
        });
    }
    let mut address = linked.end().next_multiple_of(PAGE);
    if !linked.unallocated.is_empty() {
        let mut sections = Vec::new();
        let mut at = 0;
        for (section, bytes) in &linked.unallocated {
            let spelt: String = section.chars().filter(|c| *c != '.').collect();
            contents.push((file_end + at, bytes.as_slice()));
            sections.push(Section {
                name: format!("__{spelt}"),
                address: address + at,
                size: bytes.len() as u64,
                offset: file_end + at,
                align: 0,
                kind: S_REGULAR,
            });
            at += bytes.len() as u64;
        }
        let size = at.next_multiple_of(PAGE);
        segments.push(Segment {
            name: "__NAN",
            address,
            size,
            offset: file_end,
            file_size: size,
            protection: READ,
            flags: 0,
            sections,
        });
        address += size;
        file_end += size;
    }
    let segment_of = |at: u64| {
        let index = segments
            .iter()
            .position(|segment| (segment.address..segment.address + segment.size).contains(&at))
            .expect("a segment holding a linked address");
        (index as u8, at - segments[index].address)
    };
    let rebases = rebase_info(&linked.absolute, |at| {
        let (index, offset) = segment_of(at);
        assert!(
            segments[index as usize].protection & EXECUTE == 0,
            "an absolute address in a macOS image's code"
        );
        (index, offset)
    });
    let binds = bind_info(&linked.imports, segment_of);
    let exports = export_trie();
    let mut strings = vec![0];
    strings.extend_from_slice(HEADER_SYMBOL.as_bytes());
    strings.push(0);
    pad(&mut strings, 8);
    let mut symbols = Vec::new();
    symbols.extend_from_slice(&1u32.to_le_bytes());
    symbols.extend_from_slice(&[N_SECT_EXTERNAL, 1]);
    symbols.extend_from_slice(&REFERENCED_DYNAMICALLY.to_le_bytes());
    symbols.extend_from_slice(&IMAGE_BASE.to_le_bytes());
    let mut linkedit = Vec::new();
    let mut place = |bytes: &[u8]| {
        let at = file_end + linkedit.len() as u64;
        linkedit.extend_from_slice(bytes);
        pad(&mut linkedit, 8);
        (at as u32, bytes.len() as u32)
    };
    let rebase = place(&rebases);
    let bind = place(&binds);
    let export = place(&exports);
    let symtab = place(&symbols);
    let strtab = place(&strings);
    pad(&mut linkedit, 16);
    let signature_at = file_end + linkedit.len() as u64;
    let signature_size = signature_size(signature_at, name);
    segments.push(Segment {
        name: "__LINKEDIT",
        address,
        size: (linkedit.len() as u64 + signature_size).next_multiple_of(PAGE),
        offset: file_end,
        file_size: linkedit.len() as u64 + signature_size,
        protection: READ,
        flags: 0,
        sections: Vec::new(),
    });
    let mut commands = Vec::new();
    for segment in &segments {
        segment_command(&mut commands, segment);
    }
    command(&mut commands, LC_DYLD_INFO_ONLY, &{
        let mut words = vec![rebase.0, rebase.1, bind.0, bind.1, 0, 0, 0, 0];
        words.extend([export.0, export.1]);
        words
    });
    command(&mut commands, LC_SYMTAB, &[symtab.0, 1, strtab.0, strtab.1]);
    let mut dynamic = [0u32; 18];
    dynamic[3] = 1;
    dynamic[4] = 1;
    command(&mut commands, LC_DYSYMTAB, &dynamic);
    path_command(&mut commands, LC_LOAD_DYLINKER, &[], DYLINKER);
    let uuid_at = HEADER as usize + commands.len() + 8;
    command(&mut commands, LC_UUID, &[0; 4]);
    command(
        &mut commands,
        LC_BUILD_VERSION,
        &[PLATFORM_MACOS, MACOS_VERSION, MACOS_VERSION, 0],
    );
    let entry_offset = entry - IMAGE_BASE;
    command(
        &mut commands,
        LC_MAIN,
        &[entry_offset as u32, (entry_offset >> 32) as u32, 0, 0],
    );
    path_command(
        &mut commands,
        LC_LOAD_DYLIB,
        &[2, LIBSYSTEM_VERSION, LIBSYSTEM_VERSION],
        LIBSYSTEM,
    );
    command(
        &mut commands,
        LC_CODE_SIGNATURE,
        &[signature_at as u32, signature_size as u32],
    );
    assert!(
        HEADER + commands.len() as u64 <= PAGE,
        "a Mach-O image's load commands within its first page"
    );
    let (cpu, subtype) = match arch {
        Arch::Amd64 => (CPU_TYPE_X86_64, CPU_SUBTYPE_X86_64_ALL),
        Arch::Aarch64 => (CPU_TYPE_ARM64, CPU_SUBTYPE_ARM64_ALL),
    };
    let command_count = segments.len() as u32 + 9;
    let mut out = Vec::new();
    for word in [
        MH_MAGIC_64,
        cpu,
        subtype,
        MH_EXECUTE,
        command_count,
        commands.len() as u32,
        MH_FLAGS,
        0,
    ] {
        out.extend_from_slice(&word.to_le_bytes());
    }
    out.extend_from_slice(&commands);
    for (at, bytes) in contents {
        out.resize(at as usize, 0);
        out.extend_from_slice(bytes);
    }
    out.resize(file_end as usize, 0);
    out.extend_from_slice(&linkedit);
    let uuid = sha256(&out);
    out[uuid_at..uuid_at + 16].copy_from_slice(&uuid[..16]);
    let text = &segments[1];
    let signature = sign(&out, name, text.offset, text.file_size);
    out.extend_from_slice(&signature);
    out
}

/// Pads bytes with zeros to a multiple of `align`.
fn pad(bytes: &mut Vec<u8>, align: usize) {
    bytes.resize(bytes.len().next_multiple_of(align), 0);
}

/// Appends a load command of 32-bit words.
fn command(out: &mut Vec<u8>, kind: u32, words: &[u32]) {
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&(8 + 4 * words.len() as u32).to_le_bytes());
    for word in words {
        out.extend_from_slice(&word.to_le_bytes());
    }
}

/// Appends a load command of 32-bit words followed by a path, which the word after its size
/// locates, padded to 8 bytes.
fn path_command(out: &mut Vec<u8>, kind: u32, words: &[u32], path: &str) {
    let fixed = 12 + 4 * words.len();
    let size = (fixed + path.len() + 1).next_multiple_of(8);
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&(fixed as u32).to_le_bytes());
    for word in words {
        out.extend_from_slice(&word.to_le_bytes());
    }
    let start = out.len();
    out.extend_from_slice(path.as_bytes());
    out.resize(start + size - fixed, 0);
}

/// A name of 16 bytes, padded with zeros.
fn name16(name: &str) -> [u8; 16] {
    let mut bytes = [0; 16];
    bytes[..name.len()].copy_from_slice(name.as_bytes());
    bytes
}

/// Appends the load command of a segment and its sections.
fn segment_command(out: &mut Vec<u8>, segment: &Segment) {
    let size = SEGMENT_COMMAND + SECTION * segment.sections.len() as u64;
    out.extend_from_slice(&LC_SEGMENT_64.to_le_bytes());
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&name16(segment.name));
    for word in [
        segment.address,
        segment.size,
        segment.offset,
        segment.file_size,
    ] {
        out.extend_from_slice(&word.to_le_bytes());
    }
    for word in [
        segment.protection,
        segment.protection,
        segment.sections.len() as u32,
        segment.flags,
    ] {
        out.extend_from_slice(&word.to_le_bytes());
    }
    for section in &segment.sections {
        out.extend_from_slice(&name16(&section.name));
        out.extend_from_slice(&name16(segment.name));
        out.extend_from_slice(&section.address.to_le_bytes());
        out.extend_from_slice(&section.size.to_le_bytes());
        for word in [
            section.offset as u32,
            section.align,
            0,
            0,
            section.kind,
            0,
            0,
            0,
        ] {
            out.extend_from_slice(&word.to_le_bytes());
        }
    }
}

/// Appends an unsigned LEB128 number.
fn uleb(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// The rebasing information sliding each 8-byte absolute address, in address order, which
/// `segment_of` gives as a segment's index and an offset in it.
fn rebase_info(absolute: &[u64], segment_of: impl Fn(u64) -> (u8, u64)) -> Vec<u8> {
    let mut out = vec![REBASE_TYPE_POINTER];
    let mut cursor: Option<(u8, u64)> = None;
    for at in absolute {
        let (segment, offset) = segment_of(*at);
        match cursor {
            Some((current, next)) if current == segment && offset >= next => {
                if offset > next {
                    out.push(REBASE_ADD_ADDRESS);
                    uleb(&mut out, offset - next);
                }
            }
            _ => {
                out.push(REBASE_SEGMENT_AND_OFFSET | segment);
                uleb(&mut out, offset);
            }
        }
        out.push(REBASE_ONCE);
        cursor = Some((segment, offset + 8));
    }
    out.push(DONE);
    out
}

/// The binding information setting each imported word to the address of the `libSystem`
/// function it names.
fn bind_info(imports: &[(u64, String)], segment_of: impl Fn(u64) -> (u8, u64)) -> Vec<u8> {
    let mut out = vec![BIND_LIBSYSTEM, BIND_TYPE_POINTER];
    for (at, name) in imports {
        out.push(BIND_SYMBOL);
        out.push(b'_');
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        let (segment, offset) = segment_of(*at);
        out.push(BIND_SEGMENT_AND_OFFSET | segment);
        uleb(&mut out, offset);
        out.push(BIND_ONCE);
    }
    out.push(DONE);
    out
}

/// The export trie: the header's symbol at offset 0, a regular symbol.
fn export_trie() -> Vec<u8> {
    let mut out = vec![0, 1];
    out.extend_from_slice(HEADER_SYMBOL.as_bytes());
    out.push(0);
    let child = out.len() as u64 + 1;
    uleb(&mut out, child);
    out.extend_from_slice(&[2, 0, 0, 0]);
    out
}

/// The size of the code signature of the `code` bytes before it, identified by `name`.
fn signature_size(code: u64, name: &str) -> u64 {
    let pages = code.div_ceil(1 << SIGNATURE_PAGE_BITS);
    SUPER_BLOB + BLOB_INDEX + CODE_DIRECTORY + name.len() as u64 + 1 + 32 * pages
}

/// The ad-hoc code signature of an executable's bytes, identified by `name`, whose executable
/// segment lies at `text` for `text_size` bytes.
fn sign(code: &[u8], name: &str, text: u64, text_size: u64) -> Vec<u8> {
    let size = signature_size(code.len() as u64, name);
    let directory = SUPER_BLOB + BLOB_INDEX;
    let identifier = CODE_DIRECTORY;
    let hashes = identifier + name.len() as u64 + 1;
    let pages = code.chunks(1 << SIGNATURE_PAGE_BITS);
    let mut out = Vec::new();
    for word in [
        CSMAGIC_EMBEDDED_SIGNATURE,
        size as u32,
        1,
        0,
        directory as u32,
        CSMAGIC_CODEDIRECTORY,
        (size - directory) as u32,
        CODE_DIRECTORY_VERSION,
        CS_ADHOC_LINKER_SIGNED,
        hashes as u32,
        identifier as u32,
        0,
        pages.len() as u32,
        code.len() as u32,
    ] {
        out.extend_from_slice(&word.to_be_bytes());
    }
    out.extend_from_slice(&[32, CS_HASHTYPE_SHA256, 0, SIGNATURE_PAGE_BITS]);
    out.extend_from_slice(&[0; 16]);
    for word in [0, text, text_size, CS_EXECSEG_MAIN_BINARY] {
        out.extend_from_slice(&word.to_be_bytes());
    }
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    for page in pages {
        out.extend_from_slice(&sha256(page));
    }
    out
}

/// The round constants of SHA-256.
const ROUND: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// The SHA-256 digest of `bytes`.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut state: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    let mut padded = bytes.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend((bytes.len() as u64 * 8).to_be_bytes());
    for block in padded.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = state;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let choice = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(ROUND[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let majority = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(majority);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for i in 0..8 {
            state[i] = state[i].wrapping_add(v[i]);
        }
    }
    let mut digest = [0; 32];
    for (i, word) in state.iter().enumerate() {
        digest[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
    }
    digest
}
