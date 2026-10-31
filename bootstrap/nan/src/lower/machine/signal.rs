//! The CPU signal, the interrupt one CPU sends another on the vector the runtime keeps for it:
//! `hw::Ring0.signal_cpu`, the shootdowns of `hw::Ring0.invalidate_everywhere`, which x86_64
//! CPUs serve on the signal and AArch64 CPUs leave to the broadcast invalidation of their
//! interconnect, and `hw::Ring0.wait_for_interrupt`, whose wait no preemption interrupts.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, condcodes::IntCC};

use super::super::fx::{Fx, PTR};
use super::super::runtime::{fetch_add, state};
use super::Lowering;
use super::cpus::cpu;
use super::stubs::Stub;
use crate::workspace::Arch;

/// The vector of the CPU signal, and the interrupt command sending it to one CPU: a fixed
/// interrupt on that vector, asserted.
pub const SIGNAL_VECTOR: i64 = 0xFE;
const IPI_SIGNAL: u64 = 0x4000 | SIGNAL_VECTOR as u64;

impl Lowering<'_, '_> {
    /// Sends the CPU signal to CPU `index` when it is a started CPU.
    pub fn signal_cpu(&mut self, fx: &mut Fx, index: Value) {
        let state_address = self.state(fx);
        let count = fx.load(PTR, state_address, state::THREAD_COUNT);
        let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, index, count);
        fx.when(inside, |fx| {
            let record = self.cpu_record(fx, index);
            let started = fx.offset(record, cpu::STARTED);
            let started =
                fx.b.ins()
                    .atomic_load(PTR, MemFlagsData::trusted(), started);
            let started = fx.b.ins().icmp_imm_s(IntCC::NotEqual, started, 0);
            fx.when(started, |fx| self.send_signal(fx, record));
        });
    }

    /// Sends the CPU signal to the CPU of a record: through its local APIC on x86_64, with
    /// maskable interrupts disabled so that no interrupt fn's own signal splits the command, and
    /// as software-generated interrupt 1 on AArch64.
    fn send_signal(&mut self, fx: &mut Fx, record: Value) {
        let id = fx.load(PTR, record, cpu::HARDWARE_ID);
        match self.machine_arch() {
            Arch::Amd64 => {
                let flags = self.call_stub(fx, Stub::IrqSave, &[])[0];
                self.send_ipi(fx, id, IPI_SIGNAL);
                self.call_stub(fx, Stub::IrqRestore, &[flags]);
            }
            Arch::Aarch64 => {
                self.call_stub(fx, Stub::SendSignal, &[id]);
            }
        }
    }

    /// Invalidates a scope's translations on every started CPU, the scope being a page's address
    /// or 1 for every translation. On AArch64 one broadcast invalidation reaches them all. On
    /// x86_64 the caller takes the shootdown lock, spinning with maskable interrupts enabled so
    /// that it serves other shootdowns meanwhile; then, with them disabled so that it stays on
    /// its CPU, it posts the scope to every other started CPU, counting each, sends it the
    /// signal and invalidates its own translations; it then waits, with them enabled, until
    /// each has served the shootdown, and releases the lock.
    pub fn invalidate_everywhere(&mut self, fx: &mut Fx, scope: Value) {
        if self.machine_arch() == Arch::Aarch64 {
            self.call_stub(fx, Stub::TlbiBroadcast, &[scope]);
            return;
        }
        let state_address = self.state(fx);
        let lock = fx.offset(state_address, state::SHOOTDOWN_LOCK);
        let left = fx.offset(state_address, state::SHOOTDOWN_LEFT);
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
        let flags = self.call_stub(fx, Stub::IrqSave, &[])[0];
        let posted = fx.offset(state_address, state::SHOOTDOWN_SCOPE);
        fx.b.ins()
            .atomic_store(MemFlagsData::trusted(), scope, posted);
        let own = self.running_index(fx);
        let count = fx.load(PTR, state_address, state::THREAD_COUNT);
        let zero = fx.ptr(0);
        fx.each(zero, count, |fx, index| {
            let other = fx.b.ins().icmp(IntCC::NotEqual, index, own);
            let record = self.cpu_record(fx, index);
            let started = fx.offset(record, cpu::STARTED);
            let started =
                fx.b.ins()
                    .atomic_load(PTR, MemFlagsData::trusted(), started);
            let started = fx.b.ins().icmp_imm_s(IntCC::NotEqual, started, 0);
            let target = fx.b.ins().band(other, started);
            fx.when(target, |fx| {
                let one = fx.ptr(1);
                fetch_add(fx, left, one);
                let pending = fx.offset(record, cpu::SHOOTDOWN);
                fx.b.ins()
                    .atomic_store(MemFlagsData::trusted(), one, pending);
                let id = fx.load(PTR, record, cpu::HARDWARE_ID);
                self.send_ipi(fx, id, IPI_SIGNAL);
            });
        });
        self.call_stub(fx, Stub::Shootdown, &[scope]);
        self.call_stub(fx, Stub::IrqRestore, &[flags]);
        fx.repeat(&[], |fx, _| {
            let count = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), left);
            let waiting = fx.b.ins().icmp_imm_s(IntCC::NotEqual, count, 0);
            fx.when(waiting, |fx| {
                self.call_stub(fx, Stub::Pause, &[]);
            });
            (waiting, Vec::new())
        });
        let free = fx.ptr(0);
        fx.b.ins().atomic_store(MemFlagsData::trusted(), free, lock);
    }

    /// Serves the shootdown posted to the running CPU, if any, when the CPU signal arrives on
    /// x86_64: invalidates the posted scope's translations and counts the CPU as done.
    pub(super) fn serve_shootdown(&mut self, fx: &mut Fx) {
        let record = self.running_cpu(fx);
        let pending = fx.offset(record, cpu::SHOOTDOWN);
        let posted = fx.ptr(1);
        let served = fx.ptr(0);
        let seen =
            fx.b.ins()
                .atomic_cas(MemFlagsData::trusted(), pending, posted, served);
        let due = fx.b.ins().icmp_imm_s(IntCC::NotEqual, seen, 0);
        fx.when(due, |fx| {
            let state_address = self.state(fx);
            let scope = fx.offset(state_address, state::SHOOTDOWN_SCOPE);
            let scope = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), scope);
            self.call_stub(fx, Stub::Shootdown, &[scope]);
            let left = fx.offset(state_address, state::SHOOTDOWN_LEFT);
            let done = fx.int(PTR, -1);
            fetch_add(fx, left, done);
        });
    }

    /// Halts the running CPU until an interrupt arrives, with maskable interrupts enabled while it
    /// waits and its record saying so, which keeps the seam from preempting the caller. Returns at
    /// once inside an interrupt fn and before the anchor is installed.
    pub fn wait_for_interrupt(&mut self, fx: &mut Fx) {
        let state_address = self.state(fx);
        let anchor = fx.load(PTR, state_address, state::ANCHOR);
        let installed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, anchor, 0);
        let record = self.running_cpu(fx);
        let serving = fx.load(PTR, record, cpu::FRAME);
        let outside = fx.b.ins().icmp_imm_s(IntCC::Equal, serving, 0);
        let waits = fx.b.ins().band(installed, outside);
        fx.when(waits, |fx| {
            let one = fx.ptr(1);
            fx.store(one, record, cpu::WAITING);
            self.call_stub(fx, Stub::IdleWait, &[]);
            let zero = fx.ptr(0);
            fx.store(zero, record, cpu::WAITING);
        });
    }
}
