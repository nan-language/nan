//! The x86_64 lines `hw::IrqShared.install_line` installs: pins of an I/O APIC, edge-triggered
//! or level-triggered and active low or high, each delivered to one vector of the boot CPU whose
//! devices sit in the row of every CPU, in the remapped format once the IOMMU remaps interrupts,
//! and masked once no device is left on them. A line names its pin as the I/O APIC's register
//! base shifted left by 8 bits with the pin's number.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, condcodes::IntCC, types};

use super::super::fx::{Fx, PTR};
use super::Lowering;
use super::bindings::{MSI_ID_MAX, MSI_VECTORS, device, msi};

/// The I/O APIC's register window after its register select, its version register, whose bits
/// 16 to 23 hold its last pin's number, and the first of the two registers of pin 0's entry.
const WINDOW: i64 = 0x10;
const VERSION: u64 = 0x01;
const ENTRIES: i64 = 0x10;

/// The bits of an entry's first word making it level-triggered and active low, or masked; and
/// its second, in the remapped format with the remapping entry's index from bit 17.
const LEVEL: i64 = 0x8000;
const ACTIVE_LOW: i64 = 0x2000;
const MASKED: u64 = 0x1_0000;
const REMAPPABLE: i64 = 0x1_0000;

impl Lowering<'_, '_> {
    /// The x86_64 part of `install_line`, under the bindings lock: a level-triggered record joins
    /// the devices installed on the line when its pin has the record's mode, and any record takes
    /// a vector of its own on a line no device is installed on.
    pub(super) fn x64_install_line(
        &mut self,
        fx: &mut Fx,
        record: Value,
        line: Value,
        source: Value,
        edge: Value,
        active_low: Value,
    ) -> Value {
        let level = fx.b.ins().bxor_imm_s(edge, 1);
        let trigger = fx.b.ins().ishl_imm_s(level, 15);
        let polarity = fx.b.ins().ishl_imm_s(active_low, 13);
        let mode = fx.b.ins().bor(trigger, polarity);
        let every = fx.ptr(0);
        let first = fx.ptr(MSI_VECTORS.0 as u64);
        let vector = fx.repeat(&[first], |fx, at| {
            let vector = at[0];
            let slot = self.irq_binding(fx, every, vector);
            let on = self.line_chained(fx, slot, line);
            let off = fx.b.ins().bxor_imm_s(on, 1);
            let next = fx.b.ins().iadd_imm_s(vector, 1);
            let inside =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedLessThan, next, MSI_VECTORS.1);
            let more = fx.b.ins().band(off, inside);
            (more, vec![fx.b.ins().select(off, next, vector)])
        })[0];
        let shared =
            fx.b.ins()
                .icmp_imm_s(IntCC::UnsignedLessThan, vector, MSI_VECTORS.1);
        fx.choose_in(
            self,
            shared,
            &[PTR],
            |lo, fx| {
                let apic = fx.b.ins().ushr_imm_s(line, 8);
                let pin = fx.b.ins().band_imm_s(line, 0xFF);
                let entry = lo.ioapic_entry(fx, pin);
                let held = lo.ioapic_read(fx, apic, entry);
                let held = fx.b.ins().band_imm_s(held, LEVEL | ACTIVE_LOW);
                let same = fx.b.ins().icmp(IntCC::Equal, held, mode);
                let shares = fx.b.ins().icmp_imm_s(IntCC::NotEqual, level, 0);
                let joins = fx.b.ins().band(same, shares);
                fx.when(joins, |fx| lo.line_chain(fx, record, line, vector));
                let installed = fx.ptr(msi::INSTALLED as u64);
                let unsupported = fx.ptr(msi::UNSUPPORTED as u64);
                vec![fx.b.ins().select(joins, installed, unsupported)]
            },
            |lo, fx| vec![lo.x64_new_line(fx, record, line, source, level, mode)],
        )[0]
    }

    /// Installs a device record on a line no device is installed on, its pin in `mode`, which is
    /// level-triggered when `level` is 1: the lowest vector of `MSI_VECTORS` the final build does
    /// not bind and no device is installed on for any CPU, with the boot CPU's remapping entry
    /// for it, accepting only requester `source`, once the IOMMU remaps interrupts. Unsupported
    /// when the I/O APIC has no such pin, and unreachable when the entry cannot name the boot
    /// CPU's local APIC id.
    fn x64_new_line(
        &mut self,
        fx: &mut Fx,
        record: Value,
        line: Value,
        source: Value,
        level: Value,
        mode: Value,
    ) -> Value {
        let apic = fx.b.ins().ushr_imm_s(line, 8);
        let pin = fx.b.ins().band_imm_s(line, 0xFF);
        let version = fx.ptr(VERSION);
        let version = self.ioapic_read(fx, apic, version);
        let last = fx.b.ins().ushr_imm_s(version, 16);
        let last = fx.b.ins().band_imm_s(last, 0xFF);
        let held = fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, pin, last);
        let unsupported = fx.ptr(msi::UNSUPPORTED as u64);
        let outcome = fx.var(PTR, unsupported);
        fx.when(held, |fx| {
            let bound = self.statically_bound();
            let first = fx.ptr(MSI_VECTORS.0 as u64);
            let vector = fx.repeat(&[first], |fx, at| {
                let vector = at[0];
                let mut taken = self.installed_for(fx, vector, None);
                for bound in &bound {
                    let same = fx.b.ins().icmp_imm_s(IntCC::Equal, vector, *bound);
                    taken = fx.b.ins().bor(taken, same);
                }
                let next = fx.b.ins().iadd_imm_s(vector, 1);
                let inside =
                    fx.b.ins()
                        .icmp_imm_s(IntCC::UnsignedLessThan, next, MSI_VECTORS.1);
                let more = fx.b.ins().band(taken, inside);
                (more, vec![fx.b.ins().select(taken, next, vector)])
            })[0];
            let free =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedLessThan, vector, MSI_VECTORS.1);
            let exhausted = fx.ptr(msi::EXHAUSTED as u64);
            fx.b.def_var(outcome, exhausted);
            fx.when(free, |fx| {
                let boot = fx.ptr(0);
                let id = self.hardware_id(fx, boot);
                let unit = self.iommu_unit(fx);
                let remapping = fx.b.ins().icmp_imm_s(IntCC::NotEqual, unit, 0);
                let entry = fx.choose_in(
                    self,
                    remapping,
                    &[PTR, types::I8],
                    |lo, fx| {
                        let address =
                            lo.remap(fx, unit, boot, vector, id, source, true, Some(level));
                        let reached = fx.b.ins().icmp_imm_s(IntCC::NotEqual, address, 0);
                        let index = fx.b.ins().ishl_imm_s(vector, 17);
                        vec![fx.b.ins().bor_imm_s(index, REMAPPABLE), reached]
                    },
                    |_, fx| {
                        let near =
                            fx.b.ins()
                                .icmp_imm_s(IntCC::UnsignedLessThanOrEqual, id, MSI_ID_MAX);
                        vec![fx.b.ins().ishl_imm_s(id, 24), near]
                    },
                );
                let (high, reached) = (entry[0], entry[1]);
                let installed = fx.ptr(msi::INSTALLED as u64);
                let unreachable = fx.ptr(msi::UNREACHABLE as u64);
                let chosen = fx.b.ins().select(reached, installed, unreachable);
                fx.b.def_var(outcome, chosen);
                fx.when(reached, |fx| {
                    self.line_chain(fx, record, line, vector);
                    let entry = self.ioapic_entry(fx, pin);
                    let masked = fx.ptr(MASKED);
                    self.ioapic_write(fx, apic, entry, masked);
                    let upper = fx.b.ins().iadd_imm_s(entry, 1);
                    self.ioapic_write(fx, apic, upper, high);
                    let low = fx.b.ins().bor(vector, mode);
                    self.ioapic_write(fx, apic, entry, low);
                });
            });
        });
        fx.b.use_var(outcome)
    }

    /// After a device record leaves its binding's chain at `slot`, masks the pin of the line it
    /// was installed on, and clears the line's remapping entry, when no other device is left on
    /// the line.
    pub(super) fn x64_line_left(&mut self, fx: &mut Fx, record: Value, slot: Value) {
        let line = fx.load(PTR, record, device::LINE);
        let lined = fx.b.ins().icmp_imm_s(IntCC::NotEqual, line, 0);
        fx.when(lined, |fx| {
            let on = self.line_chained(fx, slot, line);
            let last = fx.b.ins().bxor_imm_s(on, 1);
            fx.when(last, |fx| {
                let apic = fx.b.ins().ushr_imm_s(line, 8);
                let pin = fx.b.ins().band_imm_s(line, 0xFF);
                let entry = self.ioapic_entry(fx, pin);
                let masked = fx.ptr(MASKED);
                self.ioapic_write(fx, apic, entry, masked);
                let unit = self.iommu_unit(fx);
                let remapping = fx.b.ins().icmp_imm_s(IntCC::NotEqual, unit, 0);
                fx.when(remapping, |fx| {
                    let vector = fx.load(PTR, record, device::VECTOR);
                    self.clear_remapping(fx, unit, vector);
                });
            });
        });
    }

    /// Whether a device installed on a line is chained in the binding at `slot`.
    fn line_chained(&mut self, fx: &mut Fx, slot: Value, line: Value) -> Value {
        let first = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), slot);
        let none = fx.int(types::I8, 0);
        fx.repeat(&[first, none], |fx, state| {
            let (current, found) = (state[0], state[1]);
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
            let seen = fx.choose(
                present,
                &[PTR, types::I8],
                |fx| {
                    let next = fx.load(PTR, current, device::NEXT);
                    let named = fx.load(PTR, current, device::LINE);
                    vec![next, fx.b.ins().icmp(IntCC::Equal, named, line)]
                },
                |fx| vec![fx.ptr(0), fx.int(types::I8, 0)],
            );
            let found = fx.b.ins().bor(found, seen[1]);
            let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, seen[0], 0);
            (more, vec![seen[0], found])
        })[1]
    }

    /// Chains a device record installed on a line at the end of the devices of its vector in
    /// the row of every CPU.
    fn line_chain(&mut self, fx: &mut Fx, record: Value, line: Value, vector: Value) {
        fx.store(vector, record, device::VECTOR);
        fx.store(line, record, device::LINE);
        let every = fx.ptr(0);
        let slot = self.irq_binding(fx, every, vector);
        self.chain(fx, record, slot);
    }

    /// The register of the first word of a pin's entry.
    fn ioapic_entry(&mut self, fx: &mut Fx, pin: Value) -> Value {
        let entry = fx.b.ins().ishl_imm_s(pin, 1);
        fx.b.ins().iadd_imm_s(entry, ENTRIES)
    }

    /// Reads an I/O APIC's register, selecting it first.
    fn ioapic_read(&mut self, fx: &mut Fx, apic: Value, register: Value) -> Value {
        self.mmio_write(fx, 4, apic, register);
        let window = fx.b.ins().iadd_imm_s(apic, WINDOW);
        self.mmio_read(fx, 4, window)
    }

    /// Writes an I/O APIC's register, selecting it first.
    fn ioapic_write(&mut self, fx: &mut Fx, apic: Value, register: Value, value: Value) {
        self.mmio_write(fx, 4, apic, register);
        let window = fx.b.ins().iadd_imm_s(apic, WINDOW);
        self.mmio_write(fx, 4, window, value);
    }
}
