//! Cancelling constructs: the region record of each task scope, `within` and `race` call and of
//! the program's termination requests, which tasks run in; cancelling one, and the origin of a
//! cancellation.
//!
//! A region record is the construct's id, which no other construct takes, the region enclosing
//! it, whether it has cancelled, and its kind; a construct's own record follows. A task's
//! region is the innermost one holding the call it runs. A cancellation's origin is the id of
//! the outermost cancelled construct holding it, zero for none.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, condcodes::IntCC, types};

use super::super::Lowering;
use super::super::fx::{Fx, PTR};
use super::tasks::{self, task};
use super::{Routine, state};

pub mod region {
    pub const ID: u64 = 0;
    pub const PARENT: u64 = 8;
    pub const CANCELLED: u64 = 16;
    /// What the construct waits on: the task to wake when its tasks end.
    pub const WAITER: u64 = 24;
    /// The count of its tasks that have not ended.
    pub const LIVE: u64 = 32;
    pub const SIZE: u64 = 40;
}

/// The value a waiting task's `IO` field holds while it waits at a cancellation point for
/// something no kernel operation completes.
pub const WAITING: i64 = 1;

pub fn define(lo: &mut Lowering, fx: &mut Fx, routine: Routine) {
    let params = fx.entry();
    match routine {
        Routine::RegionOpen => {
            let size = params[0];
            let record = lo.record_alloc(fx, size);
            let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
            fx.when(failed, |fx| {
                super::report::raise_kind(lo, fx, "trap.exhausted")
            });
            let state_address = lo.state(fx);
            let counter = fx.offset(state_address, state::REGIONS);
            let one = fx.ptr(1);
            let previous = super::fetch_add(fx, counter, one);
            let id = fx.b.ins().iadd_imm_s(previous, 1);
            fx.store(id, record, region::ID);
            let current = tasks::current_task(lo, fx);
            let parent = fx.load(PTR, current, task::REGION);
            fx.store(parent, record, region::PARENT);
            fx.b.ins().return_(&[record]);
        }
        Routine::Cancel => {
            cancel(lo, fx, params[0]);
            fx.b.ins().return_(&[]);
        }
        Routine::Origin => {
            let current = tasks::current_task(lo, fx);
            let region = fx.load(PTR, current, task::REGION);
            let origin = origin(fx, region);
            fx.b.ins().return_(&[origin]);
        }
        _ => unreachable!("a region routine"),
    }
}

/// The id of the outermost cancelled construct among a region and those enclosing it, zero when
/// none has cancelled.
pub fn origin(fx: &mut Fx, region: Value) -> Value {
    let zero = fx.ptr(0);
    let found = fx.repeat(&[region, zero], |fx, state| {
        let (current, found) = (state[0], state[1]);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
        let next = fx.choose(
            present,
            &[PTR, PTR],
            |fx| {
                let flag = fx.offset(current, region::CANCELLED);
                let cancelled =
                    fx.b.ins()
                        .atomic_load(types::I64, MemFlagsData::trusted(), flag);
                let cancelled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, cancelled, 0);
                let id = fx.load(PTR, current, region::ID);
                let found = fx.b.ins().select(cancelled, id, found);
                vec![fx.load(PTR, current, region::PARENT), found]
            },
            |_| vec![current, found],
        );
        (present, next)
    });
    found[1]
}

/// Cancels the operation a task waits on in the region of `target`, claiming its record first
/// so the task keeps it while the cancellation reads it; nothing when the task has left that
/// operation, whatever operation it waits on since.
fn cancel_claimed(lo: &mut Lowering, fx: &mut Fx, record: Value, op: Value, target: Value) {
    let word = fx.offset(record, task::IO);
    let claimed = fx.b.ins().bor_imm_s(op, tasks::CLAIMED);
    let seen =
        fx.b.ins()
            .atomic_cas(MemFlagsData::trusted(), word, op, claimed);
    let held = fx.b.ins().icmp(IntCC::Equal, seen, op);
    fx.when(held, |fx| {
        let region = fx.load(PTR, record, task::REGION);
        let reached = within(fx, region, target);
        fx.when(reached, |fx| lo.cancel_operation(fx, op));
        fx.b.ins().atomic_store(MemFlagsData::trusted(), op, word);
    });
}

/// Whether a region is `target` or lies within it.
fn within(fx: &mut Fx, region: Value, target: Value) -> Value {
    let no = fx.int(types::I8, 0);
    let found = fx.repeat(&[region, no], |fx, state| {
        let (current, found) = (state[0], state[1]);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
        let next = fx.choose(
            present,
            &[PTR, types::I8],
            |fx| {
                let same = fx.b.ins().icmp(IntCC::Equal, current, target);
                let found = fx.b.ins().bor(found, same);
                vec![fx.load(PTR, current, region::PARENT), found]
            },
            |_| vec![current, found],
        );
        let open = fx.b.ins().bxor_imm_s(next[1], 1);
        let again = fx.b.ins().band(present, open);
        (again, next)
    });
    found[1]
}

/// Cancels a construct once: every task in its region waiting at a cancellation point has its
/// operation cancelled, or is woken when it waits for no operation.
fn cancel(lo: &mut Lowering, fx: &mut Fx, target: Value) {
    let flag = fx.offset(target, region::CANCELLED);
    let zero = fx.ptr(0);
    let one = fx.ptr(1);
    let previous =
        fx.b.ins()
            .atomic_cas(MemFlagsData::trusted(), flag, zero, one);
    let first = fx.b.ins().icmp_imm_s(IntCC::Equal, previous, 0);
    fx.when(first, |fx| {
        let state_address = lo.state(fx);
        let lock = fx.offset(state_address, state::TASKS_LOCK);
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let head = fx.load(PTR, state_address, state::TASKS);
        fx.repeat(&[head], |fx, state| {
            let record = state[0];
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
            let next = fx.choose_in(
                lo,
                present,
                &[PTR],
                |lo, fx| {
                    let waiting = fx.load(PTR, record, task::IO);
                    let waits = fx.b.ins().icmp_imm_s(IntCC::NotEqual, waiting, 0);
                    let region = fx.load(PTR, record, task::REGION);
                    let held = within(fx, region, target);
                    let reached = fx.b.ins().band(waits, held);
                    fx.when(reached, |fx| {
                        let plain = fx.b.ins().icmp_imm_s(IntCC::Equal, waiting, WAITING);
                        fx.choose_in(
                            lo,
                            plain,
                            &[],
                            |lo, fx| {
                                tasks::wake(lo, fx, record);
                                Vec::new()
                            },
                            |lo, fx| {
                                cancel_claimed(lo, fx, record, waiting, target);
                                Vec::new()
                            },
                        );
                    });
                    vec![fx.load(PTR, record, task::ALL_NEXT)]
                },
                |_, fx| vec![fx.ptr(0)],
            );
            let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next[0], 0);
            (more, next)
        });
        lo.call_routine(fx, Routine::Unlock, &[lock]);
    });
}
