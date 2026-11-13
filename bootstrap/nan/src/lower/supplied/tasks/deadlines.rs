//! Deadlines: `task::within` and `within_ns`, whose deadline cancels their body, `task::paced`,
//! whose deadline cancels nothing, and `task::deadline_ns`, which reads the earliest of the
//! deadlines a task runs under.
//!
//! The running task's record links the deadline of the innermost of these calls it runs, whose
//! record links that of the next one out; each record holds when its deadline is due, in
//! nanoseconds of the clock that delays count.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::super::Lowering;
use super::super::super::body::Held;
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::regions::region;
use super::super::super::runtime::tasks::{self, kind, operation, task};
use super::super::super::runtime::{Routine, tables, timers};
use super::super::Supply;
use super::super::mem::lists::fn_param;
use super::super::sys::round_down;

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    match supply.path.as_str() {
        "task::within" | "task::within_ns" => within(lo, fx, supply),
        "task::paced" => paced(lo, fx, supply),
        "task::deadline_ns" => deadline_ns(lo, fx, supply),
        path => panic!("no lowering of {path}"),
    }
}

/// `task::within(clock, ms, body: b)` and `task::within_ns(clock, ns, body: b)`: runs `b` in a
/// region that a deadline cancels; `None` when `b` fails with a cancellation of that region.
fn within(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let body_ty = fn_param(supply, 2);
    let clock = fx.load(PTR, supply.param(0).address(), 0);
    let ns = match supply.path.as_str() {
        "task::within" => timers::from_ms(fx, supply.param(1).scalar()),
        _ => supply.param(1).scalar(),
    };
    let size = fx.ptr(region::SIZE);
    let record = lo.call_routine(fx, Routine::RegionOpen, &[size])[0];
    let timer = fx.slot(tables::deadline::SIZE, 8);
    let zero = fx.ptr(0);
    let timer_size = fx.ptr(tables::deadline::SIZE);
    lo.call_routine(fx, Routine::Fill, &[timer, zero, timer_size]);
    let deadline = fx.int(PTR, kind::DEADLINE);
    fx.store(deadline, timer, operation::KIND);
    fx.store(record, timer, operation::TASK);
    let (live, minting) = tables::link_deadline(lo, fx, clock, timer);
    set(lo, fx, live, minting, ns, record, timer);
    let current = tasks::current_task(lo, fx);
    let enclosing = fx.load(PTR, current, task::REGION);
    fx.store(record, current, task::REGION);
    let called = lo.call_value(fx, &body_ty, supply.param(2).address(), &[]);
    fx.store(enclosing, current, task::REGION);
    fx.choose_in(
        lo,
        live,
        &[],
        |lo, fx| {
            tables::unlink_deadline(lo, fx, timer);
            disarm(lo, fx, timer);
            Vec::new()
        },
        |lo, fx| {
            pop(lo, fx, timer);
            Vec::new()
        },
    );
    let id = fx.load(PTR, record, region::ID);
    lo.record_free(fx, record);
    let option = supply.abi.result.clone();
    let layout = lo.layout(&option);
    let slot = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    let fails = body_ty.fails.clone();
    if let (Some(failed), Some(failure), Some(fails)) = (called.failed, called.failure, fails) {
        fx.when(failed, |fx| {
            let held = lo.held_at(fx, &fails, failure);
            let own = lo
                .cancellation_origin(fx, &fails, held)
                .map(|(is_cancellation, origin)| {
                    let same = fx.b.ins().icmp(IntCC::Equal, origin, id);
                    fx.b.ins().band(is_cancellation, same)
                })
                .unwrap_or_else(|| fx.int(types::I8, 0));
            fx.choose_in(
                lo,
                own,
                &[],
                |lo, fx| {
                    let none = fx.int(types::I8, 0);
                    lo.write_option(fx, &option, slot, none, Held::Empty);
                    let held = lo.held_at(fx, &option, slot);
                    supply.ret(lo, fx, held);
                    Vec::new()
                },
                |lo, fx| {
                    let target = supply.failure.expect("a failure address");
                    lo.store_held(fx, target, held, &fails);
                    supply.fail(fx);
                    Vec::new()
                },
            );
        });
    }
    let some = fx.int(types::I8, 1);
    lo.write_option(fx, &option, slot, some, called.result);
    let held = lo.held_at(fx, &option, slot);
    supply.ret(lo, fx, held);
}

/// Sets the deadline of a `within` call, whose region's record is `record` and whose timer is
/// `timer`, on a clock minted at `minted` with `resolution`: when the clock is live, arms the
/// timer; otherwise cancels the region and makes the deadline due at once.
fn set(
    lo: &mut Lowering,
    fx: &mut Fx,
    live: Value,
    [minted, resolution]: [Value; 2],
    ns: Value,
    record: Value,
    timer: Value,
) {
    fx.choose_in(
        lo,
        live,
        &[],
        |lo, fx| {
            let now = lo.now_ns(fx);
            let delay = timers::on_grid(fx, now, minted, resolution, ns);
            let due = timers::due_after(fx, now, delay);
            fx.store(due, timer, operation::DUE);
            push(lo, fx, timer);
            let spec = fx.offset(timer, operation::SIZE);
            lo.arm_timer(fx, timer, delay, spec);
            Vec::new()
        },
        |lo, fx| {
            lo.call_routine(fx, Routine::Cancel, &[record]);
            let paced = fx.int(PTR, kind::PACED);
            fx.store(paced, timer, operation::KIND);
            let now = lo.now_ns(fx);
            fx.store(now, timer, operation::DUE);
            push(lo, fx, timer);
            Vec::new()
        },
    );
}

/// `task::paced(clock, at_ns, body: b)`: calls `b` under a deadline due once `clock` reads
/// `at_ns`, or at the call for a stale clock, which no timer ends.
fn paced(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let clock = fx.load(PTR, supply.param(0).address(), 0);
    let (live, [minted, resolution, _, _]) = tables::snapshot(lo, fx, clock);
    let at = supply.param(1).scalar();
    let due = fx.choose_in(
        lo,
        live,
        &[PTR],
        |_, fx| {
            let end = timers::on_grid(fx, minted, minted, resolution, at);
            vec![timers::due_after(fx, minted, end)]
        },
        |lo, fx| vec![lo.now_ns(fx)],
    )[0];
    let deadline = fx.slot(operation::SIZE, 8);
    let paced = fx.int(PTR, kind::PACED);
    fx.store(paced, deadline, operation::KIND);
    fx.store(due, deadline, operation::DUE);
    push(lo, fx, deadline);
    let body_ty = fn_param(supply, 2);
    let called = lo.call_value(fx, &body_ty, supply.param(2).address(), &[]);
    pop(lo, fx, deadline);
    supply.forward_failure(lo, fx, &called);
    supply.ret(lo, fx, called.result);
}

/// `task::deadline_ns(clock)`: the earliest deadline the running task runs under, read on
/// `clock`; none outside a task, without a deadline or for a stale clock.
fn deadline_ns(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let clock = fx.load(PTR, supply.param(0).address(), 0);
    let (live, [minted, resolution, _, _]) = tables::snapshot(lo, fx, clock);
    let current = tasks::current_task(lo, fx);
    let inside = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
    let found = fx.choose(
        inside,
        &[types::I8, PTR],
        |fx| tasks::earliest_deadline(fx, current).to_vec(),
        |fx| vec![fx.int(types::I8, 0), fx.ptr(0)],
    );
    let present = fx.b.ins().band(live, found[0]);
    let before = fx.b.ins().icmp(IntCC::UnsignedLessThan, found[1], minted);
    let elapsed = fx.b.ins().isub(found[1], minted);
    let zero = fx.ptr(0);
    let elapsed = fx.b.ins().select(before, zero, elapsed);
    let reading = round_down(fx, resolution, elapsed);
    let option = supply.abi.result.clone();
    let layout = lo.layout(&option);
    let slot = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    lo.write_option(fx, &option, slot, present, Held::Scalar(reading));
    let held = lo.held_at(fx, &option, slot);
    supply.ret(lo, fx, held);
}

/// Makes a deadline's record the running task's innermost.
fn push(lo: &mut Lowering, fx: &mut Fx, deadline: Value) {
    let current = tasks::current_task(lo, fx);
    let outer = fx.load(PTR, current, task::DEADLINES);
    fx.store(outer, deadline, operation::LINK);
    fx.store(deadline, current, task::DEADLINES);
}

/// Takes the running task's innermost deadline, whose record is `deadline`, out of its
/// deadlines.
fn pop(lo: &mut Lowering, fx: &mut Fx, deadline: Value) {
    let current = tasks::current_task(lo, fx);
    let outer = fx.load(PTR, deadline, operation::LINK);
    fx.store(outer, current, task::DEADLINES);
}

/// Cancels a deadline's timeout unless it has expired, waits for its completion, and takes it
/// out of the running task's deadlines, of which it is the innermost.
fn disarm(lo: &mut Lowering, fx: &mut Fx, timer: Value) {
    let current = tasks::current_task(lo, fx);
    let waiter = fx.offset(timer, operation::WAITER);
    fx.atomic_store(current, waiter);
    lo.cancel_operation(fx, timer);
    tasks::wait_delivered(lo, fx, timer);
    pop(lo, fx, timer);
}
