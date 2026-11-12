//! Components: `host::Runner.run`, which runs a component's handlers on a task of their own
//! while a body runs, `sys::Stepper`, whose holder runs them on its own task, the operations of
//! the component context, keyed tasks and posts through outboxes.
//!
//! A runner's record, drawn through the allocator of the run or the stepper, holds its lock, the
//! intake from its oldest to its newest entry, whether the intake has stopped, the task waiting
//! for a message, the component task or the stepper's holder, and whether it waits, the task
//! waiting for the record's tasks, the delayed entries in the order they enter the intake, the
//! timers in flight and their count, the keyed tasks not ended and their count, the count
//! of what references the record, its allocator and executor cells, the number of `after` calls
//! so far, whether the component task has ended, the outbox of the run's body or of the stepper,
//! the nanosecond at which the executor's clock was minted with the executor's resolution, on
//! whose grid delays end, the request whose handler runs, the stepper's holding, zero for a run,
//! the executor's handle, the attached runner's handle, zero for none, and the index of keyed
//! tasks by key, zero before the first spawn; the state, the Env and,
//! for a protocol declaring one, the summary follow. An intake entry
//! holds its links, the task posting it (zero for a message the component sends itself), its
//! status, its deadline and order while delayed, whether a keyed outbox posted it and that
//! outbox's key, where a request's reply goes (zero for any other message), the number a holder
//! serving the request gave it and its numbering, the header of the task that queued it, and the
//! message; a
//! posted entry lies on the posting task's stack, any other is drawn through the runner's
//! allocator.

mod intake;
mod keyed;
mod keys;
mod run;
mod stepper;

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::super::body::Held;
use super::super::super::call::Invoked;
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::tables::{self, entry as cap_entry};
use super::super::super::runtime::tasks;
use super::super::super::runtime::{Routine, fetch_add, report};
use super::super::super::{Instance, Lowering};
use super::super::Supply;
use super::super::mem::lists::fn_param;
use crate::check::props::Property;
use crate::check::sig::SigKind;
use crate::check::ty::{Arg, Ty};
use crate::nir::{Callable, FnRef, Role};

pub use intake::{
    await_taken, enqueue, fail_closed, give_posted, leave_post, leave_untaken, timer_completed,
};

pub mod runner {
    pub const LOCK: u64 = 0;
    pub const OLDEST: u64 = 8;
    pub const NEWEST: u64 = 16;
    pub const STOPPED: u64 = 24;
    pub const COMPONENT: u64 = 32;
    pub const IDLE: u64 = 40;
    pub const WAITER: u64 = 48;
    pub const DELAYED: u64 = 56;
    pub const TIMERS: u64 = 64;
    pub const TIMER_COUNT: u64 = 72;
    pub const KEYED: u64 = 80;
    pub const LIVE_KEYED: u64 = 88;
    pub const REFS: u64 = 96;
    pub const CELL: u64 = 104;
    pub const EXECUTOR: u64 = 112;
    pub const ORDER: u64 = 120;
    pub const ENDED: u64 = 128;
    pub const INBOX: u64 = 136;
    pub const ORIGIN: u64 = 144;
    pub const RESOLUTION: u64 = 152;
    pub const CURRENT: u64 = 160;
    pub const HOLDING: u64 = 168;
    /// The handle of the executor its tasks run on.
    pub const HANDLE: u64 = 176;
    /// The handle of the runner a stepper's holder attached, zero for none.
    pub const RUNNER: u64 = 184;
    pub const KEYS: u64 = 192;
    pub const HEADER: u64 = 200;
}

pub mod entry {
    pub const NEXT: u64 = 0;
    pub const PREVIOUS: u64 = 8;
    pub const POSTER: u64 = 16;
    pub const STATUS: u64 = 24;
    pub const DEADLINE: u64 = 32;
    pub const ORDER: u64 = 40;
    pub const KEY: u64 = 48;
    pub const KEYED: u64 = 56;
    pub const REPLY: u64 = 64;
    pub const NUMBER: u64 = 72;
    pub const NUMBERING: u64 = 80;
    pub const HEADER: u64 = 88;
    pub const MESSAGE: u64 = 128;
}

/// The numberings of a request's entry: none, numbered by the holder serving it, or numbered and
/// postponed by its handler.
pub mod numbering {
    pub const NUMBERED: i64 = 1;
    pub const POSTPONED: i64 = 2;
}

/// The statuses of a posted entry: taken by the component task, removed from the intake untaken,
/// a request's handler having returned or failed, its poster stopped, the component task
/// stopped while its handler ran, or its handler having postponed it.
pub mod status {
    pub const TAKEN: i64 = 1;
    pub const REMOVED: i64 = 2;
    pub const REPLIED: i64 = 3;
    pub const FAILED: i64 = 4;
    pub const WITHDRAWN: i64 = 5;
    pub const ABANDONED: i64 = 6;
    pub const POSTPONED: i64 = 7;
}

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    match supply.path.as_str() {
        "host::Runner.run" => run::run(lo, fx, supply),
        "sys::Cx.send" => {
            let runner = fx.load(PTR, supply.param(0).address(), 0);
            intake::send_now(lo, fx, supply, runner, supply.param(1));
        }
        "sys::Cx.after" | "sys::Cx.after_ns" => intake::after(lo, fx, supply),
        "sys::Cx.spawn" => keyed::spawn(lo, fx, supply),
        "sys::current_header" => current_header(lo, fx, supply),
        "sys::run_headed" => run_headed(lo, fx, supply),
        "sys::Cx.cancel" => {
            let runner = fx.load(PTR, supply.param(0).address(), 0);
            let lock = fx.offset(runner, runner::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            keyed::supersede_key(lo, fx, runner, supply.param(1).scalar());
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            supply.done(lo, fx);
        }
        "sys::Cx.postpone" => super::protocols::postpone(lo, fx, supply),
        "sys::Cx.runner" => {
            let record = fx.load(PTR, supply.param(0).address(), 0);
            let attached = fx.load(PTR, record, runner::RUNNER);
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, attached, 0);
            let handle = fx.choose_in(
                lo,
                present,
                &[PTR],
                |_, _| vec![attached],
                |lo, fx| vec![tables::denied(lo, fx)],
            )[0];
            let sret = supply.sret.expect("a runner's address");
            fx.store(handle, sret, 0);
            supply.done(lo, fx);
        }
        "sys::Outbox.post" => {
            let message = supply.type_arg(0);
            let (record, node) = enqueue(lo, fx, supply, &message, supply.param(1), None, None);
            await_taken(lo, fx, supply, &message, record, node);
        }
        _ => stepper::lower(lo, fx, supply),
    }
}

/// Where a runner's record holds the summary, the state and the Env, and its size and
/// alignment.
pub struct Shape {
    pub state_at: u64,
    pub env_at: u64,
    pub summary_at: Option<(u64, Ty)>,
    size: u64,
    align: u64,
}

pub fn shape(lo: &mut Lowering, state: &Ty, env: &Ty, message: &Ty) -> Shape {
    let summary = super::protocols::summary_type(lo, message).map(|summary| {
        let at = super::protocols::summary_offset(lo, &summary);
        (at, summary)
    });
    let mut end = runner::HEADER;
    let mut align = 8;
    if let Some((at, summary)) = &summary {
        let layout = lo.layout(summary);
        end = at + layout.size;
        align = align.max(layout.align);
    }
    let state_layout = lo.layout(state);
    let env_layout = lo.layout(env);
    let state_at = end.next_multiple_of(state_layout.align.max(1));
    let env_at = (state_at + state_layout.size).next_multiple_of(env_layout.align.max(1));
    Shape {
        state_at,
        env_at,
        summary_at: summary,
        size: env_at + env_layout.size,
        align: align.max(state_layout.align).max(env_layout.align),
    }
}

/// The instance of a handler of a component state for its Env and messages: a method of
/// `sys::Component` or of a system interface, whose name in `sys` is `owner`.
fn handler(
    lo: &mut Lowering,
    owner: &str,
    name: &str,
    state: &Ty,
    env: &Ty,
    message: &Ty,
) -> Instance {
    let interface = lo
        .ck
        .db
        .std_item("sys", owner)
        .expect("a component interface of sys");
    let method = lo
        .ck
        .interface_method(interface, name)
        .expect("a component handler") as u32;
    let args = vec![
        Arg::Ty(state.clone()),
        Arg::Ty(env.clone()),
        Arg::Ty(message.clone()),
    ];
    let fn_ref = FnRef {
        callable: Callable::Method { interface, method },
        args: args.into(),
    };
    lo.callable_instance(&fn_ref, false)
}

/// Calls an instance with values held as its parameters take them.
pub fn call_handler(lo: &mut Lowering, fx: &mut Fx, instance: Instance, args: &[Held]) -> Held {
    let abi = lo.abi(&instance);
    let id = lo.instance(instance);
    lo.invoke(fx, Invoked::Direct(id), &abi, args).result
}

/// Releases a run's Env: through its type's abandon method when the Env is linear, as its end of
/// scope does otherwise.
fn release_env(lo: &mut Lowering, fx: &mut Fx, env: &Ty, held: Held) {
    if lo.ck.has(env, Property::Linear) {
        let abandon = lo
            .ck
            .abandon_method(env)
            .expect("a linear Env's abandon method");
        let fn_ref = FnRef {
            callable: Callable::Item {
                item: abandon,
                role: Role::Body,
                index: 0,
            },
            args: env.args().into(),
        };
        let instance = lo.callable_instance(&fn_ref, false);
        call_handler(lo, fx, instance, &[held]);
    } else if let Held::Address(address) = held {
        lo.release(fx, env, address);
    }
}

/// Wakes the task waiting for a run's tasks, if any. The caller holds the lock.
fn wake_waiter(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let waiter = fx.load(PTR, record, runner::WAITER);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, waiter, 0);
    fx.when(present, |fx| {
        lo.call_routine(fx, Routine::Wake, &[waiter]);
    });
}

/// Waits until a condition read under a runner's lock holds; whatever wakes the waiter
/// changes what the condition reads.
fn wait_for(lo: &mut Lowering, fx: &mut Fx, record: Value, condition: impl Fn(&mut Fx) -> Value) {
    let lock = fx.offset(record, runner::LOCK);
    let current = tasks::current_task(lo, fx);
    fx.repeat(&[], |fx, _| {
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let holds = condition(fx);
        let waiting = fx.b.ins().bxor_imm_s(holds, 1);
        let zero = fx.ptr(0);
        let waiter = fx.b.ins().select(waiting, current, zero);
        fx.store(waiter, record, runner::WAITER);
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        fx.when(waiting, |fx| {
            lo.call_routine(fx, Routine::Park, &[]);
        });
        (waiting, Vec::new())
    });
}

/// Adds a reference to a runner's record.
fn reference(fx: &mut Fx, record: Value) {
    let refs = fx.offset(record, runner::REFS);
    let one = fx.ptr(1);
    fetch_add(fx, refs, one);
}

/// Drops one reference to a runner's record, freeing it with the last.
pub fn unreference(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let refs = fx.offset(record, runner::REFS);
    let minus = fx.ptr(u64::MAX);
    let previous = fetch_add(fx, refs, minus);
    let last = fx.b.ins().icmp_imm_s(IntCC::Equal, previous, 1);
    fx.when(last, |fx| {
        lo.call_routine(fx, Routine::Free, &[record]);
    });
}

/// A new runner's record drawn through an allocator's cell, trapping when it is exhausted, with
/// its header cleared but for the reference its drawer holds, the cells of the allocator and the
/// executor, the executor's handle, and the origin and resolution of the executor's clock, given
/// its handle.
#[allow(clippy::too_many_arguments)]
fn draw_record(
    lo: &mut Lowering,
    fx: &mut Fx,
    shape: &Shape,
    cell: Value,
    exec: Value,
    exec_cell: Value,
    clock: Value,
    resolution: Value,
) -> Value {
    let size = fx.ptr(shape.size);
    let align = fx.ptr(shape.align);
    let record = lo.call_routine(fx, Routine::Alloc, &[cell, size, align])[0];
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
    fx.when(missing, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
    let zero = fx.ptr(0);
    let header = fx.ptr(runner::HEADER);
    lo.call_routine(fx, Routine::Fill, &[record, zero, header]);
    let one = fx.ptr(1);
    fx.store(one, record, runner::REFS);
    fx.store(cell, record, runner::CELL);
    fx.store(exec_cell, record, runner::EXECUTOR);
    fx.store(exec, record, runner::HANDLE);
    fx.store(resolution, record, runner::RESOLUTION);
    let clock = tables::entry_at(lo, fx, clock);
    let origin = fx.load(PTR, clock, cap_entry::STATE);
    fx.store(origin, record, runner::ORIGIN);
    record
}

/// Removes the oldest entry of a runner's intake and handles its message on the running task:
/// a one-way post completes, a request's entry awaits its handler's reply, and the entry of a
/// message the component sent is freed. The caller holds the lock, which this releases. With a
/// `sys::Handled` type and an address, it writes there what it handles.
#[allow(clippy::too_many_arguments)]
fn handle_oldest(
    lo: &mut Lowering,
    fx: &mut Fx,
    record: Value,
    oldest: Value,
    state: &Ty,
    env: &Ty,
    message: &Ty,
    handled: Option<(&Ty, Value)>,
) {
    let layout = lo.layout(message);
    let slot = fx.slot(layout.size, layout.align);
    intake::unlink(fx, record, oldest);
    let source = fx.offset(oldest, entry::MESSAGE);
    lo.copy(fx, slot, source, layout.size, layout.align);
    let poster = fx.load(PTR, oldest, entry::POSTER);
    let posted = fx.b.ins().icmp_imm_s(IntCC::NotEqual, poster, 0);
    let reply = fx.load(PTR, oldest, entry::REPLY);
    let request = fx.b.ins().icmp_imm_s(IntCC::NotEqual, reply, 0);
    fx.when(posted, |fx| {
        intake::take_posted(lo, fx, message, slot, poster);
        let taken = fx.ptr(status::TAKEN as u64);
        fx.store(taken, oldest, entry::STATUS);
        let one_way = fx.b.ins().bxor_imm_s(request, 1);
        fx.when(one_way, |fx| {
            lo.call_routine(fx, Routine::Wake, &[poster]);
        });
    });
    fx.when(request, |fx| fx.store(oldest, record, runner::CURRENT));
    let header = fx.slot(tasks::HEADER_BLOCK, 8);
    let queued = fx.offset(oldest, entry::HEADER);
    lo.copy(fx, header, queued, tasks::HEADER_BLOCK, 8);
    if let Some((handled, at)) = handled {
        write_handled(lo, fx, (handled, at), message, (slot, header));
    }
    let lock = fx.offset(record, runner::LOCK);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    let sent = fx.b.ins().bxor_imm_s(posted, 1);
    fx.when(sent, |fx| {
        lo.call_routine(fx, Routine::Free, &[oldest]);
    });
    handle(
        lo,
        fx,
        record,
        state,
        env,
        message,
        (slot, header),
        Some(oldest),
    );
}

/// Writes the `sys::Handled` of a message in a slot handled under the header of a block: that
/// header, and the index of the message's variant for an enum or a protocol, 0 for any other
/// message type.
fn write_handled(
    lo: &mut Lowering,
    fx: &mut Fx,
    (handled, at): (&Ty, Value),
    message: &Ty,
    (slot, header): (Value, Value),
) {
    let header_field = lo.layouts.field(lo.ck, handled, None, "header");
    let operation_field = lo.layouts.field(lo.ck, handled, None, "operation");
    let header_at = fx.offset(at, header_field.offset);
    write_header(lo, fx, &header_field.ty, header_at, header);
    let variants = matches!(message, Ty::Item(item, _) if matches!(
        lo.ck.sig(*item).kind,
        SigKind::Enum { .. } | SigKind::Protocol { .. }
    ));
    let operation = if variants {
        let index = lo.variant_index(fx, message, slot);
        fx.b.ins().ireduce(types::I16, index)
    } else {
        fx.int(types::I16, 0)
    };
    fx.store(operation, at, operation_field.offset);
}

/// Writes at `at` the `Option[sys::Header]` of type `option` that a header block holds.
fn write_header(lo: &mut Lowering, fx: &mut Fx, option: &Ty, at: Value, block: Value) {
    let flag = fx.load(PTR, block, 0);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, flag, 0);
    lo.write_option_with(fx, option, at, present, |lo, fx, payload| {
        let bytes = fx.offset(block, 8);
        lo.copy(fx, payload, bytes, HEADER_BYTES, 1);
    });
}

/// Removes a stepper's oldest message that its component sent itself, from its intake or else
/// from its delayed messages, and writes at `at` the `sys::Sent` of type `sent` holding it, its
/// header and the nanoseconds left before it would enter the intake. The caller holds the lock
/// and has found `node`, in the intake when `in_intake` holds.
fn take_sent(
    lo: &mut Lowering,
    fx: &mut Fx,
    record: Value,
    (node, in_intake): (Value, Value),
    message: &Ty,
    (sent, at): (&Ty, Value),
) {
    fx.choose(
        in_intake,
        &[],
        |fx| {
            intake::unlink(fx, record, node);
            Vec::new()
        },
        |fx| {
            let next = fx.load(PTR, node, entry::NEXT);
            fx.store(next, record, runner::DELAYED);
            Vec::new()
        },
    );
    let message_field = lo.layouts.field(lo.ck, sent, None, "message");
    let header_field = lo.layouts.field(lo.ck, sent, None, "header");
    let delay_field = lo.layouts.field(lo.ck, sent, None, "delay_ns");
    let layout = lo.layout(message);
    let message_at = fx.offset(at, message_field.offset);
    let source = fx.offset(node, entry::MESSAGE);
    lo.copy(fx, message_at, source, layout.size, layout.align);
    let header_at = fx.offset(at, header_field.offset);
    let block = fx.offset(node, entry::HEADER);
    write_header(lo, fx, &header_field.ty, header_at, block);
    let delay = fx.choose_in(
        lo,
        in_intake,
        &[PTR],
        |_, fx| vec![fx.ptr(0)],
        |lo, fx| {
            let deadline = fx.load(PTR, node, entry::DEADLINE);
            let now = lo.now_ns(fx);
            let later = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, deadline, now);
            let left = fx.b.ins().isub(deadline, now);
            let zero = fx.ptr(0);
            vec![fx.b.ins().select(later, left, zero)]
        },
    )[0];
    fx.store(delay, at, delay_field.offset);
    lo.call_routine(fx, Routine::Free, &[node]);
}

/// The oldest entry of a runner's intake that its component sent itself, zero for none. The
/// caller holds the lock.
fn oldest_sent(fx: &mut Fx, record: Value) -> Value {
    let first = fx.load(PTR, record, runner::OLDEST);
    fx.repeat(&[first], |fx, state| {
        let node = state[0];
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, node, 0);
        let moved = fx.choose(
            present,
            &[types::I8, PTR],
            |fx| {
                let poster = fx.load(PTR, node, entry::POSTER);
                let posted = fx.b.ins().icmp_imm_s(IntCC::NotEqual, poster, 0);
                let next = fx.load(PTR, node, entry::NEXT);
                vec![posted, fx.b.ins().select(posted, next, node)]
            },
            |fx| vec![fx.int(types::I8, 0), node],
        );
        (moved[0], vec![moved[1]])
    })[0]
}

/// Passes the message in a slot to `on`, or to its operation's handler for a protocol, with the
/// state and the Env a runner's record holds, then publishes the summary; a request's reply goes
/// to the entry `node` that posted it. The running task has the header of a block while the
/// handler runs.
#[allow(clippy::too_many_arguments)]
fn handle(
    lo: &mut Lowering,
    fx: &mut Fx,
    record: Value,
    state: &Ty,
    env: &Ty,
    message: &Ty,
    (slot, header): (Value, Value),
    node: Option<Value>,
) {
    let current = tasks::current_task(lo, fx);
    let own = fx.offset(current, tasks::task::HEADER);
    let saved = fx.slot(tasks::HEADER_BLOCK, 8);
    lo.copy(fx, saved, own, tasks::HEADER_BLOCK, 8);
    lo.copy(fx, own, header, tasks::HEADER_BLOCK, 8);
    let shape = shape(lo, state, env, message);
    let state_at = fx.offset(record, shape.state_at);
    let env_at = fx.offset(record, shape.env_at);
    let cx = fx.slot(8, 8);
    fx.store(record, cx, 0);
    let args = [
        Held::Address(state_at),
        Held::Address(slot),
        Held::Address(env_at),
        Held::Address(cx),
    ];
    match super::protocols::served_protocol(lo, message) {
        Some(protocol) => {
            super::protocols::dispatch(lo, fx, record, protocol, state, &args, node);
        }
        None => {
            let on = handler(lo, "Component", "on", state, env, message);
            call_handler(lo, fx, on, &args);
        }
    }
    super::protocols::publish(lo, fx, record, &shape, state);
    lo.copy(fx, own, saved, tasks::HEADER_BLOCK, 8);
}

/// `sys::current_header()`: the running task's header, none outside a task or for a task without one.
fn current_header(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let option = supply.abi.result.clone();
    let layout = lo.layout(&option);
    let result = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    let current = tasks::current_task(lo, fx);
    let inside = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
    let present = fx.choose(
        inside,
        &[types::I8],
        |fx| {
            let held = fx.load(PTR, current, tasks::task::HEADER);
            vec![fx.b.ins().icmp_imm_s(IntCC::NotEqual, held, 0)]
        },
        |fx| vec![fx.int(types::I8, 0)],
    )[0];
    lo.write_option_with(fx, &option, result, present, |lo, fx, payload| {
        let bytes = fx.offset(current, tasks::task::HEADER + 8);
        lo.copy(fx, payload, bytes, HEADER_BYTES, 1);
    });
    let held = lo.held_at(fx, &option, result);
    supply.ret(lo, fx, held);
}

/// `sys::run_headed(header, body: b)`: calls `b` with `header` as the running task's header,
/// then the task's own again.
fn run_headed(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let current = tasks::current_task(lo, fx);
    let own = fx.offset(current, tasks::task::HEADER);
    let saved = fx.slot(tasks::HEADER_BLOCK, 8);
    lo.copy(fx, saved, own, tasks::HEADER_BLOCK, 8);
    let one = fx.ptr(1);
    fx.store(one, own, 0);
    let bytes = fx.offset(own, 8);
    lo.copy(fx, bytes, supply.param(0).address(), HEADER_BYTES, 1);
    let body_ty = fn_param(supply, 1);
    let called = lo.call_value(fx, &body_ty, supply.param(1).address(), &[]);
    lo.copy(fx, own, saved, tasks::HEADER_BLOCK, 8);
    supply.forward_failure(lo, fx, &called);
    supply.ret(lo, fx, called.result);
}

/// The bytes of a `sys::Header`.
pub const HEADER_BYTES: u64 = 32;

/// Settles the record of a run whose caller has stopped or of a stepper whose domain is
/// abandoned, the tasks of that domain having stopped: its intake stops, each posted entry is
/// removed and its poster woken, the poster of the request whose handler ran learns that it
/// stopped, its timers are cancelled and waited for, its outboxes become stale, and the
/// references of its holder and of its stopped keyed tasks are dropped.
pub fn withdraw(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let lock = fx.offset(record, runner::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let one = fx.ptr(1);
    fx.store(one, record, runner::STOPPED);
    let current = fx.load(PTR, record, runner::CURRENT);
    let handling = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
    fx.when(handling, |fx| {
        let abandoned = fx.ptr(status::ABANDONED as u64);
        fx.store(abandoned, current, entry::STATUS);
        let poster = fx.load(PTR, current, entry::POSTER);
        lo.call_routine(fx, Routine::Wake, &[poster]);
    });
    let zero = fx.ptr(0);
    for field in [
        runner::COMPONENT,
        runner::IDLE,
        runner::WAITER,
        runner::CURRENT,
    ] {
        fx.store(zero, record, field);
    }
    let oldest = fx.load(PTR, record, runner::OLDEST);
    fx.repeat(&[oldest], |fx, state| {
        let node = state[0];
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, node, 0);
        let next = fx.choose_in(
            lo,
            present,
            &[PTR],
            |lo, fx| {
                let next = fx.load(PTR, node, entry::NEXT);
                let poster = fx.load(PTR, node, entry::POSTER);
                let posted = fx.b.ins().icmp_imm_s(IntCC::NotEqual, poster, 0);
                fx.when(posted, |fx| {
                    intake::unlink(fx, record, node);
                    let removed = fx.ptr(status::REMOVED as u64);
                    fx.store(removed, node, entry::STATUS);
                    lo.call_routine(fx, Routine::Wake, &[poster]);
                });
                vec![next]
            },
            |_, fx| vec![fx.ptr(0)],
        );
        (present, next)
    });
    let claimed = intake::cancel_timers(lo, fx, record);
    let stopped = fx.load(PTR, record, runner::LIVE_KEYED);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    intake::complete_claimed(lo, fx, claimed);
    fx.repeat(&[], |fx, _| {
        lo.reap(fx);
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let timers = fx.load(PTR, record, runner::TIMER_COUNT);
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        let pending = fx.b.ins().icmp_imm_s(IntCC::NotEqual, timers, 0);
        fx.when(pending, |fx| {
            lo.relax(fx);
        });
        (pending, Vec::new())
    });
    tables::retire_outboxes(lo, fx, record);
    let held = fx.b.ins().iadd_imm_s(stopped, 1);
    let minus = fx.b.ins().ineg(held);
    let refs = fx.offset(record, runner::REFS);
    let previous = fetch_add(fx, refs, minus);
    let last = fx.b.ins().icmp(IntCC::Equal, previous, held);
    fx.when(last, |fx| {
        lo.call_routine(fx, Routine::Free, &[record]);
    });
}
