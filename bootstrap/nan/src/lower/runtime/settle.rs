//! Settlement of an abandoned `sys::Domain` body, which the task waiting for the domain runs:
//! every task of the domain and of the domains nested in it stops, then what those tasks and
//! domains hold is released and their memory is freed.

use cranelift_codegen::ir::{
    AbiParam, AtomicRmwOp, InstBuilder, MemFlagsData, Signature, TrapCode, Value, condcodes::IntCC,
    types,
};
use cranelift_module::FuncId;

use super::super::fx::{Fx, PTR};
use super::super::host;
use super::super::supplied::tasks::cowns;
use super::super::{CALL_CONV, Lowering};
use super::regions::WAITING;
use super::tasks::{self, domain, operation, run, task};
use super::{Routine, holdings, memory, state, tables};

/// The offsets of a wait record's fields: the glue withdrawing the wait, given the record and
/// the waiting task, the structure waited in, the waiting node and the record of the wait it
/// is nested in.
pub mod wait {
    pub const GLUE: u64 = 0;
    pub const OBJECT: u64 = 8;
    pub const NODE: u64 = 16;
    pub const OUTER: u64 = 24;
    pub const SIZE: u64 = 32;
}

/// The signature of a glue withdrawing a wait: the wait record and the task.
pub fn withdraw_signature() -> Signature {
    let mut signature = Signature::new(CALL_CONV);
    signature.params.push(AbiParam::new(PTR));
    signature.params.push(AbiParam::new(PTR));
    signature
}

/// Records the running task's wait in a structure other domains reach, innermost of its waits,
/// which settlement withdraws through `glue` should the task stop.
pub fn enter_wait(lo: &mut Lowering, fx: &mut Fx, glue: FuncId, object: Value, node: Value) {
    let record = fx.slot(wait::SIZE, 8);
    let glue = lo.func_ref(fx, glue);
    let glue = fx.b.ins().func_addr(PTR, glue);
    fx.store(glue, record, wait::GLUE);
    fx.store(object, record, wait::OBJECT);
    fx.store(node, record, wait::NODE);
    let current = tasks::current_task(lo, fx);
    let outer = fx.load(PTR, current, task::WAIT);
    fx.store(outer, record, wait::OUTER);
    fx.store(record, current, task::WAIT);
}

/// Ends the running task's innermost recorded wait.
pub fn leave_wait(lo: &mut Lowering, fx: &mut Fx) {
    let current = tasks::current_task(lo, fx);
    let innermost = fx.load(PTR, current, task::WAIT);
    let outer = fx.load(PTR, innermost, wait::OUTER);
    fx.store(outer, current, task::WAIT);
}

pub fn define(lo: &mut Lowering, fx: &mut Fx, routine: Routine) {
    match routine {
        Routine::Settle => {
            let params = fx.entry();
            settle(lo, fx, params[0]);
            fx.b.ins().return_(&[]);
        }
        _ => unreachable!("a settle routine"),
    }
}

/// `Settle(domain)`: stops every task of an abandoned domain, takes them out of the list of
/// live tasks, cancels their operations in flight and waits for each acknowledgement, withdraws
/// their waits from what other domains reach, releases their stacks and records, then ends the
/// domains nested in it and itself: each one's holdings are released and its chunks freed.
fn settle(lo: &mut Lowering, fx: &mut Fx, abandoned: Value) {
    stop_tasks(lo, fx, abandoned);
    let first = detach_tasks(lo, fx, abandoned);
    each_stopped(fx, first, &mut |fx, record| {
        cancel_operations(lo, fx, record)
    });
    each_stopped(fx, first, &mut |fx, record| withdraw(fx, record));
    each_stopped(fx, first, &mut |fx, record| poison_held(lo, fx, record));
    each_stopped(fx, first, &mut |fx, record| {
        tasks::release_stack(lo, fx, record);
        lo.record_free(fx, record);
    });
    end_nested(lo, fx, abandoned);
    end(lo, fx, abandoned);
}

/// Ends an abandoned domain: releases its holdings, then frees its chunks.
fn end(lo: &mut Lowering, fx: &mut Fx, domain: Value) {
    holdings::release_all(lo, fx, domain);
    memory::free_chunks(lo, fx, domain);
}

/// Runs `visit` on each task of the chain of stopped tasks from `first`, reading each task's
/// link before the visit.
fn each_stopped(fx: &mut Fx, first: Value, visit: &mut dyn FnMut(&mut Fx, Value)) {
    let any = fx.b.ins().icmp_imm_s(IntCC::NotEqual, first, 0);
    fx.when(any, |fx| {
        fx.repeat(&[first], |fx, state| {
            let record = state[0];
            let next = fx.load(PTR, record, task::SETTLE);
            visit(fx, record);
            let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next, 0);
            (more, vec![next])
        });
    });
}

/// Cancels the operation a stopped task waits on and the deadlines it armed, and waits until
/// each one's completion has been delivered; a deadline then leaves its clock's deadlines and
/// its region is freed.
fn cancel_operations(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let op = claim_operation(lo, fx, record);
    let waits = fx.b.ins().icmp_imm_s(IntCC::NotEqual, op, 0);
    fx.when(waits, |fx| settle_operation(lo, fx, op));
    let first = fx.load(PTR, record, task::DEADLINES);
    let armed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, first, 0);
    fx.when(armed, |fx| {
        fx.repeat(&[first], |fx, state| {
            let timer = state[0];
            let next = fx.load(PTR, timer, operation::LINK);
            settle_operation(lo, fx, timer);
            tables::unlink_deadline(lo, fx, timer);
            let region = fx.load(PTR, timer, operation::TASK);
            lo.record_free(fx, region);
            let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next, 0);
            (more, vec![next])
        });
    });
}

/// The operation a stopped task waits on, claimed so that no cancellation reads its record
/// any more, or zero when it waits on none.
fn claim_operation(lo: &mut Lowering, fx: &mut Fx, record: Value) -> Value {
    let word = fx.offset(record, task::IO);
    let zero = fx.ptr(0);
    fx.repeat(&[zero], |fx, _| {
        let io = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), word);
        let claimed = fx.b.ins().band_imm_s(io, tasks::CLAIMED);
        let claimed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, claimed, 0);
        let operation =
            fx.b.ins()
                .icmp_imm_s(IntCC::UnsignedGreaterThan, io, WAITING);
        let unclaimed = fx.b.ins().bxor_imm_s(claimed, 1);
        let operation = fx.b.ins().band(operation, unclaimed);
        let result = fx.choose(
            operation,
            &[types::I8, PTR],
            |fx| {
                let held = fx.b.ins().bor_imm_s(io, tasks::CLAIMED);
                let seen =
                    fx.b.ins()
                        .atomic_cas(MemFlagsData::trusted(), word, io, held);
                let taken = fx.b.ins().icmp(IntCC::Equal, seen, io);
                let zero = fx.ptr(0);
                let op = fx.b.ins().select(taken, io, zero);
                vec![fx.b.ins().bxor_imm_s(taken, 1), op]
            },
            |fx| vec![claimed, fx.ptr(0)],
        );
        fx.when(result[0], |fx| {
            lo.relax(fx);
        });
        (result[0], vec![result[1]])
    })[0]
}

/// Cancels an operation and waits until its completion has been delivered, handling the
/// completions on the running thread's ring meanwhile.
fn settle_operation(lo: &mut Lowering, fx: &mut Fx, op: Value) {
    lo.cancel_operation(fx, op);
    let word = fx.offset(op, operation::DONE);
    fx.repeat(&[], |fx, _| {
        lo.reap(fx);
        let done = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), word);
        let pending =
            fx.b.ins()
                .icmp_imm_s(IntCC::NotEqual, done, operation::DELIVERED);
        fx.when(pending, |fx| {
            lo.relax(fx);
        });
        (pending, Vec::new())
    });
}

/// Runs the glue withdrawing each of a stopped task's waits in structures other domains reach,
/// innermost first.
fn withdraw(fx: &mut Fx, record: Value) {
    let innermost = fx.load(PTR, record, task::WAIT);
    let waiting = fx.b.ins().icmp_imm_s(IntCC::NotEqual, innermost, 0);
    fx.when(waiting, |fx| {
        fx.repeat(&[innermost], |fx, state| {
            let recorded = state[0];
            let outer = fx.load(PTR, recorded, wait::OUTER);
            let glue = fx.load(PTR, recorded, wait::GLUE);
            let signature = fx.b.import_signature(withdraw_signature());
            fx.b.ins()
                .call_indirect(signature, glue, &[recorded, record]);
            let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, outer, 0);
            (more, vec![outer])
        });
    });
}

/// Poisons the cowns of the `await when` body a stopped task runs.
fn poison_held(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let held = fx.load(PTR, record, task::HELD);
    let holds = fx.b.ins().icmp_imm_s(IntCC::NotEqual, held, 0);
    fx.when(holds, |fx| {
        let domain = fx.load(PTR, record, task::DOMAIN);
        cowns::poison(lo, fx, held, domain);
    });
}

/// Visits each live task of a domain or of the domains nested in it, holding the lock of the
/// list of live tasks; `visit` gives the next task to visit after the one it receives.
fn each_task(
    lo: &mut Lowering,
    fx: &mut Fx,
    abandoned: Value,
    visit: &mut dyn FnMut(&mut Lowering, &mut Fx, Value) -> Value,
) {
    let state_address = lo.state(fx);
    let lock = fx.offset(state_address, state::TASKS_LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let head = fx.load(PTR, state_address, state::TASKS);
    let any = fx.b.ins().icmp_imm_s(IntCC::NotEqual, head, 0);
    fx.when(any, |fx| {
        fx.repeat(&[head], |fx, state| {
            let record = state[0];
            let owner = fx.load(PTR, record, task::DOMAIN);
            let inside = tasks::within(fx, owner, abandoned);
            let next = fx.choose_in(
                lo,
                inside,
                &[PTR],
                |lo, fx| vec![visit(lo, fx, record)],
                |_, fx| vec![fx.load(PTR, record, task::ALL_NEXT)],
            )[0];
            let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next, 0);
            (more, vec![next])
        });
    });
    lo.call_routine(fx, Routine::Unlock, &[lock]);
}

/// Asks every task of the domain to stop and waits until each has: a new or parked task stops
/// at once, a ready one leaves the run queue, and a running one stops once its thread has
/// switched away from it, its thread receiving the stop signal until then.
fn stop_tasks(lo: &mut Lowering, fx: &mut Fx, abandoned: Value) {
    fx.repeat(&[], |fx, _| {
        let pending = fx.b.declare_var(types::I8);
        let none = fx.int(types::I8, 0);
        fx.b.def_var(pending, none);
        each_task(lo, fx, abandoned, &mut |lo, fx, record| {
            let held = stop(lo, fx, record);
            let waiting = fx.b.use_var(pending);
            let waiting = fx.b.ins().bor(waiting, held);
            fx.b.def_var(pending, waiting);
            fx.load(PTR, record, task::ALL_NEXT)
        });
        let waiting = fx.b.use_var(pending);
        fx.when(waiting, |fx| {
            lo.relax(fx);
        });
        (waiting, Vec::new())
    });
}

/// Asks a task to stop and stops it when it is off every thread; gives whether it has yet to
/// stop. A running task's thread receives the stop signal; on a kernel, its CPU receives the
/// CPU signal when the task is first asked, and the CPU's timer interrupts ask again.
fn stop(lo: &mut Lowering, fx: &mut Fx, record: Value) -> Value {
    let one = fx.ptr(1);
    let asked = fx.offset(record, task::STOP);
    let first = match lo.kernel() {
        true => {
            let before =
                fx.b.ins()
                    .atomic_rmw(PTR, MemFlagsData::trusted(), AtomicRmwOp::Xchg, asked, one);
            Some(fx.b.ins().icmp_imm_s(IntCC::Equal, before, 0))
        }
        false => {
            fx.b.ins().atomic_store(MemFlagsData::trusted(), one, asked);
            None
        }
    };
    let word = fx.offset(record, task::STATE);
    let current =
        fx.b.ins()
            .atomic_load(types::I32, MemFlagsData::trusted(), word);
    let stopped = fx.int(types::I32, run::STOPPED);
    let is_new = fx.b.ins().icmp_imm_s(IntCC::Equal, current, run::NEW);
    let is_parked = fx.b.ins().icmp_imm_s(IntCC::Equal, current, run::PARKED);
    let idle = fx.b.ins().bor(is_new, is_parked);
    let is_ready = fx.b.ins().icmp_imm_s(IntCC::Equal, current, run::READY);
    let is_stopped = fx.b.ins().icmp_imm_s(IntCC::Equal, current, run::STOPPED);
    fx.choose_in(
        lo,
        idle,
        &[types::I8],
        |_, fx| {
            let seen =
                fx.b.ins()
                    .atomic_cas(MemFlagsData::trusted(), word, current, stopped);
            vec![fx.b.ins().icmp(IntCC::NotEqual, seen, current)]
        },
        |lo, fx| {
            let left = fx.choose_in(
                lo,
                is_ready,
                &[types::I8],
                |lo, fx| {
                    let removed = remove_ready(lo, fx, record);
                    fx.when(removed, |fx| {
                        fx.b.ins()
                            .atomic_store(MemFlagsData::trusted(), stopped, word);
                    });
                    vec![fx.b.ins().bxor_imm_s(removed, 1)]
                },
                |lo, fx| {
                    let running = fx.b.ins().bxor_imm_s(is_stopped, 1);
                    if let Some(first) = first {
                        let signals = fx.b.ins().band(running, first);
                        fx.when(signals, |fx| {
                            let thread = fx.load(PTR, record, task::THREAD);
                            let index = fx.load(PTR, thread, tasks::thread::INDEX);
                            lo.signal_cpu(fx, index);
                        });
                        return vec![running];
                    }
                    fx.when(running, |fx| {
                        // A machine test image settles domains on its one executor, which runs
                        // the settling task.
                        if lo.machine() {
                            fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
                            return;
                        }
                        let thread = fx.load(PTR, record, task::THREAD);
                        let pthread = fx.load(PTR, thread, tasks::thread::PTHREAD);
                        let signal = fx.int(types::I32, i64::from(host::SIGURG));
                        lo.call_libc(fx, "pthread_kill", &[pthread, signal], &[types::I32]);
                    });
                    vec![running]
                },
            )[0];
            vec![left]
        },
    )[0]
}

/// Takes a task out of the run queue; gives whether it was there.
fn remove_ready(lo: &mut Lowering, fx: &mut Fx, record: Value) -> Value {
    let state_address = lo.state(fx);
    let lock = fx.offset(state_address, state::QUEUE_LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let head = fx.load(PTR, state_address, state::READY_HEAD);
    let zero = fx.ptr(0);
    let found = fx.repeat(&[head, zero, zero], |fx, state| {
        let (current, previous, _) = (state[0], state[1], state[2]);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
        let result = fx.choose(
            present,
            &[PTR, PTR, PTR, types::I8],
            |fx| {
                let same = fx.b.ins().icmp(IntCC::Equal, current, record);
                let next = fx.load(PTR, current, task::NEXT);
                let at = fx.b.ins().select(same, current, next);
                let before = fx.b.ins().select(same, previous, current);
                let zero = fx.ptr(0);
                let found = fx.b.ins().select(same, current, zero);
                let again = fx.b.ins().bxor_imm_s(same, 1);
                vec![at, before, found, again]
            },
            |fx| vec![fx.ptr(0), previous, fx.ptr(0), fx.int(types::I8, 0)],
        );
        (result[3], vec![result[0], result[1], result[2]])
    });
    let (previous, found) = (found[1], found[2]);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, found, 0);
    fx.when(present, |fx| {
        let next = fx.load(PTR, record, task::NEXT);
        let first = fx.b.ins().icmp_imm_s(IntCC::Equal, previous, 0);
        fx.choose(
            first,
            &[],
            |fx| {
                fx.store(next, state_address, state::READY_HEAD);
                Vec::new()
            },
            |fx| {
                fx.store(next, previous, task::NEXT);
                Vec::new()
            },
        );
        let last = fx.b.ins().icmp_imm_s(IntCC::Equal, next, 0);
        fx.when(last, |fx| {
            fx.store(previous, state_address, state::READY_TAIL)
        });
    });
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    present
}

/// Takes every stopped task of the domain out of the list of live tasks into a chain of their
/// own; gives the chain's first task.
fn detach_tasks(lo: &mut Lowering, fx: &mut Fx, abandoned: Value) -> Value {
    let chain = fx.b.declare_var(PTR);
    let none = fx.ptr(0);
    fx.b.def_var(chain, none);
    each_task(lo, fx, abandoned, &mut |lo, fx, record| {
        let state_address = lo.state(fx);
        let next = fx.load(PTR, record, task::ALL_NEXT);
        let previous = fx.load(PTR, record, task::ALL_PREVIOUS);
        let has_previous = fx.b.ins().icmp_imm_s(IntCC::NotEqual, previous, 0);
        fx.choose(
            has_previous,
            &[],
            |fx| {
                fx.store(next, previous, task::ALL_NEXT);
                Vec::new()
            },
            |fx| {
                fx.store(next, state_address, state::TASKS);
                Vec::new()
            },
        );
        let has_next = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next, 0);
        fx.when(has_next, |fx| fx.store(previous, next, task::ALL_PREVIOUS));
        let first = fx.b.use_var(chain);
        fx.store(first, record, task::SETTLE);
        fx.b.def_var(chain, record);
        next
    });
    fx.b.use_var(chain)
}

/// Ends the domains nested in the abandoned one, innermost first, and frees their records.
fn end_nested(lo: &mut Lowering, fx: &mut Fx, abandoned: Value) {
    fx.repeat(&[], |fx, _| {
        let child = fx.load(PTR, abandoned, domain::CHILDREN);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, child, 0);
        fx.when(present, |fx| {
            let innermost = fx.repeat(&[child], |fx, state| {
                let current = state[0];
                let inner = fx.load(PTR, current, domain::CHILDREN);
                let deeper = fx.b.ins().icmp_imm_s(IntCC::NotEqual, inner, 0);
                let next = fx.b.ins().select(deeper, inner, current);
                (deeper, vec![next])
            })[0];
            let outer = fx.load(PTR, innermost, domain::PARENT);
            let sibling = fx.load(PTR, innermost, domain::SIBLING);
            fx.store(sibling, outer, domain::CHILDREN);
            end(lo, fx, innermost);
            lo.record_free(fx, innermost);
        });
        (present, Vec::new())
    });
}
