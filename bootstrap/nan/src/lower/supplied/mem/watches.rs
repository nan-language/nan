//! Watches of a cell's charge: a watch holds the allocator naming its cell. A wait for the charge
//! to leave a band arms the cell's arm record, made on the cell's first wait, with the band and
//! the waiting task, which a draw or return taking the charge out of the band wakes.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC};
use cranelift_module::FuncId;

use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::memory::{self, arm, cell};
use super::super::super::runtime::regions::WAITING;
use super::super::super::runtime::{Routine, report, settle, state, tasks};
use super::super::super::{Glue, Lowering};
use super::super::{Supply, tasks as task_ops};

/// `wait_outside(low, high)` waits until the charge of the watch's cell lies outside the charges
/// from `low` up to below `high`.
pub fn watch(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let watch_ty = supply.owner.clone().expect("a watch type");
    let at = lo.layouts.field(lo.ck, &watch_ty, None, "cell").offset;
    let cell = fx.load(PTR, supply.param(0).address(), at);
    match supply.member() {
        "wait_outside" => wait_outside(lo, fx, supply, cell),
        member => panic!("no lowering of mem::Watch.{member}"),
    }
}

/// Returns at once when the charge lies outside the band; otherwise arms the cell's arm record
/// with the band and the running task, which another task waiting on it traps, and parks until a
/// wake. A wait cancelled at its start fails at once; a cancelled one returns only when the
/// charge left the band.
fn wait_outside(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, handle: Value) {
    let low = supply.param(1).scalar();
    let high = supply.param(2).scalar();
    let span = fx.b.ins().isub(high, low);
    let early = task_ops::origin(lo, fx);
    let cancelled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, early, 0);
    fx.when(cancelled, |fx| {
        task_ops::fail_cancelled(lo, fx, supply, early)
    });
    let current = tasks::current_task(lo, fx);
    let state_address = lo.state(fx);
    let lock = fx.offset(state_address, state::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let record = memory::cell_record(lo, fx, handle);
    let charge = fx.load(PTR, record, cell::CHARGE);
    let left = memory::outside(fx, charge, low, span);
    fx.when(left, |fx| {
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        supply.done(lo, fx);
    });
    let held = fx.load(PTR, record, cell::ARM);
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, held, 0);
    let record_arm = fx.choose_in(
        lo,
        missing,
        &[PTR],
        |lo, fx| {
            let size = fx.ptr(arm::SIZE);
            let made = lo.record_alloc(fx, size);
            let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, made, 0);
            fx.when(failed, |fx| {
                lo.call_routine(fx, Routine::Unlock, &[lock]);
                report::raise_kind(lo, fx, "trap.exhausted");
            });
            let zero = fx.ptr(0);
            fx.store(zero, made, arm::LOW);
            let every = fx.int(PTR, -1);
            fx.store(every, made, arm::SPAN);
            fx.store(zero, made, arm::WAITER);
            fx.store(zero, made, arm::NEXT);
            fx.store(zero, made, arm::LISTED);
            fx.store(made, record, cell::ARM);
            vec![made]
        },
        |_, _| vec![held],
    )[0];
    let waiter = fx.load(PTR, record_arm, arm::WAITER);
    let waits = fx.b.ins().icmp_imm_s(IntCC::NotEqual, waiter, 0);
    let elsewhere = fx.b.ins().icmp(IntCC::NotEqual, waiter, current);
    let other = fx.b.ins().band(waits, elsewhere);
    fx.when(other, |fx| {
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        report::raise_kind(lo, fx, "trap.duplicate-access");
    });
    fx.store(low, record_arm, arm::LOW);
    fx.store(span, record_arm, arm::SPAN);
    fx.store(current, record_arm, arm::WAITER);
    tasks::waiting_on(lo, fx, WAITING);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    let withdraw = withdraw_arm(lo);
    settle::enter_wait(lo, fx, withdraw, record_arm, current);
    task_ops::park_waiting(lo, fx);
    settle::leave_wait(lo, fx);
    tasks::waiting_on(lo, fx, 0);
    let origin = task_ops::origin(lo, fx);
    let cancelled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, origin, 0);
    fx.when(cancelled, |fx| {
        lo.call_routine(fx, Routine::Lock, &[lock]);
        clear_waiter(fx, record_arm, current);
        let charge = fx.load(PTR, record, cell::CHARGE);
        let left = memory::outside(fx, charge, low, span);
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        let stays = fx.b.ins().bxor_imm_s(left, 1);
        fx.when(stays, |fx| task_ops::fail_cancelled(lo, fx, supply, origin));
    });
    supply.done(lo, fx);
}

/// Takes a task out of an arm record when it waits there. The caller holds the lock.
fn clear_waiter(fx: &mut Fx, record_arm: Value, task: Value) {
    let waiter = fx.load(PTR, record_arm, arm::WAITER);
    let ours = fx.b.ins().icmp(IntCC::Equal, waiter, task);
    fx.when(ours, |fx| {
        let none = fx.ptr(0);
        fx.store(none, record_arm, arm::WAITER);
    });
}

/// The glue withdrawing a stopped task's wait on a charge, which the wait records with its arm
/// record.
fn withdraw_arm(lo: &mut Lowering) -> FuncId {
    lo.glue_function(Glue::WithdrawArm, settle::withdraw_signature(), |lo, fx| {
        let params = fx.entry();
        let record_arm = fx.load(PTR, params[0], settle::wait::OBJECT);
        let state_address = lo.state(fx);
        let lock = fx.offset(state_address, state::LOCK);
        lo.call_routine(fx, Routine::Lock, &[lock]);
        clear_waiter(fx, record_arm, params[1]);
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        fx.b.ins().return_(&[]);
    })
}
