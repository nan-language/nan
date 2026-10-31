//! The devices `hw::IrqShared` installs: the binding table, whose first row of vectors serves
//! every CPU and whose other rows serve one CPU each; the chains of devices sharing a binding,
//! which the dispatcher runs and installs and uninstalls change under the bindings lock; and the
//! vectors of x86_64 message-signalled interrupts. On AArch64 a vector from 32 up is a shared
//! peripheral interrupt, routed to the CPU of its devices.

use cranelift_codegen::ir::{
    AbiParam, InstBuilder, MemFlagsData, Signature, Value, condcodes::IntCC, types,
};

use super::super::fx::{Fx, PTR};
use super::super::runtime::{fetch_add, report, state};
use super::Lowering;
use super::cpus::{MAX_CPUS, cpu};
use super::stubs::Stub;
use crate::workspace::Arch;

/// A binding: the record of its first device, zero while none is installed, then the number of
/// CPUs running its devices.
pub mod binding {
    pub const RUNNING: u64 = 8;
    pub const SIZE: u64 = 16;
}

/// The vectors of a row of the binding table, and its rows: the row of every CPU, then one per
/// CPU.
const VECTORS: u64 = 256;
const ROWS: u64 = MAX_CPUS + 1;

/// The offsets of a device record's fields: the lock word, the vector or, for an AArch64
/// message-signalled interrupt, the LPI's identifier, the glue the dispatcher calls with the
/// record, the interrupt fn as a function value, the next device of its binding, the binding,
/// the address and data of its message, zero without one, the ITS device id and CPU of an
/// AArch64 message-signalled interrupt, the holding through which its owner's settlement
/// uninstalls it, and the device from `VALUE` on, at its alignment.
pub mod device {
    pub const LOCK: u64 = 0;
    pub const VECTOR: u64 = 8;
    pub const GLUE: u64 = 16;
    pub const HANDLER: u64 = 24;
    pub const NEXT: u64 = 40;
    pub const BINDING: u64 = 48;
    pub const MESSAGE: u64 = 56;
    pub const DATA: u64 = 64;
    pub const SOURCE: u64 = 72;
    pub const CPU: u64 = 80;
    pub const HOLDING: u64 = 88;
    pub const VALUE: u64 = 96;
}

/// What `install_msi` gives: the interrupt installed, or the `hw::MsiError` variant refusing it.
pub mod msi {
    pub const INSTALLED: i64 = 0;
    pub const EXHAUSTED: i64 = 1;
    pub const UNREACHABLE: i64 = 2;
    pub const UNSUPPORTED: i64 = 3;
}

/// The vectors an x86_64 message-signalled interrupt may take, past the timer's and short of
/// the CPU signal's, the largest local APIC id a message names, and the base of its address.
const MSI_VECTORS: (i64, i64) = (0x21, 0xFE);
const MSI_ID_MAX: i64 = 255;
const MSI_ADDRESS: i64 = 0xFEE0_0000;

impl Lowering<'_, '_> {
    /// The binding of a vector in a row of the binding table.
    pub fn irq_binding(&mut self, fx: &mut Fx, row: Value, vector: Value) -> Value {
        let table = self.reserved(fx, "_NSirq_bindings", ROWS * VECTORS * binding::SIZE, 8);
        let at = fx.b.ins().imul_imm_s(row, VECTORS as i64);
        let at = fx.b.ins().iadd(at, vector);
        let at = fx.b.ins().imul_imm_s(at, binding::SIZE as i64);
        fx.b.ins().iadd(table, at)
    }

    /// Runs the devices installed on a vector of 32 or more for the running CPU, then those
    /// installed for every CPU. Gives whether any was installed and, when one was, whether any
    /// interrupt fn gave `Reschedule`.
    pub(super) fn installed_reschedule(&mut self, fx: &mut Fx, vector: Value) -> (Value, Value) {
        let index = self.running_index(fx);
        let row = fx.b.ins().iadd_imm_s(index, 1);
        let own = self.irq_binding(fx, row, vector);
        let every = fx.ptr(0);
        let every = self.irq_binding(fx, every, vector);
        self.run_installed(fx, &[own, every])
    }

    /// Runs the devices chained in `bindings`, in order, with the interrupt deferred in the
    /// running CPU's record so that their `complete` leaves it to this, then ends it when any
    /// device ran. Gives whether any ran and whether any interrupt fn gave `Reschedule`.
    pub(super) fn run_installed(&mut self, fx: &mut Fx, bindings: &[Value]) -> (Value, Value) {
        let record = self.running_cpu(fx);
        let deferred = fx.ptr(1);
        fx.store(deferred, record, cpu::DEFERRED);
        let mut present = fx.int(types::I8, 0);
        let mut reschedule = fx.int(types::I8, 0);
        for slot in bindings {
            let (ran, rescheduled) = self.run_chain(fx, *slot);
            present = fx.b.ins().bor(present, ran);
            reschedule = fx.b.ins().bor(reschedule, rescheduled);
        }
        let ended = fx.ptr(0);
        fx.store(ended, record, cpu::DEFERRED);
        fx.when(present, |fx| self.end_of_interrupt(fx));
        (present, reschedule)
    }

    /// Calls the glue of each device chained in a binding, counting the CPU among the binding's
    /// runners meanwhile so an uninstall waits for it. Gives whether one was chained and whether
    /// any gave `Reschedule`.
    fn run_chain(&mut self, fx: &mut Fx, slot: Value) -> (Value, Value) {
        let running = fx.offset(slot, binding::RUNNING);
        let one = fx.ptr(1);
        fetch_add(fx, running, one);
        let first = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), slot);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, first, 0);
        let reschedule = fx.choose(
            present,
            &[types::I8],
            |fx| {
                let none = fx.int(types::I8, 0);
                let done = fx.repeat(&[first, none], |fx, state| {
                    let (device, reschedule) = (state[0], state[1]);
                    let at = fx.offset(device, device::NEXT);
                    let next = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), at);
                    let glue = fx.load(PTR, device, device::GLUE);
                    let mut call = Signature::new(super::super::CALL_CONV);
                    call.params.push(AbiParam::new(PTR));
                    call.returns.push(AbiParam::new(types::I8));
                    let call = fx.b.import_signature(call);
                    let inst = fx.b.ins().call_indirect(call, glue, &[device]);
                    let rescheduled = fx.b.inst_results(inst)[0];
                    let reschedule = fx.b.ins().bor(reschedule, rescheduled);
                    let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next, 0);
                    (more, vec![next, reschedule])
                });
                vec![done[1]]
            },
            |fx| vec![fx.int(types::I8, 0)],
        )[0];
        let minus = fx.int(PTR, -1);
        fetch_add(fx, running, minus);
        (present, reschedule)
    }

    /// Takes the bindings lock, with maskable interrupts disabled so that its holder stays on
    /// its CPU, giving the flags to restore.
    fn lock_bindings(&mut self, fx: &mut Fx) -> Value {
        let flags = self.call_stub(fx, Stub::IrqSave, &[])[0];
        let state_address = self.state(fx);
        let lock = fx.offset(state_address, state::IRQ_LOCK);
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

    fn unlock_bindings(&mut self, fx: &mut Fx, flags: Value) {
        let state_address = self.state(fx);
        let lock = fx.offset(state_address, state::IRQ_LOCK);
        let free = fx.ptr(0);
        fx.b.ins().atomic_store(MemFlagsData::trusted(), free, lock);
        self.call_stub(fx, Stub::IrqRestore, &[flags]);
    }

    /// Installs a device record on a vector of CPU `cpu`, or of every CPU for `None`, at the end
    /// of its binding's chain, trapping on a vector the final build binds, the CPU signal's,
    /// or, on AArch64, one whose devices are installed for another CPU, to which the interrupt
    /// controller routes it. The routing CPU of devices installed for every CPU is the boot
    /// CPU.
    pub fn install_device(
        &mut self,
        fx: &mut Fx,
        record: Value,
        cpu: Option<Value>,
        vector: Value,
    ) {
        for bound in self.statically_bound() {
            let same = fx.b.ins().icmp_imm_s(IntCC::Equal, vector, bound);
            fx.when(same, |fx| {
                report::raise_kind(self, fx, "trap.duplicate-access")
            });
        }
        let flags = self.lock_bindings(fx);
        let (row, target) = match cpu {
            Some(cpu) => (fx.b.ins().iadd_imm_s(cpu, 1), cpu),
            None => (fx.ptr(0), fx.ptr(0)),
        };
        let aarch64 = self.machine_arch() == Arch::Aarch64;
        if aarch64 {
            let taken = self.installed_for(fx, vector, Some(target));
            fx.when(taken, |fx| {
                report::raise_kind(self, fx, "trap.duplicate-access")
            });
        }
        fx.store(vector, record, device::VECTOR);
        let slot = self.irq_binding(fx, row, vector);
        self.chain(fx, record, slot);
        if aarch64 {
            self.aarch64_route(fx, vector, target);
        }
        self.unlock_bindings(fx, flags);
    }

    /// Whether devices are installed on a vector for a CPU other than `cpu`, or for any CPU for
    /// `None`, the devices installed for every CPU counting as the boot CPU's.
    fn installed_for(&mut self, fx: &mut Fx, vector: Value, cpu: Option<Value>) -> Value {
        let state_address = self.state(fx);
        let count = fx.load(PTR, state_address, state::THREAD_COUNT);
        let rows = fx.b.ins().iadd_imm_s(count, 1);
        let none = fx.int(types::I8, 0);
        let found = fx.var(types::I8, none);
        let zero = fx.ptr(0);
        fx.each(zero, rows, |fx, row| {
            let slot = self.irq_binding(fx, row, vector);
            let first = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), slot);
            let mut hit = fx.b.ins().icmp_imm_s(IntCC::NotEqual, first, 0);
            if let Some(cpu) = cpu {
                let every = fx.b.ins().icmp_imm_s(IntCC::Equal, row, 0);
                let own = fx.b.ins().iadd_imm_s(row, -1);
                let boot = fx.ptr(0);
                let owner = fx.b.ins().select(every, boot, own);
                let other = fx.b.ins().icmp(IntCC::NotEqual, owner, cpu);
                hit = fx.b.ins().band(hit, other);
            }
            let seen = fx.b.use_var(found);
            let seen = fx.b.ins().bor(seen, hit);
            fx.b.def_var(found, seen);
        });
        fx.b.use_var(found)
    }

    /// Chains a device record at the end of a binding's devices.
    pub(super) fn chain(&mut self, fx: &mut Fx, record: Value, slot: Value) {
        fx.store(slot, record, device::BINDING);
        let link = fx.repeat(&[slot], |fx, at| {
            let next = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), at[0]);
            let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next, 0);
            let following = fx.b.ins().iadd_imm_s(next, device::NEXT as i64);
            (more, vec![fx.b.ins().select(more, following, at[0])])
        })[0];
        fx.b.ins()
            .atomic_store(MemFlagsData::trusted(), record, link);
    }

    /// Installs a device record on a new message-signalled interrupt of CPU `cpu`, from the
    /// device `source` on AArch64, giving one of the `msi` outcomes.
    pub fn install_msi(&mut self, fx: &mut Fx, record: Value, cpu: Value, source: Value) -> Value {
        let flags = self.lock_bindings(fx);
        let outcome = match self.machine_arch() {
            Arch::Amd64 => self.x64_install_msi(fx, record, cpu),
            Arch::Aarch64 => self.its_install(fx, record, cpu, source),
        };
        self.unlock_bindings(fx, flags);
        outcome
    }

    /// The x86_64 part of `install_msi`: the lowest vector of `MSI_VECTORS` the final build does
    /// not bind and no device is installed on for the CPU, with a message naming the CPU's local
    /// APIC id, which no message reaches past `MSI_ID_MAX`.
    fn x64_install_msi(&mut self, fx: &mut Fx, record: Value, cpu: Value) -> Value {
        let row = fx.b.ins().iadd_imm_s(cpu, 1);
        let bound = self.statically_bound();
        let first = fx.ptr(MSI_VECTORS.0 as u64);
        let vector = fx.repeat(&[first], |fx, at| {
            let vector = at[0];
            let own = self.irq_binding(fx, row, vector);
            let own = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), own);
            let every = fx.ptr(0);
            let every = self.irq_binding(fx, every, vector);
            let every = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), every);
            let installed = fx.b.ins().bor(own, every);
            let mut taken = fx.b.ins().icmp_imm_s(IntCC::NotEqual, installed, 0);
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
        let found =
            fx.b.ins()
                .icmp_imm_s(IntCC::UnsignedLessThan, vector, MSI_VECTORS.1);
        let id = self.hardware_id(fx, cpu);
        let far =
            fx.b.ins()
                .icmp_imm_s(IntCC::UnsignedGreaterThan, id, MSI_ID_MAX);
        let installed = fx.ptr(msi::INSTALLED as u64);
        let exhausted = fx.ptr(msi::EXHAUSTED as u64);
        let unreachable = fx.ptr(msi::UNREACHABLE as u64);
        let outcome = fx.b.ins().select(found, installed, exhausted);
        let outcome = fx.b.ins().select(far, unreachable, outcome);
        let done = fx.b.ins().icmp_imm_s(IntCC::Equal, outcome, msi::INSTALLED);
        fx.when(done, |fx| {
            fx.store(vector, record, device::VECTOR);
            let address = fx.b.ins().ishl_imm_s(id, 12);
            let address = fx.b.ins().bor_imm_s(address, MSI_ADDRESS);
            fx.store(address, record, device::MESSAGE);
            fx.store(vector, record, device::DATA);
            let slot = self.irq_binding(fx, row, vector);
            self.chain(fx, record, slot);
        });
        outcome
    }

    /// Removes a device record from its binding's chain, and on AArch64 disables a shared
    /// peripheral interrupt no device is left on or discards an LPI's translation, then spins
    /// until no CPU runs the binding's devices.
    pub fn uninstall_device(&mut self, fx: &mut Fx, record: Value) {
        let flags = self.lock_bindings(fx);
        let slot = fx.load(PTR, record, device::BINDING);
        let link = fx.repeat(&[slot], |fx, at| {
            let current = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), at[0]);
            let other = fx.b.ins().icmp(IntCC::NotEqual, current, record);
            let following = fx.b.ins().iadd_imm_s(current, device::NEXT as i64);
            (other, vec![fx.b.ins().select(other, following, at[0])])
        })[0];
        let next = fx.load(PTR, record, device::NEXT);
        fx.b.ins().atomic_store(MemFlagsData::trusted(), next, link);
        if self.machine_arch() == Arch::Aarch64 {
            let vector = fx.load(PTR, record, device::VECTOR);
            let lpi = fx.b.ins().icmp_imm_s(
                IntCC::UnsignedGreaterThanOrEqual,
                vector,
                super::its::LPI_BASE,
            );
            fx.choose_in(
                self,
                lpi,
                &[],
                |lo, fx| {
                    lo.its_discard(fx, record);
                    Vec::new()
                },
                |lo, fx| {
                    let installed = lo.installed_for(fx, vector, None);
                    let unused = fx.b.ins().bxor_imm_s(installed, 1);
                    fx.when(unused, |fx| lo.aarch64_disable(fx, vector));
                    Vec::new()
                },
            );
        }
        self.unlock_bindings(fx, flags);
        let running = fx.offset(slot, binding::RUNNING);
        fx.repeat(&[], |fx, _| {
            let count =
                fx.b.ins()
                    .atomic_load(PTR, MemFlagsData::trusted(), running);
            let busy = fx.b.ins().icmp_imm_s(IntCC::NotEqual, count, 0);
            fx.when(busy, |fx| {
                self.call_stub(fx, Stub::Pause, &[]);
            });
            (busy, Vec::new())
        });
    }
}
