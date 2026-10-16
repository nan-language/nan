//! Holdings: what a domain owns that settlement releases once the domain is abandoned, each a
//! record linked in its owner's list: a file or listener, whose descriptor closes; a
//! connection, which is abandoned; a revoker, which revokes; a budget or an allocator's revoker,
//! whose cell closes; and a channel's sender or receiver, which counts as released.
//!
//! A holding's record is its links, its owner, zero while no domain owns it, its kind and what
//! it holds: a descriptor, a capability or cell handle, or a channel's record. The lock of the
//! cell and capability tables guards the lists.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::Lowering;
use super::super::body::Held;
use super::super::fx::{Fx, PTR};
use super::super::supplied::tasks::channels;
use super::super::transfer;
use super::{Routine, state, tasks};

pub mod holding {
    pub const NEXT: u64 = 0;
    pub const PREVIOUS: u64 = 8;
    pub const OWNER: u64 = 16;
    pub const KIND: u64 = 24;
    pub const PAYLOAD: u64 = 32;
    pub const SIZE: u64 = 40;
}

pub mod kind {
    pub const CLOSE: i64 = 0;
    pub const ABANDON: i64 = 1;
    pub const REVOKE: i64 = 2;
    pub const CLOSE_CELL: i64 = 3;
    pub const SENDER: i64 = 4;
    pub const RECEIVER: i64 = 5;
}

pub fn define(lo: &mut Lowering, fx: &mut Fx, routine: Routine) {
    let params = fx.entry();
    let state_address = lo.state(fx);
    let lock = fx.offset(state_address, state::LOCK);
    match routine {
        Routine::Hold => {
            let one = fx.ptr(1);
            let size = fx.ptr(holding::SIZE);
            let record = lo.call_libc(fx, "calloc", &[one, size], &[PTR])[0];
            let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
            fx.when(missing, |fx| {
                super::report::raise_kind(lo, fx, "trap.exhausted")
            });
            fx.store(params[0], record, holding::KIND);
            fx.store(params[1], record, holding::PAYLOAD);
            let owner = tasks::current_domain(lo, fx);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            link(fx, record, owner);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            fx.b.ins().return_(&[record]);
        }
        Routine::Unhold => {
            lo.call_routine(fx, Routine::Lock, &[lock]);
            unlink(fx, params[0]);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            lo.call_libc(fx, "free", &[params[0]], &[]);
            fx.b.ins().return_(&[]);
        }
        Routine::Rehome => {
            let (record, owner) = (params[0], params[1]);
            let raise = fx.b.ins().band_imm_s(owner, transfer::RAISE);
            let raise = fx.b.ins().icmp_imm_s(IntCC::NotEqual, raise, 0);
            let to = fx.b.ins().band_imm_s(owner, !transfer::RAISE);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let current = fx.load(PTR, record, holding::OWNER);
            let inside = tasks::nested_in(fx, current, to);
            let kept = fx.b.ins().bxor_imm_s(inside, 1);
            let kept = fx.b.ins().band(raise, kept);
            let moves = fx.b.ins().bxor_imm_s(kept, 1);
            fx.when(moves, |fx| {
                unlink(fx, record);
                link(fx, record, to);
            });
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            fx.b.ins().return_(&[]);
        }
        _ => unreachable!("a holding routine"),
    }
}

/// What a holding holds, from the address of the word naming its record.
pub fn payload(fx: &mut Fx, address: Value) -> Value {
    let record = fx.load(PTR, address, 0);
    fx.load(PTR, record, holding::PAYLOAD)
}

/// Links a holding first in its owner's list, or leaves it unowned for a zero owner. The
/// caller holds the lock.
fn link(fx: &mut Fx, record: Value, owner: Value) {
    fx.store(owner, record, holding::OWNER);
    let zero = fx.ptr(0);
    fx.store(zero, record, holding::PREVIOUS);
    fx.store(zero, record, holding::NEXT);
    let owned = fx.b.ins().icmp_imm_s(IntCC::NotEqual, owner, 0);
    fx.when(owned, |fx| {
        let head = fx.load(PTR, owner, tasks::domain::HOLDINGS);
        fx.store(head, record, holding::NEXT);
        let has_head = fx.b.ins().icmp_imm_s(IntCC::NotEqual, head, 0);
        fx.when(has_head, |fx| fx.store(record, head, holding::PREVIOUS));
        fx.store(record, owner, tasks::domain::HOLDINGS);
    });
}

/// Takes a holding out of its owner's list. The caller holds the lock.
fn unlink(fx: &mut Fx, record: Value) {
    let owner = fx.load(PTR, record, holding::OWNER);
    let owned = fx.b.ins().icmp_imm_s(IntCC::NotEqual, owner, 0);
    fx.when(owned, |fx| {
        let next = fx.load(PTR, record, holding::NEXT);
        let previous = fx.load(PTR, record, holding::PREVIOUS);
        let has_previous = fx.b.ins().icmp_imm_s(IntCC::NotEqual, previous, 0);
        fx.choose(
            has_previous,
            &[],
            |fx| {
                fx.store(next, previous, holding::NEXT);
                Vec::new()
            },
            |fx| {
                fx.store(next, owner, tasks::domain::HOLDINGS);
                Vec::new()
            },
        );
        let has_next = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next, 0);
        fx.when(has_next, |fx| fx.store(previous, next, holding::PREVIOUS));
    });
}

/// Takes every holding out of a domain's list, giving the first of them, still linked to the
/// next.
fn take_all(lo: &mut Lowering, fx: &mut Fx, domain: Value) -> Value {
    let state_address = lo.state(fx);
    let lock = fx.offset(state_address, state::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let head = fx.load(PTR, domain, tasks::domain::HOLDINGS);
    let zero = fx.ptr(0);
    fx.store(zero, domain, tasks::domain::HOLDINGS);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    head
}

/// Runs `visit` on each holding of a chain from `first`, reading each one's link first.
fn each(fx: &mut Fx, first: Value, visit: &mut dyn FnMut(&mut Fx, Value)) {
    let any = fx.b.ins().icmp_imm_s(IntCC::NotEqual, first, 0);
    fx.when(any, |fx| {
        fx.repeat(&[first], |fx, state| {
            let record = state[0];
            let next = fx.load(PTR, record, holding::NEXT);
            visit(fx, record);
            let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next, 0);
            (more, vec![next])
        });
    });
}

/// Releases every holding of an abandoned domain without waiting, as settlement does.
pub fn release_all(lo: &mut Lowering, fx: &mut Fx, domain: Value) {
    let first = take_all(lo, fx, domain);
    each(fx, first, &mut |fx, record| {
        let kind = fx.load(PTR, record, holding::KIND);
        let payload = fx.load(PTR, record, holding::PAYLOAD);
        let is = |fx: &mut Fx, wanted: i64| fx.b.ins().icmp_imm_s(IntCC::Equal, kind, wanted);
        for (wanted, platform) in [(kind::CLOSE, "release"), (kind::ABANDON, "conn_abandon")] {
            let this = is(fx, wanted);
            fx.when(this, |fx| {
                let descriptor = fx.b.ins().ireduce(types::I32, payload);
                lo.platform_call(fx, platform, &[Held::Scalar(descriptor)]);
            });
        }
        for (wanted, routine) in [
            (kind::REVOKE, Routine::Revoke),
            (kind::CLOSE_CELL, Routine::Close),
        ] {
            let this = is(fx, wanted);
            fx.when(this, |fx| {
                lo.call_routine(fx, routine, &[payload]);
            });
        }
        for (wanted, sender) in [(kind::SENDER, true), (kind::RECEIVER, false)] {
            let this = is(fx, wanted);
            fx.when(this, |fx| channels::settle_end(lo, fx, payload, sender));
        }
        lo.call_libc(fx, "free", &[record], &[]);
    });
}

/// Passes every holding of a domain that has ended to another.
pub fn pass_all(lo: &mut Lowering, fx: &mut Fx, from: Value, to: Value) {
    let first = take_all(lo, fx, from);
    let state_address = lo.state(fx);
    let lock = fx.offset(state_address, state::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    each(fx, first, &mut |fx, record| link(fx, record, to));
    lo.call_routine(fx, Routine::Unlock, &[lock]);
}
