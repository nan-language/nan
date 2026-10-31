//! The intake of a runner's record: messages entering it through sends, delays and posts, its
//! stop, and the posts waiting until their messages are taken.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};
use cranelift_module::FuncId;

use super::super::super::super::body::Held;
use super::super::super::super::fx::{Fx, PTR};
use super::super::super::super::runtime::regions::WAITING;
use super::super::super::super::runtime::tables;
use super::super::super::super::runtime::tasks::{self, kind, operation, task};
use super::super::super::super::runtime::{Routine, report, settle, timers};
use super::super::super::super::{Glue, Lowering};
use super::super::super::Supply;
use super::{entry, reference, runner, status, unreference, wake_waiter};
use crate::check::ty::Ty;

/// The fields of a delayed message's timer after its operation record: its links in its
/// runner's timers, its relative time, and on a kernel the next timer a cancellation claimed.
mod timer {
    use super::operation;
    pub const NEXT: u64 = operation::SIZE;
    pub const PREVIOUS: u64 = operation::SIZE + 8;
    pub const SPEC: u64 = operation::SIZE + 16;
    pub const CLAIMED: u64 = operation::SIZE + 32;
    pub const SIZE: u64 = operation::SIZE + 40;
}

/// Stops a runner's intake: no message enters it afterwards, the delayed messages are released
/// and their timers cancelled, and the task waiting for a message wakes.
pub fn stop(lo: &mut Lowering, fx: &mut Fx, record: Value, message: &Ty) {
    let lock = fx.offset(record, runner::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let one = fx.ptr(1);
    fx.store(one, record, runner::STOPPED);
    let delayed = fx.load(PTR, record, runner::DELAYED);
    let zero = fx.ptr(0);
    fx.store(zero, record, runner::DELAYED);
    let claimed = cancel_timers(lo, fx, record);
    wake_component(lo, fx, record);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    complete_claimed(lo, fx, claimed);
    let message = message.clone();
    fx.repeat(&[delayed], |fx, state| {
        let node = state[0];
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, node, 0);
        let next = fx.choose_in(
            lo,
            present,
            &[PTR],
            |lo, fx| {
                let next = fx.load(PTR, node, entry::NEXT);
                let held = fx.offset(node, entry::MESSAGE);
                lo.release(fx, &message, held);
                lo.call_routine(fx, Routine::Free, &[node]);
                vec![next]
            },
            |_, fx| vec![fx.ptr(0)],
        );
        (present, next)
    });
}

/// Cancels the timers of a runner's delayed messages. On a kernel, where a cancelled timer
/// completes at once, it only claims them, giving the chain of those it claimed, whose
/// completions the caller delivers once it has released the lock. The caller holds the lock.
pub fn cancel_timers(lo: &mut Lowering, fx: &mut Fx, record: Value) -> Option<Value> {
    let first = fx.load(PTR, record, runner::TIMERS);
    if lo.seeded() || !lo.kernel() {
        fx.repeat(&[first], |fx, state| {
            let op = state[0];
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, op, 0);
            let next = fx.choose_in(
                lo,
                present,
                &[PTR],
                |lo, fx| {
                    lo.cancel_operation(fx, op);
                    vec![fx.load(PTR, op, timer::NEXT)]
                },
                |_, fx| vec![fx.ptr(0)],
            );
            (present, next)
        });
        return None;
    }
    let zero = fx.ptr(0);
    let claimed = fx.repeat(&[first, zero], |fx, state| {
        let (op, claimed) = (state[0], state[1]);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, op, 0);
        let next = fx.choose_in(
            lo,
            present,
            &[PTR, PTR],
            |lo, fx| {
                let taken = lo.kernel_claim(fx, op);
                fx.store(claimed, op, timer::CLAIMED);
                let claimed = fx.b.ins().select(taken, op, claimed);
                vec![fx.load(PTR, op, timer::NEXT), claimed]
            },
            |_, fx| vec![fx.ptr(0), claimed],
        );
        (present, next)
    });
    Some(claimed[1])
}

/// Delivers the cancelled completions of the timers `cancel_timers` claimed.
pub fn complete_claimed(lo: &mut Lowering, fx: &mut Fx, claimed: Option<Value>) {
    let Some(first) = claimed else {
        return;
    };
    fx.repeat(&[first], |fx, state| {
        let op = state[0];
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, op, 0);
        let next = fx.choose_in(
            lo,
            present,
            &[PTR],
            |lo, fx| {
                let next = fx.load(PTR, op, timer::CLAIMED);
                lo.kernel_cancelled(fx, op);
                vec![next]
            },
            |_, fx| vec![fx.ptr(0)],
        );
        (present, next)
    });
}

/// Takes a message a task of another domain posted into the component task's domain, before
/// the post completes; the caller holds the runner's lock.
pub fn take_posted(lo: &mut Lowering, fx: &mut Fx, message: &Ty, address: Value, poster: Value) {
    if !lo.transfers(message) {
        return;
    }
    let from = fx.load(PTR, poster, task::DOMAIN);
    let domain = tasks::current_domain(lo, fx);
    let other = fx.b.ins().icmp(IntCC::NotEqual, from, domain);
    fx.when(other, |fx| {
        lo.transfer(fx, message, address, domain, domain)
    });
}

/// Gives a reply to a poster of another domain than the component task's; the caller holds the
/// runner's lock.
pub fn give_posted(lo: &mut Lowering, fx: &mut Fx, ty: &Ty, address: Value, poster: Value) {
    if !lo.transfers(ty) {
        return;
    }
    let to = fx.load(PTR, poster, task::DOMAIN);
    let domain = tasks::current_domain(lo, fx);
    let other = fx.b.ins().icmp(IntCC::NotEqual, to, domain);
    fx.when(other, |fx| lo.transfer(fx, ty, address, to, to));
}

/// Wakes a runner's component task when it waits for a message. The caller holds the lock.
pub fn wake_component(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let idle = fx.load(PTR, record, runner::IDLE);
    let idle = fx.b.ins().icmp_imm_s(IntCC::NotEqual, idle, 0);
    fx.when(idle, |fx| {
        let zero = fx.ptr(0);
        fx.store(zero, record, runner::IDLE);
        let component = fx.load(PTR, record, runner::COMPONENT);
        lo.call_routine(fx, Routine::Wake, &[component]);
    });
}

/// Appends an entry to a runner's intake, waking the component task. The caller holds the lock.
pub fn append(lo: &mut Lowering, fx: &mut Fx, record: Value, node: Value) {
    let zero = fx.ptr(0);
    fx.store(zero, node, entry::NEXT);
    let newest = fx.load(PTR, record, runner::NEWEST);
    fx.store(newest, node, entry::PREVIOUS);
    let has_newest = fx.b.ins().icmp_imm_s(IntCC::NotEqual, newest, 0);
    fx.choose(
        has_newest,
        &[],
        |fx| {
            fx.store(node, newest, entry::NEXT);
            Vec::new()
        },
        |fx| {
            fx.store(node, record, runner::OLDEST);
            Vec::new()
        },
    );
    fx.store(node, record, runner::NEWEST);
    wake_component(lo, fx, record);
}

/// Removes an entry from a runner's intake. The caller holds the lock.
pub fn unlink(fx: &mut Fx, record: Value, node: Value) {
    let next = fx.load(PTR, node, entry::NEXT);
    let previous = fx.load(PTR, node, entry::PREVIOUS);
    let has_previous = fx.b.ins().icmp_imm_s(IntCC::NotEqual, previous, 0);
    fx.choose(
        has_previous,
        &[],
        |fx| {
            fx.store(next, previous, entry::NEXT);
            Vec::new()
        },
        |fx| {
            fx.store(next, record, runner::OLDEST);
            Vec::new()
        },
    );
    let has_next = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next, 0);
    fx.choose(
        has_next,
        &[],
        |fx| {
            fx.store(previous, next, entry::PREVIOUS);
            Vec::new()
        },
        |fx| {
            fx.store(previous, record, runner::NEWEST);
            Vec::new()
        },
    );
}

/// A new entry holding a message, drawn through a runner's allocator; traps when it is
/// exhausted.
pub fn new_entry(lo: &mut Lowering, fx: &mut Fx, record: Value, message: &Ty, held: Held) -> Value {
    let layout = lo.layout(message);
    let cell = fx.load(PTR, record, runner::CELL);
    let size = fx.ptr(entry::MESSAGE + layout.size);
    let align = fx.ptr(layout.align.max(8));
    let node = lo.call_routine(fx, Routine::Alloc, &[cell, size, align])[0];
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, node, 0);
    fx.when(missing, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
    let zero = fx.ptr(0);
    let header = fx.ptr(entry::MESSAGE);
    lo.call_routine(fx, Routine::Fill, &[node, zero, header]);
    let at = fx.offset(node, entry::MESSAGE);
    lo.store_held(fx, at, held, message);
    node
}

/// `mut cx.send(m)`, and `after` with no delay: appends `m` to the intake, or releases it once
/// the intake has stopped.
pub fn send_now(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, record: Value, held: Held) {
    let message = supply.type_arg(0);
    let node = new_entry(lo, fx, record, &message, held);
    let lock = fx.offset(record, runner::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let stopped = fx.load(PTR, record, runner::STOPPED);
    let stopped = fx.b.ins().icmp_imm_s(IntCC::NotEqual, stopped, 0);
    fx.choose_in(
        lo,
        stopped,
        &[],
        |lo, fx| {
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            let at = fx.offset(node, entry::MESSAGE);
            lo.release(fx, &message, at);
            lo.call_routine(fx, Routine::Free, &[node]);
            Vec::new()
        },
        |lo, fx| {
            append(lo, fx, record, node);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            Vec::new()
        },
    );
    supply.done(lo, fx);
}

/// `mut cx.after(ms: n, send: m)` and `mut cx.after_ns(ns: n, send: m)`: `m` enters the intake
/// once `n` milliseconds or nanoseconds have elapsed and the elapsed time of the executor's clock
/// is a multiple of the executor's resolution, through a timer whose expiry moves the messages
/// due.
pub fn after(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let message = supply.type_arg(0);
    let record = fx.load(PTR, supply.param(0).address(), 0);
    let ns = match supply.path.as_str() {
        "sys::Cx.after" => timers::from_ms(fx, supply.param(1).scalar()),
        _ => supply.param(1).scalar(),
    };
    let held = supply.param(2);
    let immediate = fx.b.ins().icmp_imm_s(IntCC::Equal, ns, 0);
    fx.when(immediate, |fx| send_now(lo, fx, supply, record, held));
    let node = new_entry(lo, fx, record, &message, held);
    let size = fx.ptr(timer::SIZE);
    let op = lo.record_alloc(fx, size);
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, op, 0);
    fx.when(missing, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
    let after_kind = fx.ptr(kind::AFTER as u64);
    fx.store(after_kind, op, operation::KIND);
    fx.store(record, op, operation::TASK);
    let spec = fx.offset(op, timer::SPEC);
    let origin = fx.load(PTR, record, runner::ORIGIN);
    let resolution = fx.load(PTR, record, runner::RESOLUTION);
    let now = lo.now_ns(fx);
    let delay = timers::on_grid(fx, now, origin, resolution, ns);
    let deadline = timers::due_after(fx, now, delay);
    fx.store(deadline, node, entry::DEADLINE);
    let lock = fx.offset(record, runner::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let stopped = fx.load(PTR, record, runner::STOPPED);
    let stopped = fx.b.ins().icmp_imm_s(IntCC::NotEqual, stopped, 0);
    fx.choose_in(
        lo,
        stopped,
        &[],
        |lo, fx| {
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            let at = fx.offset(node, entry::MESSAGE);
            lo.release(fx, &message, at);
            lo.call_routine(fx, Routine::Free, &[node]);
            lo.record_free(fx, op);
            Vec::new()
        },
        |lo, fx| {
            let order = fx.load(PTR, record, runner::ORDER);
            let order = fx.b.ins().iadd_imm_s(order, 1);
            fx.store(order, record, runner::ORDER);
            fx.store(order, node, entry::ORDER);
            insert_delayed(fx, record, node, deadline);
            let timers = fx.load(PTR, record, runner::TIMERS);
            fx.store(timers, op, timer::NEXT);
            let has_timers = fx.b.ins().icmp_imm_s(IntCC::NotEqual, timers, 0);
            fx.when(has_timers, |fx| fx.store(op, timers, timer::PREVIOUS));
            fx.store(op, record, runner::TIMERS);
            let count = fx.load(PTR, record, runner::TIMER_COUNT);
            let count = fx.b.ins().iadd_imm_s(count, 1);
            fx.store(count, record, runner::TIMER_COUNT);
            reference(fx, record);
            lo.arm_timer(fx, op, delay, spec);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            Vec::new()
        },
    );
    supply.done(lo, fx);
}

/// Inserts a delayed entry after every delayed entry whose deadline is not later than its own.
/// The caller holds the lock.
fn insert_delayed(fx: &mut Fx, record: Value, node: Value, deadline: Value) {
    let first = fx.load(PTR, record, runner::DELAYED);
    let zero = fx.ptr(0);
    fx.repeat(&[zero, first], |fx, state| {
        let (previous, current) = (state[0], state[1]);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
        let later = fx.choose(
            present,
            &[types::I8],
            |fx| {
                let due = fx.load(PTR, current, entry::DEADLINE);
                vec![fx.b.ins().icmp(IntCC::UnsignedGreaterThan, due, deadline)]
            },
            |fx| vec![fx.int(types::I8, 1)],
        )[0];
        fx.when(later, |fx| {
            fx.store(current, node, entry::NEXT);
            let has_previous = fx.b.ins().icmp_imm_s(IntCC::NotEqual, previous, 0);
            fx.choose(
                has_previous,
                &[],
                |fx| {
                    fx.store(node, previous, entry::NEXT);
                    Vec::new()
                },
                |fx| {
                    fx.store(node, record, runner::DELAYED);
                    Vec::new()
                },
            );
        });
        let next = fx.choose(
            later,
            &[PTR],
            |_| vec![current],
            |fx| vec![fx.load(PTR, current, entry::NEXT)],
        )[0];
        (fx.b.ins().bxor_imm_s(later, 1), vec![current, next])
    });
}

/// What the thread whose ring held a delayed message's timer does once it completes: every
/// delayed message now due enters the intake, in order, the timer's record is freed and its
/// reference to the runner's record dropped.
pub fn timer_completed(lo: &mut Lowering, fx: &mut Fx, op: Value) {
    let record = fx.load(PTR, op, operation::TASK);
    let lock = fx.offset(record, runner::LOCK);
    let now = lo.now_ns(fx);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    fx.repeat(&[], |fx, _| {
        let first = fx.load(PTR, record, runner::DELAYED);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, first, 0);
        let due = fx.choose(
            present,
            &[types::I8],
            |fx| {
                let deadline = fx.load(PTR, first, entry::DEADLINE);
                vec![
                    fx.b.ins()
                        .icmp(IntCC::UnsignedLessThanOrEqual, deadline, now),
                ]
            },
            |fx| vec![fx.int(types::I8, 0)],
        )[0];
        fx.when(due, |fx| {
            let next = fx.load(PTR, first, entry::NEXT);
            fx.store(next, record, runner::DELAYED);
            append(lo, fx, record, first);
        });
        (due, Vec::new())
    });
    let next = fx.load(PTR, op, timer::NEXT);
    let previous = fx.load(PTR, op, timer::PREVIOUS);
    let has_previous = fx.b.ins().icmp_imm_s(IntCC::NotEqual, previous, 0);
    fx.choose(
        has_previous,
        &[],
        |fx| {
            fx.store(next, previous, timer::NEXT);
            Vec::new()
        },
        |fx| {
            fx.store(next, record, runner::TIMERS);
            Vec::new()
        },
    );
    let has_next = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next, 0);
    fx.when(has_next, |fx| fx.store(previous, next, timer::PREVIOUS));
    let count = fx.load(PTR, record, runner::TIMER_COUNT);
    let count = fx.b.ins().iadd_imm_s(count, -1);
    fx.store(count, record, runner::TIMER_COUNT);
    wake_waiter(lo, fx, record);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    lo.record_free(fx, op);
    unreference(lo, fx, record);
}

/// Fails a post with `SendError.Closed`, or a request with `CallError.Closed` holding it.
pub fn fail_closed(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let fails = supply.abi.fails.clone().expect("a post's failure type");
    let send_error = lo.std_ty("task", "SendError", Vec::new());
    let mut value = lo.variant_value(fx, &send_error, "Closed", &[], None);
    if fails != send_error {
        value = lo.variant_value(fx, &fails, "Closed", &[value], None);
    }
    let failure = supply.failure.expect("a failure address");
    lo.store_held(fx, failure, value, &fails);
    supply.fail(fx);
}

/// The steps of a post before it waits: a cancellation point, failing when the call is
/// cancelled at its start or `out` is stale or the intake has stopped; otherwise `m` enters the
/// intake in an entry on the posting task's stack, which a request's reply follows. Gives the
/// runner's record, which the post references, and the entry.
pub fn enqueue(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    message: &Ty,
    held: Held,
    reply: Option<Value>,
) -> (Value, Value) {
    let handle = fx.load(PTR, supply.param(0).address(), 0);
    let release_held = |lo: &mut Lowering, fx: &mut Fx| {
        if let Held::Address(address) = held {
            lo.release(fx, message, address);
        }
    };
    let origin = super::super::origin(lo, fx);
    let cancelled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, origin, 0);
    fx.when(cancelled, |fx| {
        release_held(lo, fx);
        super::super::fail_cancelled(lo, fx, supply, origin);
    });
    let record = lo.call_routine(fx, Routine::OutboxClaim, &[handle])[0];
    let stale = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
    fx.when(stale, |fx| {
        release_held(lo, fx);
        fail_closed(lo, fx, supply);
    });
    let lock = fx.offset(record, runner::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let (live, [_, key, keyed_post, _]) = tables::snapshot(lo, fx, handle);
    let stale = fx.b.ins().bxor_imm_s(live, 1);
    let stopped = fx.load(PTR, record, runner::STOPPED);
    let stopped = fx.b.ins().icmp_imm_s(IntCC::NotEqual, stopped, 0);
    let closed = fx.b.ins().bor(stale, stopped);
    fx.when(closed, |fx| {
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        unreference(lo, fx, record);
        release_held(lo, fx);
        fail_closed(lo, fx, supply);
    });
    let layout = lo.layout(message);
    let node = fx.slot(entry::MESSAGE + layout.size, layout.align.max(8));
    let zero = fx.ptr(0);
    let header = fx.ptr(entry::MESSAGE);
    lo.call_routine(fx, Routine::Fill, &[node, zero, header]);
    let current = tasks::current_task(lo, fx);
    fx.store(current, node, entry::POSTER);
    fx.store(key, node, entry::KEY);
    fx.store(keyed_post, node, entry::KEYED);
    if let Some(reply) = reply {
        fx.store(reply, node, entry::REPLY);
    }
    let at = fx.offset(node, entry::MESSAGE);
    lo.store_held(fx, at, held, message);
    append(lo, fx, record, node);
    tasks::waiting_on(lo, fx, WAITING);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    let withdraw = withdraw_post(lo);
    settle::enter_wait(lo, fx, withdraw, record, node);
    (record, node)
}

/// Ends a post that leaves its wait: it stops waiting and drops its reference to the runner.
/// The caller holds the runner's lock, which this releases.
pub fn leave_post(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let lock = fx.offset(record, runner::LOCK);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    settle::leave_wait(lo, fx);
    tasks::waiting_on(lo, fx, 0);
    unreference(lo, fx, record);
}

/// Waits until the component task takes a posted message, which completes the post. Fails, the
/// message leaving the intake, with the cancellation when the call is cancelled while it waits
/// and with `SendError.Closed` once it is removed.
pub fn await_taken(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    message: &Ty,
    record: Value,
    node: Value,
) {
    let lock = fx.offset(record, runner::LOCK);
    let at = fx.offset(node, entry::MESSAGE);
    fx.repeat(&[], |fx, _| {
        lo.call_routine(fx, Routine::Park, &[]);
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let status = fx.load(PTR, node, entry::STATUS);
        let taken = fx.b.ins().icmp_imm_s(IntCC::Equal, status, status::TAKEN);
        fx.when(taken, |fx| {
            leave_post(lo, fx, record);
            supply.done(lo, fx);
        });
        leave_untaken(lo, fx, supply, message, record, node, status, at);
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        (fx.int(types::I8, 1), Vec::new())
    });
    fx.b.ins()
        .trap(cranelift_codegen::ir::TrapCode::user(1).expect("a trap code"));
}

/// Leaves a post whose message the component task has not taken once it is removed, or once
/// the call is cancelled while the message waits in the intake, which it then leaves: the
/// message is released and the call fails with the cancellation or as closed. The caller holds
/// the runner's lock.
#[allow(clippy::too_many_arguments)]
pub fn leave_untaken(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    message: &Ty,
    record: Value,
    node: Value,
    status: Value,
    at: Value,
) {
    let removed = fx.b.ins().icmp_imm_s(IntCC::Equal, status, status::REMOVED);
    let waiting = fx.b.ins().icmp_imm_s(IntCC::Equal, status, 0);
    let origin = super::super::origin(lo, fx);
    let cancelled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, origin, 0);
    let cancelled = fx.b.ins().band(cancelled, waiting);
    let leaving = fx.b.ins().bor(removed, cancelled);
    fx.when(leaving, |fx| {
        fx.when(waiting, |fx| unlink(fx, record, node));
        leave_post(lo, fx, record);
        lo.release(fx, message, at);
        fx.when(cancelled, |fx| {
            super::super::fail_cancelled(lo, fx, supply, origin)
        });
        fail_closed(lo, fx, supply);
    });
}

/// Withdraws a stopped poster's entry from a runner's intake unless the runner has taken or
/// removed it, marks a taken one so that no reply reaches it, and drops the poster's reference
/// to the runner.
fn withdraw_post(lo: &mut Lowering) -> FuncId {
    lo.glue_function(
        Glue::WithdrawPost,
        settle::withdraw_signature(),
        |lo, fx| {
            let params = fx.entry();
            let record = fx.load(PTR, params[0], settle::wait::OBJECT);
            let node = fx.load(PTR, params[0], settle::wait::NODE);
            let lock = fx.offset(record, runner::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let status = fx.load(PTR, node, entry::STATUS);
            let waiting = fx.b.ins().icmp_imm_s(IntCC::Equal, status, 0);
            fx.when(waiting, |fx| unlink(fx, record, node));
            let taken = fx.b.ins().icmp_imm_s(IntCC::Equal, status, status::TAKEN);
            fx.when(taken, |fx| {
                let withdrawn = fx.ptr(status::WITHDRAWN as u64);
                fx.store(withdrawn, node, entry::STATUS);
            });
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            unreference(lo, fx, record);
            fx.b.ins().return_(&[]);
        },
    )
}
