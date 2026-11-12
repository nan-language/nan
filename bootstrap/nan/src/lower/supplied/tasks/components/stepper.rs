//! Steppers: `sys::Stepper`, whose holder handles the messages of a component's intake on its
//! own task, one call at a time, and decides when its keyed tasks start. A stepper's value is its
//! runner's record, a holding of the domain that started it, which settlement withdraws.

use std::rc::Rc;

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};
use cranelift_module::FuncId;

use super::super::super::super::body::Held;
use super::super::super::super::fx::{Fx, PTR};
use super::super::super::super::runtime::regions::WAITING;
use super::super::super::super::runtime::{Routine, holdings, seeded, settle, tasks};
use super::super::super::super::{Glue, Lowering};
use super::super::super::Supply;
use super::{
    HEADER_BYTES, call_handler, draw_record, entry, handle, handle_oldest, handler, intake, keyed,
    oldest_sent, release_env, runner, shape, status, take_sent, unreference, wait_for,
};
use crate::check::ty::{Arg, Bound, Ty};

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let record = |fx: &mut Fx| fx.load(PTR, supply.param(0).address(), 0);
    match supply.member() {
        "start" => open(lo, fx, supply, false),
        "resume" => open(lo, fx, supply, true),
        "attach" => {
            let record = record(fx);
            let handle = fx.load(PTR, supply.param(1).address(), 0);
            fx.store(handle, record, runner::RUNNER);
            supply.done(lo, fx);
        }
        "outbox" | "waker" => {
            let record = record(fx);
            let inbox = fx.load(PTR, record, runner::INBOX);
            let sret = supply.sret.expect("an outbox's address");
            fx.store(inbox, sret, 0);
            supply.done(lo, fx);
        }
        "deliver" | "deliver_headed" => {
            let record = record(fx);
            let (state, env, message) = types_of(supply);
            let layout = lo.layout(&message);
            let slot = fx.slot(layout.size, layout.align);
            lo.store_held(fx, slot, supply.param(1), &message);
            let header = fx.slot(tasks::HEADER_BLOCK, 8);
            let zero = fx.ptr(0);
            let block = fx.ptr(tasks::HEADER_BLOCK);
            lo.call_routine(fx, Routine::Fill, &[header, zero, block]);
            if supply.member() == "deliver_headed" {
                let one = fx.ptr(1);
                fx.store(one, header, 0);
                let bytes = fx.offset(header, 8);
                lo.copy(fx, bytes, supply.param(2).address(), HEADER_BYTES, 1);
            }
            handle(lo, fx, record, &state, &env, &message, (slot, header), None);
            supply.done(lo, fx);
        }
        "step" => {
            let record = record(fx);
            let (state, env, message) = types_of(supply);
            let option = supply.abi.result.clone();
            let handled = lo.std_ty("sys", "Handled", Vec::new());
            let layout = lo.layout(&option);
            let result = supply
                .sret
                .unwrap_or_else(|| fx.slot(layout.size, layout.align));
            let lock = fx.offset(record, runner::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let oldest = fx.load(PTR, record, runner::OLDEST);
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, oldest, 0);
            lo.write_option_with(fx, &option, result, present, |lo, fx, at| {
                let handled = Some((&handled, at));
                handle_oldest(lo, fx, record, oldest, &state, &env, &message, handled);
            });
            let empty = fx.b.ins().bxor_imm_s(present, 1);
            fx.when(empty, |fx| {
                lo.call_routine(fx, Routine::Unlock, &[lock]);
            });
            let held = lo.held_at(fx, &option, result);
            supply.ret(lo, fx, held);
        }
        "take_sent" => {
            let record = record(fx);
            let (_, _, message) = types_of(supply);
            let option = supply.abi.result.clone();
            let sent = lo.std_ty("sys", "Sent", vec![message.clone()]);
            let layout = lo.layout(&option);
            let result = supply
                .sret
                .unwrap_or_else(|| fx.slot(layout.size, layout.align));
            let lock = fx.offset(record, runner::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let found = oldest_sent(fx, record);
            let in_intake = fx.b.ins().icmp_imm_s(IntCC::NotEqual, found, 0);
            let delayed = fx.load(PTR, record, runner::DELAYED);
            let node = fx.b.ins().select(in_intake, found, delayed);
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, node, 0);
            lo.write_option_with(fx, &option, result, present, |lo, fx, at| {
                take_sent(lo, fx, record, (node, in_intake), &message, (&sent, at));
            });
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            let held = lo.held_at(fx, &option, result);
            supply.ret(lo, fx, held);
        }
        "take_held" => {
            let record = record(fx);
            let option = supply.abi.result.clone();
            let layout = lo.layout(&option);
            let result = supply
                .sret
                .unwrap_or_else(|| fx.slot(layout.size, layout.align));
            let lock = fx.offset(record, runner::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let node = keyed::oldest_held(fx, record);
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, node, 0);
            let key = fx.choose(
                present,
                &[PTR],
                |fx| vec![fx.load(PTR, node, keyed::node::KEY)],
                |fx| vec![fx.ptr(0)],
            )[0];
            fx.when(present, |fx| {
                keyed::supersede_key(lo, fx, record, key);
                keyed::start_node(lo, fx, node);
            });
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            seeded::scheduling_point(lo, fx);
            lo.write_option(fx, &option, result, present, Held::Scalar(key));
            let held = lo.held_at(fx, &option, result);
            supply.ret(lo, fx, held);
        }
        "held_task" => {
            let record = record(fx);
            let option = supply.abi.result.clone();
            let layout = lo.layout(&option);
            let result = supply
                .sret
                .unwrap_or_else(|| fx.slot(layout.size, layout.align));
            let lock = fx.offset(record, runner::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let node = keyed::nth_held(fx, record, supply.param(1).scalar());
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, node, 0);
            lo.write_option_with(fx, &option, result, present, |lo, fx, at| {
                held_task(lo, fx, node, at);
            });
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            let held = lo.held_at(fx, &option, result);
            supply.ret(lo, fx, held);
        }
        member @ ("start_held" | "drop_held") => {
            let record = record(fx);
            let key = supply.param(1).scalar();
            let lock = fx.offset(record, runner::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let node = keyed::held_keyed(fx, record, key);
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, node, 0);
            fx.when(present, |fx| {
                if member == "drop_held" {
                    keyed::supersede_key(lo, fx, record, key);
                }
                keyed::start_node(lo, fx, node);
            });
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            seeded::scheduling_point(lo, fx);
            supply.ret(lo, fx, Held::Scalar(present));
        }
        "incoming" => {
            let record = record(fx);
            wait(lo, fx, supply, record, false);
        }
        "ready" => {
            let record = record(fx);
            wait(lo, fx, supply, record, true);
        }
        "start_keyed" => {
            let record = record(fx);
            let lock = fx.offset(record, runner::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let started = keyed::start_held(lo, fx, record);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            seeded::scheduling_point(lo, fx);
            supply.ret(lo, fx, Held::Scalar(started));
        }
        "idle" => {
            let record = record(fx);
            let lock = fx.offset(record, runner::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let idle = idle(fx, record);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            supply.ret(lo, fx, Held::Scalar(idle));
        }
        "state" => {
            let record = record(fx);
            let (state, env, message) = types_of(supply);
            let shape = shape(lo, &state, &env, &message);
            let state_at = fx.offset(record, shape.state_at);
            supply.ret(lo, fx, Held::Address(state_at));
        }
        "end" => {
            let record = record(fx);
            end(lo, fx, supply, record);
        }
        "abandon" => {
            let record = record(fx);
            abandon(lo, fx, supply, record);
        }
        "resumed" => system(lo, fx, supply, "Resume", "resume"),
        "pressure" => system(lo, fx, supply, "Pressure", "pressure"),
        "health" => system(lo, fx, supply, "Health", "health"),
        "suspend" => system(lo, fx, supply, "Suspend", "suspend"),
        "wake" => system(lo, fx, supply, "Suspend", "wake"),
        member => panic!("no lowering of sys::Stepper.{member}"),
    }
}

/// Writes the `sys::HeldTask` of a keyed task's node at `at`: its key and its class.
fn held_task(lo: &mut Lowering, fx: &mut Fx, node: Value, at: Value) {
    let task = lo.std_ty("sys", "HeldTask", Vec::new());
    let reversibility = lo.std_ty("sys", "Reversibility", Vec::new());
    let key_at = lo.layouts.field(lo.ck, &task, None, "key").offset;
    let class_at = lo.layouts.field(lo.ck, &task, None, "class").offset;
    let key = fx.load(PTR, node, keyed::node::KEY);
    fx.store(key, at, key_at);
    let from = fx.offset(node, keyed::node::CLASS);
    let to = fx.offset(at, class_at);
    let size = lo.layout(&reversibility).size;
    lo.copy(fx, to, from, size, 1);
}

/// The state, Env and message types of a stepper.
fn types_of(supply: &Supply) -> (Ty, Ty, Ty) {
    (supply.type_arg(0), supply.type_arg(1), supply.type_arg(2))
}

/// `Stepper.start(env, exec, alloc)`, which calls `start` for the state, and
/// `Stepper.resume(state, env, exec, alloc)`: a new record holding the state and the Env, a
/// holding of the running task's domain, with its outbox and the summary published. Fails with
/// the cancellation, releasing what it took, when `exec` is stale.
fn open(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, resumed: bool) {
    let (state_ty, env_ty, message_ty) = types_of(supply);
    let first = usize::from(resumed);
    let env = supply.param(first);
    let (live, [exec_cell, clock, resolution, _]) =
        super::super::executor(lo, fx, supply.param(first + 1).address());
    let stale = fx.b.ins().bxor_imm_s(live, 1);
    fx.when(stale, |fx| {
        if let (true, Held::Address(state)) = (resumed, supply.param(0)) {
            lo.release(fx, &state_ty, state);
        }
        release_env(lo, fx, &env_ty, env);
        let none = fx.ptr(0);
        super::super::fail_cancelled(lo, fx, supply, none);
    });
    let cell = fx.load(PTR, supply.param(first + 2).address(), 0);
    let shape = shape(lo, &state_ty, &env_ty, &message_ty);
    let exec = fx.load(PTR, supply.param(first + 1).address(), 0);
    let record = draw_record(lo, fx, &shape, cell, exec, exec_cell, clock, resolution);
    let kind = fx.ptr(holdings::kind::STEPPER as u64);
    let holding = lo.call_routine(fx, Routine::Hold, &[kind, record])[0];
    fx.store(holding, record, runner::HOLDING);
    let env_at = fx.offset(record, shape.env_at);
    lo.store_held(fx, env_at, env, &env_ty);
    let state = if resumed {
        supply.param(0)
    } else {
        let start = handler(lo, "Component", "start", &state_ty, &env_ty, &message_ty);
        call_handler(lo, fx, start, &[Held::Address(env_at)])
    };
    let state_at = fx.offset(record, shape.state_at);
    lo.store_held(fx, state_at, state, &state_ty);
    super::super::protocols::publish(lo, fx, record, &shape, &state_ty);
    let zero = fx.ptr(0);
    let inbox = lo.call_routine(fx, Routine::OutboxMint, &[record, zero, zero])[0];
    fx.store(inbox, record, runner::INBOX);
    let sret = supply.sret.expect("a stepper's address");
    fx.store(record, sret, 0);
    supply.done(lo, fx);
}

/// `mut s.resumed(reason)`, `mut s.pressure(level)`, `mut s.health()`, `mut s.suspend()` and
/// `mut s.wake()`: when the state implements the system interface for the stepper's Env and
/// messages, a call of its method on the calling task with the state, the operation's argument,
/// the Env and a context, then the summary published; otherwise nothing, `health` giving
/// `Healthy`.
fn system(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, owner: &str, method: &str) {
    let (state_ty, env_ty, message_ty) = types_of(supply);
    let bound = Bound {
        interface: lo.ck.db.std_item("sys", owner).expect("a system interface"),
        args: Rc::from(vec![Arg::Ty(env_ty.clone()), Arg::Ty(message_ty.clone())]),
    };
    if !lo.ck.satisfies(&state_ty, &bound) {
        let reply = match supply.member() {
            "health" => lo.variant_value(fx, &supply.abi.result, "Healthy", &[], None),
            _ => Held::Empty,
        };
        supply.ret(lo, fx, reply);
        return;
    }
    let record = fx.load(PTR, supply.param(0).address(), 0);
    let shape = shape(lo, &state_ty, &env_ty, &message_ty);
    let state_at = fx.offset(record, shape.state_at);
    let env_at = fx.offset(record, shape.env_at);
    let cx = fx.slot(8, 8);
    fx.store(record, cx, 0);
    let mut args = vec![Held::Address(state_at)];
    args.extend(supply.params.get(1).copied());
    args.extend([Held::Address(env_at), Held::Address(cx)]);
    let instance = handler(lo, owner, method, &state_ty, &env_ty, &message_ty);
    let reply = call_handler(lo, fx, instance, &args);
    super::super::protocols::publish(lo, fx, record, &shape, &state_ty);
    supply.ret(lo, fx, reply);
}

/// Whether a runner's record is idle: its intake is empty, no message waits for its delay and no
/// keyed task is held or running. The caller holds the lock.
fn idle(fx: &mut Fx, record: Value) -> Value {
    let oldest = fx.load(PTR, record, runner::OLDEST);
    let delayed = fx.load(PTR, record, runner::DELAYED);
    let live = fx.load(PTR, record, runner::LIVE_KEYED);
    let pending = fx.b.ins().bor(oldest, delayed);
    let pending = fx.b.ins().bor(pending, live);
    fx.b.ins().icmp_imm_s(IntCC::Equal, pending, 0)
}

/// `s.incoming()` and `s.ready()`: a cancellation point waiting until a message waits in the
/// intake, or, for `ready`, until the stepper is idle, giving whether a message waits.
fn wait(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, record: Value, until_idle: bool) {
    let lock = fx.offset(record, runner::LOCK);
    let current = tasks::current_task(lo, fx);
    let withdraw = withdraw_stepper(lo);
    let none = fx.ptr(0);
    settle::enter_wait(lo, fx, withdraw, record, none);
    fx.repeat(&[], |fx, _| {
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let oldest = fx.load(PTR, record, runner::OLDEST);
        let queued = fx.b.ins().icmp_imm_s(IntCC::NotEqual, oldest, 0);
        let woken = fx.load(PTR, record, runner::WOKEN);
        let woken = fx.b.ins().icmp_imm_s(IntCC::NotEqual, woken, 0);
        let present = fx.b.ins().bor(queued, woken);
        let mut done = present;
        if until_idle {
            let idle = idle(fx, record);
            done = fx.b.ins().bor(done, idle);
        }
        let origin = super::super::origin(lo, fx);
        let cancelled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, origin, 0);
        let leaving = fx.b.ins().bor(done, cancelled);
        fx.when(leaving, |fx| {
            let zero = fx.ptr(0);
            for field in [runner::COMPONENT, runner::IDLE, runner::WAITER] {
                fx.store(zero, record, field);
            }
            fx.when(done, |fx| {
                let zero = fx.ptr(0);
                fx.store(zero, record, runner::WOKEN);
            });
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            settle::leave_wait(lo, fx);
            tasks::waiting_on(lo, fx, 0);
            fx.when(done, |fx| {
                let given = if until_idle {
                    Held::Scalar(present)
                } else {
                    Held::Empty
                };
                supply.ret(lo, fx, given);
            });
            super::super::fail_cancelled(lo, fx, supply, origin);
        });
        fx.store(current, record, runner::COMPONENT);
        let one = fx.ptr(1);
        fx.store(one, record, runner::IDLE);
        if until_idle {
            fx.store(current, record, runner::WAITER);
        }
        tasks::waiting_on(lo, fx, WAITING);
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        super::super::park_waiting(lo, fx);
        (fx.int(types::I8, 1), Vec::new())
    });
    fx.b.ins()
        .trap(cranelift_codegen::ir::TrapCode::user(1).expect("a trap code"));
}

/// Closes a stepper: its intake stops, its posts failing as closed and its other messages
/// released, every key is superseded and every held keyed task started.
fn close(lo: &mut Lowering, fx: &mut Fx, record: Value, message: &Ty) {
    intake::stop(lo, fx, record, message);
    let lock = fx.offset(record, runner::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let zero = fx.ptr(0);
    let oldest = fx.load(PTR, record, runner::OLDEST);
    fx.store(zero, record, runner::OLDEST);
    fx.store(zero, record, runner::NEWEST);
    let sent = fx.repeat(&[oldest, zero], |fx, state| {
        let (node, sent) = (state[0], state[1]);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, node, 0);
        let next = fx.choose_in(
            lo,
            present,
            &[PTR, PTR],
            |lo, fx| {
                let next = fx.load(PTR, node, entry::NEXT);
                let poster = fx.load(PTR, node, entry::POSTER);
                let posted = fx.b.ins().icmp_imm_s(IntCC::NotEqual, poster, 0);
                let kept = fx.choose_in(
                    lo,
                    posted,
                    &[PTR],
                    |lo, fx| {
                        let removed = fx.ptr(status::REMOVED as u64);
                        fx.store(removed, node, entry::STATUS);
                        lo.call_routine(fx, Routine::Wake, &[poster]);
                        vec![sent]
                    },
                    |_, fx| {
                        fx.store(sent, node, entry::NEXT);
                        vec![node]
                    },
                )[0];
                vec![next, kept]
            },
            |_, fx| vec![fx.ptr(0), sent],
        );
        (present, next)
    })[1];
    keyed::supersede_all(lo, fx, record);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    fx.repeat(&[sent], |fx, state| {
        let node = state[0];
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, node, 0);
        let next = fx.choose_in(
            lo,
            present,
            &[PTR],
            |lo, fx| {
                let next = fx.load(PTR, node, entry::NEXT);
                let held = fx.offset(node, entry::MESSAGE);
                lo.release(fx, message, held);
                lo.call_routine(fx, Routine::Free, &[node]);
                vec![next]
            },
            |_, fx| vec![fx.ptr(0)],
        );
        (present, next)
    });
}

/// Makes a closed stepper's outbox stale and ends its holding.
fn retire(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let inbox = fx.load(PTR, record, runner::INBOX);
    lo.call_routine(fx, Routine::OutboxRetire, &[inbox]);
    let holding = fx.load(PTR, record, runner::HOLDING);
    lo.call_routine(fx, Routine::Unhold, &[holding]);
}

/// `take s.end()`: closes the stepper, then, once every keyed task and timer has ended, retires
/// it and gives the state and the Env.
fn end(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, record: Value) {
    let (state_ty, env_ty, message_ty) = types_of(supply);
    close(lo, fx, record, &message_ty);
    let withdraw = withdraw_stepper(lo);
    let none = fx.ptr(0);
    settle::enter_wait(lo, fx, withdraw, record, none);
    wait_for(lo, fx, record, |fx| {
        let live = fx.load(PTR, record, runner::LIVE_KEYED);
        let timers = fx.load(PTR, record, runner::TIMER_COUNT);
        let pending = fx.b.ins().bor(live, timers);
        fx.b.ins().icmp_imm_s(IntCC::Equal, pending, 0)
    });
    settle::leave_wait(lo, fx);
    retire(lo, fx, record);
    let shape = shape(lo, &state_ty, &env_ty, &message_ty);
    let stopped = supply.abi.result.clone();
    if let Some(result) = supply.sret {
        for (field, ty, at) in [
            ("state", &state_ty, shape.state_at),
            ("env", &env_ty, shape.env_at),
        ] {
            let offset = lo.layouts.field(lo.ck, &stopped, None, field).offset;
            let layout = lo.layout(ty);
            let target = fx.offset(result, offset);
            let source = fx.offset(record, at);
            lo.copy(fx, target, source, layout.size, layout.align);
        }
    }
    unreference(lo, fx, record);
    supply.done(lo, fx);
}

/// `take s.abandon()`: closes and retires the stepper at once and releases the state and the
/// Env; its keyed tasks and timers end on their own.
fn abandon(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, record: Value) {
    let (state_ty, env_ty, message_ty) = types_of(supply);
    close(lo, fx, record, &message_ty);
    retire(lo, fx, record);
    let shape = shape(lo, &state_ty, &env_ty, &message_ty);
    let state_at = fx.offset(record, shape.state_at);
    lo.release(fx, &state_ty, state_at);
    let env_at = fx.offset(record, shape.env_at);
    release_env(lo, fx, &env_ty, Held::Address(env_at));
    unreference(lo, fx, record);
    supply.done(lo, fx);
}

/// Withdraws a stopped holder's wait on its stepper.
fn withdraw_stepper(lo: &mut Lowering) -> FuncId {
    lo.glue_function(
        Glue::WithdrawStepper,
        settle::withdraw_signature(),
        |lo, fx| {
            let params = fx.entry();
            let record = fx.load(PTR, params[0], settle::wait::OBJECT);
            let lock = fx.offset(record, runner::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let zero = fx.ptr(0);
            for field in [runner::COMPONENT, runner::IDLE, runner::WAITER] {
                fx.store(zero, record, field);
            }
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            fx.b.ins().return_(&[]);
        },
    )
}
