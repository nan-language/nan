//! The machine's IOMMU as the runtime drives it once std's `hw` has enabled it: the unit record
//! `hw` builds and the runtime state names, its lock, the invalidation of everything the unit
//! caches through its command queue, and on VT-d the remapping table entries of the
//! message-signalled interrupts `install_msi` gives, one per CPU and vector.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, condcodes::IntCC};

use super::super::fx::{Fx, PTR};
use super::super::runtime::{Routine, state};
use super::Lowering;
use super::stubs::Stub;

/// The offsets of the unit record's words the runtime reads: its kind, its register base, its
/// command queue and the queue's next entry, the remapping table, its flags, its lock and the
/// word a VT-d invalidation wait writes.
mod unit {
    pub const KIND: u64 = 0;
    pub const REGISTERS: u64 = 8;
    pub const QUEUE: u64 = 16;
    pub const TAIL: u64 = 24;
    pub const REMAPPING: u64 = 32;
    pub const FLAGS: u64 = 40;
    pub const LOCK: u64 = 48;
    pub const WAIT: u64 = 56;
}

/// The unit's kinds and the flags the runtime reads: interrupt remapping in extended mode,
/// reads and writes an invalidation drains, and table walks that miss the caches.
const VTD: i64 = 1;
const EXTENDED: i64 = 1;
const DRAIN_READS: i64 = 2;
const DRAIN_WRITES: i64 = 4;
const UNCOHERENT: i64 = 8;

/// The entries of the command queue and the bytes of one.
const QUEUE_ENTRIES: i64 = 256;
const QUEUE_ENTRY: i64 = 16;

/// The VT-d invalidation descriptors: of every context, of every translation with its read
/// and write drains, of every interrupt entry, and a wait writing 1 to an address; and the
/// queue tail register.
const CONTEXTS: i64 = 0x11;
const TRANSLATIONS: i64 = 0x12;
const DRAIN_READ: i64 = 0x80;
const DRAIN_WRITE: i64 = 0x40;
const INTERRUPT_ENTRIES: i64 = 0x4;
const WAIT: i64 = 0x1_0000_0025;
const QUEUE_TAIL: u64 = 0x88;

/// The SMMUv3 commands: invalidating every configuration, every non-secure translation, and a
/// synchronisation; its queue's produce and consume registers, and the index bits with the
/// wrap bit of a queue of `QUEUE_ENTRIES`.
const CONFIGURATIONS: i64 = 0x04;
const EVERY_CONFIGURATION: i64 = 31;
const NON_SECURE: i64 = 0x30;
const SYNC: i64 = 0x46;
const PRODUCE: u64 = 0x98;
const CONSUME: u64 = 0x9C;
const WRAPPED: i64 = 2 * QUEUE_ENTRIES - 1;

/// A remapping table entry's first word: present, its vector and destination; its second, the
/// requester id it accepts and the bit validating it. The message naming entry `i` has address
/// `0xFEE0_0000 | (i & 0x7FFF) << 5 | REMAPPED | (i >> 15) << 2`, which the format bit marks,
/// and data 0.
const PRESENT: i64 = 1;
const VALIDATED: i64 = 1 << 18;
const REMAPPED: i64 = 0x10;
const MSI_BASE: i64 = 0xFEE0_0000;

impl Lowering<'_, '_> {
    /// The unit record, zero while no IOMMU is enabled.
    pub(crate) fn iommu_unit(&mut self, fx: &mut Fx) -> Value {
        let state_address = self.state(fx);
        fx.load(PTR, state_address, state::IOMMU)
    }

    /// Takes a unit's lock with maskable interrupts disabled, giving the state to restore.
    pub(crate) fn iommu_lock(&mut self, fx: &mut Fx, unit: Value) -> Value {
        let flags = self.call_stub(fx, Stub::IrqSave, &[])[0];
        let lock = fx.offset(unit, unit::LOCK);
        fx.repeat(&[], |fx, _| {
            let free = fx.ptr(0);
            let held = fx.ptr(1);
            let seen =
                fx.b.ins()
                    .atomic_cas(MemFlagsData::trusted(), lock, free, held);
            let busy = fx.b.ins().icmp_imm_s(IntCC::NotEqual, seen, 0);
            fx.when(busy, |fx| {
                self.call_stub(fx, Stub::Pause, &[]);
            });
            (busy, Vec::new())
        });
        flags
    }

    pub(crate) fn iommu_unlock(&mut self, fx: &mut Fx, unit: Value, flags: Value) {
        let lock = fx.offset(unit, unit::LOCK);
        let free = fx.ptr(0);
        fx.b.ins().atomic_store(MemFlagsData::trusted(), free, lock);
        self.call_stub(fx, Stub::IrqRestore, &[flags]);
    }

    /// `IommuFlush()`: under the unit's lock, invalidates every context, translation and
    /// interrupt entry the unit caches, draining the requests in flight where it can, and
    /// waits until the unit has.
    pub(crate) fn define_iommu_flush(&mut self, fx: &mut Fx) {
        fx.entry();
        let unit = self.iommu_unit(fx);
        let kind = fx.load(PTR, unit, unit::KIND);
        let vtd = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, VTD);
        fx.choose_in(
            self,
            vtd,
            &[],
            |lo, fx| {
                lo.vtd_flush(fx, unit);
                Vec::new()
            },
            |lo, fx| {
                lo.smmu_flush(fx, unit);
                Vec::new()
            },
        );
        fx.b.ins().return_(&[]);
    }

    /// Queues VT-d's global invalidations and a wait writing the unit's wait word, then polls
    /// that word.
    fn vtd_flush(&mut self, fx: &mut Fx, unit: Value) {
        let flags = fx.load(PTR, unit, unit::FLAGS);
        let reads = fx.b.ins().band_imm_s(flags, DRAIN_READS);
        let reads = fx.b.ins().icmp_imm_s(IntCC::NotEqual, reads, 0);
        let writes = fx.b.ins().band_imm_s(flags, DRAIN_WRITES);
        let writes = fx.b.ins().icmp_imm_s(IntCC::NotEqual, writes, 0);
        let none = fx.ptr(0);
        let read_bit = fx.ptr(DRAIN_READ as u64);
        let write_bit = fx.ptr(DRAIN_WRITE as u64);
        let read_bit = fx.b.ins().select(reads, read_bit, none);
        let write_bit = fx.b.ins().select(writes, write_bit, none);
        let translations = fx.b.ins().bor(read_bit, write_bit);
        let translations = fx.b.ins().bor_imm_s(translations, TRANSLATIONS);
        let wait = fx.offset(unit, unit::WAIT);
        fx.store(none, wait, 0);
        let contexts = fx.ptr(CONTEXTS as u64);
        let entries = fx.ptr(INTERRUPT_ENTRIES as u64);
        let waiting = fx.ptr(WAIT as u64);
        for [low, high] in [
            [contexts, none],
            [translations, none],
            [entries, none],
            [waiting, wait],
        ] {
            self.queue_command(fx, unit, low, high);
        }
        self.call_stub(fx, Stub::Mfence, &[]);
        let tail = fx.load(PTR, unit, unit::TAIL);
        let tail = fx.b.ins().imul_imm_s(tail, QUEUE_ENTRY);
        let registers = fx.load(PTR, unit, unit::REGISTERS);
        let at = fx.b.ins().iadd_imm_s(registers, QUEUE_TAIL as i64);
        self.mmio_write(fx, 8, at, tail);
        fx.repeat(&[], |fx, _| {
            let written = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), wait);
            let busy = fx.b.ins().icmp_imm_s(IntCC::Equal, written, 0);
            fx.when(busy, |fx| {
                self.call_stub(fx, Stub::Pause, &[]);
            });
            (busy, Vec::new())
        });
    }

    /// Queues SMMUv3's invalidation of every configuration and non-secure translation and a
    /// synchronisation, then polls until the unit has consumed them.
    fn smmu_flush(&mut self, fx: &mut Fx, unit: Value) {
        let none = fx.ptr(0);
        let configurations = fx.ptr(CONFIGURATIONS as u64);
        let every = fx.ptr(EVERY_CONFIGURATION as u64);
        let translations = fx.ptr(NON_SECURE as u64);
        let sync = fx.ptr(SYNC as u64);
        for [low, high] in [[configurations, every], [translations, none], [sync, none]] {
            self.queue_command(fx, unit, low, high);
        }
        self.call_stub(fx, Stub::Mfence, &[]);
        let produced = fx.load(PTR, unit, unit::TAIL);
        let registers = fx.load(PTR, unit, unit::REGISTERS);
        let at = fx.b.ins().iadd_imm_s(registers, PRODUCE as i64);
        self.mmio_write(fx, 4, at, produced);
        let at = fx.b.ins().iadd_imm_s(registers, CONSUME as i64);
        fx.repeat(&[], |fx, _| {
            let consumed = self.mmio_read(fx, 4, at);
            let consumed = fx.b.ins().band_imm_s(consumed, WRAPPED);
            let busy = fx.b.ins().icmp(IntCC::NotEqual, consumed, produced);
            fx.when(busy, |fx| {
                self.call_stub(fx, Stub::Pause, &[]);
            });
            (busy, Vec::new())
        });
    }

    /// Writes a command of two words at the queue's next entry and advances it: a VT-d tail
    /// counts entries modulo the queue's, an SMMUv3 index carries its wrap bit.
    fn queue_command(&mut self, fx: &mut Fx, unit: Value, low: Value, high: Value) {
        let queue = fx.load(PTR, unit, unit::QUEUE);
        let tail = fx.load(PTR, unit, unit::TAIL);
        let index = fx.b.ins().band_imm_s(tail, QUEUE_ENTRIES - 1);
        let at = fx.b.ins().imul_imm_s(index, QUEUE_ENTRY);
        let at = fx.b.ins().iadd(queue, at);
        fx.store(low, at, 0);
        fx.store(high, at, 8);
        let next = fx.b.ins().iadd_imm_s(tail, 1);
        let kind = fx.load(PTR, unit, unit::KIND);
        let vtd = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, VTD);
        let mask = fx.ptr(QUEUE_ENTRIES as u64 - 1);
        let wrapped = fx.ptr(WRAPPED as u64);
        let mask = fx.b.ins().select(vtd, mask, wrapped);
        let next = fx.b.ins().band(next, mask);
        fx.store(next, unit, unit::TAIL);
    }

    /// Writes the remapping table entry of CPU `cpu`'s vector `vector`, whose local APIC has id
    /// `id`, accepting only requester `source` when `validated` holds, and invalidates the unit's
    /// caches; gives the message naming it, or zero when the entry cannot name an id past 255.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn remap(
        &mut self,
        fx: &mut Fx,
        unit: Value,
        cpu: Value,
        vector: Value,
        id: Value,
        source: Value,
        validated: bool,
    ) -> Value {
        let flags = fx.load(PTR, unit, unit::FLAGS);
        let extended = fx.b.ins().band_imm_s(flags, EXTENDED);
        let extended = fx.b.ins().icmp_imm_s(IntCC::NotEqual, extended, 0);
        let narrow =
            fx.b.ins()
                .icmp_imm_s(IntCC::UnsignedLessThanOrEqual, id, 0xFF);
        let reachable = fx.b.ins().bor(extended, narrow);
        let zero = fx.ptr(0);
        let address = fx.var(PTR, zero);
        fx.when(reachable, |fx| {
            let index = fx.b.ins().ishl_imm_s(cpu, 8);
            let index = fx.b.ins().iadd(index, vector);
            let entry = self.remapping_entry(fx, unit, index);
            let shifted = fx.b.ins().ishl_imm_s(id, 8);
            let destination = fx.b.ins().select(extended, id, shifted);
            let destination = fx.b.ins().ishl_imm_s(destination, 32);
            let low = fx.b.ins().ishl_imm_s(vector, 16);
            let low = fx.b.ins().bor(low, destination);
            let low = fx.b.ins().bor_imm_s(low, PRESENT);
            let high = match validated {
                true => {
                    let requester = fx.b.ins().band_imm_s(source, 0xFFFF);
                    fx.b.ins().bor_imm_s(requester, VALIDATED)
                }
                false => fx.ptr(0),
            };
            fx.store(high, entry, 8);
            self.call_stub(fx, Stub::Mfence, &[]);
            fx.store(low, entry, 0);
            self.publish_entry(fx, unit, entry);
            let handle = fx.b.ins().band_imm_s(index, 0x7FFF);
            let handle = fx.b.ins().ishl_imm_s(handle, 5);
            let upper = fx.b.ins().ushr_imm_s(index, 15);
            let upper = fx.b.ins().ishl_imm_s(upper, 2);
            let message = fx.b.ins().bor(handle, upper);
            let message = fx.b.ins().bor_imm_s(message, MSI_BASE | REMAPPED);
            fx.b.def_var(address, message);
        });
        fx.b.use_var(address)
    }

    /// Clears the remapping table entry a remapped message names, and invalidates the unit's
    /// caches.
    pub(super) fn unremap(&mut self, fx: &mut Fx, unit: Value, message: Value) {
        let low = fx.b.ins().ushr_imm_s(message, 5);
        let low = fx.b.ins().band_imm_s(low, 0x7FFF);
        let high = fx.b.ins().ushr_imm_s(message, 2);
        let high = fx.b.ins().band_imm_s(high, 1);
        let high = fx.b.ins().ishl_imm_s(high, 15);
        let index = fx.b.ins().bor(low, high);
        let entry = self.remapping_entry(fx, unit, index);
        let none = fx.ptr(0);
        fx.store(none, entry, 0);
        self.publish_entry(fx, unit, entry);
    }

    /// Whether a message is in the remapped format.
    pub(super) fn is_remapped(&mut self, fx: &mut Fx, message: Value) -> Value {
        let format = fx.b.ins().band_imm_s(message, REMAPPED);
        fx.b.ins().icmp_imm_s(IntCC::NotEqual, format, 0)
    }

    /// The address of a remapping table entry.
    fn remapping_entry(&mut self, fx: &mut Fx, unit: Value, index: Value) -> Value {
        let table = fx.load(PTR, unit, unit::REMAPPING);
        let at = fx.b.ins().imul_imm_s(index, 16);
        fx.b.ins().iadd(table, at)
    }

    /// Writes a remapping table entry back to memory when the unit's walks miss the caches,
    /// then invalidates the unit's caches under its lock.
    fn publish_entry(&mut self, fx: &mut Fx, unit: Value, entry: Value) {
        let flags = fx.load(PTR, unit, unit::FLAGS);
        let uncoherent = fx.b.ins().band_imm_s(flags, UNCOHERENT);
        let uncoherent = fx.b.ins().icmp_imm_s(IntCC::NotEqual, uncoherent, 0);
        fx.when(uncoherent, |fx| {
            self.call_stub(fx, Stub::FlushLine, &[entry]);
        });
        let held = self.iommu_lock(fx, unit);
        self.call_routine(fx, Routine::IommuFlush, &[]);
        self.iommu_unlock(fx, unit, held);
    }
}
