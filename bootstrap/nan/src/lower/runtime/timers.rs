//! The timers of the clocks the runtime keeps itself, the seeded scheduler's virtual clock and a
//! kernel's clock: one list of operations ordered by the nanosecond each is due, then by the
//! order in which they were armed. A kernel's CPUs share the list behind the timer lock.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::Lowering;
use super::super::fx::{Fx, PTR};
use super::tasks::operation;
use super::{seeded, state};

/// The nanoseconds of a millisecond.
pub const NS_PER_MS: u64 = 1_000_000;

/// Arms a timer due at nanosecond `due`, after every timer due no later.
pub fn arm(lo: &mut Lowering, fx: &mut Fx, op: Value, due: Value) {
    fx.store(due, op, operation::DUE);
    let order = seeded::next_order(lo, fx);
    fx.store(order, op, operation::ORDER);
    let zero = fx.ptr(0);
    fx.store(zero, op, operation::WITHDRAW);
    fx.store(zero, op, operation::DONE);
    let state_address = lo.state(fx);
    let head = fx.load(PTR, state_address, state::TIMERS);
    let found = fx.repeat(&[head, zero], |fx, state| {
        let (current, previous) = (state[0], state[1]);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
        let earlier = fx.choose(
            present,
            &[types::I8],
            |fx| {
                let other = fx.load(PTR, current, operation::DUE);
                vec![fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, other, due)]
            },
            |fx| vec![fx.int(types::I8, 0)],
        )[0];
        let next = fx.choose(
            earlier,
            &[PTR, PTR],
            |fx| vec![fx.load(PTR, current, operation::NEXT), current],
            |_| vec![current, previous],
        );
        (earlier, next)
    });
    let (after, before) = (found[0], found[1]);
    fx.store(after, op, operation::NEXT);
    fx.store(before, op, operation::PREVIOUS);
    let has_after = fx.b.ins().icmp_imm_s(IntCC::NotEqual, after, 0);
    fx.when(has_after, |fx| fx.store(op, after, operation::PREVIOUS));
    let first = fx.b.ins().icmp_imm_s(IntCC::Equal, before, 0);
    fx.choose(
        first,
        &[],
        |fx| {
            fx.store(op, state_address, state::TIMERS);
            Vec::new()
        },
        |fx| {
            fx.store(op, before, operation::NEXT);
            Vec::new()
        },
    );
}

/// Takes a timer out of the timers.
pub fn disarm(lo: &mut Lowering, fx: &mut Fx, op: Value) {
    let state_address = lo.state(fx);
    let next = fx.load(PTR, op, operation::NEXT);
    let previous = fx.load(PTR, op, operation::PREVIOUS);
    let first = fx.b.ins().icmp_imm_s(IntCC::Equal, previous, 0);
    fx.choose(
        first,
        &[],
        |fx| {
            fx.store(next, state_address, state::TIMERS);
            Vec::new()
        },
        |fx| {
            fx.store(next, previous, operation::NEXT);
            Vec::new()
        },
    );
    let has_next = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next, 0);
    fx.when(has_next, |fx| fx.store(previous, next, operation::PREVIOUS));
}

/// The nanosecond `delay` after `now`, the latest one when that is past it.
pub fn due_after(fx: &mut Fx, now: Value, delay: Value) -> Value {
    let (due, overflow) = fx.b.ins().uadd_overflow(now, delay);
    let latest = fx.int(PTR, -1);
    fx.b.ins().select(overflow, latest, due)
}

/// The delay from `now` until `ns` nanoseconds have passed and the time elapsed since `origin`
/// is a multiple of `resolution`, zero standing for one, saturating.
pub fn on_grid(fx: &mut Fx, now: Value, origin: Value, resolution: Value, ns: Value) -> Value {
    let one = fx.ptr(1);
    let resolution = fx.b.ins().umax(resolution, one);
    let elapsed = fx.b.ins().isub(now, origin);
    let end = due_after(fx, elapsed, ns);
    let rest = fx.b.ins().urem(end, resolution);
    let short = fx.b.ins().isub(resolution, rest);
    let rounded = due_after(fx, end, short);
    let exact = fx.b.ins().icmp_imm_s(IntCC::Equal, rest, 0);
    let end = fx.b.ins().select(exact, end, rounded);
    fx.b.ins().isub(end, elapsed)
}

/// The nanoseconds of `ms` milliseconds, the most a word holds when they are more.
pub fn from_ms(fx: &mut Fx, ms: Value) -> Value {
    let per_ms = fx.ptr(NS_PER_MS);
    let low = fx.b.ins().imul(ms, per_ms);
    let high = fx.b.ins().umulhi(ms, per_ms);
    let overflow = fx.b.ins().icmp_imm_s(IntCC::NotEqual, high, 0);
    let latest = fx.int(PTR, -1);
    fx.b.ins().select(overflow, latest, low)
}
