use cranelift_codegen::binemit::Reloc;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_module::{DataDescription, Linkage, Module, ModuleReloc, ModuleRelocTarget};
use cranelift_object::{ObjectBuilder, ObjectModule};

use super::*;

/// An object module as lowering makes it for a machine target of the architecture of `triple`.
fn module(triple: &str) -> ObjectModule {
    let mut flags = settings::builder();
    flags.set("is_pic", "false").expect("a Cranelift setting");
    let isa = cranelift_codegen::isa::lookup_by_name(triple)
        .expect("the architecture's backend")
        .finish(settings::Flags::new(flags))
        .expect("the architecture's settings");
    let names = cranelift_module::default_libcall_names();
    ObjectModule::new(ObjectBuilder::new(isa, "test", names).expect("a builder"))
}

/// An object as lowering writes it for a machine target: `entry` calling `callee`, `table`
/// holding the address of `callee`, then 64 zeroed bytes.
fn object() -> Vec<u8> {
    let mut module = module("x86_64-unknown-none-elf");
    let signature = module.make_signature();
    let mut function = |name: &str| {
        module
            .declare_function(name, Linkage::Local, &signature)
            .expect("a function")
    };
    let (entry, callee) = (function("entry"), function("callee"));
    let relocation = ModuleReloc {
        offset: 1,
        kind: Reloc::X86PCRel4,
        name: callee.into(),
        addend: -4,
    };
    module
        .define_function_bytes(entry, 16, &[0xe8, 0, 0, 0, 0, 0xc3], &[relocation])
        .expect("the entry");
    module
        .define_function_bytes(callee, 16, &[0xc3], &[])
        .expect("the callee");
    let mut table = DataDescription::new();
    table.define(vec![0; 8].into_boxed_slice());
    let address = module.declare_func_in_data(callee, &mut table);
    table.write_function_addr(0, address);
    let mut zeroed = DataDescription::new();
    zeroed.define_zeroinit(64);
    zeroed.set_align(64);
    for (name, description) in [("table", table), ("zeroed", zeroed)] {
        let id = module
            .declare_data(name, Linkage::Local, true, false)
            .expect("a data object");
        module.define_data(id, &description).expect("a data object");
    }
    module.finish().emit().expect("an object")
}

/// The A64 instructions `aarch64_object` holds, their immediates 0: `adrp x0`, `add x0, x0`,
/// `bl` and `ret`.
const ADRP_X0: u32 = 0x9000_0000;
const ADD_X0: u32 = 0x9100_0000;
const BL: u32 = 0x9400_0000;
const RET: u32 = 0xD65F_03C0;

/// The bytes of A64 instructions.
fn code(instructions: &[u32]) -> Vec<u8> {
    instructions
        .iter()
        .flat_map(|instruction| instruction.to_le_bytes())
        .collect()
}

/// The AArch64 twin of `object`: `entry` forming the address of `table` plus 0x4008 with `adrp`
/// and `add`, `callee` calling `entry` with `bl`, and `table` holding the address of `callee`.
fn aarch64_object() -> Vec<u8> {
    let mut module = module("aarch64-unknown-none-elf");
    let signature = module.make_signature();
    let mut function = |name: &str| {
        module
            .declare_function(name, Linkage::Local, &signature)
            .expect("a function")
    };
    let (entry, callee) = (function("entry"), function("callee"));
    let table = module
        .declare_data("table", Linkage::Local, true, false)
        .expect("a data object");
    let relocation = |offset, kind, name: ModuleRelocTarget, addend| ModuleReloc {
        offset,
        kind,
        name,
        addend,
    };
    let page = relocation(0, Reloc::Aarch64AdrPrelPgHi21, table.into(), 0x4008);
    let low = relocation(4, Reloc::Aarch64AddAbsLo12Nc, table.into(), 0x4008);
    module
        .define_function_bytes(entry, 16, &code(&[ADRP_X0, ADD_X0, RET]), &[page, low])
        .expect("the entry");
    let call = relocation(0, Reloc::Arm64Call, entry.into(), 0);
    module
        .define_function_bytes(callee, 16, &code(&[BL, RET]), &[call])
        .expect("the callee");
    let mut description = DataDescription::new();
    description.define(vec![0; 8].into_boxed_slice());
    let address = module.declare_func_in_data(callee, &mut description);
    description.write_function_addr(0, address);
    module
        .define_data(table, &description)
        .expect("a data object");
    module.finish().emit().expect("an object")
}

#[test]
fn link_patches_relative_and_absolute_addresses_and_counts_zeroed_bytes_in_memory_only() {
    let base = 0x0010_0000;
    let linked = link(&object(), base, Imports::None);
    let [text, data] = &linked.segments[..] else {
        panic!("{} segments", linked.segments.len());
    };
    assert_eq!((text.address, text.access), (base, Access::Execute));
    let mut code = vec![0xe8, 11, 0, 0, 0, 0xc3];
    code.resize(16, 0);
    code.push(0xc3);
    assert_eq!(text.bytes, code);
    assert_eq!(
        (data.address, data.access, data.size),
        (base + 0x1000, Access::Write, 128)
    );
    assert_eq!(data.bytes, (base + 16).to_le_bytes());
    assert_eq!(linked.absolute, [base + 0x1000]);
}

#[test]
fn link_patches_aarch64_branches_pages_and_page_offsets() {
    let base = elf::base(workspace::Arch::Aarch64);
    let linked = link(&aarch64_object(), base, Imports::None);
    let [text, data] = &linked.segments[..] else {
        panic!("{} segments", linked.segments.len());
    };
    let five_pages = ADRP_X0 | 1 << 29 | 1 << 5;
    let back_four = BL | 0x03FF_FFFC;
    let patched = code(&[five_pages, ADD_X0 | 8 << 10, RET, 0, back_four, RET]);
    assert_eq!(text.bytes, patched);
    assert_eq!(data.bytes, (base + 16).to_le_bytes());
    assert_eq!(linked.absolute, [base + 0x1000]);
}

#[test]
fn link_lists_a_relocated_images_absolute_addresses_after_its_segments() {
    let mut module = module("aarch64-unknown-none-elf");
    let signature = module.make_signature();
    let entry = module
        .declare_function("entry", Linkage::Local, &signature)
        .expect("a function");
    module
        .define_function_bytes(entry, 16, &code(&[RET]), &[])
        .expect("the entry");
    for (name, writable, size) in [
        (RELOCATION_HEADER, false, RELOCATION_HEADER_BYTES),
        (IMAGE_EXTENT, true, 16),
    ] {
        let id = module
            .declare_data(name, Linkage::Local, writable, false)
            .expect("a data object");
        let mut description = DataDescription::new();
        description.define(vec![0; size].into_boxed_slice());
        module.define_data(id, &description).expect("a data object");
    }
    let table = module
        .declare_data("table", Linkage::Local, true, false)
        .expect("a data object");
    let mut description = DataDescription::new();
    description.define(vec![0; 8].into_boxed_slice());
    let address = module.declare_func_in_data(entry, &mut description);
    description.write_function_addr(0, address);
    module
        .define_data(table, &description)
        .expect("a data object");
    let base = elf::base(workspace::Arch::Aarch64);
    let linked = link(
        &module.finish().emit().expect("an object"),
        base,
        Imports::None,
    );
    let [_, header, data, listed] = &linked.segments[..] else {
        panic!("{} segments", linked.segments.len());
    };
    let words = |bytes: &[u8]| -> Vec<u64> {
        bytes
            .chunks(8)
            .map(|word| u64::from_le_bytes(word.try_into().expect("8 bytes")))
            .collect()
    };
    let places = &linked.absolute;
    assert_eq!(places.len(), 3, "the table's word and the extent's two");
    assert_eq!(
        (listed.address, listed.access),
        (data.address + 0x1000, Access::Read)
    );
    assert_eq!(&words(&listed.bytes), places);
    let end = listed.address + 24;
    assert_eq!(
        words(&header.bytes[..RELOCATION_HEADER_BYTES]),
        [header.address, base, end, listed.address, 3]
    );
}

#[test]
fn base_relocations_group_addresses_by_page_and_pad_odd_blocks() {
    let first: [u8; 16] = [
        0x00, 0x10, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x08, 0xa0, 0x10, 0xa0, 0xf8, 0xaf, 0x00,
        0x00,
    ];
    let second: [u8; 12] = [
        0x00, 0x30, 0x00, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x00, 0xa0, 0x00, 0x00,
    ];
    assert_eq!(
        pe::base_relocations(&[0x1008, 0x1010, 0x1ff8, 0x3000]),
        [&first[..], &second].concat()
    );
}

#[test]
fn a_console_program_imports_each_function_from_its_library_through_its_table() {
    let mut module = module("x86_64-unknown-none-elf");
    let signature = module.make_signature();
    let entry = module
        .declare_function("entry", Linkage::Local, &signature)
        .expect("a function");
    let mut relocations = Vec::new();
    let imports = [
        "GetStdHandle",
        "ExitProcess",
        "bcryptprimitives.dll!ProcessPrng",
    ];
    for (index, function) in imports.into_iter().enumerate() {
        let slot = module
            .declare_data(
                &format!("{IMPORT_PREFIX}{function}"),
                Linkage::Import,
                false,
                false,
            )
            .expect("an import");
        relocations.push(ModuleReloc {
            offset: 2 + 10 * index as u32,
            kind: Reloc::Abs8,
            name: slot.into(),
            addend: 0,
        });
    }
    let mut code = Vec::new();
    for _ in imports {
        code.extend_from_slice(&[0x48, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0]);
    }
    code.push(0xc3);
    module
        .define_function_bytes(entry, 16, &code, &relocations)
        .expect("the entry");
    let object = module.finish().emit().expect("an object");
    let base = pe::base(pe::Program::Console);
    let linked = link(&object, base, Imports::Table);
    let [text, table] = &linked.segments[..] else {
        panic!("{} segments", linked.segments.len());
    };
    assert_eq!(
        (table.address, table.access, table.size),
        (base + 0x1000, Access::Write, 40)
    );
    let functions: Vec<(u64, &str)> = linked
        .imports
        .iter()
        .map(|(address, name)| (*address, name.as_str()))
        .collect();
    assert_eq!(
        functions,
        [
            (table.address, "bcryptprimitives.dll!ProcessPrng"),
            (table.address + 16, "kernel32.dll!ExitProcess"),
            (table.address + 24, "kernel32.dll!GetStdHandle")
        ]
    );
    for (at, slot) in [(2, 24), (12, 16), (22, 0)] {
        assert_eq!(text.bytes[at..at + 8], (table.address + slot).to_le_bytes());
    }
    assert_eq!(linked.absolute, [base + 2, base + 12, base + 22]);
    let image = pe::write(&linked, base, workspace::Arch::Amd64, pe::Program::Console);
    let u32_at = |at: usize| u32::from_le_bytes(image[at..at + 4].try_into().expect("four bytes"));
    let optional = 0x40 + 4 + 20;
    assert_eq!(image[optional + 68], 3, "the console subsystem");
    let directory = |index: usize| {
        (
            u32_at(optional + 112 + 8 * index),
            u32_at(optional + 112 + 8 * index + 4),
        )
    };
    let rva = |address: u64| (address - base + 0x1000) as u32;
    let (entries, entries_size) = directory(1);
    assert_eq!(directory(12), (rva(table.address), 40));
    assert_eq!(entries_size, 60);
    let sections = optional + 240;
    let idata = sections + 40;
    assert_eq!(&image[idata..idata + 6], b".idata");
    let file_offset = |rva: u32| (rva - u32_at(idata + 12) + u32_at(idata + 20)) as usize;
    let text_at = |rva: u32| {
        let at = file_offset(rva);
        let end = at
            + image[at..]
                .iter()
                .position(|byte| *byte == 0)
                .expect("a NUL");
        String::from_utf8_lossy(&image[at..end]).into_owned()
    };
    let libraries = [
        ("bcryptprimitives.dll", &["ProcessPrng"][..], 0),
        ("kernel32.dll", &["ExitProcess", "GetStdHandle"][..], 16),
    ];
    for (index, (library, names, first)) in libraries.into_iter().enumerate() {
        let entry_at = file_offset(entries) + 20 * index;
        assert_eq!(text_at(u32_at(entry_at + 12)), library);
        let lookup = file_offset(u32_at(entry_at));
        let named: Vec<String> = (0..names.len())
            .map(|index| text_at(u32_at(lookup + 8 * index) + 2))
            .collect();
        assert_eq!(named, names);
        assert_eq!(
            u32_at(lookup + 8 * names.len()),
            0,
            "the lookup table's end"
        );
        assert_eq!(u32_at(entry_at + 16), rva(table.address + first));
    }
    assert_eq!(
        u32_at(file_offset(entries) + 40),
        0,
        "the terminating entry"
    );
}

#[test]
fn a_console_program_holds_the_objects_unallocated_sections_after_its_relocations() {
    let mut module = module("x86_64-unknown-none-elf");
    let signature = module.make_signature();
    let entry = module
        .declare_function("entry", Linkage::Local, &signature)
        .expect("a function");
    let slot = module
        .declare_data(
            &format!("{IMPORT_PREFIX}ExitProcess"),
            Linkage::Import,
            false,
            false,
        )
        .expect("an import");
    let relocation = ModuleReloc {
        offset: 2,
        kind: Reloc::Abs8,
        name: slot.into(),
        addend: 0,
    };
    let code = [0x48, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0xc3];
    module
        .define_function_bytes(entry, 16, &code, &[relocation])
        .expect("the entry");
    let mut product = module.finish();
    for (name, bytes) in [(".nan", &b"artefacts"[..]), (".nan.tree", &b"tree"[..])] {
        let id = product.object.add_section(
            Vec::new(),
            name.as_bytes().to_vec(),
            cranelift_object::object::SectionKind::Other,
        );
        product.object.append_section_data(id, bytes, 1);
    }
    let object = product.emit().expect("an object");
    let base = pe::base(pe::Program::Console);
    let linked = link(&object, base, Imports::Table);
    let image = pe::write(&linked, base, workspace::Arch::Amd64, pe::Program::Console);
    let u32_at = |at: usize| u32::from_le_bytes(image[at..at + 4].try_into().expect("four bytes"));
    let count = u16::from_le_bytes([image[0x46], image[0x47]]) as usize;
    let optional = 0x40 + 4 + 20;
    let headers: Vec<(&[u8], u32, u32, u32, u32)> = (0..count)
        .map(|index| {
            let at = optional + 240 + 40 * index;
            let name = image[at..at + 8]
                .split(|byte| *byte == 0)
                .next()
                .expect("a name");
            (
                name,
                u32_at(at + 8),
                u32_at(at + 12),
                u32_at(at + 20),
                u32_at(at + 36),
            )
        })
        .collect();
    let [.., relocations, artefacts, tree] = &headers[..] else {
        panic!("{count} sections");
    };
    assert_eq!(relocations.0, b".reloc");
    let expected: [(&[u8], &[u8]); 2] = [(b".nan", b"artefacts"), (b".nantree", b"tree")];
    for (&(name, size, rva, offset, flags), (wanted, bytes)) in
        [artefacts, tree].into_iter().zip(expected)
    {
        assert_eq!(name, wanted);
        assert_eq!(&image[offset as usize..(offset + size) as usize], bytes);
        assert_eq!((rva % 0x1000, flags), (0, 0x4200_0040));
    }
    assert!(artefacts.2 > relocations.2 && tree.2 > artefacts.2);
    assert_eq!(u32_at(optional + 56), (tree.2 + 4).next_multiple_of(0x1000));
}
