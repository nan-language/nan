//! The runtime's locks: words its routines spin on until they take them, `Lock` and `Unlock`.
//! The running executor thread records the locks it holds, which a trap of its task releases. On
//! a kernel a lock is IRQ-saving, its outermost holder disabling maskable interrupts so that no
//! interrupt fn on the same CPU spins on a lock the code it interrupted holds, and a lock word
//! holds its CPU's number plus one, so a CPU reaching a lock it holds halts the kernel.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, condcodes::IntCC, types};

use super::super::Lowering;
use super::super::fx::{Fx, PTR};
use super::super::machine::Stub;
use super::Routine;
use super::tasks::thread;

/// The most locks a thread holds at once that a trap of its task releases.
pub const HELD_LOCKS: u64 = 8;

pub fn define(lo: &mut Lowering, fx: &mut Fx, routine: Routine) {
    let lock = fx.entry()[0];
    match routine {
        Routine::Lock => acquire(lo, fx, lock),
        _ => release(lo, fx, lock),
    }
    fx.b.ins().return_(&[]);
}

/// `Lock(lock)`: spins until the running thread takes the lock, then records it.
fn acquire(lo: &mut Lowering, fx: &mut Fx, lock: Value) {
    let flags = lo.kernel().then(|| lo.call_stub(fx, Stub::IrqSave, &[])[0]);
    let thread = lo.thread(fx);
    let inside = fx.b.ins().icmp_imm_s(IntCC::NotEqual, thread, 0);
    let holder = match lo.kernel() {
        true => fx.choose(
            inside,
            &[types::I32],
            |fx| {
                let index = fx.load(types::I32, thread, thread::INDEX);
                vec![fx.b.ins().iadd_imm_s(index, 1)]
            },
            |fx| vec![fx.int(types::I32, 1)],
        )[0],
        false => fx.int(types::I32, 1),
    };
    fx.repeat(&[], |fx, _| {
        let free = fx.int(types::I32, 0);
        let seen =
            fx.b.ins()
                .atomic_cas(MemFlagsData::trusted(), lock, free, holder);
        let busy = fx.b.ins().icmp_imm_s(IntCC::NotEqual, seen, 0);
        fx.when(busy, |fx| {
            if lo.kernel() {
                let again = fx.b.ins().icmp(IntCC::Equal, seen, holder);
                fx.when(again, |fx| {
                    let location = lo.interrupted_location(fx);
                    lo.call_routine(fx, Routine::Reacquired, &[location]);
                });
            }
            lo.relax(fx);
        });
        (busy, Vec::new())
    });
    fx.when(inside, |fx| {
        let held = fx.load(PTR, thread, thread::LOCKS);
        if let Some(flags) = flags {
            let outermost = fx.b.ins().icmp_imm_s(IntCC::Equal, held, 0);
            fx.when(outermost, |fx| fx.store(flags, thread, thread::SAVED_IF));
        }
        let recorded =
            fx.b.ins()
                .icmp_imm_s(IntCC::UnsignedLessThan, held, HELD_LOCKS as i64);
        fx.when(recorded, |fx| {
            let slot = fx.b.ins().imul_imm_s(held, 8);
            let slot = fx.b.ins().iadd(thread, slot);
            fx.store(lock, slot, thread::HELD);
        });
        let held = fx.b.ins().iadd_imm_s(held, 1);
        fx.store(held, thread, thread::LOCKS);
    });
}

/// `Unlock(lock)`: forgets the lock the running thread holds and frees it; on a kernel, the
/// outermost one restores the interrupt flag from before it was taken.
fn release(lo: &mut Lowering, fx: &mut Fx, lock: Value) {
    let thread = lo.thread(fx);
    let inside = fx.b.ins().icmp_imm_s(IntCC::NotEqual, thread, 0);
    let held = fx.choose(
        inside,
        &[PTR],
        |fx| {
            let held = fx.load(PTR, thread, thread::LOCKS);
            let held = fx.b.ins().iadd_imm_s(held, -1);
            fx.store(held, thread, thread::LOCKS);
            let recorded =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedLessThan, held, HELD_LOCKS as i64);
            fx.when(recorded, |fx| {
                let zero = fx.ptr(0);
                fx.each(zero, held, |fx, index| {
                    let slot = fx.b.ins().imul_imm_s(index, 8);
                    let slot = fx.b.ins().iadd(thread, slot);
                    let taken = fx.load(PTR, slot, thread::HELD);
                    let same = fx.b.ins().icmp(IntCC::Equal, taken, lock);
                    fx.when(same, |fx| {
                        let last = fx.b.ins().imul_imm_s(held, 8);
                        let last = fx.b.ins().iadd(thread, last);
                        let moved = fx.load(PTR, last, thread::HELD);
                        fx.store(moved, slot, thread::HELD);
                    });
                });
            });
            vec![held]
        },
        |fx| vec![fx.ptr(1)],
    )[0];
    let zero = fx.int(types::I32, 0);
    fx.b.ins().atomic_store(MemFlagsData::trusted(), zero, lock);
    if lo.kernel() {
        let outermost = fx.b.ins().icmp_imm_s(IntCC::Equal, held, 0);
        fx.when(outermost, |fx| {
            let flags = fx.load(PTR, thread, thread::SAVED_IF);
            lo.call_stub(fx, Stub::IrqRestore, &[flags]);
        });
    }
}

/// Releases every lock the running thread holds, for its trapping task.
pub fn release_held(lo: &mut Lowering, fx: &mut Fx) {
    let thread = lo.thread(fx);
    let held = fx.load(PTR, thread, thread::LOCKS);
    let most = fx.ptr(HELD_LOCKS);
    let recorded = fx.b.ins().umin(held, most);
    let zero = fx.ptr(0);
    fx.each(zero, recorded, |fx, index| {
        let slot = fx.b.ins().imul_imm_s(index, 8);
        let slot = fx.b.ins().iadd(thread, slot);
        let lock = fx.load(PTR, slot, thread::HELD);
        let free = fx.int(types::I32, 0);
        fx.b.ins().atomic_store(MemFlagsData::trusted(), free, lock);
    });
    fx.store(zero, thread, thread::LOCKS);
}
