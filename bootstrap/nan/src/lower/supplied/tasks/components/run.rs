//! The runner `host::Runner`: its run, whose component task handles the intake while the body
//! runs, and the settlement of a run whose caller has stopped.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};
use cranelift_module::FuncId;

use super::super::super::super::body::Held;
use super::super::super::super::fx::{Fx, PTR};
use super::super::super::super::runtime::tasks::{self, task};
use super::super::super::super::runtime::{Routine, report, settle};
use super::super::super::super::{Glue, Lowering};
use super::super::super::Supply;
use super::super::super::mem::lists::fn_param;
use super::{
    call_handler, draw_record, handle_oldest, handler, intake, keyed, release_env, runner, shape,
    unreference, wait_for, wake_waiter, withdraw,
};
use crate::check::ty::Ty;

/// `host::Runner[S].run(env, exec, alloc, body: b)`.
pub fn run(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let state_ty = supply.type_arg(0);
    let env_ty = supply.type_arg(1);
    let message_ty = supply.type_arg(2);
    let body_ty = fn_param(supply, 3);
    let env = supply.param(0);
    let (live, [exec_cell, clock, resolution, _]) =
        super::super::executor(lo, fx, supply.param(1).address());
    let stale = fx.b.ins().bxor_imm_s(live, 1);
    fx.when(stale, |fx| {
        release_env(lo, fx, &env_ty, env);
        let none = fx.ptr(0);
        super::super::fail_cancelled(lo, fx, supply, none);
    });
    let cell = fx.load(PTR, supply.param(2).address(), 0);
    let shape = shape(lo, &state_ty, &env_ty, &message_ty);
    let record = draw_record(lo, fx, &shape, cell, exec_cell, clock, resolution);
    let withdraw = withdraw_run(lo);
    let none = fx.ptr(0);
    settle::enter_wait(lo, fx, withdraw, record, none);
    let env_at = fx.offset(record, shape.env_at);
    lo.store_held(fx, env_at, env, &env_ty);
    let start = handler(lo, "start", &state_ty, &env_ty, &message_ty);
    let state = call_handler(lo, fx, start, &[Held::Address(env_at)]);
    let state_at = fx.offset(record, shape.state_at);
    lo.store_held(fx, state_at, state, &state_ty);
    super::super::protocols::publish(lo, fx, record, &shape, &state_ty);
    start_component(lo, fx, record, exec_cell, &state_ty, &env_ty, &message_ty);
    let zero = fx.ptr(0);
    let inbox = lo.call_routine(fx, Routine::OutboxMint, &[record, zero, zero])[0];
    fx.store(inbox, record, runner::INBOX);
    let inbox_slot = fx.slot(8, 8);
    fx.store(inbox, inbox_slot, 0);
    let called = lo.call_value(
        fx,
        &body_ty,
        supply.param(3).address(),
        &[Held::Address(inbox_slot)],
    );
    intake::stop(lo, fx, record, &message_ty);
    wait_for(lo, fx, record, |fx| {
        let ended = fx.load(PTR, record, runner::ENDED);
        let ended = fx.b.ins().icmp_imm_s(IntCC::NotEqual, ended, 0);
        let timers = fx.load(PTR, record, runner::TIMER_COUNT);
        let settled = fx.b.ins().icmp_imm_s(IntCC::Equal, timers, 0);
        fx.b.ins().band(ended, settled)
    });
    let lock = fx.offset(record, runner::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    keyed::supersede_all(lo, fx, record);
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

/// The component task: removes the intake's messages one at a time, oldest first, and handles
/// each, until the intake has stopped and is empty; then releases the Env.
fn component_glue(lo: &mut Lowering, state: &Ty, env: &Ty, message: &Ty) -> FuncId {
    let glue = Glue::Component(state.clone(), env.clone(), message.clone());
    let (state, env, message) = (state.clone(), env.clone(), message.clone());
    lo.glue_function(glue, super::super::record_signature(), move |lo, fx| {
        let params = fx.entry();
        let record = fx.load(PTR, params[0], task::ENV);
        let lock = fx.offset(record, runner::LOCK);
        fx.repeat(&[], |fx, _| {
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let oldest = fx.load(PTR, record, runner::OLDEST);
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, oldest, 0);
            let again = fx.choose_in(
                lo,
                present,
                &[types::I8],
                |lo, fx| {
                    handle_oldest(lo, fx, record, oldest, &state, &env, &message);
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
        let shape = shape(lo, &state, &env, &message);
        let env_at = fx.offset(record, shape.env_at);
        release_env(lo, fx, &env, Held::Address(env_at));
        fx.b.ins().return_(&[]);
    })
}

/// What the executor thread does once a component task has ended: the run's caller wakes and
/// the task's record is freed.
fn component_done(lo: &mut Lowering) -> FuncId {
    lo.glue_function(
        Glue::ComponentDone,
        super::super::record_signature(),
        |lo, fx| {
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
        },
    )
}

/// Settles a runner whose run a stopped task makes.
fn withdraw_run(lo: &mut Lowering) -> FuncId {
    lo.glue_function(Glue::WithdrawRun, settle::withdraw_signature(), |lo, fx| {
        let params = fx.entry();
        let record = fx.load(PTR, params[0], settle::wait::OBJECT);
        withdraw(lo, fx, record);
        fx.b.ins().return_(&[]);
    })
}
