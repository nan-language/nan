use cranelift_codegen::binemit::Reloc;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_module::{DataDescription, Linkage, Module, ModuleReloc};
use cranelift_object::{ObjectBuilder, ObjectModule};

use super::*;

/// An object as lowering writes it for a machine target: `entry` calling `callee` through a
/// relocation of the kind given, `table` holding the address of `callee`, then 64 zeroed bytes.
fn object(call: Reloc) -> Vec<u8> {
    let mut flags = settings::builder();
    flags.set("is_pic", "false").expect("a Cranelift setting");
    let isa = cranelift_codegen::isa::lookup_by_name("x86_64-unknown-none-elf")
        .expect("the x86-64 backend")
        .finish(settings::Flags::new(flags))
        .expect("the x86-64 settings");
    let names = cranelift_module::default_libcall_names();
    let mut module = ObjectModule::new(ObjectBuilder::new(isa, "test", names).expect("a builder"));
    let signature = module.make_signature();
    let mut function = |name: &str| {
        module
            .declare_function(name, Linkage::Local, &signature)
            .expect("a function")
    };
    let (entry, callee) = (function("entry"), function("callee"));
    let relocation = ModuleReloc {
        offset: 1,
        kind: call,
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

#[test]
fn link_patches_relative_and_absolute_addresses_and_counts_zeroed_bytes_in_memory_only() {
    let base = 0x0010_0000;
    let linked = link(&object(Reloc::X86PCRel4), base);
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
    assert_eq!(linked.absolute, [(base + 0x1000, 8)]);
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
#[should_panic(expected = "32-bit absolute address")]
fn uefi_images_reject_32_bit_absolute_addresses() {
    write(Target::Uefi, &object(Reloc::Abs4), "entry");
}
