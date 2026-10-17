//! Machine targets: the `x86_64-uefi` and `x86_64-bare` program entry seams, which Cranelift
//! cannot express, and the boot glue the seams call; its submodules hold the other compiler
//! stubs, the ports the descriptors name, the CPUs and interrupt delivery. Each seam and stub is
//! fixed machine code the implementation emits, with relocations the image writer resolves into
//! the image's own addresses.

mod cpus;
pub mod heap;
mod interrupts;
mod ports;
mod stubs;

pub use interrupts::{IDT_BYTES, binding, device, frame};
pub use stubs::Stub;

use cranelift_codegen::binemit::Reloc;
use cranelift_codegen::ir::{AbiParam, InstBuilder, Signature, TrapCode, Value, condcodes::IntCC};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module, ModuleReloc};

use super::body::Held;
use super::call::Invoked;
use super::fx::{Fx, PTR};
use super::runtime::{Routine, state, system};
use super::{Instance, Lowering, Program, lines};
use crate::nir::Role;
use crate::workspace::Target;

/// The program entry seam's symbol, which the image writer enters.
pub const ENTRY: &str = "_NSentry";

/// The least boot stack a machine image reserves, the stack UEFI guarantees an application's
/// calls into the firmware.
pub const BOOT_STACK: u64 = 128 * 1024;

/// A seam frame: the most stack a vector seam uses for the state it saves.
const SEAM_FRAME: u64 = 4_096;

/// The `x86_64-bare` entry seam's machine code, which the PVH boot protocol enters in 32-bit
/// protected mode: it loads the descriptor table and the entry root, enables SSE, switches to
/// long mode, sets the boot stack and calls the boot glue with the start-information pointer.
/// Its relocations, applied by the image writer, patch the addresses of the descriptor table's
/// pseudo-descriptor, of the entry root and of the 64-bit code the far jump enters, the boot
/// stack top and the call.
const BARE_SEAM: &[u8] = &[
    0xfa, 0xfc, 0x0f, 0x01, 0x15, 0x00, 0x00, 0x00, 0x00, 0xb8, 0x00, 0x00, 0x00, 0x00, 0x0f, 0x22,
    0xd8, 0x0f, 0x20, 0xe0, 0x0d, 0x20, 0x06, 0x00, 0x00, 0x0f, 0x22, 0xe0, 0xb9, 0x80, 0x00, 0x00,
    0xc0, 0x0f, 0x32, 0x0d, 0x00, 0x01, 0x00, 0x00, 0x0f, 0x30, 0x0f, 0x20, 0xc0, 0x83, 0xe0, 0xfb,
    0x0d, 0x03, 0x00, 0x00, 0x80, 0x0f, 0x22, 0xc0, 0xea, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x66,
    0xb8, 0x10, 0x00, 0x8e, 0xd8, 0x8e, 0xc0, 0x8e, 0xd0, 0x31, 0xc0, 0x8e, 0xe0, 0x8e, 0xe8, 0x48,
    0xbc, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x89, 0xdf, 0x31, 0xf6, 0xe8, 0x00, 0x00,
    0x00, 0x00, 0xf4, 0xeb, 0xfd,
];

/// The offset within `BARE_SEAM` of the 64-bit code the far jump enters.
const BARE_LONG_MODE: i64 = 0x3f;

/// The memory the entry root maps at its physical addresses: the first 4 GiB.
pub const MAPPED: u64 = 1 << 32;

/// The bits of a page-table entry: present, writable, a large page, and caching disabled.
pub const PRESENT: u64 = 0x1;
const WRITABLE: u64 = 0x2;
pub const LARGE: u64 = 0x80;
const UNCACHED: u64 = 0x18;

/// The bytes a 2 MiB page maps, and the first address the entry root maps uncached: the last
/// gigabyte below 4 GiB, where the memory-mapped devices lie.
const LARGE_PAGE: u64 = 2 * 1024 * 1024;
const DEVICES: u64 = 3 << 30;

/// The `x86_64-uefi` entry seam's machine code: already in long mode, it sets the boot stack and
/// calls the boot glue with the image handle and system table the firmware passed.
const UEFI_SEAM: &[u8] = &[
    0x48, 0xbc, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x48, 0x89, 0xcf, 0x48, 0x89, 0xd6,
    0xe8, 0x00, 0x00, 0x00, 0x00, 0xf4, 0xeb, 0xfd,
];

impl Lowering<'_, '_> {
    /// The boot stack a final build's image reserves: the larger of its entry function's and its
    /// entry landing fn's `stack N` and a seam frame, at least `BOOT_STACK`, in 16-byte units so
    /// that its top is aligned.
    pub fn boot_stack(&self, program: &Program) -> u64 {
        let own = [program.entry, program.landing]
            .iter()
            .filter_map(|item| self.ck.fn_sig(*item)?.limits.stack)
            .max()
            .unwrap_or(0);
        (own + SEAM_FRAME).max(BOOT_STACK).next_multiple_of(16)
    }

    /// Emits the program entry seam of the target, which calls `boot` on a boot stack of
    /// `boot_stack` bytes.
    pub fn entry_seam(&mut self, boot: FuncId, boot_stack: u64) {
        match self.ck.db.target {
            Target::Bare => self.bare_entry_seam(boot, boot_stack),
            _ => self.uefi_entry_seam(boot, boot_stack),
        }
    }

    /// Reserves an `x86_64-bare` image's boot stack of `boot_stack` bytes, defines its
    /// descriptor table, and emits the program entry seam, which calls `boot`.
    fn bare_entry_seam(&mut self, boot: FuncId, boot_stack: u64) {
        let root = self.entry_root();
        let stack = self.reserved_data("_NSboot_stack", boot_stack, 16);
        let gdtr = self.define_gdt();
        let seam = self
            .module
            .declare_function(ENTRY, Linkage::Export, &Signature::new(super::CALL_CONV))
            .expect("the program entry seam");
        let abs4 = |offset: u32, data: DataId, addend: i64| ModuleReloc {
            offset,
            kind: Reloc::Abs4,
            name: data.into(),
            addend,
        };
        let relocs = vec![
            abs4(0x05, gdtr, 0),
            abs4(0x0a, root, 0),
            ModuleReloc {
                offset: 0x39,
                kind: Reloc::Abs4,
                name: seam.into(),
                addend: BARE_LONG_MODE,
            },
            ModuleReloc {
                offset: 0x51,
                kind: Reloc::Abs8,
                name: stack.into(),
                addend: boot_stack as i64,
            },
            ModuleReloc {
                offset: 0x5e,
                kind: Reloc::X86PCRel4,
                name: boot.into(),
                addend: -4,
            },
        ];
        self.module
            .define_function_bytes(seam, 16, BARE_SEAM, &relocs)
            .expect("the bare entry seam");
    }

    /// Reserves an `x86_64-uefi` image's boot stack of `boot_stack` bytes and emits the program
    /// entry seam. The firmware enters it in long mode under `"win64"` with the image handle and
    /// system table, which it passes to `boot`.
    fn uefi_entry_seam(&mut self, boot: FuncId, boot_stack: u64) {
        let stack = self.reserved_data("_NSboot_stack", boot_stack, 16);
        let seam = self
            .module
            .declare_function(ENTRY, Linkage::Export, &Signature::new(super::CALL_CONV))
            .expect("the program entry seam");
        let relocs = vec![
            ModuleReloc {
                offset: 0x02,
                kind: Reloc::Abs8,
                name: stack.into(),
                addend: boot_stack as i64,
            },
            ModuleReloc {
                offset: 0x11,
                kind: Reloc::X86PCRel4,
                name: boot.into(),
                addend: -4,
            },
        ];
        self.module
            .define_function_bytes(seam, 16, UEFI_SEAM, &relocs)
            .expect("the uefi entry seam");
    }

    /// The boot glue a final build's entry seam calls with the two values the platform entered
    /// the image with: it records them, sets up the runtime, mints the root grant through the
    /// entry landing fn, which takes as many of the values as it has parameters, and calls the
    /// entry function with it.
    pub fn boot_glue(&mut self, program: &Program) -> FuncId {
        let mut signature = Signature::new(super::CALL_CONV);
        signature.params.push(AbiParam::new(PTR));
        signature.params.push(AbiParam::new(PTR));
        let id = self
            .module
            .declare_function("_NSboot", Linkage::Local, &signature)
            .expect("the boot glue");
        let landing = Instance::Nir {
            item: program.landing,
            role: Role::Body,
            index: 0,
            args: crate::check::ty::no_args(),
        };
        let main = Instance::Nir {
            item: program.entry,
            role: self.entry_role(program.entry),
            index: 0,
            args: crate::check::ty::no_args(),
        };
        self.define(id, signature, lines::HIDDEN | lines::CRITICAL, |lo, fx| {
            let params = fx.entry();
            let state_address = lo.state(fx);
            fx.store(params[0], state_address, state::PLATFORM);
            fx.store(params[1], state_address, state::PLATFORM_TABLE);
            let failed = lo.call_routine(fx, Routine::Init, &[])[0];
            let unavailable = fx.b.ins().icmp_imm_s(IntCC::NotEqual, failed, 0);
            fx.when(unavailable, |fx| system::unavailable(lo, fx));
            let landing_abi = lo.abi(&landing);
            let args: Vec<Held> = params
                .iter()
                .take(landing_abi.params.len())
                .map(|value| Held::Scalar(*value))
                .collect();
            let landing_id = lo.instance(landing.clone());
            let grant = lo
                .invoke(fx, Invoked::Direct(landing_id), &landing_abi, &args)
                .result;
            let main_abi = lo.abi(&main);
            let main_id = lo.instance(main.clone());
            lo.invoke(fx, Invoked::Direct(main_id), &main_abi, &[grant]);
            fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
        });
        id
    }

    /// The entry root, whose tables the image holds: a top-level table, a page-directory-pointer
    /// table and four page directories of 2 MiB pages mapping the first 4 GiB writable at their
    /// physical addresses, the last gigabyte uncached.
    pub fn entry_root(&mut self) -> DataId {
        const NAME: &str = "_NSentry_root";
        if let Some(id) = self.reserved.get(NAME) {
            return *id;
        }
        let id = self
            .module
            .declare_data(NAME, Linkage::Local, true, false)
            .expect("the entry root");
        let mut bytes = vec![0u8; 2 * 4_096];
        for page in 0..MAPPED / LARGE_PAGE {
            let address = page * LARGE_PAGE;
            let cached = if address >= DEVICES { UNCACHED } else { 0 };
            let entry = address | LARGE | WRITABLE | PRESENT | cached;
            bytes.extend_from_slice(&entry.to_le_bytes());
        }
        let mut description = DataDescription::new();
        description.define(bytes.into_boxed_slice());
        description.set_align(4_096);
        let own = self.module.declare_data_in_data(id, &mut description);
        let table = WRITABLE | PRESENT;
        description.write_data_addr(0, own, (4_096 + table) as i64);
        for directory in 0..4 {
            let entry = 4_096 + 8 * directory;
            let target = 2 * 4_096 + 4_096 * directory + table;
            description.write_data_addr(entry as u32, own, target as i64);
        }
        self.module
            .define_data(id, &description)
            .expect("the entry root");
        self.reserved.insert(NAME, id);
        id
    }

    /// The image's first address and last address plus one, which the image writer records.
    pub fn image_extent(&mut self, fx: &mut Fx) -> (Value, Value) {
        let id = match self.reserved.get(crate::image::IMAGE_EXTENT) {
            Some(id) => *id,
            None => {
                let id = self
                    .module
                    .declare_data(crate::image::IMAGE_EXTENT, Linkage::Local, true, false)
                    .expect("the image extent");
                let mut description = DataDescription::new();
                description.define(vec![0; 16].into_boxed_slice());
                description.set_align(8);
                self.module
                    .define_data(id, &description)
                    .expect("the image extent");
                self.reserved.insert(crate::image::IMAGE_EXTENT, id);
                id
            }
        };
        let extent = self.data_address(fx, id);
        (fx.load(PTR, extent, 0), fx.load(PTR, extent, 8))
    }

    /// The descriptor table's pseudo-descriptor, which `lgdt` loads: a null descriptor, a 64-bit
    /// privilege-0 code segment and a data segment, then a limit and the table's base address.
    fn define_gdt(&mut self) -> DataId {
        let table = self
            .module
            .declare_data("_NSgdt", Linkage::Local, false, false)
            .expect("the gdt");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&0x0020_9A00_0000_0000u64.to_le_bytes());
        bytes.extend_from_slice(&0x0000_9200_0000_0000u64.to_le_bytes());
        let mut description = DataDescription::new();
        description.define(bytes.into_boxed_slice());
        description.set_align(8);
        self.module
            .define_data(table, &description)
            .expect("the gdt");
        let gdtr = self
            .module
            .declare_data("_NSgdtr", Linkage::Local, false, false)
            .expect("the gdtr");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&23u16.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        let mut description = DataDescription::new();
        description.define(bytes.into_boxed_slice());
        description.set_align(8);
        let reference = self.module.declare_data_in_data(table, &mut description);
        description.write_data_addr(2, reference, 0);
        self.module
            .define_data(gdtr, &description)
            .expect("the gdtr");
        gdtr
    }
}
