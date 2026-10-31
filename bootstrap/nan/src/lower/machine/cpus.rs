//! The processors of a machine image: their records, which the GS base addresses on x86_64 and
//! `TPIDR_EL1` on AArch64; the CPUs the platform reports, through the ACPI MADT or PSCI, the boot
//! CPU first; and the starting of the others, through a trampoline the anchor copies below 1 MiB
//! or through PSCI.

use cranelift_codegen::ir::{
    AbiParam, InstBuilder, MemFlagsData, Signature, TrapCode, Value, condcodes::IntCC, types,
};
use cranelift_module::{DataDescription, FuncId, Linkage, Module};

use super::super::Lowering;
use super::super::fx::{Fx, PTR, unaligned};
use super::super::runtime::memory::CHUNK;
use super::super::runtime::system::{self, unavailable_unless};
use super::super::runtime::tasks::thread;
use super::super::runtime::{Routine, report, state};
use super::stubs::Stub;
use crate::workspace::{Arch, Kind, Target};

/// The most CPUs a machine image runs; the platform's others stay stopped.
pub const MAX_CPUS: u64 = 64;

/// The PSCI functions the AArch64 machine layer calls through the hypervisor conduit: the power
/// state of a processor, which discovers the CPUs, and powering one on; and the largest status a
/// query gives for a processor that exists — on, off or on-pending — an affinity no processor
/// answers for giving a negative status instead.
const PSCI_AFFINITY_INFO: u64 = 0xC400_0004;
const PSCI_CPU_ON: u64 = 0xC400_0003;
const PSCI_PRESENT_MAX: i64 = 2;

/// The offsets of a CPU record's fields: its executor thread's record, which the `thread` stub
/// reads; the vector its running interrupt fn serves; the top and lowest address of its
/// interrupt stack, which the vector seam reads; its hardware id, its local APIC's id on x86_64 and
/// its affinity on AArch64; whether it has started its executor; its global descriptor table, the
/// pseudo-descriptor loading it, and its task-state segment; the page-table root
/// `hw::Ring0.write_cr3` or `write_ttbr0` last installed, zero for the entry root;
/// whether it has AVX enabled, which the vector seam reads to save the extended state; the
/// frame the vector seam saved for the interrupt it serves, zero outside one; on x86_64, the
/// features the `CpuFeatures` stub records; on a kernel, the nanosecond of its timer's next
/// pacing tick and the ticks its timer counts in a millisecond; whether it waits in
/// `hw::Ring0.wait_for_interrupt`, which no preemption interrupts; and whether a shootdown
/// another CPU posted awaits it.
pub mod cpu {
    pub const THREAD: u64 = 0;
    pub const VECTOR: u64 = 8;
    pub const STACK_TOP: u64 = 16;
    pub const STACK_BASE: u64 = 24;
    pub const HARDWARE_ID: u64 = 32;
    pub const STARTED: u64 = 40;
    pub const GDT: u64 = 48;
    pub const GDTR: u64 = 104;
    pub const TSS: u64 = 120;
    pub const ROOT: u64 = 224;
    pub const AVX: u64 = 232;
    pub const FRAME: u64 = 240;
    pub const FEATURES: u64 = 248;
    pub const NEXT_PACE: u64 = 256;
    pub const TIMER_RATE: u64 = 264;
    pub const WAITING: u64 = 272;
    pub const SHOOTDOWN: u64 = 280;
    pub const SIZE: u64 = 320;
}

/// The model-specific register holding the GS base.
const GS_BASE: u32 = 0xC000_0101;

/// The descriptors of a CPU's global descriptor table before its task-state segment's: a null
/// descriptor, 64-bit code and data segments of privilege level 0, and data and 64-bit code
/// segments of privilege level 3.
const DESCRIPTORS: [u64; 5] = [
    0,
    0x0020_9A00_0000_0000,
    0x0000_9200_0000_0000,
    0x0000_F200_0000_0000,
    0x0020_FA00_0000_0000,
];

/// The task-state segment's size, the offset of its first interrupt stack table entry and that
/// of its I/O permission map's base, set past its end so it has none.
const TSS_SIZE: u64 = 104;
const TSS_IST: u64 = 36;
const TSS_IO_MAP: u64 = 102;

impl Lowering<'_, '_> {
    /// The address of the CPU records.
    pub fn cpu_records(&mut self, fx: &mut Fx) -> Value {
        self.reserved(fx, "_NScpus", MAX_CPUS * cpu::SIZE, 64)
    }

    /// The record of CPU `index`.
    pub fn cpu_record(&mut self, fx: &mut Fx, index: Value) -> Value {
        let cpus = self.cpu_records(fx);
        let offset = fx.b.ins().imul_imm_s(index, cpu::SIZE as i64);
        fx.b.ins().iadd(cpus, offset)
    }

    /// The number of the CPU running the caller.
    pub fn running_index(&mut self, fx: &mut Fx) -> Value {
        let thread_record = self.thread(fx);
        fx.load(PTR, thread_record, thread::INDEX)
    }

    /// The record of the CPU running the caller.
    pub fn running_cpu(&mut self, fx: &mut Fx) -> Value {
        let index = self.running_index(fx);
        self.cpu_record(fx, index)
    }

    /// A machine image's part of `Init`: the CPU records, the boot CPU's first, which its base
    /// register addresses from now on; the boot CPU's data-independent timing mode; on a kernel,
    /// its clock started and a CPU for each processor the platform reports; an executor thread
    /// record for each CPU; on x86_64, the boot CPU's features recorded; and, on `x86_64-bare`,
    /// the boot CPU's descriptor tables loaded and AVX enabled when it has it.
    pub fn machine_init(&mut self, fx: &mut Fx) {
        let boot = self.cpu_records(fx);
        self.set_cpu_base(fx, boot);
        self.call_stub(fx, Stub::ConstantTime, &[]);
        if self.kernel() {
            self.start_clock(fx);
        }
        let count = match (self.kernel(), self.ck.db.target.arch()) {
            (true, Some(Arch::Amd64)) => self.discover(fx, boot),
            (true, Some(Arch::Aarch64)) => self.aarch64_discover(fx, boot),
            _ => fx.ptr(1),
        };
        let state_address = self.state(fx);
        fx.store(count, state_address, state::THREAD_COUNT);
        let words = fx.b.ins().imul_imm_s(count, 8);
        let threads = self.record_alloc(fx, words);
        unavailable_unless(fx, threads);
        fx.store(threads, state_address, state::THREADS);
        let zero = fx.ptr(0);
        fx.each(zero, count, |fx, index| {
            let size = fx.ptr(thread::SIZE);
            let record = self.record_alloc(fx, size);
            unavailable_unless(fx, record);
            fx.store(index, record, thread::INDEX);
            let slot = fx.b.ins().imul_imm_s(index, 8);
            let slot = fx.b.ins().iadd(threads, slot);
            fx.store(record, slot, 0);
            let cpu = self.cpu_record(fx, index);
            fx.store(record, cpu, cpu::THREAD);
        });
        if self.ck.db.target.arch() == Some(Arch::Amd64) {
            self.call_stub(fx, Stub::CpuFeatures, &[zero]);
        }
        if self.ck.db.target == Target::X64Bare {
            self.load_cpu_tables(fx, boot);
            self.enable_avx(fx, boot);
        }
    }

    /// Enables AVX on the running CPU when it has it, recording whether in the CPU's record.
    pub fn enable_avx(&mut self, fx: &mut Fx, record: Value) {
        let enabled = self.call_stub(fx, Stub::EnableAvx, &[])[0];
        fx.store(enabled, record, cpu::AVX);
    }

    /// Writes a CPU's global descriptor table, task-state segment and pseudo-descriptor into its
    /// record, then loads them on the running CPU.
    pub fn load_cpu_tables(&mut self, fx: &mut Fx, record: Value) {
        for (index, descriptor) in DESCRIPTORS.iter().enumerate() {
            let value = fx.ptr(*descriptor);
            fx.store(value, record, cpu::GDT + 8 * index as u64);
        }
        let tss = fx.offset(record, cpu::TSS);
        let limit = fx.ptr(TSS_SIZE - 1);
        let low_base = fx.b.ins().band_imm_s(tss, 0xFF_FFFF);
        let low_base = fx.b.ins().ishl_imm_s(low_base, 16);
        let high_byte = fx.b.ins().ushr_imm_s(tss, 24);
        let high_byte = fx.b.ins().band_imm_s(high_byte, 0xFF);
        let high_byte = fx.b.ins().ishl_imm_s(high_byte, 56);
        let present = fx.ptr(0x89 << 40);
        let low = fx.b.ins().bor(limit, low_base);
        let low = fx.b.ins().bor(low, high_byte);
        let low = fx.b.ins().bor(low, present);
        fx.store(low, record, cpu::GDT + 40);
        let upper = fx.b.ins().ushr_imm_s(tss, 32);
        fx.store(upper, record, cpu::GDT + 48);
        let io_map = fx.int(types::I16, TSS_SIZE as i64);
        fx.store(io_map, tss, TSS_IO_MAP);
        let gdt_limit = fx.int(types::I16, 7 * 8 - 1);
        fx.store(gdt_limit, record, cpu::GDTR);
        let gdt = fx.offset(record, cpu::GDT);
        fx.b.ins()
            .store(unaligned(), gdt, record, (cpu::GDTR + 2) as i32);
        let gdtr = fx.offset(record, cpu::GDTR);
        self.call_stub(fx, Stub::LoadTables, &[gdtr]);
    }

    /// Installs `root` as the running CPU's page-table root, with maskable interrupts disabled so
    /// that its task stays on the CPU, and gives the root it replaces, zero for the entry root.
    /// The root is the x86_64 `cr3` or the AArch64 `TTBR0_EL1`, each installed by its stub, which
    /// receives `tagged` instead when given: the root with a PCID or ASID, whose translations
    /// stay cached.
    pub fn swap_root(&mut self, fx: &mut Fx, root: Value, tagged: Option<Value>) -> Value {
        let flags = self.call_stub(fx, Stub::IrqSave, &[])[0];
        let record = self.running_cpu(fx);
        let replaced = fx.load(PTR, record, cpu::ROOT);
        fx.store(root, record, cpu::ROOT);
        let install = match (self.ck.db.target.arch(), tagged) {
            (Some(Arch::Aarch64), Some(_)) => Stub::WriteTtbr0Tagged,
            (Some(Arch::Aarch64), None) => Stub::WriteTtbr0,
            _ => Stub::WriteCr3,
        };
        self.call_stub(fx, install, &[tagged.unwrap_or(root)]);
        self.call_stub(fx, Stub::IrqRestore, &[flags]);
        replaced
    }

    /// Points the running CPU's base register, the x86_64 `GS` base or the AArch64 `TPIDR_EL1`,
    /// at its CPU record, from where the `Thread` stub reads its executor thread.
    pub fn set_cpu_base(&mut self, fx: &mut Fx, record: Value) {
        match self.ck.db.target.arch() {
            Some(Arch::Aarch64) => {
                self.call_stub(fx, Stub::SetCpu, &[record]);
            }
            _ => self.write_msr(fx, GS_BASE, record),
        }
    }

    /// Points interrupt stack table entry `ist` of a CPU's task-state segment at a stack top.
    pub fn set_interrupt_stack(&mut self, fx: &mut Fx, record: Value, ist: u8, top: Value) {
        let at = cpu::TSS + TSS_IST + 8 * u64::from(ist - 1);
        fx.b.ins().store(unaligned(), top, record, at as i32);
    }

    /// The number of CPUs the board provides, at least 1 and at most `MAX_CPUS`, recording each
    /// one's affinity in its record: the boot CPU, affinity zero, first, then every consecutive
    /// affinity a processor answers a power-state query for. The board numbers its processors from
    /// zero without gaps, so each affinity is its CPU's index.
    fn aarch64_discover(&mut self, fx: &mut Fx, boot: Value) -> Value {
        let zero = fx.ptr(0);
        fx.store(zero, boot, cpu::HARDWARE_ID);
        let one = fx.ptr(1);
        let count = fx.var(PTR, one);
        fx.repeat(&[one], |fx, at| {
            let affinity = at[0];
            let room =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedLessThan, affinity, MAX_CPUS as i64);
            let present = fx.choose_in(
                self,
                room,
                &[types::I8],
                |lo, fx| {
                    let function = fx.ptr(PSCI_AFFINITY_INFO);
                    let level = fx.ptr(0);
                    let pad = fx.ptr(0);
                    let status = lo.call_stub(fx, Stub::Hvc, &[function, affinity, level, pad])[0];
                    let present = fx.b.ins().icmp_imm_s(
                        IntCC::UnsignedLessThanOrEqual,
                        status,
                        PSCI_PRESENT_MAX,
                    );
                    fx.when(present, |fx| {
                        let counted = fx.b.use_var(count);
                        let record = lo.cpu_record(fx, counted);
                        fx.store(affinity, record, cpu::HARDWARE_ID);
                        let counted = fx.b.ins().iadd_imm_s(counted, 1);
                        fx.b.def_var(count, counted);
                    });
                    vec![present]
                },
                |_, fx| vec![fx.int(types::I8, 0)],
            )[0];
            let next = fx.b.ins().iadd_imm_s(affinity, 1);
            (present, vec![next])
        });
        fx.b.use_var(count)
    }

    /// The number of CPUs the platform reports as usable, at least 1 and at most `MAX_CPUS`,
    /// recording each one's local APIC id in its record: the boot CPU's first, then the others
    /// in the order of the MADT.
    fn discover(&mut self, fx: &mut Fx, boot: Value) -> Value {
        let own = self.local_apic_id(fx);
        fx.store(own, boot, cpu::HARDWARE_ID);
        let one = fx.ptr(1);
        let count = fx.var(PTR, one);
        let rsdp = self.rsdp(fx);
        let madt = self.madt(fx, rsdp);
        let found = fx.b.ins().icmp_imm_s(IntCC::NotEqual, madt, 0);
        fx.when(found, |fx| {
            let length = load_u32(fx, madt, 4);
            let end = fx.b.ins().iadd(madt, length);
            let first = fx.b.ins().iadd_imm_s(madt, MADT_ENTRIES as i64);
            fx.repeat(&[first], |fx, at| {
                let at = at[0];
                let header = fx.b.ins().iadd_imm_s(at, 2);
                let inside = fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, header, end);
                let next = fx.choose_in(
                    self,
                    inside,
                    &[PTR, types::I8],
                    |lo, fx| {
                        let kind = fx.b.ins().load(types::I8, unaligned(), at, 0);
                        let len = fx.b.ins().load(types::I8, unaligned(), at, 1);
                        let len = fx.b.ins().uextend(PTR, len);
                        let processor = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, 0);
                        fx.when(processor, |fx| {
                            let flags = load_u32(fx, at, 4);
                            let usable = fx.b.ins().band_imm_s(flags, PROCESSOR_ENABLED);
                            let usable = fx.b.ins().icmp_imm_s(IntCC::NotEqual, usable, 0);
                            let id = fx.b.ins().load(types::I8, unaligned(), at, 3);
                            let id = fx.b.ins().uextend(PTR, id);
                            let other = fx.b.ins().icmp(IntCC::NotEqual, id, own);
                            let counted = fx.b.use_var(count);
                            let room = fx.b.ins().icmp_imm_s(
                                IntCC::UnsignedLessThan,
                                counted,
                                MAX_CPUS as i64,
                            );
                            let taken = fx.b.ins().band(usable, other);
                            let taken = fx.b.ins().band(taken, room);
                            fx.when(taken, |fx| {
                                let counted = fx.b.use_var(count);
                                let record = lo.cpu_record(fx, counted);
                                fx.store(id, record, cpu::HARDWARE_ID);
                                let counted = fx.b.ins().iadd_imm_s(counted, 1);
                                fx.b.def_var(count, counted);
                            });
                        });
                        let next = fx.b.ins().iadd(at, len);
                        let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, len, 0);
                        vec![next, more]
                    },
                    |_, fx| vec![at, fx.int(types::I8, 0)],
                );
                (next[1], vec![next[0]])
            });
        });
        fx.b.use_var(count)
    }

    /// The address of the ACPI root system description pointer, zero when the platform gives
    /// none: on `x86_64-bare` the one the start information names, or else the one found in the
    /// BIOS area; on `x86_64-uefi` the one the firmware's configuration table names.
    fn rsdp(&mut self, fx: &mut Fx) -> Value {
        let state_address = self.state(fx);
        if self.ck.db.target.kind() == Some(Kind::Uefi) {
            let table = fx.load(PTR, state_address, state::PLATFORM_TABLE);
            return self.configured_rsdp(fx, table);
        }
        let info = fx.load(PTR, state_address, state::PLATFORM);
        let magic = load_u32(fx, info, 0);
        let valid = fx.b.ins().icmp_imm_s(IntCC::Equal, magic, START_INFO_MAGIC);
        let given = fx.choose(
            valid,
            &[PTR],
            |fx| vec![fx.load(PTR, info, START_INFO_RSDP)],
            |fx| vec![fx.ptr(0)],
        )[0];
        let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, given, 0);
        fx.choose(missing, &[PTR], |fx| vec![scan_rsdp(fx)], |_| vec![given])[0]
    }

    /// The RSDP a UEFI system table's configuration table names, preferring the ACPI 2.0 entry.
    fn configured_rsdp(&mut self, fx: &mut Fx, table: Value) -> Value {
        let count = fx.load(PTR, table, SYSTEM_TABLE_ENTRIES);
        let entries = fx.load(PTR, table, SYSTEM_TABLE_CONFIGURATION);
        let zero = fx.ptr(0);
        let found = fx.var(PTR, zero);
        for guid in [ACPI_10_TABLE, ACPI_20_TABLE] {
            fx.each(zero, count, |fx, index| {
                let entry = fx.b.ins().imul_imm_s(index, CONFIGURATION_ENTRY as i64);
                let entry = fx.b.ins().iadd(entries, entry);
                let low = fx.load(PTR, entry, 0);
                let high = fx.load(PTR, entry, 8);
                let low = fx.b.ins().icmp_imm_s(IntCC::Equal, low, guid.0 as i64);
                let high = fx.b.ins().icmp_imm_s(IntCC::Equal, high, guid.1 as i64);
                let same = fx.b.ins().band(low, high);
                fx.when(same, |fx| {
                    let pointer = fx.load(PTR, entry, 16);
                    fx.b.def_var(found, pointer);
                });
            });
        }
        fx.b.use_var(found)
    }

    /// The MADT the RSDP's XSDT or RSDT lists, zero when none.
    fn madt(&mut self, fx: &mut Fx, rsdp: Value) -> Value {
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, rsdp, 0);
        fx.choose(
            present,
            &[PTR],
            |fx| {
                let revision = fx.b.ins().load(types::I8, unaligned(), rsdp, 15);
                let extended =
                    fx.b.ins()
                        .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, revision, 2);
                let xsdt = fx.choose(
                    extended,
                    &[PTR],
                    |fx| {
                        let xsdt = fx.b.ins().load(PTR, unaligned(), rsdp, 24);
                        vec![xsdt]
                    },
                    |fx| vec![fx.ptr(0)],
                )[0];
                let wide = fx.b.ins().icmp_imm_s(IntCC::NotEqual, xsdt, 0);
                let rsdt = load_u32(fx, rsdp, 16);
                let table = fx.b.ins().select(wide, xsdt, rsdt);
                let eight = fx.ptr(8);
                let four = fx.ptr(4);
                let width = fx.b.ins().select(wide, eight, four);
                let length = load_u32(fx, table, 4);
                let listed = fx.b.ins().iadd_imm_s(length, -(SDT_HEADER as i64));
                let count = fx.b.ins().udiv(listed, width);
                let zero = fx.ptr(0);
                let found = fx.var(PTR, zero);
                fx.each(zero, count, |fx, index| {
                    let offset = fx.b.ins().imul(index, width);
                    let at = fx.b.ins().iadd(table, offset);
                    let at = fx.b.ins().iadd_imm_s(at, SDT_HEADER as i64);
                    let entry = fx.choose(
                        wide,
                        &[PTR],
                        |fx| vec![fx.b.ins().load(PTR, unaligned(), at, 0)],
                        |fx| vec![load_u32(fx, at, 0)],
                    )[0];
                    let signature = load_u32(fx, entry, 0);
                    let apic =
                        fx.b.ins()
                            .icmp_imm_s(IntCC::Equal, signature, MADT_SIGNATURE);
                    fx.when(apic, |fx| fx.b.def_var(found, entry));
                });
                vec![fx.b.use_var(found)]
            },
            |fx| vec![fx.ptr(0)],
        )[0]
    }
}

/// The magic number of a PVH start information structure and the offset of its RSDP address.
const START_INFO_MAGIC: i64 = 0x336E_C578;
const START_INFO_RSDP: u64 = 32;

/// The offsets in a UEFI system table of its configuration table's entry count and address, the
/// size of an entry, a GUID then the table's address, and the GUIDs of the ACPI tables, each as
/// its two little-endian words.
const SYSTEM_TABLE_ENTRIES: u64 = 104;
const SYSTEM_TABLE_CONFIGURATION: u64 = 112;
const CONFIGURATION_ENTRY: u64 = 24;
const ACPI_10_TABLE: (u64, u64) = (0x11D3_2D88_EB9D_2D30, 0x4DC1_3F27_9000_169A);
const ACPI_20_TABLE: (u64, u64) = (0x11D3_E4F1_8868_E871, 0x8188_3CC7_8000_22BC);

/// The size of an ACPI table's header, the MADT's signature, and the offset of its first entry.
const SDT_HEADER: u64 = 36;
const MADT_SIGNATURE: i64 = 0x4349_5041;
const MADT_ENTRIES: u64 = 44;

/// The flag of a MADT processor entry saying its processor is enabled and ready for use.
const PROCESSOR_ENABLED: i64 = 1;

/// The BIOS area the RSDP may lie in, on a 16-byte boundary, and its signature.
const BIOS_AREA: (u64, u64) = (0xE_0000, 0x10_0000);
const RSDP_SIGNATURE: u64 = 0x2052_5450_2044_5352;

/// The RSDP found in the BIOS area, zero when none.
fn scan_rsdp(fx: &mut Fx) -> Value {
    let start = fx.ptr(BIOS_AREA.0);
    let zero = fx.ptr(0);
    let found = fx.repeat(&[start, zero], |fx, state| {
        let (at, _) = (state[0], state[1]);
        let word = fx.load(PTR, at, 0);
        let signature = fx.ptr(RSDP_SIGNATURE);
        let same = fx.b.ins().icmp(IntCC::Equal, word, signature);
        let zero = fx.ptr(0);
        let found = fx.b.ins().select(same, at, zero);
        let next = fx.b.ins().iadd_imm_s(at, 16);
        let inside =
            fx.b.ins()
                .icmp_imm_s(IntCC::UnsignedLessThan, next, BIOS_AREA.1 as i64);
        let missing = fx.b.ins().bxor_imm_s(same, 1);
        let more = fx.b.ins().band(inside, missing);
        (more, vec![next, found])
    });
    found[1]
}

/// The physical page the trampoline starting the other CPUs is copied to, whose page number the
/// startup IPI carries; each started CPU's executor runs on a stack of one chunk.
const TRAMPOLINE: u64 = 0x8000;

/// The trampoline, assembled to run at `TRAMPOLINE`: in real mode it loads its own descriptor
/// table and enters protected mode; there it enables PAE and SSE, loads the page-table root its
/// parameters hold, enables long mode and paging and jumps to 64-bit code, which sets the stack
/// its parameters hold and calls their start glue with their CPU record. Its parameters follow
/// it: the root, the stack top, the record and the glue.
const TRAMPOLINE_CODE: &[u8] = &[
    0xfa, 0xfc, 0x31, 0xc0, 0x8e, 0xd8, 0x66, 0x0f, 0x01, 0x16, 0xa0, 0x80, 0x0f, 0x20, 0xc0, 0x0c,
    0x01, 0x0f, 0x22, 0xc0, 0x66, 0xea, 0x1c, 0x80, 0x00, 0x00, 0x08, 0x00, 0x66, 0xb8, 0x10, 0x00,
    0x8e, 0xd8, 0x8e, 0xc0, 0x8e, 0xd0, 0x0f, 0x20, 0xe0, 0x0d, 0x20, 0x06, 0x00, 0x00, 0x0f, 0x22,
    0xe0, 0xa1, 0xa8, 0x80, 0x00, 0x00, 0x0f, 0x22, 0xd8, 0xb9, 0x80, 0x00, 0x00, 0xc0, 0x0f, 0x32,
    0x0d, 0x00, 0x01, 0x00, 0x00, 0x0f, 0x30, 0x0f, 0x20, 0xc0, 0x83, 0xe0, 0xfb, 0x0d, 0x02, 0x00,
    0x00, 0x80, 0x0f, 0x22, 0xc0, 0xea, 0x5c, 0x80, 0x00, 0x00, 0x18, 0x00, 0x48, 0x8b, 0x24, 0x25,
    0xb0, 0x80, 0x00, 0x00, 0x48, 0x8b, 0x3c, 0x25, 0xb8, 0x80, 0x00, 0x00, 0x48, 0x8b, 0x04, 0x25,
    0xc0, 0x80, 0x00, 0x00, 0xff, 0xd0, 0xfa, 0xf4, 0xeb, 0xfc, 0x66, 0x0f, 0x1f, 0x44, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0x00, 0x00, 0x00, 0x9a, 0xcf, 0x00,
    0xff, 0xff, 0x00, 0x00, 0x00, 0x92, 0xcf, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x9a, 0x20, 0x00,
    0x1f, 0x00, 0x80, 0x80, 0x00, 0x00, 0x90, 0x90,
];

/// The offsets of the trampoline's parameters.
mod start {
    pub const ROOT: u64 = 0xa8;
    pub const STACK: u64 = 0xb0;
    pub const CPU: u64 = 0xb8;
    pub const GLUE: u64 = 0xc0;
}

// The trampoline's code addresses its descriptor table and its parameters at these addresses,
// and the `Thread` and `LoadTables` stubs read the record and select the descriptors as these
// constants place them.
const _: () = assert!(TRAMPOLINE == 0x8000 && start::ROOT == 0xa8 && start::GLUE == 0xc0);
const _: () = assert!(cpu::THREAD == 0 && DESCRIPTORS.len() == 5);

/// The startup IPI tries before a CPU counts as not starting, and the pauses each waits.
const START_TRIES: u64 = 2;
const START_WAIT: u64 = 2_000_000;

impl Lowering<'_, '_> {
    /// Starts every CPU other than the boot CPU on the entry root, through PSCI on AArch64 and
    /// otherwise through the trampoline: each prepares its tables and interrupt delivery through
    /// `prepare` and starts its executor, after which the boot CPU starts the next one. A CPU that
    /// does not start within its tries ends the sequence, keeping the trampoline's parameters, and
    /// the CPUs after it stay stopped.
    pub fn start_cpus(&mut self, fx: &mut Fx, prepare: FuncId) {
        if self.ck.db.target.arch() == Some(Arch::Aarch64) {
            self.aarch64_start_cpus(fx, prepare);
            return;
        }
        let state_address = self.state(fx);
        let count = fx.load(PTR, state_address, state::THREAD_COUNT);
        let several = fx.b.ins().icmp_imm_s(IntCC::UnsignedGreaterThan, count, 1);
        fx.when(several, |fx| {
            let code = self.trampoline_code();
            let code = self.data_address(fx, code);
            let page = fx.ptr(TRAMPOLINE);
            let len = fx.ptr(TRAMPOLINE_CODE.len() as u64);
            self.call_routine(fx, Routine::Copy, &[page, code, len]);
            let root = self.entry_root();
            let root = self.data_address(fx, root);
            fx.store(root, page, start::ROOT);
            let glue = self.cpu_glue(prepare);
            let glue = self.func_ref(fx, glue);
            let glue = fx.b.ins().func_addr(PTR, glue);
            fx.store(glue, page, start::GLUE);
            let one = fx.ptr(1);
            fx.repeat(&[one], |fx, state| {
                let index = state[0];
                let record = self.cpu_record(fx, index);
                let chunk = fx.ptr(1);
                let stack = system::map_chunks(self, fx, chunk);
                let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, stack, 0);
                fx.when(missing, |fx| report::raise_kind(self, fx, "trap.exhausted"));
                let top = fx.b.ins().iadd_imm_s(stack, CHUNK as i64);
                fx.store(top, page, start::STACK);
                fx.store(record, page, start::CPU);
                let id = fx.load(PTR, record, cpu::HARDWARE_ID);
                self.send_ipi(fx, id, super::interrupts::IPI_INIT);
                self.spin(fx, START_WAIT);
                let tries = fx.ptr(START_TRIES);
                let no = fx.int(types::I8, 0);
                let started = fx.repeat(&[tries, no], |fx, state| {
                    self.send_ipi(fx, id, super::interrupts::IPI_STARTUP | TRAMPOLINE >> 12);
                    let started = self.wait_started(fx, record);
                    let left = fx.b.ins().iadd_imm_s(state[0], -1);
                    let waiting = fx.b.ins().bxor_imm_s(started, 1);
                    let tries_left = fx.b.ins().icmp_imm_s(IntCC::NotEqual, left, 0);
                    (fx.b.ins().band(waiting, tries_left), vec![left, started])
                })[1];
                let next = fx.b.ins().iadd_imm_s(index, 1);
                let remaining = fx.b.ins().icmp(IntCC::UnsignedLessThan, next, count);
                (fx.b.ins().band(started, remaining), vec![next])
            });
        });
    }

    /// Starts every CPU other than the boot CPU, each on the entry root through the secondary
    /// entry seam: it reads the start record the boot CPU fills with the entry root, the CPU's
    /// executor stack, the start glue and the CPU's record, turns its MMU on and runs the glue,
    /// which prepares its interrupt delivery and starts its executor. The boot CPU starts the CPUs
    /// one at a time, filling the record anew for each, and a CPU that does not start ends the
    /// sequence, leaving the CPUs after it stopped.
    fn aarch64_start_cpus(&mut self, fx: &mut Fx, prepare: FuncId) {
        let state_address = self.state(fx);
        let count = fx.load(PTR, state_address, state::THREAD_COUNT);
        let several = fx.b.ins().icmp_imm_s(IntCC::UnsignedGreaterThan, count, 1);
        fx.when(several, |fx| {
            let seam = self.aarch64_secondary_seam();
            let seam = self.func_ref(fx, seam);
            let seam = fx.b.ins().func_addr(PTR, seam);
            let record = self.secondary_record(fx);
            let root = self.aarch64_entry_root();
            let root = self.data_address(fx, root);
            fx.store(root, record, super::secondary::ROOT);
            let glue = self.cpu_glue(prepare);
            let glue = self.func_ref(fx, glue);
            let glue = fx.b.ins().func_addr(PTR, glue);
            fx.store(glue, record, super::secondary::GLUE);
            let one = fx.ptr(1);
            fx.repeat(&[one], |fx, state| {
                let index = state[0];
                let cpu = self.cpu_record(fx, index);
                let chunk = fx.ptr(1);
                let stack = system::map_chunks(self, fx, chunk);
                let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, stack, 0);
                fx.when(missing, |fx| report::raise_kind(self, fx, "trap.exhausted"));
                let top = fx.b.ins().iadd_imm_s(stack, CHUNK as i64);
                fx.store(top, record, super::secondary::STACK);
                fx.store(cpu, record, super::secondary::CPU);
                let affinity = fx.load(PTR, cpu, cpu::HARDWARE_ID);
                let function = fx.ptr(PSCI_CPU_ON);
                self.call_stub(fx, Stub::Hvc, &[function, affinity, seam, record]);
                let started = self.wait_started(fx, cpu);
                let next = fx.b.ins().iadd_imm_s(index, 1);
                let remaining = fx.b.ins().icmp(IntCC::UnsignedLessThan, next, count);
                (fx.b.ins().band(started, remaining), vec![next])
            });
        });
    }

    /// Whether a CPU has started within `START_WAIT` pauses.
    fn wait_started(&mut self, fx: &mut Fx, record: Value) -> Value {
        let tries = fx.ptr(START_WAIT);
        let flag = fx.offset(record, cpu::STARTED);
        fx.repeat(&[tries], |fx, left| {
            self.call_stub(fx, Stub::Pause, &[]);
            let started = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), flag);
            let waiting = fx.b.ins().icmp_imm_s(IntCC::Equal, started, 0);
            let left = fx.b.ins().iadd_imm_s(left[0], -1);
            let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, left, 0);
            (fx.b.ins().band(waiting, more), vec![left])
        });
        let started = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), flag);
        fx.b.ins().icmp_imm_s(IntCC::NotEqual, started, 0)
    }

    /// Records that a CPU has started its executor.
    pub fn cpu_started(&mut self, fx: &mut Fx, record: Value) {
        let state_address = self.state(fx);
        let started = fx.offset(state_address, state::STARTED_CPUS);
        let one = fx.ptr(1);
        super::super::runtime::fetch_add(fx, started, one);
        let flag = fx.offset(record, cpu::STARTED);
        fx.b.ins().atomic_store(MemFlagsData::trusted(), one, flag);
    }

    /// Spins for `pauses` pauses.
    fn spin(&mut self, fx: &mut Fx, pauses: u64) {
        let zero = fx.ptr(0);
        let end = fx.ptr(pauses);
        fx.each(zero, end, |fx, _| {
            self.call_stub(fx, Stub::Pause, &[]);
        });
    }

    /// The trampoline's code, a read-only data object the boot CPU copies.
    fn trampoline_code(&mut self) -> cranelift_module::DataId {
        let id = self
            .module
            .declare_data("_NStrampoline", Linkage::Local, false, false)
            .expect("the trampoline");
        let mut description = DataDescription::new();
        description.define(TRAMPOLINE_CODE.to_vec().into_boxed_slice());
        description.set_align(8);
        self.module
            .define_data(id, &description)
            .expect("the trampoline");
        id
    }

    /// The glue a started CPU calls with its record: it points its base register at the record,
    /// sets its data-independent timing mode, on x86_64 records its features, loads its
    /// descriptor tables and enables AVX when it has it, prepares its interrupt delivery through
    /// `prepare`, records that it started and runs the executor's scheduler.
    fn cpu_glue(&mut self, prepare: FuncId) -> FuncId {
        let mut signature = Signature::new(super::super::CALL_CONV);
        signature.params.push(AbiParam::new(PTR));
        let id = self
            .module
            .declare_function("_NScpu_start", Linkage::Local, &signature)
            .expect("the CPU start glue");
        self.define(
            id,
            signature,
            super::super::lines::HIDDEN | super::super::lines::CRITICAL,
            |lo, fx| {
                let record = fx.entry()[0];
                lo.set_cpu_base(fx, record);
                lo.call_stub(fx, Stub::ConstantTime, &[]);
                if lo.ck.db.target.arch() != Some(Arch::Aarch64) {
                    let index = lo.running_index(fx);
                    lo.call_stub(fx, Stub::CpuFeatures, &[index]);
                    lo.load_cpu_tables(fx, record);
                    lo.enable_avx(fx, record);
                }
                let prepare = lo.func_ref(fx, prepare);
                fx.b.ins().call(prepare, &[]);
                lo.cpu_started(fx, record);
                lo.call_routine(fx, Routine::Schedule, &[]);
                fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
            },
        );
        id
    }
}

/// A 32-bit value of a firmware table at an address, zero-extended.
fn load_u32(fx: &mut Fx, address: Value, offset: u64) -> Value {
    let value =
        fx.b.ins()
            .load(types::I32, unaligned(), address, offset as i32);
    fx.b.ins().uextend(PTR, value)
}
