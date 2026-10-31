//! The ITS of the AArch64 interrupt controller, which translates the messages devices write into
//! LPIs: its tables and command queue, set up with the interrupts when the distributor reports
//! LPIs; each CPU's LPI tables; and the commands mapping a device's event to an LPI on a CPU,
//! which the runtime drives synchronously under the bindings lock. An installed device's event
//! is its LPI's index, so each device id's interrupt table covers every LPI.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, condcodes::IntCC, types};

use super::super::fx::{Fx, PTR};
use super::super::runtime::system::map_chunks;
use super::super::runtime::{report, state};
use super::Lowering;
use super::aarch64::{GICD, GICR, GICR_STRIDE};
use super::bindings::{binding, device, msi};
use super::cpus::MAX_CPUS;
use super::stubs::Stub;

/// The first LPI's identifier, and the LPIs the runtime installs devices on: those that
/// identifiers of `LPI_BITS` bits reach, whose indexes, the events of their devices, take
/// `EVENT_BITS` bits.
pub const LPI_BASE: i64 = 8_192;
const LPIS: u64 = 8_192;
const LPI_BITS: u64 = 14;
const EVENT_BITS: u64 = 13;

/// The distributor's type register and its bit reporting LPIs.
const GICD_TYPER: u64 = 0x4;
const GICD_LPIS: i64 = 1 << 17;

/// The ITS the descriptor names and its registers: its control register with its enable bit,
/// its type register, the command queue's base, write and read registers, its first table base
/// register, a doubleword per table, and the translation register devices write.
const GITS: u64 = 0x0808_0000;
const GITS_CTLR: u64 = 0x0;
const GITS_ENABLED: u64 = 1;
const GITS_TYPER: u64 = 0x8;
const GITS_CBASER: u64 = 0x80;
const GITS_CWRITER: u64 = 0x88;
const GITS_CREADR: u64 = 0x90;
const GITS_BASER: u64 = 0x100;
const GITS_TABLES: u64 = 8;
const GITS_TRANSLATER: u64 = GITS + 0x1_0040;

/// The fields of the ITS type register: the bytes of an interrupt table entry less one, the
/// event id bits less one, the device id bits less one, each at its position under its mask,
/// and the bit saying collections name redistributors by address.
const TYPER_ENTRY: (i64, i64) = (4, 0xF);
const TYPER_EVENTS: (i64, i64) = (8, 0x1F);
const TYPER_DEVICES: (i64, i64) = (13, 0x1F);
const TYPER_ADDRESSES: i64 = 1 << 19;

/// The device id bits the runtime's device table covers at most.
const DEVICE_BITS: i64 = 16;

/// The fields of a table base register: its type and entry size, which it keeps, the device and
/// collection types, and the size of an entry less one at its position under its mask.
const BASER_KEPT: i64 = (0x7 << 56) | (0x1F << 48);
const BASER_TYPE: (i64, i64) = (56, 0x7);
const BASER_DEVICES: i64 = 1;
const BASER_COLLECTIONS: i64 = 4;
const BASER_ENTRY: (i64, i64) = (48, 0x1F);

/// The attributes the runtime gives the ITS's tables and queue: valid, inner write-back
/// cacheable and inner shareable, a table's pages of 64 KiB and the queue's 16 pages of 4 KiB
/// less one.
const VALID: i64 = 1 << 63;
const ITS_CACHED: i64 = 0x7 << 59;
const SHAREABLE: i64 = 1 << 10;
const PAGES_64K: i64 = 0x2 << 8;
const QUEUE_PAGES: i64 = 15;

/// The bytes of the command queue, of a command, and of the chunks the tables draw.
const QUEUE_BYTES: u64 = 65_536;
const COMMAND_BYTES: u64 = 32;
const CHUNK_SHIFT: i64 = 16;

/// A redistributor's control register with its bit enabling LPIs, its type register with the
/// position and mask of the processor number, and its LPI property and pending table base
/// registers, whose attributes are inner write-back cacheable and inner shareable, the pending
/// table zeroed.
const GICR_CTLR: u64 = 0x0;
const GICR_LPIS: i64 = 1;
const GICR_TYPER: u64 = 0x8;
const GICR_PROCESSOR: (i64, i64) = (8, 0xFFFF);
const GICR_PROPBASER: u64 = 0x70;
const GICR_PENDBASER: u64 = 0x78;
const RD_CACHED: i64 = 0x7 << 7;
const PENDING_ZEROED: i64 = 1 << 62;

/// The ITS commands the runtime issues, and the property byte of an enabled LPI at the priority
/// the runtime gives it.
const MAPD: i64 = 0x08;
const MAPC: i64 = 0x09;
const MAPTI: i64 = 0x0A;
const INV: i64 = 0x0C;
const DISCARD: i64 = 0x0F;
const SYNC: i64 = 0x05;
const LPI_ENABLED: i64 = 0xA1;

/// The device ids the ITS keeps interrupt tables for, each entry its id plus one, zero while
/// free, then its interrupt table.
const DEVICES: u64 = 256;
const DEVICE_ENTRY: u64 = 16;

impl Lowering<'_, '_> {
    /// The binding of an LPI by its identifier, in the image's table of them.
    pub(super) fn lpi_binding(&mut self, fx: &mut Fx, intid: Value) -> Value {
        let table = self.reserved(fx, "_NSirq_lpis", LPIS * binding::SIZE, 8);
        let index = fx.b.ins().iadd_imm_s(intid, -LPI_BASE);
        let at = fx.b.ins().imul_imm_s(index, binding::SIZE as i64);
        fx.b.ins().iadd(table, at)
    }

    /// On the boot CPU, when the distributor reports LPIs and the ITS takes the event ids of
    /// every LPI: gives the ITS its device and collection tables, the device table covering at
    /// most `DEVICE_BITS` id bits, and its command queue, enables it, draws the LPI property
    /// table and maps a collection to each CPU's redistributor, numbered as the CPU.
    pub(super) fn its_setup(&mut self, fx: &mut Fx) {
        let at = fx.ptr(GICD + GICD_TYPER);
        let gicd = self.mmio_read(fx, 4, at);
        let lpis = fx.b.ins().band_imm_s(gicd, GICD_LPIS);
        let lpis = fx.b.ins().icmp_imm_s(IntCC::NotEqual, lpis, 0);
        fx.when(lpis, |fx| {
            let typer = self.its_typer(fx);
            let events = field(fx, typer, TYPER_EVENTS);
            let events = fx.b.ins().iadd_imm_s(events, 1);
            let enough =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, events, EVENT_BITS as i64);
            fx.when(enough, |fx| {
                let bits = field(fx, typer, TYPER_DEVICES);
                let bits = fx.b.ins().iadd_imm_s(bits, 1);
                let wide =
                    fx.b.ins()
                        .icmp_imm_s(IntCC::UnsignedGreaterThan, bits, DEVICE_BITS);
                let most = fx.ptr(DEVICE_BITS as u64);
                let bits = fx.b.ins().select(wide, most, bits);
                let one = fx.ptr(1);
                let devices = fx.b.ins().ishl(one, bits);
                let state_address = self.state(fx);
                fx.store(devices, state_address, state::ITS_DEVICES);
                let zero = fx.ptr(0);
                let tables = fx.ptr(GITS_TABLES);
                fx.each(zero, tables, |fx, index| {
                    let at = fx.b.ins().imul_imm_s(index, 8);
                    let at = fx.b.ins().iadd_imm_s(at, (GITS + GITS_BASER) as i64);
                    let baser = self.mmio_read(fx, 8, at);
                    let kind = field(fx, baser, BASER_TYPE);
                    let device_table = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, BASER_DEVICES);
                    let collections = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, BASER_COLLECTIONS);
                    let used = fx.b.ins().bor(device_table, collections);
                    fx.when(used, |fx| {
                        let size = field(fx, baser, BASER_ENTRY);
                        let size = fx.b.ins().iadd_imm_s(size, 1);
                        let cpus = fx.ptr(MAX_CPUS);
                        let entries = fx.b.ins().select(device_table, devices, cpus);
                        let bytes = fx.b.ins().imul(entries, size);
                        let table = self.its_chunks(fx, bytes);
                        let last = fx.b.ins().iadd_imm_s(bytes, -1);
                        let pages = fx.b.ins().ushr_imm_s(last, CHUNK_SHIFT);
                        let kept = fx.b.ins().band_imm_s(baser, BASER_KEPT);
                        let value = fx.b.ins().bor(kept, table);
                        let value = fx.b.ins().bor(value, pages);
                        let value =
                            fx.b.ins()
                                .bor_imm_s(value, VALID | ITS_CACHED | SHAREABLE | PAGES_64K);
                        self.mmio_write(fx, 8, at, value);
                    });
                });
                let bytes = fx.ptr(QUEUE_BYTES);
                let queue = self.its_chunks(fx, bytes);
                let base =
                    fx.b.ins()
                        .bor_imm_s(queue, VALID | ITS_CACHED | SHAREABLE | QUEUE_PAGES);
                let at = fx.ptr(GITS + GITS_CBASER);
                self.mmio_write(fx, 8, at, base);
                let at = fx.ptr(GITS + GITS_CWRITER);
                self.mmio_write(fx, 8, at, zero);
                let at = fx.ptr(GITS + GITS_CTLR);
                let enabled = fx.ptr(GITS_ENABLED);
                self.mmio_write(fx, 4, at, enabled);
                fx.store(queue, state_address, state::ITS_QUEUE);
                let bytes = fx.ptr(LPIS);
                let properties = self.its_chunks(fx, bytes);
                fx.store(properties, state_address, state::ITS_PROPERTIES);
                let count = fx.load(PTR, state_address, state::THREAD_COUNT);
                fx.each(zero, count, |fx, cpu| {
                    let target = self.its_target(fx, typer, cpu);
                    let mapped = fx.b.ins().bor(target, cpu);
                    let mapped = fx.b.ins().bor_imm_s(mapped, VALID);
                    let none = fx.ptr(0);
                    let command = fx.ptr(MAPC as u64);
                    self.its_command(fx, [command, none, mapped, none]);
                });
                self.its_flush(fx);
            });
        });
    }

    /// The ITS's type register.
    fn its_typer(&mut self, fx: &mut Fx) -> Value {
        let at = fx.ptr(GITS + GITS_TYPER);
        self.mmio_read(fx, 8, at)
    }

    /// Zeroed chunks holding at least `bytes`, trapping on exhaustion.
    fn its_chunks(&mut self, fx: &mut Fx, bytes: Value) -> Value {
        let rounded = fx.b.ins().iadd_imm_s(bytes, (1 << CHUNK_SHIFT) - 1);
        let count = fx.b.ins().ushr_imm_s(rounded, CHUNK_SHIFT);
        let chunks = map_chunks(self, fx, count);
        let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, chunks, 0);
        fx.when(missing, |fx| report::raise_kind(self, fx, "trap.exhausted"));
        chunks
    }

    /// The redistributor of CPU `cpu` as ITS commands name it, at the place of their target
    /// field: by its address, or by its processor number.
    fn its_target(&mut self, fx: &mut Fx, typer: Value, cpu: Value) -> Value {
        let base = fx.b.ins().imul_imm_s(cpu, GICR_STRIDE as i64);
        let base = fx.b.ins().iadd_imm_s(base, GICR as i64);
        let at = fx.b.ins().iadd_imm_s(base, GICR_TYPER as i64);
        let rd_typer = self.mmio_read(fx, 8, at);
        let processor = field(fx, rd_typer, GICR_PROCESSOR);
        let processor = fx.b.ins().ishl_imm_s(processor, 16);
        let by_address = fx.b.ins().band_imm_s(typer, TYPER_ADDRESSES);
        let by_address = fx.b.ins().icmp_imm_s(IntCC::NotEqual, by_address, 0);
        fx.b.ins().select(by_address, base, processor)
    }

    /// Writes a command of four doublewords at the end of the command queue.
    fn its_command(&mut self, fx: &mut Fx, words: [Value; 4]) {
        let state_address = self.state(fx);
        let queue = fx.load(PTR, state_address, state::ITS_QUEUE);
        let written = fx.load(PTR, state_address, state::ITS_WRITE);
        let at = fx.b.ins().iadd(queue, written);
        for (index, word) in words.iter().enumerate() {
            fx.store(*word, at, 8 * index as u64);
        }
        let next = fx.b.ins().iadd_imm_s(written, COMMAND_BYTES as i64);
        let next = fx.b.ins().band_imm_s(next, QUEUE_BYTES as i64 - 1);
        fx.store(next, state_address, state::ITS_WRITE);
    }

    /// Hands the ITS the commands written and waits until it has processed them.
    fn its_flush(&mut self, fx: &mut Fx) {
        let state_address = self.state(fx);
        let written = fx.load(PTR, state_address, state::ITS_WRITE);
        let at = fx.ptr(GITS + GITS_CWRITER);
        self.mmio_write(fx, 8, at, written);
        let at = fx.ptr(GITS + GITS_CREADR);
        fx.repeat(&[], |fx, _| {
            let read = self.mmio_read(fx, 8, at);
            let read =
                fx.b.ins()
                    .band_imm_s(read, QUEUE_BYTES as i64 - COMMAND_BYTES as i64);
            let waiting = fx.b.ins().icmp(IntCC::NotEqual, read, written);
            fx.when(waiting, |fx| {
                self.call_stub(fx, Stub::Pause, &[]);
            });
            (waiting, Vec::new())
        });
    }

    /// Enables LPIs on the running CPU's redistributor at `base` when the ITS is set up, with the
    /// shared property table and a pending table of its own.
    pub(super) fn its_cpu(&mut self, fx: &mut Fx, base: Value) {
        let state_address = self.state(fx);
        let properties = fx.load(PTR, state_address, state::ITS_PROPERTIES);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, properties, 0);
        fx.when(present, |fx| {
            let value =
                fx.b.ins()
                    .bor_imm_s(properties, SHAREABLE | RD_CACHED | (LPI_BITS as i64 - 1));
            let at = fx.b.ins().iadd_imm_s(base, GICR_PROPBASER as i64);
            self.mmio_write(fx, 8, at, value);
            let bytes = fx.ptr(1);
            let pending = self.its_chunks(fx, bytes);
            let value =
                fx.b.ins()
                    .bor_imm_s(pending, PENDING_ZEROED | SHAREABLE | RD_CACHED);
            let at = fx.b.ins().iadd_imm_s(base, GICR_PENDBASER as i64);
            self.mmio_write(fx, 8, at, value);
            let at = fx.b.ins().iadd_imm_s(base, GICR_CTLR as i64);
            let control = self.mmio_read(fx, 4, at);
            let control = fx.b.ins().bor_imm_s(control, GICR_LPIS);
            self.mmio_write(fx, 4, at, control);
        });
    }

    /// The AArch64 part of `install_msi`, under the bindings lock: the device id's interrupt
    /// table, mapped on its first install, and the lowest LPI no device is installed on, whose
    /// event of that device the ITS then translates to it on CPU `cpu`'s redistributor. Gives
    /// one of the `msi` outcomes.
    pub(super) fn its_install(
        &mut self,
        fx: &mut Fx,
        record: Value,
        cpu: Value,
        source: Value,
    ) -> Value {
        let state_address = self.state(fx);
        let devices = fx.load(PTR, state_address, state::ITS_DEVICES);
        let accepted = fx.b.ins().icmp(IntCC::UnsignedLessThan, source, devices);
        let unsupported = fx.ptr(msi::UNSUPPORTED as u64);
        let outcome = fx.var(PTR, unsupported);
        fx.when(accepted, |fx| {
            let table = self.its_device(fx, source);
            let lpis = self.reserved(fx, "_NSirq_lpis", LPIS * binding::SIZE, 8);
            let first = fx.ptr(0);
            let index = fx.repeat(&[first], |fx, at| {
                let slot = fx.b.ins().imul_imm_s(at[0], binding::SIZE as i64);
                let slot = fx.b.ins().iadd(lpis, slot);
                let installed = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), slot);
                let taken = fx.b.ins().icmp_imm_s(IntCC::NotEqual, installed, 0);
                let next = fx.b.ins().iadd_imm_s(at[0], 1);
                let inside =
                    fx.b.ins()
                        .icmp_imm_s(IntCC::UnsignedLessThan, next, LPIS as i64);
                let more = fx.b.ins().band(taken, inside);
                (more, vec![fx.b.ins().select(taken, next, at[0])])
            })[0];
            let free =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedLessThan, index, LPIS as i64);
            let mapped = fx.b.ins().icmp_imm_s(IntCC::NotEqual, table, 0);
            let room = fx.b.ins().band(free, mapped);
            let installed = fx.ptr(msi::INSTALLED as u64);
            let exhausted = fx.ptr(msi::EXHAUSTED as u64);
            let chosen = fx.b.ins().select(room, installed, exhausted);
            fx.b.def_var(outcome, chosen);
            fx.when(room, |fx| {
                let properties = fx.load(PTR, state_address, state::ITS_PROPERTIES);
                let property = fx.b.ins().iadd(properties, index);
                let enabled = fx.int(types::I8, LPI_ENABLED);
                fx.store(enabled, property, 0);
                let intid = fx.b.ins().iadd_imm_s(index, LPI_BASE);
                fx.store(intid, record, device::VECTOR);
                let address = fx.ptr(GITS_TRANSLATER);
                fx.store(address, record, device::MESSAGE);
                fx.store(index, record, device::DATA);
                fx.store(source, record, device::SOURCE);
                fx.store(cpu, record, device::CPU);
                let slot = self.lpi_binding(fx, intid);
                self.chain(fx, record, slot);
                let device_word = fx.b.ins().ishl_imm_s(source, 32);
                let none = fx.ptr(0);
                let command = fx.b.ins().bor_imm_s(device_word, MAPTI);
                let translated = fx.b.ins().ishl_imm_s(intid, 32);
                let translated = fx.b.ins().bor(translated, index);
                self.its_command(fx, [command, translated, cpu, none]);
                let command = fx.b.ins().bor_imm_s(device_word, INV);
                self.its_command(fx, [command, index, none, none]);
                self.its_sync(fx, cpu);
            });
        });
        fx.b.use_var(outcome)
    }

    /// The interrupt table of device id `source`, mapping it to a new one on its first install;
    /// zero when `DEVICES` ids already have one or the chunks run out.
    fn its_device(&mut self, fx: &mut Fx, source: Value) -> Value {
        let entries = self.reserved(fx, "_NSits_devices", DEVICES * DEVICE_ENTRY, 8);
        let key = fx.b.ins().iadd_imm_s(source, 1);
        let zero = fx.ptr(0);
        let found = fx.var(PTR, zero);
        let free = fx.var(PTR, zero);
        let count = fx.ptr(DEVICES);
        fx.each(zero, count, |fx, index| {
            let entry = fx.b.ins().imul_imm_s(index, DEVICE_ENTRY as i64);
            let entry = fx.b.ins().iadd(entries, entry);
            let held = fx.load(PTR, entry, 0);
            let same = fx.b.ins().icmp(IntCC::Equal, held, key);
            fx.when(same, |fx| {
                let table = fx.load(PTR, entry, 8);
                fx.b.def_var(found, table);
            });
            let empty = fx.b.ins().icmp_imm_s(IntCC::Equal, held, 0);
            let first = fx.b.use_var(free);
            let unseen = fx.b.ins().icmp_imm_s(IntCC::Equal, first, 0);
            let fresh = fx.b.ins().band(empty, unseen);
            fx.when(fresh, |fx| fx.b.def_var(free, entry));
        });
        let table = fx.b.use_var(found);
        let entry = fx.b.use_var(free);
        let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, table, 0);
        let room = fx.b.ins().icmp_imm_s(IntCC::NotEqual, entry, 0);
        let mapping = fx.b.ins().band(missing, room);
        fx.when(mapping, |fx| {
            let typer = self.its_typer(fx);
            let size = field(fx, typer, TYPER_ENTRY);
            let size = fx.b.ins().iadd_imm_s(size, 1);
            let bytes = fx.b.ins().imul_imm_s(size, LPIS as i64);
            let rounded = fx.b.ins().iadd_imm_s(bytes, (1 << CHUNK_SHIFT) - 1);
            let count = fx.b.ins().ushr_imm_s(rounded, CHUNK_SHIFT);
            let table = map_chunks(self, fx, count);
            let drawn = fx.b.ins().icmp_imm_s(IntCC::NotEqual, table, 0);
            fx.when(drawn, |fx| {
                fx.store(key, entry, 0);
                fx.store(table, entry, 8);
                let command = fx.b.ins().ishl_imm_s(source, 32);
                let command = fx.b.ins().bor_imm_s(command, MAPD);
                let size = fx.ptr(EVENT_BITS - 1);
                let valid = fx.b.ins().bor_imm_s(table, VALID);
                let none = fx.ptr(0);
                self.its_command(fx, [command, size, valid, none]);
                fx.b.def_var(found, table);
            });
        });
        fx.b.use_var(found)
    }

    /// Ends the translation of an installed device's event, under the bindings lock, and
    /// disables its LPI.
    pub(super) fn its_discard(&mut self, fx: &mut Fx, record: Value) {
        let index = fx.load(PTR, record, device::DATA);
        let source = fx.load(PTR, record, device::SOURCE);
        let cpu = fx.load(PTR, record, device::CPU);
        let command = fx.b.ins().ishl_imm_s(source, 32);
        let command = fx.b.ins().bor_imm_s(command, DISCARD);
        let none = fx.ptr(0);
        self.its_command(fx, [command, index, none, none]);
        let state_address = self.state(fx);
        let properties = fx.load(PTR, state_address, state::ITS_PROPERTIES);
        let property = fx.b.ins().iadd(properties, index);
        let disabled = fx.int(types::I8, 0);
        fx.store(disabled, property, 0);
        self.its_sync(fx, cpu);
    }

    /// Ends the commands written with a synchronisation of CPU `cpu`'s redistributor and hands
    /// them to the ITS.
    fn its_sync(&mut self, fx: &mut Fx, cpu: Value) {
        let typer = self.its_typer(fx);
        let target = self.its_target(fx, typer, cpu);
        let command = fx.ptr(SYNC as u64);
        let none = fx.ptr(0);
        self.its_command(fx, [command, none, target, none]);
        self.its_flush(fx);
    }
}

/// The field of a register at a position under a mask.
fn field(fx: &mut Fx, register: Value, (position, mask): (i64, i64)) -> Value {
    let shifted = fx.b.ins().ushr_imm_s(register, position);
    fx.b.ins().band_imm_s(shifted, mask)
}
