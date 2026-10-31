//! Machine targets: the program entry seams of the UEFI and bare targets, which Cranelift
//! cannot express, and the boot glue the seams call; its submodules hold the other compiler
//! stubs, the ports the descriptors name, the CPUs, a kernel's clock and scheduling, interrupt
//! delivery with the devices installed on it and the AArch64 ITS, the IOMMU's invalidations and
//! interrupt remapping, and the CPU signal. Each seam and stub is fixed machine code the
//! implementation emits, with relocations the image writer resolves into the image's own
//! addresses.

mod aarch64;
mod acpi;
mod bindings;
mod clock;
mod cpus;
mod discovery;
mod handoff;
pub mod heap;
mod interrupts;
mod iommu;
mod its;
mod memory;
mod ports;
pub mod schedule;
mod signal;
pub mod start;
mod stubs;

pub use bindings::{device, msi};
pub use interrupts::IDT_BYTES;
pub use stubs::Stub;

use cranelift_codegen::binemit::Reloc;
use cranelift_codegen::ir::{AbiParam, InstBuilder, Signature, TrapCode, Value};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module, ModuleReloc};

use super::body::Held;
use super::call::Invoked;
use super::data::Reserved;
use super::fx::{Fx, PTR};
use super::runtime::{state, system};
use super::{Instance, Lowering, Program, lines};
use crate::nir::Role;
use crate::workspace::{Arch, Target};

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

/// The memory the entry root's own tables map at its physical addresses: the first 4 GiB.
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

/// The bytes a 1 GiB block descriptor maps, and the entries of an AArch64 translation table.
const AARCH64_BLOCK: u64 = 1 << 30;
pub const TABLE_ENTRIES: u64 = 512;

/// The bits of an AArch64 descriptor: a valid descriptor, a table rather than a block, and the
/// access flag; a block of normal write-back inner-shareable memory, and one of device memory
/// that neither EL1 nor EL0 runs code from, of the memory-attribute indices the entry seams
/// program into `MAIR_EL1`.
pub const A64_VALID: u64 = 0x1;
pub const A64_TABLE: u64 = 0x2;
pub const A64_ACCESS: u64 = 0x400;
pub const A64_NORMAL_BLOCK: u64 = A64_VALID | A64_ACCESS | 0x300 | (2 << 2);
pub const A64_DEVICE_BLOCK: u64 = A64_VALID | A64_ACCESS | (3 << 53);

/// The `x86_64-uefi` entry seam's machine code: already in long mode, it sets the boot stack and
/// calls the boot glue with the image handle and system table the firmware passed.
const UEFI_SEAM: &[u8] = &[
    0x48, 0xbc, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x48, 0x89, 0xcf, 0x48, 0x89, 0xd6,
    0xe8, 0x00, 0x00, 0x00, 0x00, 0xf4, 0xeb, 0xfd,
];

/// The `aarch64-bare` entry seam's machine code, entered at EL1 or EL2 with the MMU off.
/// Entered at EL2, its first 29 instructions, which the secondary seam begins with too, make
/// EL1 run in AArch64 with the counters, the GIC CPU interface's system registers and the
/// floating-point registers untrapped and no virtual counter offset, its `MIDR_EL1` and
/// `MPIDR_EL1` the processor's and its `SCTLR_EL1` with the MMU, caches and alignment checks
/// off, and return to the code after them at EL1 with every exception masked. It enables
/// floating point and Advanced SIMD and makes `x0` `0x4000_0000` when `x0` and `x1` are both
/// zero. From the relocation header, whose address an `adrp` and `add` form, it takes the
/// distance the image was loaded from its linked address. It maps each gigabyte the image spans
/// as normal memory in the boot root, also formed by an `adrp` and `add`, invalidating the
/// cache lines it writes, and points the boot root's level-0 entry at its level-1 table. It
/// loads the boot root, the memory attributes and the translation control, which walks the boot
/// root for the low addresses and no table for the high ones, its physical address size the
/// processor's up to 48 bits, and turns the MMU and caches on. It then adds that distance to
/// each absolute address the header's table lists, sets the boot stack, and calls the boot glue
/// with `x0` and `x1`. Its literal pool, after the code, holds the memory attributes, the
/// translation control without its physical address size, the bits of a normal 1 GiB block, the
/// boot stack's top and the boot glue's address; the image writer patches the last two.
const AARCH64_BARE_SEAM: &[u8] = &[
    0x49, 0x42, 0x38, 0xd5, 0x3f, 0x21, 0x00, 0xf1, 0x61, 0x03, 0x00, 0x54, 0x09, 0x00, 0xb0, 0xd2,
    0x09, 0x11, 0x1c, 0xd5, 0x09, 0xe1, 0x3c, 0xd5, 0x29, 0x05, 0x40, 0xb2, 0x09, 0xe1, 0x1c, 0xd5,
    0x7f, 0xe0, 0x1c, 0xd5, 0xa9, 0xc9, 0x3c, 0xd5, 0x2a, 0x01, 0x80, 0xd2, 0x29, 0x01, 0x0a, 0xaa,
    0xa9, 0xc9, 0x1c, 0xd5, 0xe9, 0x7f, 0x86, 0xd2, 0x49, 0x11, 0x1c, 0xd5, 0x7f, 0x11, 0x1c, 0xd5,
    0x09, 0x00, 0x38, 0xd5, 0x09, 0x00, 0x1c, 0xd5, 0xa9, 0x00, 0x38, 0xd5, 0xa9, 0x00, 0x1c, 0xd5,
    0x09, 0x00, 0x81, 0xd2, 0x09, 0x1a, 0xa6, 0xf2, 0x09, 0x10, 0x18, 0xd5, 0xa9, 0x78, 0x80, 0xd2,
    0x09, 0x40, 0x1c, 0xd5, 0x89, 0x00, 0x00, 0x10, 0x29, 0x40, 0x1c, 0xd5, 0xdf, 0x3f, 0x03, 0xd5,
    0xe0, 0x03, 0x9f, 0xd6, 0x49, 0x10, 0x38, 0xd5, 0x29, 0x05, 0x6c, 0xb2, 0x49, 0x10, 0x18, 0xd5,
    0xdf, 0x3f, 0x03, 0xd5, 0x60, 0x00, 0x00, 0xb5, 0x41, 0x00, 0x00, 0xb5, 0x00, 0x00, 0xa8, 0xd2,
    0x02, 0x00, 0x00, 0x90, 0x42, 0x00, 0x00, 0x91, 0x43, 0x00, 0x40, 0xf9, 0x43, 0x00, 0x03, 0xcb,
    0x4b, 0x04, 0x40, 0xf9, 0x6b, 0x01, 0x03, 0x8b, 0x4c, 0x08, 0x40, 0xf9, 0x8c, 0x01, 0x03, 0x8b,
    0x8c, 0x05, 0x00, 0xd1, 0x6b, 0xfd, 0x5e, 0xd3, 0x8c, 0xfd, 0x5e, 0xd3, 0x0d, 0x00, 0x00, 0x90,
    0xad, 0x01, 0x00, 0x91, 0xae, 0x05, 0x40, 0x91, 0xcf, 0x05, 0x40, 0xb2, 0xaf, 0x01, 0x00, 0xf9,
    0x2d, 0x76, 0x08, 0xd5, 0xaf, 0x06, 0x00, 0x58, 0x7f, 0x01, 0x08, 0xf1, 0x22, 0x01, 0x00, 0x54,
    0x70, 0x85, 0x62, 0xd3, 0x10, 0x02, 0x0f, 0xaa, 0xd1, 0x0d, 0x0b, 0x8b, 0x30, 0x02, 0x00, 0xf9,
    0x31, 0x76, 0x08, 0xd5, 0x6b, 0x05, 0x00, 0x91, 0x7f, 0x01, 0x0c, 0xeb, 0xe9, 0xfe, 0xff, 0x54,
    0x9f, 0x3f, 0x03, 0xd5, 0x0d, 0x20, 0x18, 0xd5, 0x89, 0x04, 0x00, 0x58, 0x09, 0xa2, 0x18, 0xd5,
    0x89, 0x04, 0x00, 0x58, 0x0a, 0x07, 0x38, 0xd5, 0x4a, 0x0d, 0x40, 0x92, 0x5f, 0x15, 0x00, 0xf1,
    0xab, 0x00, 0x80, 0xd2, 0x4a, 0x91, 0x8b, 0x9a, 0x29, 0x81, 0x0a, 0xaa, 0x49, 0x20, 0x18, 0xd5,
    0x1f, 0x87, 0x08, 0xd5, 0x9f, 0x37, 0x03, 0xd5, 0xdf, 0x3f, 0x03, 0xd5, 0x09, 0x10, 0x38, 0xd5,
    0xaa, 0x00, 0x82, 0xd2, 0x29, 0x01, 0x0a, 0xaa, 0x09, 0x10, 0x18, 0xd5, 0xdf, 0x3f, 0x03, 0xd5,
    0x83, 0x01, 0x00, 0xb4, 0x44, 0x0c, 0x40, 0xf9, 0x84, 0x00, 0x03, 0x8b, 0x45, 0x10, 0x40, 0xf9,
    0x05, 0x01, 0x00, 0xb4, 0x86, 0x84, 0x40, 0xf8, 0xc6, 0x00, 0x03, 0x8b, 0xc7, 0x00, 0x40, 0xf9,
    0xe7, 0x00, 0x03, 0x8b, 0xc7, 0x00, 0x00, 0xf9, 0xa5, 0x04, 0x00, 0xd1, 0xf9, 0xff, 0xff, 0x17,
    0x89, 0x01, 0x00, 0x58, 0x3f, 0x01, 0x00, 0x91, 0x89, 0x01, 0x00, 0x58, 0x20, 0x01, 0x3f, 0xd6,
    0x7f, 0x20, 0x03, 0xd5, 0xff, 0xff, 0xff, 0x17, 0x00, 0x44, 0xff, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x10, 0x35, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x09, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// The offsets within `AARCH64_BARE_SEAM` of the `adrp` and `add` forming the relocation
/// header's address and the boot root's, and of the literal-pool entries the image writer
/// patches: the boot stack's top and the boot glue's address.
const AARCH64_SEAM_RELOCATIONS: u32 = 0x90;
const AARCH64_SEAM_ROOT: u32 = 0xbc;
const AARCH64_SEAM_STACK: u32 = 0x1b0;
const AARCH64_SEAM_BOOT: u32 = 0x1b8;

/// The `aarch64-uefi` entry seam's machine code: the firmware calls it in the Procedure Call
/// Standard with the image handle in `x0` and the system table in `x1`, under the MMU it set up;
/// it enables floating point and Advanced SIMD, sets the boot stack, and calls the boot glue with
/// the two values. Its literal pool holds the boot stack's top and the boot glue's address.
const AARCH64_UEFI_SEAM: &[u8] = &[
    0x49, 0x10, 0x38, 0xd5, 0x29, 0x05, 0x6c, 0xb2, 0x49, 0x10, 0x18, 0xd5, 0xdf, 0x3f, 0x03, 0xd5,
    0xc9, 0x00, 0x00, 0x58, 0x3f, 0x01, 0x00, 0x91, 0xc9, 0x00, 0x00, 0x58, 0x20, 0x01, 0x3f, 0xd6,
    0x7f, 0x20, 0x03, 0xd5, 0xff, 0xff, 0xff, 0x17, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// The offsets within `AARCH64_UEFI_SEAM` of the boot stack's top and the boot glue's address.
const AARCH64_UEFI_STACK: u32 = 0x28;
const AARCH64_UEFI_BOOT: u32 = 0x30;

/// The AArch64 secondary entry seam's machine code, which `PSCI CPU_ON` enters at EL1 or EL2
/// with the MMU off, the start record's address in `x0`. Entered at EL2, it drops to EL1 as the
/// boot seam does, by the same instructions. It enables floating point and Advanced SIMD,
/// installs the entry root and turns the MMU and caches on as the boot seam does, sets the
/// stack the record names, and calls the record's glue with the record's CPU record. The record
/// holds the entry root, the stack's top, the glue's address and the CPU record, which it reads
/// before the MMU is on, from identity-mapped memory.
const AARCH64_SECONDARY_SEAM: &[u8] = &[
    0x49, 0x42, 0x38, 0xd5, 0x3f, 0x21, 0x00, 0xf1, 0x61, 0x03, 0x00, 0x54, 0x09, 0x00, 0xb0, 0xd2,
    0x09, 0x11, 0x1c, 0xd5, 0x09, 0xe1, 0x3c, 0xd5, 0x29, 0x05, 0x40, 0xb2, 0x09, 0xe1, 0x1c, 0xd5,
    0x7f, 0xe0, 0x1c, 0xd5, 0xa9, 0xc9, 0x3c, 0xd5, 0x2a, 0x01, 0x80, 0xd2, 0x29, 0x01, 0x0a, 0xaa,
    0xa9, 0xc9, 0x1c, 0xd5, 0xe9, 0x7f, 0x86, 0xd2, 0x49, 0x11, 0x1c, 0xd5, 0x7f, 0x11, 0x1c, 0xd5,
    0x09, 0x00, 0x38, 0xd5, 0x09, 0x00, 0x1c, 0xd5, 0xa9, 0x00, 0x38, 0xd5, 0xa9, 0x00, 0x1c, 0xd5,
    0x09, 0x00, 0x81, 0xd2, 0x09, 0x1a, 0xa6, 0xf2, 0x09, 0x10, 0x18, 0xd5, 0xa9, 0x78, 0x80, 0xd2,
    0x09, 0x40, 0x1c, 0xd5, 0x89, 0x00, 0x00, 0x10, 0x29, 0x40, 0x1c, 0xd5, 0xdf, 0x3f, 0x03, 0xd5,
    0xe0, 0x03, 0x9f, 0xd6, 0x49, 0x10, 0x38, 0xd5, 0x29, 0x05, 0x6c, 0xb2, 0x49, 0x10, 0x18, 0xd5,
    0xdf, 0x3f, 0x03, 0xd5, 0x09, 0x00, 0x40, 0xf9, 0x09, 0x20, 0x18, 0xd5, 0x09, 0x80, 0x88, 0xd2,
    0xe9, 0x1f, 0xa0, 0xf2, 0x09, 0xa2, 0x18, 0xd5, 0x09, 0xa2, 0x86, 0xd2, 0x09, 0x10, 0xa0, 0xf2,
    0x0a, 0x07, 0x38, 0xd5, 0x4a, 0x0d, 0x40, 0x92, 0x5f, 0x15, 0x00, 0xf1, 0xab, 0x00, 0x80, 0xd2,
    0x4a, 0x91, 0x8b, 0x9a, 0x29, 0x81, 0x0a, 0xaa, 0x49, 0x20, 0x18, 0xd5, 0x1f, 0x87, 0x08, 0xd5,
    0x9f, 0x37, 0x03, 0xd5, 0xdf, 0x3f, 0x03, 0xd5, 0x09, 0x10, 0x38, 0xd5, 0xaa, 0x00, 0x82, 0xd2,
    0x29, 0x01, 0x0a, 0xaa, 0x09, 0x10, 0x18, 0xd5, 0xdf, 0x3f, 0x03, 0xd5, 0x09, 0x04, 0x40, 0xf9,
    0x3f, 0x01, 0x00, 0x91, 0x0a, 0x08, 0x40, 0xf9, 0x00, 0x0c, 0x40, 0xf9, 0x40, 0x01, 0x3f, 0xd6,
    0x7f, 0x20, 0x03, 0xd5, 0xff, 0xff, 0xff, 0x17,
];

/// The offsets of a secondary start record's fields: the entry root, the stack's top, the glue's
/// address and the CPU record, as `AARCH64_SECONDARY_SEAM` reads them.
pub mod secondary {
    pub const ROOT: u64 = 0;
    pub const STACK: u64 = 8;
    pub const GLUE: u64 = 16;
    pub const CPU: u64 = 24;
    pub const SIZE: u64 = 32;
}

/// The secondary start record.
const SECONDARY_RECORD: Reserved = Reserved {
    name: "_NSsecondary_record",
    size: secondary::SIZE,
    align: 16,
};

/// The boot stack of `size` bytes.
fn boot_stack_data(size: u64) -> Reserved {
    Reserved {
        name: "_NSboot_stack",
        size,
        align: 16,
    }
}

/// The data a kernel's runtime reserves that a bundle's code reaches, which a kernel exports.
pub fn shared_data(arch: Arch) -> Vec<Reserved> {
    let mut shared = vec![cpus::CPUS, bindings::IRQ_BINDINGS, heap::HEAP_STATE];
    match arch {
        Arch::Amd64 => shared.push(interrupts::IDT),
        Arch::Aarch64 => shared.extend([its::IRQ_LPIS, its::ITS_DEVICES, discovery::MACHINE]),
    }
    shared
}

impl Lowering<'_, '_> {
    /// Switches task stacks: saves the running stack's execution to the context at `store`,
    /// restores the one at `load` and resumes it with `payload`. Cranelift lowers the switch on
    /// x86_64; on AArch64, which Cranelift's backend cannot, a compiler stub does, keeping the
    /// floating-point registers host code keeps across calls.
    pub fn switch_stacks(&mut self, fx: &mut Fx, store: Value, load: Value, payload: Value) {
        match self.ck.db.target.arch() {
            Some(Arch::Aarch64) => {
                let stub = match self.machine() {
                    true => Stub::MachineSwitch,
                    false => Stub::Switch,
                };
                self.call_stub(fx, stub, &[store, load, payload]);
            }
            _ => {
                fx.b.ins().stack_switch(store, load, payload);
            }
        }
    }

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
            Target::X64Bare => self.bare_entry_seam(boot, boot_stack),
            Target::Aarch64Bare => self.aarch64_bare_entry_seam(boot, boot_stack),
            Target::Aarch64Uefi => self.aarch64_uefi_entry_seam(boot, boot_stack),
            _ => self.uefi_entry_seam(boot, boot_stack),
        }
    }

    /// Reserves an `aarch64-uefi` image's boot stack and emits the program entry seam, which the
    /// firmware calls with the image handle and system table and which calls `boot`.
    fn aarch64_uefi_entry_seam(&mut self, boot: FuncId, boot_stack: u64) {
        let stack = self.reserved_data(boot_stack_data(boot_stack));
        let seam = self
            .module
            .declare_function(ENTRY, Linkage::Export, &Signature::new(super::CALL_CONV))
            .expect("the program entry seam");
        let relocs = vec![
            ModuleReloc {
                offset: AARCH64_UEFI_STACK,
                kind: Reloc::Abs8,
                name: stack.into(),
                addend: boot_stack as i64,
            },
            ModuleReloc {
                offset: AARCH64_UEFI_BOOT,
                kind: Reloc::Abs8,
                name: boot.into(),
                addend: 0,
            },
        ];
        self.module
            .define_function_bytes(seam, 16, AARCH64_UEFI_SEAM, &relocs)
            .expect("the aarch64 uefi entry seam");
    }

    /// Reserves an `aarch64-bare` image's boot root, relocation header and boot stack and emits
    /// the program entry seam, which the platform enters with the MMU off and which calls `boot`
    /// with the device tree's address and the start information's.
    fn aarch64_bare_entry_seam(&mut self, boot: FuncId, boot_stack: u64) {
        let root = self.aarch64_boot_root();
        let relocations = self
            .module
            .declare_data(
                crate::image::RELOCATION_HEADER,
                Linkage::Local,
                false,
                false,
            )
            .expect("the relocation header");
        let mut description = DataDescription::new();
        description.define(vec![0; crate::image::RELOCATION_HEADER_BYTES].into_boxed_slice());
        description.set_align(8);
        self.module
            .define_data(relocations, &description)
            .expect("the relocation header");
        let stack = self.reserved_data(boot_stack_data(boot_stack));
        let seam = self
            .module
            .declare_function(ENTRY, Linkage::Export, &Signature::new(super::CALL_CONV))
            .expect("the program entry seam");
        let data = |offset: u32, kind: Reloc, data: DataId, addend: i64| ModuleReloc {
            offset,
            kind,
            name: data.into(),
            addend,
        };
        let relocs = vec![
            data(
                AARCH64_SEAM_RELOCATIONS,
                Reloc::Aarch64AdrPrelPgHi21,
                relocations,
                0,
            ),
            data(
                AARCH64_SEAM_RELOCATIONS + 4,
                Reloc::Aarch64AddAbsLo12Nc,
                relocations,
                0,
            ),
            data(AARCH64_SEAM_ROOT, Reloc::Aarch64AdrPrelPgHi21, root, 0),
            data(AARCH64_SEAM_ROOT + 4, Reloc::Aarch64AddAbsLo12Nc, root, 0),
            data(AARCH64_SEAM_STACK, Reloc::Abs8, stack, boot_stack as i64),
            ModuleReloc {
                offset: AARCH64_SEAM_BOOT,
                kind: Reloc::Abs8,
                name: boot.into(),
                addend: 0,
            },
        ];
        self.module
            .define_function_bytes(seam, 16, AARCH64_BARE_SEAM, &relocs)
            .expect("the aarch64 bare entry seam");
    }

    /// The boot root the `aarch64-bare` entry seam installs, a 4 KiB-granule level-0 table and a
    /// level-1 table mapping the first 512 GiB in 1 GiB blocks of device memory that no code runs
    /// from. The seam points the level-0 table's first entry at the level-1 table and maps the
    /// gigabytes the image spans as normal memory, so that the table holds no absolute address.
    fn aarch64_boot_root(&mut self) -> DataId {
        let id = self
            .module
            .declare_data("_NSboot_root", Linkage::Local, true, false)
            .expect("the boot root");
        let mut bytes = vec![0u8; 4_096];
        for gigabyte in 0..TABLE_ENTRIES {
            let entry = (gigabyte * AARCH64_BLOCK) | A64_DEVICE_BLOCK;
            bytes.extend_from_slice(&entry.to_le_bytes());
        }
        let mut description = DataDescription::new();
        description.define(bytes.into_boxed_slice());
        description.set_align(4_096);
        self.module
            .define_data(id, &description)
            .expect("the boot root");
        id
    }

    /// Emits the AArch64 secondary entry seam, which `PSCI CPU_ON` enters, and gives its
    /// symbol so the starting of the other CPUs may name its address. It reads everything from the
    /// start record passed in `x0`, so it needs no relocations.
    pub fn aarch64_secondary_seam(&mut self) -> FuncId {
        let seam = self
            .module
            .declare_function(
                "_NSsecondary",
                Linkage::Local,
                &Signature::new(super::CALL_CONV),
            )
            .expect("the secondary entry seam");
        self.module
            .define_function_bytes(seam, 16, AARCH64_SECONDARY_SEAM, &[])
            .expect("the aarch64 secondary entry seam");
        seam
    }

    /// The address of a machine image's single secondary start record, which the boot CPU fills
    /// before it starts each other CPU.
    pub fn secondary_record(&mut self, fx: &mut Fx) -> Value {
        self.reserved(fx, SECONDARY_RECORD)
    }

    /// Reserves an `x86_64-bare` image's boot stack of `boot_stack` bytes, defines its
    /// descriptor table, and emits the program entry seam, which calls `boot`.
    fn bare_entry_seam(&mut self, boot: FuncId, boot_stack: u64) {
        let root = self.entry_root();
        let stack = self.reserved_data(boot_stack_data(boot_stack));
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
        let stack = self.reserved_data(boot_stack_data(boot_stack));
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
            lo.start_boot(fx, &params);
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

    /// The start of a boot glue: checks the image's codegen level, records the two values the
    /// platform entered the image with, then sets up the runtime, ending with status 71 when the
    /// level or the runtime cannot start.
    pub fn start_boot(&mut self, fx: &mut Fx, params: &[Value]) {
        self.check_level(fx);
        let state_address = self.state(fx);
        fx.store(params[0], state_address, state::PLATFORM);
        fx.store(params[1], state_address, state::PLATFORM_TABLE);
        system::start(self, fx);
    }

    /// The entry root, in the format of the target's architecture: on x86_64 the image's page
    /// tables mapping the first 4 GiB writable at their physical addresses, which the entry seam
    /// or `exit_services` installs; on AArch64 tables that `Init` on a bare target and
    /// `exit_services` on a UEFI target fill and install.
    pub fn entry_root(&mut self) -> DataId {
        match self.ck.db.target.arch() {
            Some(Arch::Aarch64) => self.aarch64_entry_root(),
            _ => self.x64_entry_root(),
        }
    }

    /// The x86_64 entry root: a top-level table, a page-directory-pointer table and four page
    /// directories of 2 MiB pages mapping the first 4 GiB writable at their physical addresses,
    /// the last gigabyte uncached, and in a kernel exporting the load ABI the top-level entry of
    /// the load window.
    fn x64_entry_root(&mut self) -> DataId {
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
        if let Some((window, bits)) = self.window_root_entry() {
            let reference = self.module.declare_data_in_data(window, &mut description);
            let entry = 8 * super::load::WINDOW_ENTRY;
            description.write_data_addr(entry as u32, reference, bits);
        }
        self.module
            .define_data(id, &description)
            .expect("the entry root");
        self.reserved.insert(NAME, id);
        id
    }

    /// The AArch64 entry root, a 4 KiB-granule level-0 table pointing at a level-1 table, which
    /// `Init` on a bare target and `exit_services` on a UEFI target fill.
    fn aarch64_entry_root(&mut self) -> DataId {
        const NAME: &str = "_NSentry_root";
        if let Some(id) = self.reserved.get(NAME) {
            return *id;
        }
        let id = self
            .module
            .declare_data(NAME, Linkage::Local, true, false)
            .expect("the entry root");
        let mut description = DataDescription::new();
        description.define(vec![0u8; 2 * 4_096].into_boxed_slice());
        description.set_align(4_096);
        let own = self.module.declare_data_in_data(id, &mut description);
        description.write_data_addr(0, own, (4_096 + A64_VALID + A64_TABLE) as i64);
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

#[cfg(test)]
mod tests;
