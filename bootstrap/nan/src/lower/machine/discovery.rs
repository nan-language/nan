//! The AArch64 machine an image runs on, discovered in `Init` from the flattened device
//! tree the image entered with, or a UEFI image's configuration table lists, or else from the
//! ACPI tables: the UART halt reports are written to, the wall clock's PL031, the GICv3's
//! distributor, redistributor regions and ITS, the PSCI conduit, and the CPUs with each one's
//! redistributor. The machine record holds what it finds; a field is zero for what the machine
//! lacks.

use cranelift_codegen::ir::{AbiParam, InstBuilder, Signature, Value, condcodes::IntCC, types};
use cranelift_frontend::Variable;
use cranelift_module::{FuncId, Linkage, Module};

use super::super::Lowering;
use super::super::data::Reserved;
use super::super::fx::{Fx, PTR};
use super::super::runtime::{state, system};
use super::acpi::{DEVICE_TREE_TABLE, load_u32, load_u64};
use super::cpus::{MAX_CPUS, cpu};
use super::its::GICR_TYPER;
use super::memory::{device_tree, name_end, starts_with, tree_address, tree_cell, tree_number};
use super::start;
use super::stubs::Stub;
use crate::workspace::Kind;

/// The offsets of the machine record's fields: the UART's base, kind, register shift and access
/// width; the PL031's base; the PSCI conduit; the number of CPUs found; the distributor's base;
/// the ITS's base; and the number of redistributor regions, then each region's base and length.
pub mod machine {
    pub const UART: u64 = 0;
    pub const UART_KIND: u64 = 8;
    pub const UART_SHIFT: u64 = 16;
    pub const UART_WIDTH: u64 = 24;
    pub const RTC: u64 = 32;
    pub const CONDUIT: u64 = 40;
    pub const CPUS: u64 = 48;
    pub const GICD: u64 = 56;
    pub const ITS: u64 = 64;
    pub const REGIONS: u64 = 72;
    pub const REGION: u64 = 80;
    pub const REGION_BYTES: u64 = 16;
    pub const MOST_REGIONS: u64 = 4;
    pub const SIZE: u64 = REGION + REGION_BYTES * MOST_REGIONS;
}

/// The machine record.
pub const MACHINE: Reserved = Reserved {
    name: "_NSmachine",
    size: machine::SIZE,
    align: 8,
};

/// The kinds of UART: a PL011, an SBSA generic UART, which the halt path does not program, and a
/// memory-mapped 16550 whose registers lie `1 << shift` bytes apart.
pub mod uart {
    pub const PL011: i64 = 1;
    pub const SBSA: i64 = 2;
    pub const NS16550: i64 = 3;
}

/// The PSCI conduits: the hypervisor call and the secure monitor call.
pub mod conduit {
    pub const HVC: i64 = 1;
    pub const SMC: i64 = 2;
}

/// The PSCI status a call gives when the machine has no conduit.
const NOT_SUPPORTED: i64 = -1;

/// The `MPIDR_EL1` encoding the `ReadSysReg` stub reads the boot CPU's affinity through, and the
/// affinity fields of an `MPIDR_EL1` value.
const MPIDR_EL1: u16 = 0x4005;
const AFFINITY: i64 = 0xFF_00FF_FFFF;

/// The device tree's magic number, and the depths of nodes whose cells, ranges and path a walk
/// tracks: deeper nodes are passed over.
const MAGIC: i64 = 0xD00D_FEED;
const DEPTHS: u64 = 16;

/// The offsets of the fields a walk keeps per depth: the cells of an address and a size in the
/// node's children, its `ranges` property's value, zero without one, and length, and the place
/// in the UART's path past the node's name when the path runs through the node, zero otherwise.
mod level {
    pub const ADDRESS_CELLS: u64 = 0;
    pub const SIZE_CELLS: u64 = 8;
    pub const RANGES: u64 = 16;
    pub const RANGES_LEN: u64 = 24;
    pub const PATH: u64 = 32;
    pub const SIZE: u64 = 40;
}

/// The compatible strings a walk recognises, each with its bit among a node's kinds.
mod compatible {
    pub const PL011: i64 = 1;
    pub const SBSA: i64 = 2;
    pub const NS16550: i64 = 4;
    pub const GIC: i64 = 8;
    pub const ITS: i64 = 16;
    pub const RTC: i64 = 32;
    pub const PSCI: i64 = 64;
    pub const STRINGS: [(&[u8], i64); 11] = [
        (b"arm,pl011\0", PL011),
        (b"arm,sbsa-uart\0", SBSA),
        (b"ns16550a\0", NS16550),
        (b"ns16550\0", NS16550),
        (b"snps,dw-apb-uart\0", NS16550),
        (b"arm,gic-v3\0", GIC),
        (b"arm,gic-v3-its\0", ITS),
        (b"arm,pl031\0", RTC),
        (b"arm,psci\0", PSCI),
        (b"arm,psci-0.2\0", PSCI),
        (b"arm,psci-1.0\0", PSCI),
    ];
}

/// The MADT entries a discovery reads: a GIC CPU interface, with its flags, redistributor base
/// and affinity, a distributor with its base, a redistributor region with its base and length,
/// and an ITS with its base; and the flag of a usable CPU interface.
mod madt {
    pub const GICC: i64 = 0x0B;
    pub const GICC_FLAGS: u64 = 12;
    pub const GICC_GICR: u64 = 60;
    pub const GICC_MPIDR: u64 = 68;
    pub const GICD: i64 = 0x0C;
    pub const GICD_BASE: u64 = 8;
    pub const GICR: i64 = 0x0E;
    pub const GICR_BASE: u64 = 4;
    pub const GICR_LENGTH: u64 = 12;
    pub const ITS: i64 = 0x0F;
    pub const ITS_BASE: u64 = 8;
    pub const ENABLED: i64 = 1;
}

/// The FADT's signature, its length that holds the boot architecture flags, their offset, and
/// their flags saying PSCI is implemented and is reached by `hvc`.
const FADT_SIGNATURE: i64 = 0x5043_4146;
const FADT_ARM_LENGTH: i64 = 131;
const FADT_ARM_BOOT: u64 = 129;
const PSCI_COMPLIANT: i64 = 1;
const PSCI_USE_HVC: i64 = 2;

/// The SPCR's signature, the offsets of its interface type and of its base address's space,
/// access size and address, the system memory space, and the interface types of a PL011, of the
/// SBSA generic UARTs and of the 16550s; an access size of 4 bytes.
const SPCR_SIGNATURE: i64 = 0x5243_5053;
const SPCR_TYPE: u64 = 36;
const SPCR_SPACE: u64 = 40;
const SPCR_ACCESS: u64 = 43;
const SPCR_ADDRESS: u64 = 44;
const SPCR_PL011: i64 = 0x03;
const SPCR_SBSA: [i64; 2] = [0x0D, 0x0E];
const SPCR_16550: [i64; 2] = [0x00, 0x12];
const SPCR_DWORD: i64 = 3;

/// The last-frame and virtual-LPI bits of a redistributor frame's type register and the position
/// of its affinity there; and the strides of a frame without and with virtual LPIs.
const GICR_LAST: i64 = 1 << 4;
const GICR_VLPIS: i64 = 1 << 1;
const GICR_AFFINITY: i64 = 32;
const GICR_FRAME: i64 = 0x2_0000;
const GICR_VLPI_FRAME: i64 = 0x4_0000;

/// The helper functions of a device tree walk.
#[derive(Clone, Copy)]
struct Helpers {
    listed: FuncId,
    named: FuncId,
    translate: FuncId,
}

/// What a walk records of the node whose properties it reads: its `reg` property's value and
/// length, its kinds, whether its `device_type` is `cpu`, its `reg-shift` and `reg-io-width`, its
/// PSCI method's conduit, its `#redistributor-regions`, and whether its `status` disables it.
#[derive(Clone, Copy)]
struct Node {
    reg: Variable,
    reg_len: Variable,
    kinds: Variable,
    cpu: Variable,
    shift: Variable,
    width: Variable,
    method: Variable,
    regions: Variable,
    disabled: Variable,
}

impl Lowering<'_, '_> {
    /// The address of the machine record.
    pub(super) fn machine_record(&mut self, fx: &mut Fx) -> Value {
        self.reserved(fx, MACHINE)
    }

    /// The number of CPUs discovery found.
    pub(super) fn discovered_cpus(&mut self, fx: &mut Fx) -> Value {
        let record = self.machine_record(fx);
        fx.load(PTR, record, machine::CPUS)
    }

    /// Discovers the machine, recording the boot CPU's affinity first: from the device tree, else
    /// from the ACPI tables. Without a PSCI conduit only the boot CPU runs. On a kernel, each CPU
    /// gets its redistributor, a CPU other than the boot CPU without one being left out, and
    /// `Init` returns 1 when the machine has no distributor or the boot CPU no redistributor.
    pub(super) fn discover_machine(&mut self, fx: &mut Fx) {
        let record = self.machine_record(fx);
        let mpidr = self.read_sysreg(fx, MPIDR_EL1);
        let boot = fx.b.ins().band_imm_s(mpidr, AFFINITY);
        let first = self.cpu_records(fx);
        fx.store(boot, first, cpu::HARDWARE_ID);
        let one = fx.ptr(1);
        fx.store(one, record, machine::CPUS);
        let state_address = self.state(fx);
        let tree = match self.ck.db.target.kind() {
            Some(Kind::Uefi) => {
                let table = fx.load(PTR, state_address, state::PLATFORM_TABLE);
                self.configured(fx, table, &[DEVICE_TREE_TABLE])
            }
            _ => fx.load(PTR, state_address, state::PLATFORM),
        };
        let walk = self.tree_discovery();
        let walk = self.call_ref(fx, walk);
        let call = fx.b.ins().call(walk, &[tree, boot]);
        let found = fx.b.inst_results(call)[0];
        let absent = fx.b.ins().icmp_imm_s(IntCC::Equal, found, 0);
        fx.when(absent, |fx| {
            let rsdp = self.rsdp(fx);
            self.acpi_discovery(fx, rsdp, boot);
        });
        if !self.kernel() {
            return;
        }
        let conduit = fx.load(PTR, record, machine::CONDUIT);
        let none = fx.b.ins().icmp_imm_s(IntCC::Equal, conduit, 0);
        fx.when(none, |fx| {
            let one = fx.ptr(1);
            fx.store(one, record, machine::CPUS);
        });
        let count = fx.load(PTR, record, machine::CPUS);
        let kept = fx.var(PTR, one);
        let zero = fx.ptr(0);
        fx.each(zero, count, |fx, index| {
            let from = self.cpu_record(fx, index);
            let given = fx.load(PTR, from, cpu::REDISTRIBUTOR);
            let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, given, 0);
            let affinity = fx.load(PTR, from, cpu::HARDWARE_ID);
            let redistributor = fx.choose_in(
                self,
                missing,
                &[PTR],
                |lo, fx| vec![lo.find_redistributor(fx, affinity)],
                |_, _| vec![given],
            )[0];
            let boot = fx.b.ins().icmp_imm_s(IntCC::Equal, index, 0);
            fx.when(boot, |fx| fx.store(redistributor, from, cpu::REDISTRIBUTOR));
            let other = fx.b.ins().bxor_imm_s(boot, 1);
            let found = fx.b.ins().icmp_imm_s(IntCC::NotEqual, redistributor, 0);
            let taken = fx.b.ins().band(other, found);
            fx.when(taken, |fx| {
                let at = fx.b.use_var(kept);
                let to = self.cpu_record(fx, at);
                fx.store(affinity, to, cpu::HARDWARE_ID);
                fx.store(redistributor, to, cpu::REDISTRIBUTOR);
                let at = fx.b.ins().iadd_imm_s(at, 1);
                fx.b.def_var(kept, at);
            });
        });
        let kept = fx.b.use_var(kept);
        fx.store(kept, record, machine::CPUS);
        let gicd = fx.load(PTR, record, machine::GICD);
        let redistributor = fx.load(PTR, first, cpu::REDISTRIBUTOR);
        let no_gicd = fx.b.ins().icmp_imm_s(IntCC::Equal, gicd, 0);
        let no_redistributor = fx.b.ins().icmp_imm_s(IntCC::Equal, redistributor, 0);
        let lacking = fx.b.ins().bor(no_gicd, no_redistributor);
        system::unavailable_if(fx, lacking);
    }

    /// The redistributor frame whose affinity is a CPU's, found by walking each redistributor
    /// region's frames until one is the last; zero when none is.
    fn find_redistributor(&mut self, fx: &mut Fx, affinity: Value) -> Value {
        let high = fx.b.ins().ushr_imm_s(affinity, 32);
        let high = fx.b.ins().ishl_imm_s(high, 24);
        let low = fx.b.ins().band_imm_s(affinity, 0xFF_FFFF);
        let wanted = fx.b.ins().bor(high, low);
        let record = self.machine_record(fx);
        let regions = fx.load(PTR, record, machine::REGIONS);
        let zero = fx.ptr(0);
        let found = fx.var(PTR, zero);
        fx.each(zero, regions, |fx, index| {
            let region = fx.b.ins().imul_imm_s(index, machine::REGION_BYTES as i64);
            let region = fx.b.ins().iadd(record, region);
            let base = fx.load(PTR, region, machine::REGION);
            let len = fx.load(PTR, region, machine::REGION + 8);
            let end = fx.b.ins().iadd(base, len);
            fx.repeat(&[base], |fx, at| {
                let at = at[0];
                let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, at, end);
                let held = fx.b.use_var(found);
                let open = fx.b.ins().icmp_imm_s(IntCC::Equal, held, 0);
                let looking = fx.b.ins().band(inside, open);
                let next = fx.choose_in(
                    self,
                    looking,
                    &[types::I8, PTR],
                    |lo, fx| {
                        let typer = fx.b.ins().iadd_imm_s(at, GICR_TYPER as i64);
                        let typer = lo.mmio_read(fx, 8, typer);
                        let frame = fx.b.ins().ushr_imm_s(typer, GICR_AFFINITY);
                        let same = fx.b.ins().icmp(IntCC::Equal, frame, wanted);
                        fx.when(same, |fx| fx.b.def_var(found, at));
                        let last = fx.b.ins().band_imm_s(typer, GICR_LAST);
                        let last = fx.b.ins().icmp_imm_s(IntCC::NotEqual, last, 0);
                        let virtual_lpis = fx.b.ins().band_imm_s(typer, GICR_VLPIS);
                        let virtual_lpis = fx.b.ins().icmp_imm_s(IntCC::NotEqual, virtual_lpis, 0);
                        let wide = fx.ptr(GICR_VLPI_FRAME as u64);
                        let narrow = fx.ptr(GICR_FRAME as u64);
                        let stride = fx.b.ins().select(virtual_lpis, wide, narrow);
                        let after = fx.b.ins().iadd(at, stride);
                        let done = fx.b.ins().bor(same, last);
                        vec![fx.b.ins().bxor_imm_s(done, 1), after]
                    },
                    |_, fx| vec![fx.int(types::I8, 0), at],
                );
                (next[0], vec![next[1]])
            });
        });
        fx.b.use_var(found)
    }

    /// Makes a PSCI call through the machine's conduit with a function identifier and three
    /// arguments, giving its result, `NOT_SUPPORTED` without a conduit.
    pub fn psci_call(&mut self, fx: &mut Fx, function: Value, args: [Value; 3]) -> Value {
        let record = self.machine_record(fx);
        let conduit = fx.load(PTR, record, machine::CONDUIT);
        let hvc = fx.b.ins().icmp_imm_s(IntCC::Equal, conduit, conduit::HVC);
        let smc = fx.b.ins().icmp_imm_s(IntCC::Equal, conduit, conduit::SMC);
        let call = [function, args[0], args[1], args[2]];
        fx.choose_in(
            self,
            hvc,
            &[PTR],
            |lo, fx| lo.call_stub(fx, Stub::Hvc, &call),
            |lo, fx| {
                fx.choose_in(
                    lo,
                    smc,
                    &[PTR],
                    |lo, fx| lo.call_stub(fx, Stub::Smc, &call),
                    |_, fx| vec![fx.ptr(NOT_SUPPORTED as u64)],
                )
            },
        )[0]
    }

    /// The machine's description in the ACPI tables the RSDP reaches: the MADT's CPU interfaces,
    /// usable ones other than the boot CPU's becoming CPUs, each with the redistributor it names,
    /// its distributor, redistributor regions and first ITS; the FADT's PSCI conduit; and the
    /// SPCR's UART.
    fn acpi_discovery(&mut self, fx: &mut Fx, rsdp: Value, boot: Value) {
        let record = self.machine_record(fx);
        self.madt_entries(fx, rsdp, &mut |lo, fx, at, kind| {
            let gicc = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, madt::GICC);
            fx.when(gicc, |fx| {
                let flags = load_u32(fx, at, madt::GICC_FLAGS);
                let enabled = fx.b.ins().band_imm_s(flags, madt::ENABLED);
                let enabled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, enabled, 0);
                let mpidr = load_u64(fx, at, madt::GICC_MPIDR);
                let affinity = fx.b.ins().band_imm_s(mpidr, AFFINITY);
                let redistributor = load_u64(fx, at, madt::GICC_GICR);
                let own = fx.b.ins().icmp(IntCC::Equal, affinity, boot);
                fx.when(own, |fx| {
                    let first = lo.cpu_records(fx);
                    fx.store(redistributor, first, cpu::REDISTRIBUTOR);
                });
                let other = fx.b.ins().bxor_imm_s(own, 1);
                let taken = fx.b.ins().band(enabled, other);
                fx.when(taken, |fx| lo.add_cpu(fx, affinity, redistributor));
            });
            let gicd = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, madt::GICD);
            fx.when(gicd, |fx| {
                let base = load_u64(fx, at, madt::GICD_BASE);
                first_field(fx, record, machine::GICD, base);
            });
            let gicr = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, madt::GICR);
            fx.when(gicr, |fx| {
                let base = load_u64(fx, at, madt::GICR_BASE);
                let len = load_u32(fx, at, madt::GICR_LENGTH);
                add_region(fx, record, base, len);
            });
            let its = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, madt::ITS);
            fx.when(its, |fx| {
                let base = load_u64(fx, at, madt::ITS_BASE);
                first_field(fx, record, machine::ITS, base);
            });
        });
        self.acpi_conduit(fx, rsdp, record);
        self.acpi_uart(fx, rsdp, record);
    }

    /// Records the PSCI conduit the FADT's boot architecture flags name.
    fn acpi_conduit(&mut self, fx: &mut Fx, rsdp: Value, record: Value) {
        let fadt = self.acpi_table(fx, rsdp, FADT_SIGNATURE);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, fadt, 0);
        fx.when(present, |fx| {
            let length = load_u32(fx, fadt, 4);
            let long =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, length, FADT_ARM_LENGTH);
            fx.when(long, |fx| {
                let flags = fx.load_ordered(types::I16, fadt, FADT_ARM_BOOT, true, false);
                let flags = fx.b.ins().uextend(PTR, flags);
                let compliant = fx.b.ins().band_imm_s(flags, PSCI_COMPLIANT);
                let compliant = fx.b.ins().icmp_imm_s(IntCC::NotEqual, compliant, 0);
                let hvc = fx.b.ins().band_imm_s(flags, PSCI_USE_HVC);
                let hvc = fx.b.ins().icmp_imm_s(IntCC::NotEqual, hvc, 0);
                let by_hvc = fx.ptr(conduit::HVC as u64);
                let by_smc = fx.ptr(conduit::SMC as u64);
                let conduit = fx.b.ins().select(hvc, by_hvc, by_smc);
                fx.when(compliant, |fx| fx.store(conduit, record, machine::CONDUIT));
            });
        });
    }

    /// Records the UART the SPCR names.
    fn acpi_uart(&mut self, fx: &mut Fx, rsdp: Value, record: Value) {
        let spcr = self.acpi_table(fx, rsdp, SPCR_SIGNATURE);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, spcr, 0);
        fx.when(present, |fx| {
            let address_space = fx.load_ordered(types::I8, spcr, SPCR_SPACE, true, false);
            let memory = fx.b.ins().icmp_imm_s(IntCC::Equal, address_space, 0);
            let interface = fx.load_ordered(types::I8, spcr, SPCR_TYPE, true, false);
            let access = fx.load_ordered(types::I8, spcr, SPCR_ACCESS, true, false);
            let base = load_u64(fx, spcr, SPCR_ADDRESS);
            let pl011 = fx.b.ins().icmp_imm_s(IntCC::Equal, interface, SPCR_PL011);
            let mut sbsa = fx.int(types::I8, 0);
            for kind in SPCR_SBSA {
                let same = fx.b.ins().icmp_imm_s(IntCC::Equal, interface, kind);
                sbsa = fx.b.ins().bor(sbsa, same);
            }
            let mut ns16550 = fx.int(types::I8, 0);
            for kind in SPCR_16550 {
                let same = fx.b.ins().icmp_imm_s(IntCC::Equal, interface, kind);
                ns16550 = fx.b.ins().bor(ns16550, same);
            }
            let kind = uart_kind(fx, pl011, sbsa, ns16550);
            let dword = fx.b.ins().icmp_imm_s(IntCC::Equal, access, SPCR_DWORD);
            let two = fx.ptr(2);
            let zero = fx.ptr(0);
            let shift = fx.b.ins().select(dword, two, zero);
            let four = fx.ptr(4);
            let one = fx.ptr(1);
            let narrow = fx.b.ins().select(dword, four, one);
            let width = fx.b.ins().select(ns16550, narrow, four);
            let known = fx.b.ins().icmp_imm_s(IntCC::NotEqual, kind, 0);
            let placed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, base, 0);
            let usable = fx.b.ins().band(memory, known);
            let usable = fx.b.ins().band(usable, placed);
            fx.when(usable, |fx| set_uart(fx, record, base, kind, shift, width));
        });
    }

    /// Records a CPU of an affinity and redistributor after those found, unless `MAX_CPUS` are.
    fn add_cpu(&mut self, fx: &mut Fx, affinity: Value, redistributor: Value) {
        let record = self.machine_record(fx);
        let count = fx.load(PTR, record, machine::CPUS);
        let room =
            fx.b.ins()
                .icmp_imm_s(IntCC::UnsignedLessThan, count, MAX_CPUS as i64);
        fx.when(room, |fx| {
            let at = self.cpu_record(fx, count);
            fx.store(affinity, at, cpu::HARDWARE_ID);
            fx.store(redistributor, at, cpu::REDISTRIBUTOR);
            let count = fx.b.ins().iadd_imm_s(count, 1);
            fx.store(count, record, machine::CPUS);
        });
    }

    /// The function discovering the machine from a device tree at its first argument, the boot
    /// CPU's affinity its second: it gives 0 when no device tree lies there and 1 otherwise. It
    /// finds the UART the `/chosen` node's `stdout-path` names, directly or through `/aliases`,
    /// or else the first PL011; the first GICv3, ITS and PL031; the PSCI node's method; and each
    /// node of `device_type` `cpu` other than the boot CPU. A device's `reg` is translated
    /// through its ancestors' `ranges`, and a node whose `status` is neither `okay` nor `ok` is
    /// passed over, but for a PL031, which UEFI firmware disables on taking it for its runtime
    /// services.
    fn tree_discovery(&mut self) -> FuncId {
        let helpers = Helpers {
            listed: self.listed_helper(),
            named: self.named_helper(),
            translate: self.translate_helper(),
        };
        let mut signature = Signature::new(super::super::CALL_CONV);
        signature.params.push(AbiParam::new(PTR));
        signature.params.push(AbiParam::new(PTR));
        signature.returns.push(AbiParam::new(PTR));
        let id = self
            .module
            .declare_function("_NSdiscover_tree", Linkage::Local, &signature)
            .expect("the device tree discovery");
        self.define(id, signature, super::super::lines::HIDDEN, |lo, fx| {
            let params = fx.entry();
            let (tree, boot) = (params[0], params[1]);
            let given = fx.b.ins().icmp_imm_s(IntCC::NotEqual, tree, 0);
            let magic = fx.choose(
                given,
                &[PTR],
                |fx| vec![tree_cell(fx, tree, 0)],
                |fx| vec![fx.ptr(0)],
            )[0];
            let valid = fx.b.ins().icmp_imm_s(IntCC::Equal, magic, MAGIC);
            fx.when(valid, |fx| {
                let path = lo.stdout_path(fx, tree, helpers);
                lo.walk_tree(fx, tree, path, boot, helpers);
                let one = fx.ptr(1);
                fx.b.ins().return_(&[one]);
            });
            let zero = fx.ptr(0);
            fx.b.ins().return_(&[zero]);
        });
        id
    }

    /// The path of the node the `/chosen` node's `stdout-path` or `linux,stdout-path` names, an
    /// alias resolved through `/aliases`; zero when it names none.
    fn stdout_path(&mut self, fx: &mut Fx, tree: Value, helpers: Helpers) -> Value {
        let structure = tree_address(fx, tree, device_tree::STRUCTURE);
        let strings = tree_address(fx, tree, device_tree::STRINGS);
        let zero = fx.ptr(0);
        let depth = fx.var(PTR, zero);
        let chosen = fx.var(PTR, zero);
        let aliases = fx.var(PTR, zero);
        let stdout = fx.var(PTR, zero);
        fx.repeat(&[structure], |fx, state| {
            let at = state[0];
            let token = tree_cell(fx, at, 0);
            let name = fx.b.ins().iadd_imm_s(at, 4);
            let next = fx.var(PTR, name);
            let open =
                fx.b.ins()
                    .icmp_imm_s(IntCC::Equal, token, device_tree::BEGIN_NODE);
            fx.when(open, |fx| {
                let inner = fx.b.use_var(depth);
                let inner = fx.b.ins().iadd_imm_s(inner, 1);
                fx.b.def_var(depth, inner);
                let child = fx.b.ins().icmp_imm_s(IntCC::Equal, inner, 2);
                let after = name_end(fx, name);
                fx.b.def_var(next, after);
                let named = starts_with(fx, name, b"chosen\0");
                let inside = fx.b.ins().band(child, named);
                let inside = fx.b.ins().uextend(PTR, inside);
                fx.b.def_var(chosen, inside);
                let named = starts_with(fx, name, b"aliases\0");
                let found = fx.b.ins().band(child, named);
                fx.when(found, |fx| fx.b.def_var(aliases, after));
            });
            let close =
                fx.b.ins()
                    .icmp_imm_s(IntCC::Equal, token, device_tree::END_NODE);
            fx.when(close, |fx| {
                let inner = fx.b.use_var(depth);
                let outer = fx.b.ins().iadd_imm_s(inner, -1);
                fx.b.def_var(depth, outer);
                let zero = fx.ptr(0);
                fx.b.def_var(chosen, zero);
            });
            let property =
                fx.b.ins()
                    .icmp_imm_s(IntCC::Equal, token, device_tree::PROPERTY);
            fx.when(property, |fx| {
                let read = structure_property(fx, at, strings);
                fx.b.def_var(next, read.after);
                let inside = fx.b.use_var(chosen);
                let inside = fx.b.ins().icmp_imm_s(IntCC::NotEqual, inside, 0);
                let plain = starts_with(fx, read.name, b"stdout-path\0");
                let linux = starts_with(fx, read.name, b"linux,stdout-path\0");
                let named = fx.b.ins().bor(plain, linux);
                let found = fx.b.ins().band(inside, named);
                fx.when(found, |fx| fx.b.def_var(stdout, read.value));
            });
            let more =
                fx.b.ins()
                    .icmp_imm_s(IntCC::NotEqual, token, device_tree::END);
            (more, vec![fx.b.use_var(next)])
        });
        let stdout = fx.b.use_var(stdout);
        let given = fx.b.ins().icmp_imm_s(IntCC::NotEqual, stdout, 0);
        fx.choose_in(
            self,
            given,
            &[PTR],
            |lo, fx| {
                let first = fx.load(types::I8, stdout, 0);
                let full = fx.b.ins().icmp_imm_s(IntCC::Equal, first, i64::from(b'/'));
                fx.choose_in(
                    lo,
                    full,
                    &[PTR],
                    |_, _| vec![stdout],
                    |lo, fx| {
                        let aliases = fx.b.use_var(aliases);
                        vec![lo.alias(fx, aliases, strings, stdout, helpers)]
                    },
                )
            },
            |_, fx| vec![fx.ptr(0)],
        )[0]
    }

    /// The value of the property of the `/aliases` node, whose properties start at `aliases`,
    /// named as the alias at `name` before any `:`; zero when there is none.
    fn alias(
        &mut self,
        fx: &mut Fx,
        aliases: Value,
        strings: Value,
        name: Value,
        helpers: Helpers,
    ) -> Value {
        let zero = fx.ptr(0);
        let found = fx.var(PTR, zero);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, aliases, 0);
        fx.when(present, |fx| {
            fx.repeat(&[aliases], |fx, state| {
                let at = state[0];
                let token = tree_cell(fx, at, 0);
                let property =
                    fx.b.ins()
                        .icmp_imm_s(IntCC::Equal, token, device_tree::PROPERTY);
                let next = fx.choose_in(
                    self,
                    property,
                    &[PTR],
                    |lo, fx| {
                        let read = structure_property(fx, at, strings);
                        let named = lo.call_helper(fx, helpers.named, &[read.name, name]);
                        let same = fx.b.ins().icmp_imm_s(IntCC::NotEqual, named, 0);
                        fx.when(same, |fx| fx.b.def_var(found, read.value));
                        vec![read.after]
                    },
                    |_, fx| vec![fx.b.ins().iadd_imm_s(at, 4)],
                )[0];
                let nop = fx.b.ins().icmp_imm_s(IntCC::Equal, token, NOP);
                let more = fx.b.ins().bor(property, nop);
                (more, vec![next])
            });
        });
        fx.b.use_var(found)
    }

    /// Walks the device tree's structure block, recording into the machine record what each
    /// node describes once its properties end.
    fn walk_tree(&mut self, fx: &mut Fx, tree: Value, path: Value, boot: Value, helpers: Helpers) {
        let structure = tree_address(fx, tree, device_tree::STRUCTURE);
        let strings = tree_address(fx, tree, device_tree::STRINGS);
        let levels = fx.slot(DEPTHS * level::SIZE, 8);
        let zero = fx.ptr(0);
        let depth = fx.var(PTR, zero);
        let pending = fx.var(PTR, zero);
        let fixed = fx.var(PTR, zero);
        let node = Node {
            reg: fx.var(PTR, zero),
            reg_len: fx.var(PTR, zero),
            kinds: fx.var(PTR, zero),
            cpu: fx.var(PTR, zero),
            shift: fx.var(PTR, zero),
            width: fx.var(PTR, zero),
            method: fx.var(PTR, zero),
            regions: fx.var(PTR, zero),
            disabled: fx.var(PTR, zero),
        };
        fx.repeat(&[structure], |fx, state| {
            let at = state[0];
            let token = tree_cell(fx, at, 0);
            let after = fx.b.ins().iadd_imm_s(at, 4);
            let next = fx.var(PTR, after);
            let open =
                fx.b.ins()
                    .icmp_imm_s(IntCC::Equal, token, device_tree::BEGIN_NODE);
            let close =
                fx.b.ins()
                    .icmp_imm_s(IntCC::Equal, token, device_tree::END_NODE);
            let ends = fx.b.ins().bor(open, close);
            let waiting = fx.b.use_var(pending);
            let waiting = fx.b.ins().icmp_imm_s(IntCC::NotEqual, waiting, 0);
            let ends = fx.b.ins().band(ends, waiting);
            fx.when(ends, |fx| {
                let inner = fx.b.use_var(depth);
                self.tree_node(fx, levels, inner, node, fixed, boot, helpers);
                let zero = fx.ptr(0);
                fx.b.def_var(pending, zero);
            });
            fx.when(open, |fx| {
                let walk = Walk {
                    levels,
                    depth,
                    pending,
                    next,
                    node,
                };
                self.open_node(fx, walk, after, path, helpers);
            });
            fx.when(close, |fx| {
                let inner = fx.b.use_var(depth);
                let outer = fx.b.ins().iadd_imm_s(inner, -1);
                fx.b.def_var(depth, outer);
            });
            let property =
                fx.b.ins()
                    .icmp_imm_s(IntCC::Equal, token, device_tree::PROPERTY);
            fx.when(property, |fx| {
                let read = structure_property(fx, at, strings);
                fx.b.def_var(next, read.after);
                let inner = fx.b.use_var(depth);
                let tracked =
                    fx.b.ins()
                        .icmp_imm_s(IntCC::UnsignedLessThan, inner, DEPTHS as i64);
                fx.when(tracked, |fx| {
                    let entry = fx.b.ins().imul_imm_s(inner, level::SIZE as i64);
                    let entry = fx.b.ins().iadd(levels, entry);
                    self.read_property(fx, entry, node, read, helpers);
                });
            });
            let more =
                fx.b.ins()
                    .icmp_imm_s(IntCC::NotEqual, token, device_tree::END);
            (more, vec![fx.b.use_var(next)])
        });
    }

    /// Enters the node named at `name`: a level deeper, its level entry, while tracked, given the
    /// default cells, no `ranges` and its place in the UART's path, and its record cleared.
    fn open_node(&mut self, fx: &mut Fx, walk: Walk, name: Value, path: Value, helpers: Helpers) {
        let inner = fx.b.use_var(walk.depth);
        let inner = fx.b.ins().iadd_imm_s(inner, 1);
        fx.b.def_var(walk.depth, inner);
        let end = name_end(fx, name);
        fx.b.def_var(walk.next, end);
        let tracked =
            fx.b.ins()
                .icmp_imm_s(IntCC::UnsignedLessThan, inner, DEPTHS as i64);
        fx.when(tracked, |fx| {
            let entry = fx.b.ins().imul_imm_s(inner, level::SIZE as i64);
            let entry = fx.b.ins().iadd(walk.levels, entry);
            let cells = fx.ptr(device_tree::ADDRESS_CELLS);
            fx.store(cells, entry, level::ADDRESS_CELLS);
            let cells = fx.ptr(device_tree::SIZE_CELLS);
            fx.store(cells, entry, level::SIZE_CELLS);
            let zero = fx.ptr(0);
            fx.store(zero, entry, level::RANGES);
            fx.store(zero, entry, level::RANGES_LEN);
            let root = fx.b.ins().icmp_imm_s(IntCC::Equal, inner, 1);
            let place = fx.choose_in(
                self,
                root,
                &[PTR],
                |_, fx| {
                    let given = fx.b.ins().icmp_imm_s(IntCC::NotEqual, path, 0);
                    let past = fx.b.ins().iadd_imm_s(path, 1);
                    let zero = fx.ptr(0);
                    vec![fx.b.ins().select(given, past, zero)]
                },
                |lo, fx| {
                    let parent = fx.b.ins().iadd_imm_s(entry, -(level::SIZE as i64));
                    let from = fx.load(PTR, parent, level::PATH);
                    vec![lo.path_step(fx, from, name, helpers)]
                },
            )[0];
            fx.store(place, entry, level::PATH);
        });
        let zero = fx.ptr(0);
        for variable in [
            walk.node.reg,
            walk.node.reg_len,
            walk.node.kinds,
            walk.node.cpu,
            walk.node.shift,
        ] {
            fx.b.def_var(variable, zero);
        }
        for variable in [walk.node.method, walk.node.disabled] {
            fx.b.def_var(variable, zero);
        }
        let one = fx.ptr(1);
        fx.b.def_var(walk.node.width, one);
        fx.b.def_var(walk.node.regions, one);
        fx.b.def_var(walk.pending, one);
    }

    /// The place in a path past the component at `from` when it names the node `name`, past its
    /// `/` when one follows; zero when `from` is zero or the component names another node.
    fn path_step(&mut self, fx: &mut Fx, from: Value, name: Value, helpers: Helpers) -> Value {
        let given = fx.b.ins().icmp_imm_s(IntCC::NotEqual, from, 0);
        fx.choose_in(
            self,
            given,
            &[PTR],
            |lo, fx| {
                let end = lo.call_helper(fx, helpers.named, &[name, from]);
                let matched = fx.b.ins().icmp_imm_s(IntCC::NotEqual, end, 0);
                let stop = fx.choose(
                    matched,
                    &[types::I8],
                    |fx| vec![fx.load(types::I8, end, 0)],
                    |fx| vec![fx.int(types::I8, 0)],
                )[0];
                let slash = fx.b.ins().icmp_imm_s(IntCC::Equal, stop, i64::from(b'/'));
                let past = fx.b.ins().iadd_imm_s(end, 1);
                vec![fx.b.ins().select(slash, past, end)]
            },
            |_, fx| vec![fx.ptr(0)],
        )[0]
    }

    /// Reads a property of the node whose level entry is `entry` into that entry or the node's
    /// record.
    fn read_property(
        &mut self,
        fx: &mut Fx,
        entry: Value,
        node: Node,
        read: Property,
        helpers: Helpers,
    ) {
        let Property {
            name, value, len, ..
        } = read;
        let cell = fx.choose(
            len,
            &[PTR],
            |fx| vec![tree_cell(fx, value, 0)],
            |fx| vec![fx.ptr(0)],
        )[0];
        for (property, offset) in [
            (&b"#address-cells\0"[..], level::ADDRESS_CELLS),
            (b"#size-cells\0", level::SIZE_CELLS),
        ] {
            let named = starts_with(fx, name, property);
            fx.when(named, |fx| fx.store(cell, entry, offset));
        }
        let named = starts_with(fx, name, b"ranges\0");
        fx.when(named, |fx| {
            fx.store(value, entry, level::RANGES);
            fx.store(len, entry, level::RANGES_LEN);
        });
        let named = starts_with(fx, name, b"reg\0");
        fx.when(named, |fx| {
            fx.b.def_var(node.reg, value);
            fx.b.def_var(node.reg_len, len);
        });
        for (property, variable) in [
            (&b"reg-shift\0"[..], node.shift),
            (b"reg-io-width\0", node.width),
            (b"#redistributor-regions\0", node.regions),
        ] {
            let named = starts_with(fx, name, property);
            fx.when(named, |fx| fx.b.def_var(variable, cell));
        }
        let named = starts_with(fx, name, b"compatible\0");
        fx.when(named, |fx| {
            for (string, bit) in compatible::STRINGS {
                let wanted = self.bytes_data(string);
                let wanted = self.data_address(fx, wanted);
                let listed = self.call_helper(fx, helpers.listed, &[value, len, wanted]);
                let bit = fx.b.ins().imul_imm_s(listed, bit);
                let kinds = fx.b.use_var(node.kinds);
                let kinds = fx.b.ins().bor(kinds, bit);
                fx.b.def_var(node.kinds, kinds);
            }
        });
        let named = starts_with(fx, name, b"device_type\0");
        fx.when(named, |fx| {
            let cpu = starts_with(fx, value, b"cpu\0");
            let cpu = fx.b.ins().uextend(PTR, cpu);
            fx.b.def_var(node.cpu, cpu);
        });
        let named = starts_with(fx, name, b"method\0");
        fx.when(named, |fx| {
            let hvc = starts_with(fx, value, b"hvc\0");
            let smc = starts_with(fx, value, b"smc\0");
            let by_hvc = fx.ptr(conduit::HVC as u64);
            let by_smc = fx.ptr(conduit::SMC as u64);
            let zero = fx.ptr(0);
            let method = fx.b.ins().select(smc, by_smc, zero);
            let method = fx.b.ins().select(hvc, by_hvc, method);
            fx.b.def_var(node.method, method);
        });
        let named = starts_with(fx, name, b"status\0");
        fx.when(named, |fx| {
            let okay = starts_with(fx, value, b"okay\0");
            let ok = starts_with(fx, value, b"ok\0");
            let enabled = fx.b.ins().bor(okay, ok);
            let disabled = fx.b.ins().bxor_imm_s(enabled, 1);
            let disabled = fx.b.ins().uextend(PTR, disabled);
            fx.b.def_var(node.disabled, disabled);
        });
    }

    /// Records what the node at `depth`, whose properties have ended, describes: a CPU's
    /// affinity, the GICv3's distributor and redistributor regions, the ITS's, PL031's and
    /// UART's bases, and the PSCI conduit.
    #[allow(clippy::too_many_arguments)]
    fn tree_node(
        &mut self,
        fx: &mut Fx,
        levels: Value,
        depth: Value,
        node: Node,
        fixed: Variable,
        boot: Value,
        helpers: Helpers,
    ) {
        let disabled = fx.b.use_var(node.disabled);
        let kinds = fx.b.use_var(node.kinds);
        let rtc = fx.b.ins().band_imm_s(kinds, compatible::RTC);
        let rtc = fx.b.ins().icmp_imm_s(IntCC::NotEqual, rtc, 0);
        let enabled = fx.b.ins().icmp_imm_s(IntCC::Equal, disabled, 0);
        let enabled = fx.b.ins().bor(enabled, rtc);
        let nested =
            fx.b.ins()
                .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, depth, 2);
        let tracked =
            fx.b.ins()
                .icmp_imm_s(IntCC::UnsignedLessThan, depth, DEPTHS as i64);
        let usable = fx.b.ins().band(enabled, nested);
        let usable = fx.b.ins().band(usable, tracked);
        fx.when(usable, |fx| {
            let record = self.machine_record(fx);
            let entry = fx.b.ins().imul_imm_s(depth, level::SIZE as i64);
            let entry = fx.b.ins().iadd(levels, entry);
            let parent = fx.b.ins().iadd_imm_s(entry, -(level::SIZE as i64));
            let address_cells = fx.load(PTR, parent, level::ADDRESS_CELLS);
            let size_cells = fx.load(PTR, parent, level::SIZE_CELLS);
            let reg = fx.b.use_var(node.reg);
            let reg_len = fx.b.use_var(node.reg_len);
            let cells = fx.b.ins().iadd(address_cells, size_cells);
            let pair = fx.b.ins().ishl_imm_s(cells, 2);
            let one = fx.ptr(1);
            let none = fx.b.ins().icmp_imm_s(IntCC::Equal, pair, 0);
            let divisor = fx.b.ins().select(none, one, pair);
            let count = fx.b.ins().udiv(reg_len, divisor);
            let zero = fx.ptr(0);
            let count = fx.b.ins().select(none, zero, count);
            let has_reg = fx.b.ins().icmp_imm_s(IntCC::NotEqual, count, 0);
            let cpu = fx.b.use_var(node.cpu);
            let cpu = fx.b.ins().icmp_imm_s(IntCC::NotEqual, cpu, 0);
            let cpu = fx.b.ins().band(cpu, has_reg);
            fx.when(cpu, |fx| {
                let mpidr = tree_number(fx, reg, address_cells);
                let affinity = fx.b.ins().band_imm_s(mpidr, AFFINITY);
                let other = fx.b.ins().icmp(IntCC::NotEqual, affinity, boot);
                fx.when(other, |fx| {
                    let zero = fx.ptr(0);
                    self.add_cpu(fx, affinity, zero);
                });
            });
            let psci = fx.b.ins().band_imm_s(kinds, compatible::PSCI);
            let psci = fx.b.ins().icmp_imm_s(IntCC::NotEqual, psci, 0);
            fx.when(psci, |fx| {
                let method = fx.b.use_var(node.method);
                fx.store(method, record, machine::CONDUIT);
            });
            fx.when(has_reg, |fx| {
                let size_offset = fx.b.ins().ishl_imm_s(address_cells, 2);
                let reg_entry = |lo: &mut Lowering, fx: &mut Fx, index: Value| {
                    let at = fx.b.ins().imul(index, pair);
                    let at = fx.b.ins().iadd(reg, at);
                    let address = tree_number(fx, at, address_cells);
                    let address = lo.call_helper(fx, helpers.translate, &[address, depth, levels]);
                    let size_at = fx.b.ins().iadd(at, size_offset);
                    let size = tree_number(fx, size_at, size_cells);
                    (address, size)
                };
                let zero = fx.ptr(0);
                let (base, _) = reg_entry(self, fx, zero);
                for (bit, field) in [
                    (compatible::ITS, machine::ITS),
                    (compatible::RTC, machine::RTC),
                ] {
                    let this = fx.b.ins().band_imm_s(kinds, bit);
                    let this = fx.b.ins().icmp_imm_s(IntCC::NotEqual, this, 0);
                    fx.when(this, |fx| first_field(fx, record, field, base));
                }
                let gic = fx.b.ins().band_imm_s(kinds, compatible::GIC);
                let gic = fx.b.ins().icmp_imm_s(IntCC::NotEqual, gic, 0);
                let held = fx.load(PTR, record, machine::GICD);
                let first = fx.b.ins().icmp_imm_s(IntCC::Equal, held, 0);
                let gic = fx.b.ins().band(gic, first);
                fx.when(gic, |fx| {
                    fx.store(base, record, machine::GICD);
                    let stated = fx.b.use_var(node.regions);
                    let listed = fx.b.ins().iadd_imm_s(count, -1);
                    let regions = fx.b.ins().umin(stated, listed);
                    let one = fx.ptr(1);
                    fx.each(zero, regions, |fx, index| {
                        let at = fx.b.ins().iadd(index, one);
                        let (base, len) = reg_entry(self, fx, at);
                        add_region(fx, record, base, len);
                    });
                });
                tree_uart(fx, record, entry, node, fixed, kinds, base);
            });
        });
    }

    /// Calls a helper function, giving its result.
    fn call_helper(&mut self, fx: &mut Fx, helper: FuncId, args: &[Value]) -> Value {
        let reference = self.call_ref(fx, helper);
        let call = fx.b.ins().call(reference, args);
        fx.b.inst_results(call)[0]
    }

    /// Declares a helper function of `params` words giving a word, and defines it with `body`,
    /// which receives the parameters and gives the result.
    fn helper(
        &mut self,
        name: &str,
        params: usize,
        body: impl FnOnce(&mut Self, &mut Fx, &[Value]) -> Value,
    ) -> FuncId {
        let mut signature = Signature::new(super::super::CALL_CONV);
        signature
            .params
            .extend((0..params).map(|_| AbiParam::new(PTR)));
        signature.returns.push(AbiParam::new(PTR));
        let id = self
            .module
            .declare_function(name, Linkage::Local, &signature)
            .expect("a device tree helper");
        self.define(id, signature, super::super::lines::HIDDEN, |lo, fx| {
            let params = fx.entry();
            let result = body(lo, fx, &params);
            fx.b.ins().return_(&[result]);
        });
        id
    }

    /// The helper giving 1 when the string list of a length at its first argument holds the
    /// NUL-terminated string at its third, 0 otherwise.
    fn listed_helper(&mut self) -> FuncId {
        self.helper("_NSdt_listed", 3, |_, fx, params| {
            let (list, len, wanted) = (params[0], params[1], params[2]);
            let end = fx.b.ins().iadd(list, len);
            let no = fx.int(types::I8, 0);
            let found = fx.repeat(&[list, no], |fx, state| {
                let at = state[0];
                let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, at, end);
                let next = fx.choose(
                    inside,
                    &[PTR, types::I8],
                    |fx| {
                        let same = same_text(fx, at, wanted);
                        vec![start::text_end(fx, at), same]
                    },
                    |fx| vec![at, fx.int(types::I8, 0)],
                );
                let missing = fx.b.ins().bxor_imm_s(next[1], 1);
                let more = fx.b.ins().band(inside, missing);
                (more, next)
            })[1];
            fx.b.ins().uextend(PTR, found)
        })
    }

    /// The helper giving, when the NUL-terminated name at its first argument is the path
    /// component at its second, which a NUL, `:` or `/` ends, the address of that end; zero
    /// otherwise.
    fn named_helper(&mut self) -> FuncId {
        self.helper("_NSdt_named", 2, |_, fx, params| {
            let (name, at) = (params[0], params[1]);
            let zero = fx.ptr(0);
            let ended = fx.repeat(&[zero, zero], |fx, state| {
                let index = state[0];
                let left = fx.b.ins().iadd(name, index);
                let left = fx.load(types::I8, left, 0);
                let right = fx.b.ins().iadd(at, index);
                let right = fx.load(types::I8, right, 0);
                let mut stop = fx.b.ins().icmp_imm_s(IntCC::Equal, right, 0);
                for separator in *b":/" {
                    let this =
                        fx.b.ins()
                            .icmp_imm_s(IntCC::Equal, right, i64::from(separator));
                    stop = fx.b.ins().bor(stop, this);
                }
                let last = fx.b.ins().icmp_imm_s(IntCC::Equal, left, 0);
                let both = fx.b.ins().band(last, stop);
                let both = fx.b.ins().uextend(PTR, both);
                let same = fx.b.ins().icmp(IntCC::Equal, left, right);
                let either = fx.b.ins().bor(last, stop);
                let neither = fx.b.ins().bxor_imm_s(either, 1);
                let more = fx.b.ins().band(same, neither);
                let next = fx.b.ins().iadd_imm_s(index, 1);
                let next = fx.b.ins().select(more, next, index);
                (more, vec![next, both])
            });
            let matched = fx.b.ins().icmp_imm_s(IntCC::NotEqual, ended[1], 0);
            let end = fx.b.ins().iadd(at, ended[0]);
            fx.b.ins().select(matched, end, zero)
        })
    }

    /// The helper translating an address in the space of the parent of the node at a depth,
    /// its second argument, into the root's, through the `ranges` of each ancestor below the
    /// root, from the level entries at its third. An ancestor without `ranges`, with an empty
    /// one, or with none covering the address leaves it as it is.
    fn translate_helper(&mut self) -> FuncId {
        self.helper("_NSdt_translate", 3, |_, fx, params| {
            let (address, depth, levels) = (params[0], params[1], params[2]);
            let bus = fx.b.ins().iadd_imm_s(depth, -1);
            fx.repeat(&[address, bus], |fx, state| {
                let (address, bus) = (state[0], state[1]);
                let more =
                    fx.b.ins()
                        .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, bus, 2);
                let translated = fx.choose(
                    more,
                    &[PTR],
                    |fx| vec![translate_once(fx, levels, bus, address)],
                    |_| vec![address],
                )[0];
                let outer = fx.b.ins().iadd_imm_s(bus, -1);
                (more, vec![translated, outer])
            })[0]
        })
    }
}

/// Records the UART of a node of `kinds` at `base` whose level entry is `entry`: the one the
/// UART's path ends at, or else the first PL011.
fn tree_uart(
    fx: &mut Fx,
    record: Value,
    entry: Value,
    node: Node,
    fixed: Variable,
    kinds: Value,
    base: Value,
) {
    let pl011 = fx.b.ins().band_imm_s(kinds, compatible::PL011);
    let pl011 = fx.b.ins().icmp_imm_s(IntCC::NotEqual, pl011, 0);
    let sbsa = fx.b.ins().band_imm_s(kinds, compatible::SBSA);
    let sbsa = fx.b.ins().icmp_imm_s(IntCC::NotEqual, sbsa, 0);
    let ns16550 = fx.b.ins().band_imm_s(kinds, compatible::NS16550);
    let ns16550 = fx.b.ins().icmp_imm_s(IntCC::NotEqual, ns16550, 0);
    let kind = uart_kind(fx, pl011, sbsa, ns16550);
    let place = fx.load(PTR, entry, level::PATH);
    let on_path = fx.b.ins().icmp_imm_s(IntCC::NotEqual, place, 0);
    let stop = fx.choose(
        on_path,
        &[types::I8],
        |fx| vec![fx.load(types::I8, place, 0)],
        |fx| vec![fx.int(types::I8, i64::from(b'/'))],
    )[0];
    let ended = fx.b.ins().icmp_imm_s(IntCC::Equal, stop, 0);
    let options = fx.b.ins().icmp_imm_s(IntCC::Equal, stop, i64::from(b':'));
    let named = fx.b.ins().bor(ended, options);
    let held = fx.b.use_var(fixed);
    let open = fx.b.ins().icmp_imm_s(IntCC::Equal, held, 0);
    let current = fx.load(PTR, record, machine::UART);
    let unset = fx.b.ins().icmp_imm_s(IntCC::Equal, current, 0);
    let fallback = fx.b.ins().band(open, unset);
    let fallback = fx.b.ins().band(fallback, pl011);
    let chosen = fx.b.ins().bor(named, fallback);
    let known = fx.b.ins().icmp_imm_s(IntCC::NotEqual, kind, 0);
    let taken = fx.b.ins().band(chosen, known);
    fx.when(taken, |fx| {
        let shift = fx.b.use_var(node.shift);
        let stated = fx.b.use_var(node.width);
        let zero = fx.ptr(0);
        let shift = fx.b.ins().select(ns16550, shift, zero);
        let four = fx.ptr(4);
        let width = fx.b.ins().select(ns16550, stated, four);
        set_uart(fx, record, base, kind, shift, width);
        let named = fx.b.ins().uextend(PTR, named);
        let held = fx.b.use_var(fixed);
        let held = fx.b.ins().bor(held, named);
        fx.b.def_var(fixed, held);
    });
}

/// An address in the space of a bus node's children translated by the bus's `ranges` into its
/// parent's space.
fn translate_once(fx: &mut Fx, levels: Value, bus: Value, address: Value) -> Value {
    let entry = fx.b.ins().imul_imm_s(bus, level::SIZE as i64);
    let entry = fx.b.ins().iadd(levels, entry);
    let parent = fx.b.ins().iadd_imm_s(entry, -(level::SIZE as i64));
    let ranges = fx.load(PTR, entry, level::RANGES);
    let len = fx.load(PTR, entry, level::RANGES_LEN);
    let child_cells = fx.load(PTR, entry, level::ADDRESS_CELLS);
    let size_cells = fx.load(PTR, entry, level::SIZE_CELLS);
    let parent_cells = fx.load(PTR, parent, level::ADDRESS_CELLS);
    let cells = fx.b.ins().iadd(child_cells, parent_cells);
    let cells = fx.b.ins().iadd(cells, size_cells);
    let bytes = fx.b.ins().ishl_imm_s(cells, 2);
    let one = fx.ptr(1);
    let none = fx.b.ins().icmp_imm_s(IntCC::Equal, bytes, 0);
    let divisor = fx.b.ins().select(none, one, bytes);
    let count = fx.b.ins().udiv(len, divisor);
    let zero = fx.ptr(0);
    let given = fx.b.ins().icmp_imm_s(IntCC::NotEqual, ranges, 0);
    let count = fx.b.ins().select(given, count, zero);
    let count = fx.b.ins().select(none, zero, count);
    let result = fx.var(PTR, address);
    let done = fx.var(PTR, zero);
    let parent_at = fx.b.ins().ishl_imm_s(child_cells, 2);
    let size_at = fx.b.ins().iadd(child_cells, parent_cells);
    let size_at = fx.b.ins().ishl_imm_s(size_at, 2);
    fx.each(zero, count, |fx, index| {
        let at = fx.b.ins().imul(index, bytes);
        let at = fx.b.ins().iadd(ranges, at);
        let child = tree_number(fx, at, child_cells);
        let target = fx.b.ins().iadd(at, parent_at);
        let target = tree_number(fx, target, parent_cells);
        let size = fx.b.ins().iadd(at, size_at);
        let size = tree_number(fx, size, size_cells);
        let above =
            fx.b.ins()
                .icmp(IntCC::UnsignedGreaterThanOrEqual, address, child);
        let offset = fx.b.ins().isub(address, child);
        let below = fx.b.ins().icmp(IntCC::UnsignedLessThan, offset, size);
        let held = fx.b.use_var(done);
        let open = fx.b.ins().icmp_imm_s(IntCC::Equal, held, 0);
        let within = fx.b.ins().band(above, below);
        let within = fx.b.ins().band(within, open);
        fx.when(within, |fx| {
            let translated = fx.b.ins().iadd(target, offset);
            fx.b.def_var(result, translated);
            let one = fx.ptr(1);
            fx.b.def_var(done, one);
        });
    });
    fx.b.use_var(result)
}

/// The device tree's token that holds no data.
const NOP: i64 = 4;

/// The variables and storage of a walk of a device tree's structure block: the level entries,
/// the depth of the node it is in, whether that node's properties are yet to be recorded, the
/// address of the next token, and the node's record.
#[derive(Clone, Copy)]
struct Walk {
    levels: Value,
    depth: Variable,
    pending: Variable,
    next: Variable,
    node: Node,
}

/// A property of a device tree's structure block: its name, value, value's length and the
/// address after it.
#[derive(Clone, Copy)]
struct Property {
    name: Value,
    value: Value,
    len: Value,
    after: Value,
}

/// The property at `at` of a device tree's structure block whose strings block lies at
/// `strings`.
fn structure_property(fx: &mut Fx, at: Value, strings: Value) -> Property {
    let len = tree_cell(fx, at, device_tree::LEN);
    let offset = tree_cell(fx, at, device_tree::NAME);
    let name = fx.b.ins().iadd(strings, offset);
    let value = fx.b.ins().iadd_imm_s(at, device_tree::VALUE);
    let padded = fx.b.ins().iadd_imm_s(len, 3);
    let padded = fx.b.ins().band_imm_s(padded, -4);
    let after = fx.b.ins().iadd(value, padded);
    Property {
        name,
        value,
        len,
        after,
    }
}

/// Whether the NUL-terminated strings at two addresses are the same, as a byte.
fn same_text(fx: &mut Fx, left: Value, right: Value) -> Value {
    let zero = fx.ptr(0);
    let no = fx.int(types::I8, 0);
    fx.repeat(&[zero, no], |fx, state| {
        let index = state[0];
        let one = fx.b.ins().iadd(left, index);
        let one = fx.load(types::I8, one, 0);
        let other = fx.b.ins().iadd(right, index);
        let other = fx.load(types::I8, other, 0);
        let same = fx.b.ins().icmp(IntCC::Equal, one, other);
        let ended = fx.b.ins().icmp_imm_s(IntCC::Equal, one, 0);
        let equal = fx.b.ins().band(same, ended);
        let going = fx.b.ins().bxor_imm_s(ended, 1);
        let more = fx.b.ins().band(same, going);
        let next = fx.b.ins().iadd_imm_s(index, 1);
        (more, vec![next, equal])
    })[1]
}

/// The kind of a UART that is a PL011, an SBSA generic UART or a 16550, zero for none.
fn uart_kind(fx: &mut Fx, pl011: Value, sbsa: Value, ns16550: Value) -> Value {
    let zero = fx.ptr(0);
    let kind = fx.ptr(uart::NS16550 as u64);
    let kind = fx.b.ins().select(ns16550, kind, zero);
    let sbsa_kind = fx.ptr(uart::SBSA as u64);
    let kind = fx.b.ins().select(sbsa, sbsa_kind, kind);
    let pl011_kind = fx.ptr(uart::PL011 as u64);
    fx.b.ins().select(pl011, pl011_kind, kind)
}

/// Records the UART halt reports are written to.
fn set_uart(fx: &mut Fx, record: Value, base: Value, kind: Value, shift: Value, width: Value) {
    fx.store(base, record, machine::UART);
    fx.store(kind, record, machine::UART_KIND);
    fx.store(shift, record, machine::UART_SHIFT);
    fx.store(width, record, machine::UART_WIDTH);
}

/// Stores `value` in a field of the machine record that still holds zero.
fn first_field(fx: &mut Fx, record: Value, field: u64, value: Value) {
    let held = fx.load(PTR, record, field);
    let unset = fx.b.ins().icmp_imm_s(IntCC::Equal, held, 0);
    fx.when(unset, |fx| fx.store(value, record, field));
}

/// Records a redistributor region after those recorded, unless `MOST_REGIONS` are.
fn add_region(fx: &mut Fx, record: Value, base: Value, len: Value) {
    let count = fx.load(PTR, record, machine::REGIONS);
    let room =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedLessThan, count, machine::MOST_REGIONS as i64);
    fx.when(room, |fx| {
        let at = fx.b.ins().imul_imm_s(count, machine::REGION_BYTES as i64);
        let at = fx.b.ins().iadd(record, at);
        fx.store(base, at, machine::REGION);
        fx.store(len, at, machine::REGION + 8);
        let count = fx.b.ins().iadd_imm_s(count, 1);
        fx.store(count, record, machine::REGIONS);
    });
}
