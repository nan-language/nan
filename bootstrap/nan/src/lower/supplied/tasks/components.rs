//! Components: `host::Runner.run`, which runs a component's handlers on a task of their own
//! while a body runs, the operations of the component context, keyed tasks and posts through
//! outboxes.
//!
//! A runner's record, drawn through the run's allocator, holds its lock, the intake from its
//! oldest to its newest entry, whether the intake has stopped, the component task and whether it
//! waits for a message, the task waiting for the run's tasks, the delayed entries in the order
//! they enter the intake, the timers in flight and their count, the keyed tasks and the count of
//! those not ended, the count of what references the record, its allocator and executor cells,
//! the number of `after` calls so far, whether the component task has ended, the body's outbox,
//! the nanosecond at which the executor's clock was minted with the executor's resolution, on
//! whose grid delays end, and the request whose handler runs; the state, the Env and, for a
//! protocol declaring one, the summary follow. An intake entry holds its links, the task posting
//! it (zero for a message the component sends itself), its status, its deadline and order while
//! delayed, whether a keyed outbox posted it and that outbox's key, where a request's reply goes
//! (zero for any other message), and the message; a posted entry lies on the posting task's
//! stack, any other is drawn through the runner's allocator. A
//! keyed task's node holds its links, its task, key, outbox and region, whether its key was
//! superseded and whether it has ended.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};
use cranelift_module::FuncId;

use super::super::super::body::Held;
use super::super::super::call::Invoked;
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::regions::{WAITING, region};
use super::super::super::runtime::tables::{self, entry as cap_entry};
use super::super::super::runtime::tasks::{self, kind, operation, task};
use super::super::super::runtime::{Routine, fetch_add, report, settle, timers};
use super::super::super::{Glue, Instance, Lowering};
use super::super::Supply;
use super::super::mem::lists::fn_param;
use super::scopes::start_task;
use crate::check::props::Property;
use crate::check::ty::{Arg, Ty};
use crate::nir::{Callable, FnRef, Role};

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
    pub const HEADER: u64 = 168;
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
    pub const MESSAGE: u64 = 72;
}

/// The statuses of a posted entry: taken by the component task, removed from the intake untaken,
/// a request's handler having returned or failed, its poster stopped, or the component task
/// stopped while its handler ran.
pub mod status {
    pub const TAKEN: i64 = 1;
    pub const REMOVED: i64 = 2;
    pub const REPLIED: i64 = 3;
    pub const FAILED: i64 = 4;
    pub const WITHDRAWN: i64 = 5;
    pub const ABANDONED: i64 = 6;
}

mod keyed {
    pub const NEXT: u64 = 0;
    pub const PREVIOUS: u64 = 8;
    pub const TASK: u64 = 16;
    pub const KEY: u64 = 24;
    pub const OUTBOX: u64 = 32;
    pub const REGION: u64 = 40;
    pub const SUPERSEDED: u64 = 48;
    pub const ENDED: u64 = 56;
    pub const RUNNER: u64 = 64;
    pub const SIZE: u64 = 72;
}

/// The fields of a delayed message's timer after its operation record: its links in its
/// runner's timers and its relative time.
mod timer {
    use super::operation;
    pub const NEXT: u64 = operation::SIZE;
    pub const PREVIOUS: u64 = operation::SIZE + 8;
    pub const SPEC: u64 = operation::SIZE + 16;
    pub const SIZE: u64 = operation::SIZE + 32;
}

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    match supply.path.as_str() {
        "host::Runner.run" => run(lo, fx, supply),
        "sys::Cx.send" => {
            let runner = fx.load(PTR, supply.param(0).address(), 0);
            send_now(lo, fx, supply, runner, supply.param(1));
        }
        "sys::Cx.after" | "sys::Cx.after_ns" => after(lo, fx, supply),
        "sys::Cx.spawn" => spawn(lo, fx, supply),
        "sys::Cx.cancel" => {
            let runner = fx.load(PTR, supply.param(0).address(), 0);
            let lock = fx.offset(runner, runner::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            supersede_key(lo, fx, runner, supply.param(1).scalar());
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            supply.done(lo, fx);
        }
        "sys::Outbox.post" => {
            let message = supply.type_arg(0);
            let (record, node) = enqueue(lo, fx, supply, &message, supply.param(1), None);
            await_taken(lo, fx, supply, &message, record, node);
        }
        other => panic!("no lowering of {other}"),
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

/// The instance of a handler, `start` or `on`, of a component state for its Env and messages.
fn handler(lo: &mut Lowering, name: &str, state: &Ty, env: &Ty, message: &Ty) -> Instance {
    let interface = lo
        .ck
        .db
        .std_item("sys", "Component")
        .expect("the interface sys::Component");
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

/// `host::Runner[S].run(env, exec, alloc, body: b)`.
fn run(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let state_ty = supply.type_arg(0);
    let env_ty = supply.type_arg(1);
    let message_ty = supply.type_arg(2);
    let body_ty = fn_param(supply, 3);
    let env = supply.param(0);
    let (live, [exec_cell, clock, resolution, _]) =
        super::executor(lo, fx, supply.param(1).address());
    let stale = fx.b.ins().bxor_imm_s(live, 1);
    fx.when(stale, |fx| {
        release_env(lo, fx, &env_ty, env);
        let none = fx.ptr(0);
        super::fail_cancelled(lo, fx, supply, none);
    });
    let cell = fx.load(PTR, supply.param(2).address(), 0);
    let shape = shape(lo, &state_ty, &env_ty, &message_ty);
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
    fx.store(resolution, record, runner::RESOLUTION);
    let clock = tables::entry_at(lo, fx, clock);
    let origin = fx.load(PTR, clock, cap_entry::STATE);
    fx.store(origin, record, runner::ORIGIN);
    let withdraw = withdraw_run(lo);
    let none = fx.ptr(0);
    settle::enter_wait(lo, fx, withdraw, record, none);
    let env_at = fx.offset(record, shape.env_at);
    lo.store_held(fx, env_at, env, &env_ty);
    let start = handler(lo, "start", &state_ty, &env_ty, &message_ty);
    let state = call_handler(lo, fx, start, &[Held::Address(env_at)]);
    let state_at = fx.offset(record, shape.state_at);
    lo.store_held(fx, state_at, state, &state_ty);
    super::protocols::publish(lo, fx, record, &shape, &state_ty);
    start_component(lo, fx, record, exec_cell, &state_ty, &env_ty, &message_ty);
    let keyed = fx.ptr(0);
    let inbox = lo.call_routine(fx, Routine::OutboxMint, &[record, zero, keyed])[0];
    fx.store(inbox, record, runner::INBOX);
    let inbox_slot = fx.slot(8, 8);
    fx.store(inbox, inbox_slot, 0);
    let called = lo.call_value(
        fx,
        &body_ty,
        supply.param(3).address(),
        &[Held::Address(inbox_slot)],
    );
    stop(lo, fx, record, &message_ty);
    wait_for(lo, fx, record, |fx| {
        let ended = fx.load(PTR, record, runner::ENDED);
        let ended = fx.b.ins().icmp_imm_s(IntCC::NotEqual, ended, 0);
        let timers = fx.load(PTR, record, runner::TIMER_COUNT);
        let settled = fx.b.ins().icmp_imm_s(IntCC::Equal, timers, 0);
        fx.b.ins().band(ended, settled)
    });
    let lock = fx.offset(record, runner::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    each_keyed(lo, fx, record, |lo, fx, node| {
        let superseded = fx.load(PTR, node, keyed::SUPERSEDED);
        let live = fx.b.ins().icmp_imm_s(IntCC::Equal, superseded, 0);
        fx.when(live, |fx| supersede_node(lo, fx, node));
    });
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    wait_for(lo, fx, record, |fx| {
        let live = fx.load(PTR, record, runner::LIVE_KEYED);
        fx.b.ins().icmp_imm_s(IntCC::Equal, live, 0)
    });
    lo.call_routine(fx, Routine::OutboxRetire, &[inbox]);
    let state_layout = lo.layout(&state_ty);
    let result = supply
        .sret
        .unwrap_or_else(|| fx.slot(state_layout.size, state_layout.align));
    if let (Some(failed), Some(failure), Some(fails)) =
        (called.failed, called.failure, body_ty.fails.clone())
    {
        fx.when(failed, |fx| {
            lo.release(fx, &state_ty, state_at);
            settle::leave_wait(lo, fx);
            unreference(lo, fx, record);
            let held = lo.held_at(fx, &fails, failure);
            let target = supply.failure.expect("a failure address");
            lo.store_held(fx, target, held, &fails);
            supply.fail(fx);
        });
    }
    lo.copy(fx, result, state_at, state_layout.size, state_layout.align);
    settle::leave_wait(lo, fx);
    unreference(lo, fx, record);
    let held = lo.held_at(fx, &state_ty, result);
    supply.ret(lo, fx, held);
}

/// Starts the component task of a runner on its executor, in the running task's domain and
/// region.
fn start_component(
    lo: &mut Lowering,
    fx: &mut Fx,
    record: Value,
    exec_cell: Value,
    state: &Ty,
    env: &Ty,
    message: &Ty,
) {
    let glue = component_glue(lo, state, env, message);
    let glue = lo.func_ref(fx, glue);
    let glue = fx.b.ins().func_addr(PTR, glue);
    let zero = fx.ptr(0);
    let stack = fx.ptr(tasks::STACK);
    let domain = tasks::current_domain(lo, fx);
    let current = tasks::current_task(lo, fx);
    let region = fx.load(PTR, current, task::REGION);
    let component = lo.call_routine(
        fx,
        Routine::Spawn,
        &[glue, zero, record, zero, stack, exec_cell, domain, region],
    )[0];
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, component, 0);
    fx.when(missing, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
    let done = component_done(lo);
    let done = lo.func_ref(fx, done);
    let done = fx.b.ins().func_addr(PTR, done);
    fx.store(done, component, task::DONE);
    fx.store(component, record, runner::COMPONENT);
    tasks::spawned(lo, fx, component);
}

/// The component task: removes the intake's messages one at a time, oldest first, passing each
/// to `on`, or to its operation's handler for a protocol, and publishing the summary after, until
/// the intake has stopped and is empty; then releases the Env.
fn component_glue(lo: &mut Lowering, state: &Ty, env: &Ty, message: &Ty) -> FuncId {
    let glue = Glue::Component(state.clone(), env.clone(), message.clone());
    let (state, env, message) = (state.clone(), env.clone(), message.clone());
    lo.glue_function(glue, super::record_signature(), move |lo, fx| {
        let params = fx.entry();
        let record = fx.load(PTR, params[0], task::ENV);
        let shape = shape(lo, &state, &env, &message);
        let state_at = fx.offset(record, shape.state_at);
        let env_at = fx.offset(record, shape.env_at);
        let layout = lo.layout(&message);
        let slot = fx.slot(layout.size, layout.align);
        let cx = fx.slot(8, 8);
        fx.store(record, cx, 0);
        let served = super::protocols::served_protocol(lo, &message);
        let on = served
            .is_none()
            .then(|| handler(lo, "on", &state, &env, &message));
        let lock = fx.offset(record, runner::LOCK);
        fx.repeat(&[], |fx, _| {
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let oldest = fx.load(PTR, record, runner::OLDEST);
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, oldest, 0);
            let on = on.clone();
            let again = fx.choose_in(
                lo,
                present,
                &[types::I8],
                |lo, fx| {
                    unlink(fx, record, oldest);
                    let source = fx.offset(oldest, entry::MESSAGE);
                    lo.copy(fx, slot, source, layout.size, layout.align);
                    let poster = fx.load(PTR, oldest, entry::POSTER);
                    let posted = fx.b.ins().icmp_imm_s(IntCC::NotEqual, poster, 0);
                    let reply = fx.load(PTR, oldest, entry::REPLY);
                    let request = fx.b.ins().icmp_imm_s(IntCC::NotEqual, reply, 0);
                    fx.when(posted, |fx| {
                        take_posted(lo, fx, &message, slot, poster);
                        let taken = fx.ptr(status::TAKEN as u64);
                        fx.store(taken, oldest, entry::STATUS);
                        let one_way = fx.b.ins().bxor_imm_s(request, 1);
                        fx.when(one_way, |fx| {
                            lo.call_routine(fx, Routine::Wake, &[poster]);
                        });
                    });
                    fx.when(request, |fx| fx.store(oldest, record, runner::CURRENT));
                    lo.call_routine(fx, Routine::Unlock, &[lock]);
                    let sent = fx.b.ins().bxor_imm_s(posted, 1);
                    fx.when(sent, |fx| {
                        lo.call_routine(fx, Routine::Free, &[oldest]);
                    });
                    let args = [
                        Held::Address(state_at),
                        Held::Address(slot),
                        Held::Address(env_at),
                        Held::Address(cx),
                    ];
                    match (served, on.clone()) {
                        (Some(protocol), _) => super::protocols::dispatch(
                            lo, fx, record, protocol, &state, &args, oldest,
                        ),
                        (None, Some(on)) => {
                            call_handler(lo, fx, on, &args);
                        }
                        (None, None) => unreachable!("a component's on handler"),
                    }
                    super::protocols::publish(lo, fx, record, &shape, &state);
                    vec![fx.int(types::I8, 1)]
                },
                |lo, fx| {
                    let stopped = fx.load(PTR, record, runner::STOPPED);
                    let open = fx.b.ins().icmp_imm_s(IntCC::Equal, stopped, 0);
                    fx.when(open, |fx| {
                        let one = fx.ptr(1);
                        fx.store(one, record, runner::IDLE);
                    });
                    lo.call_routine(fx, Routine::Unlock, &[lock]);
                    fx.when(open, |fx| {
                        lo.call_routine(fx, Routine::Park, &[]);
                    });
                    vec![open]
                },
            )[0];
            (again, Vec::new())
        });
        release_env(lo, fx, &env, Held::Address(env_at));
        fx.b.ins().return_(&[]);
    })
}

/// What the executor thread does once a component task has ended: the run's caller wakes and
/// the task's record is freed.
fn component_done(lo: &mut Lowering) -> FuncId {
    lo.glue_function(Glue::ComponentDone, super::record_signature(), |lo, fx| {
        let params = fx.entry();
        let component = params[0];
        let record = fx.load(PTR, component, task::ENV);
        let lock = fx.offset(record, runner::LOCK);
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let one = fx.ptr(1);
        fx.store(one, record, runner::ENDED);
        wake_waiter(lo, fx, record);
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        lo.record_free(fx, component);
        fx.b.ins().return_(&[]);
    })
}

/// Wakes the task waiting for a run's tasks, if any. The caller holds the lock.
fn wake_waiter(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let waiter = fx.load(PTR, record, runner::WAITER);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, waiter, 0);
    fx.when(present, |fx| {
        lo.call_routine(fx, Routine::Wake, &[waiter]);
    });
}

/// Settles a runner whose run a stopped task makes: its intake stops, each posted entry is
/// removed and its poster woken, its stopped tasks are forgotten, its timers are cancelled and
/// waited for, its outboxes become stale, and the run's reference to it is dropped.
fn withdraw_run(lo: &mut Lowering) -> FuncId {
    lo.glue_function(Glue::WithdrawRun, settle::withdraw_signature(), |lo, fx| {
        let params = fx.entry();
        let record = fx.load(PTR, params[0], settle::wait::OBJECT);
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
                        unlink(fx, record, node);
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
        let first = fx.load(PTR, record, runner::TIMERS);
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
        lo.call_routine(fx, Routine::Unlock, &[lock]);
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
        unreference(lo, fx, record);
        fx.b.ins().return_(&[]);
    })
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

/// Stops a runner's intake: no message enters it afterwards, the delayed messages are released
/// and their timers cancelled, and the component task wakes to handle what is left.
fn stop(lo: &mut Lowering, fx: &mut Fx, record: Value, message: &Ty) {
    let lock = fx.offset(record, runner::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let one = fx.ptr(1);
    fx.store(one, record, runner::STOPPED);
    let delayed = fx.load(PTR, record, runner::DELAYED);
    let zero = fx.ptr(0);
    fx.store(zero, record, runner::DELAYED);
    let first = fx.load(PTR, record, runner::TIMERS);
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
    wake_component(lo, fx, record);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
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

/// Takes a message a task of another domain posted into the component task's domain, before
/// the post completes; the caller holds the runner's lock.
fn take_posted(lo: &mut Lowering, fx: &mut Fx, message: &Ty, address: Value, poster: Value) {
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
fn wake_component(lo: &mut Lowering, fx: &mut Fx, record: Value) {
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
fn append(lo: &mut Lowering, fx: &mut Fx, record: Value, node: Value) {
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
fn unlink(fx: &mut Fx, record: Value, node: Value) {
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
fn new_entry(lo: &mut Lowering, fx: &mut Fx, record: Value, message: &Ty, held: Held) -> Value {
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
fn send_now(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, record: Value, held: Held) {
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
fn after(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
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
/// delayed message now due enters the intake, in order, and the timer's record is freed.
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
}

/// `mut cx.spawn(key: k, run: f)`: supersedes `k`, then starts a keyed task running `f` with a
/// new outbox of key `k`, in a region of its own.
fn spawn(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let run_ty = fn_param(supply, 2);
    let record = fx.load(PTR, supply.param(0).address(), 0);
    let key = supply.param(1).scalar();
    let lock = fx.offset(record, runner::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    supersede_key(lo, fx, record, key);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    let size = fx.ptr(region::SIZE);
    let task_region = lo.call_routine(fx, Routine::RegionOpen, &[size])[0];
    let keyed_outbox = fx.ptr(1);
    let outbox = lo.call_routine(fx, Routine::OutboxMint, &[record, key, keyed_outbox])[0];
    let node_size = fx.ptr(keyed::SIZE);
    let node = lo.record_alloc(fx, node_size);
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, node, 0);
    fx.when(missing, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
    fx.store(key, node, keyed::KEY);
    fx.store(outbox, node, keyed::OUTBOX);
    fx.store(task_region, node, keyed::REGION);
    fx.store(record, node, keyed::RUNNER);
    let cell = fx.load(PTR, record, runner::EXECUTOR);
    let domain = tasks::current_domain(lo, fx);
    let done = keyed_done(lo);
    let started = start_task(
        lo,
        fx,
        &run_ty,
        supply.param(2).address(),
        cell,
        domain,
        task_region,
        node,
        None,
        None,
        done,
    );
    fx.store(outbox, started, task::ARGUMENT);
    fx.store(started, node, keyed::TASK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let head = fx.load(PTR, record, runner::KEYED);
    fx.store(head, node, keyed::NEXT);
    let has_head = fx.b.ins().icmp_imm_s(IntCC::NotEqual, head, 0);
    fx.when(has_head, |fx| fx.store(node, head, keyed::PREVIOUS));
    fx.store(node, record, runner::KEYED);
    let live = fx.load(PTR, record, runner::LIVE_KEYED);
    let live = fx.b.ins().iadd_imm_s(live, 1);
    fx.store(live, record, runner::LIVE_KEYED);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    tasks::spawned(lo, fx, started);
    supply.done(lo, fx);
}

/// Runs `visit` on each keyed task node of a runner, which may free the node. The caller holds
/// the lock.
fn each_keyed(
    lo: &mut Lowering,
    fx: &mut Fx,
    record: Value,
    mut visit: impl FnMut(&mut Lowering, &mut Fx, Value),
) {
    let first = fx.load(PTR, record, runner::KEYED);
    fx.repeat(&[first], |fx, state| {
        let node = state[0];
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, node, 0);
        let next = fx.choose_in(
            lo,
            present,
            &[PTR],
            |lo, fx| {
                let next = fx.load(PTR, node, keyed::NEXT);
                visit(lo, fx, node);
                vec![next]
            },
            |_, fx| vec![fx.ptr(0)],
        );
        (present, next)
    });
}

/// Supersedes a key: every outbox of the key goes stale, the posts waiting through them leave
/// the intake and every keyed task of the key still running is cancelled. The caller holds the
/// lock.
fn supersede_key(lo: &mut Lowering, fx: &mut Fx, record: Value, key: Value) {
    each_keyed(lo, fx, record, |lo, fx, node| {
        let node_key = fx.load(PTR, node, keyed::KEY);
        let same = fx.b.ins().icmp(IntCC::Equal, node_key, key);
        let superseded = fx.load(PTR, node, keyed::SUPERSEDED);
        let live = fx.b.ins().icmp_imm_s(IntCC::Equal, superseded, 0);
        let target = fx.b.ins().band(same, live);
        fx.when(target, |fx| supersede_node(lo, fx, node));
    });
    let first = fx.load(PTR, record, runner::OLDEST);
    fx.repeat(&[first], |fx, state| {
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
                let keyed_post = fx.load(PTR, node, entry::KEYED);
                let keyed_post = fx.b.ins().icmp_imm_s(IntCC::NotEqual, keyed_post, 0);
                let node_key = fx.load(PTR, node, entry::KEY);
                let same = fx.b.ins().icmp(IntCC::Equal, node_key, key);
                let removed = fx.b.ins().band(posted, keyed_post);
                let removed = fx.b.ins().band(removed, same);
                fx.when(removed, |fx| {
                    unlink(fx, record, node);
                    let status = fx.ptr(status::REMOVED as u64);
                    fx.store(status, node, entry::STATUS);
                    lo.call_routine(fx, Routine::Wake, &[poster]);
                });
                vec![next]
            },
            |_, fx| vec![fx.ptr(0)],
        );
        (present, next)
    });
}

/// Supersedes one keyed task: its outbox goes stale and the task is cancelled unless it has
/// ended, in which case its node is freed. The caller holds the lock.
fn supersede_node(lo: &mut Lowering, fx: &mut Fx, node: Value) {
    let one = fx.ptr(1);
    fx.store(one, node, keyed::SUPERSEDED);
    let outbox = fx.load(PTR, node, keyed::OUTBOX);
    lo.call_routine(fx, Routine::OutboxRetire, &[outbox]);
    let ended = fx.load(PTR, node, keyed::ENDED);
    let ended = fx.b.ins().icmp_imm_s(IntCC::NotEqual, ended, 0);
    fx.choose_in(
        lo,
        ended,
        &[],
        |lo, fx| {
            free_node(lo, fx, node);
            Vec::new()
        },
        |lo, fx| {
            let task_region = fx.load(PTR, node, keyed::REGION);
            lo.call_routine(fx, Routine::Cancel, &[task_region]);
            Vec::new()
        },
    );
}

/// Removes a keyed task's node from its runner's list and frees it. The caller holds the lock.
fn free_node(lo: &mut Lowering, fx: &mut Fx, node: Value) {
    let record = fx.load(PTR, node, keyed::RUNNER);
    let next = fx.load(PTR, node, keyed::NEXT);
    let previous = fx.load(PTR, node, keyed::PREVIOUS);
    let has_previous = fx.b.ins().icmp_imm_s(IntCC::NotEqual, previous, 0);
    fx.choose(
        has_previous,
        &[],
        |fx| {
            fx.store(next, previous, keyed::NEXT);
            Vec::new()
        },
        |fx| {
            fx.store(next, record, runner::KEYED);
            Vec::new()
        },
    );
    let has_next = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next, 0);
    fx.when(has_next, |fx| fx.store(previous, next, keyed::PREVIOUS));
    lo.record_free(fx, node);
}

/// What the executor thread does once a keyed task has ended: its region is freed, its node too
/// once its key is superseded, the run's caller wakes and the task's record is freed. Its
/// failure, a cancellation, needs no release.
fn keyed_done(lo: &mut Lowering) -> FuncId {
    lo.glue_function(Glue::KeyedDone, super::record_signature(), |lo, fx| {
        let params = fx.entry();
        let started = params[0];
        let node = fx.load(PTR, started, task::OWNER);
        let record = fx.load(PTR, node, keyed::RUNNER);
        let lock = fx.offset(record, runner::LOCK);
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let one = fx.ptr(1);
        fx.store(one, node, keyed::ENDED);
        let live = fx.load(PTR, record, runner::LIVE_KEYED);
        let live = fx.b.ins().iadd_imm_s(live, -1);
        fx.store(live, record, runner::LIVE_KEYED);
        let task_region = fx.load(PTR, node, keyed::REGION);
        lo.record_free(fx, task_region);
        let superseded = fx.load(PTR, node, keyed::SUPERSEDED);
        let superseded = fx.b.ins().icmp_imm_s(IntCC::NotEqual, superseded, 0);
        fx.when(superseded, |fx| free_node(lo, fx, node));
        wake_waiter(lo, fx, record);
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        lo.record_free(fx, started);
        fx.b.ins().return_(&[]);
    })
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
    let origin = super::origin(lo, fx);
    let cancelled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, origin, 0);
    fx.when(cancelled, |fx| {
        release_held(lo, fx);
        super::fail_cancelled(lo, fx, supply, origin);
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
    let origin = super::origin(lo, fx);
    let cancelled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, origin, 0);
    let cancelled = fx.b.ins().band(cancelled, waiting);
    let leaving = fx.b.ins().bor(removed, cancelled);
    fx.when(leaving, |fx| {
        fx.when(waiting, |fx| unlink(fx, record, node));
        leave_post(lo, fx, record);
        lo.release(fx, message, at);
        fx.when(cancelled, |fx| {
            super::fail_cancelled(lo, fx, supply, origin)
        });
        fail_closed(lo, fx, supply);
    });
}
