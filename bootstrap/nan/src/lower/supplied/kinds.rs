//! Capability kinds: minters, reaches and derivations. A minter is the holding of its state's
//! record, which is a shared value's whose count holds one for the minter and one for each reach
//! or derivation in flight, and which the outermost domain reaches. An entry of a kind holds that
//! record and the two words of its own record; a reach pins the state under the table lock and
//! calls its body outside it.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC};

use super::super::Lowering;
use super::super::body::Held;
use super::super::fx::{Fx, PTR};
use super::super::runtime::{Routine, holdings, state};
use super::Supply;
use super::mem::lists::fn_param;
use super::mem::stores::{contents_at, new_shared, release_shared, transfer_shared};
use super::sys::fail_with;
use crate::check::ty::Ty;

/// The bytes of the slot holding a record's two words.
const RECORD_SLOT: u64 = 16;

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    match supply.path.as_str() {
        "sys::Minter.of" => of(lo, fx, supply),
        "sys::Minter.mint" => {
            let minter = holdings::payload(fx, supply.param(0).address());
            let (low, high) = record_words(lo, fx, supply.param(1), &supply.type_arg(2));
            let handle = lo.call_routine(fx, Routine::KindMint, &[minter, low, high])[0];
            let sret = supply.sret.expect("a capability's address");
            fx.store(handle, sret, 0);
            supply.done(lo, fx);
        }
        "sys::Minter.revoke" => {
            let minter = holdings::payload(fx, supply.param(0).address());
            let handle = fx.load(PTR, supply.param(1).address(), 0);
            lo.call_routine(fx, Routine::KindRevoke, &[handle, minter]);
            supply.done(lo, fx);
        }
        "sys::Minter.close" => {
            let holding = fx.load(PTR, supply.param(0).address(), 0);
            let minter = holdings::payload(fx, supply.param(0).address());
            lo.call_routine(fx, Routine::KindClose, &[minter]);
            lo.call_routine(fx, Routine::Unhold, &[holding]);
            unpin(lo, fx, &supply.type_arg(1), minter);
            supply.done(lo, fx);
        }
        "sys::reach" => reach(lo, fx, supply),
        "sys::derive" => derive(lo, fx, supply),
        _ => return false,
    }
    true
}

/// `sys::Minter.of(state, alloc)`: the state moves into a shared record drawn through `alloc`,
/// which passes to the outermost domain, and the minter holds it.
fn of(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let state_ty = supply.type_arg(1);
    let cell = fx.load(PTR, supply.param(1).address(), 0);
    let record = new_shared(lo, fx, cell, &state_ty, supply.param(0));
    let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
    fx.when(failed, |fx| supply.exhausted(lo, fx, &[]));
    let state_address = lo.state(fx);
    let process = fx.load(PTR, state_address, state::PROCESS);
    let held = fx.slot(8, 8);
    fx.store(record, held, 0);
    transfer_shared(lo, fx, &state_ty, held, process);
    let kind = fx.ptr(holdings::kind::MINTER as u64);
    let holding = lo.call_routine(fx, Routine::Hold, &[kind, record])[0];
    let sret = supply.sret.expect("a minter's address");
    fx.store(holding, sret, 0);
    supply.done(lo, fx);
}

/// `sys::reach(c, body)`: `None` for a stale handle, else `Some` of what `body` returns given
/// the pinned state and the entry's record, its failure becoming the operation's.
fn reach(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let option = supply.abi.result.clone();
    let state_ty = supply.type_arg(1);
    let handle = fx.load(PTR, supply.param(0).address(), 0);
    let record = fx.slot(RECORD_SLOT, 8);
    let minter = lo.call_routine(fx, Routine::KindPin, &[handle, record])[0];
    let live = fx.b.ins().icmp_imm_s(IntCC::NotEqual, minter, 0);
    fx.when(live, |fx| {
        let called = call_pinned(lo, fx, supply, &state_ty, minter, record);
        unpin(lo, fx, &state_ty, minter);
        supply.forward_failure(lo, fx, &called);
        let some = lo.variant_value(fx, &option, "Some", &[called.result], None);
        supply.ret(lo, fx, some);
    });
    let none = lo.none_value(fx, &option);
    supply.ret(lo, fx, none);
}

/// `sys::derive(c, narrow)`: an entry deriving from `c`'s, recording what `narrow` returns given
/// the pinned state and `c`'s record; `Revoked.Stale` when `c` is stale before or after.
fn derive(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let state_ty = supply.type_arg(1);
    let record_ty = supply.type_arg(2);
    let handle = fx.load(PTR, supply.param(0).address(), 0);
    let record = fx.slot(RECORD_SLOT, 8);
    let minter = lo.call_routine(fx, Routine::KindPin, &[handle, record])[0];
    let stale = fx.b.ins().icmp_imm_s(IntCC::Equal, minter, 0);
    fx.when(stale, |fx| fail_with(lo, fx, supply, "Revoked"));
    let called = call_pinned(lo, fx, supply, &state_ty, minter, record);
    unpin(lo, fx, &state_ty, minter);
    let (low, high) = record_words(lo, fx, called.result, &record_ty);
    let derived = lo.call_routine(fx, Routine::KindDerive, &[handle, low, high])[0];
    let stale = fx.b.ins().icmp_imm_s(IntCC::Equal, derived, -1);
    fx.when(stale, |fx| fail_with(lo, fx, supply, "Revoked"));
    let sret = supply.sret.expect("a capability's address");
    fx.store(derived, sret, 0);
    supply.done(lo, fx);
}

/// Calls the operation's function argument with the state of a pinned minter and the record the
/// pin wrote.
fn call_pinned(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    state_ty: &Ty,
    minter: Value,
    record: Value,
) -> super::super::call::Called {
    let at = contents_at(lo, state_ty);
    let state = fx.offset(minter, at);
    let body_ty = fn_param(supply, 1);
    lo.call_value(
        fx,
        &body_ty,
        supply.param(1).address(),
        &[Held::Address(state), Held::Address(record)],
    )
}

/// Ends one holder of a minter's state: the last releases the state and frees its record.
fn unpin(lo: &mut Lowering, fx: &mut Fx, state_ty: &Ty, minter: Value) {
    let held = fx.slot(8, 8);
    fx.store(minter, held, 0);
    release_shared(lo, fx, state_ty, held);
}

/// The two words of a record held one way or another, its bytes past its size zero.
fn record_words(lo: &mut Lowering, fx: &mut Fx, record: Held, record_ty: &Ty) -> (Value, Value) {
    let slot = fx.slot(RECORD_SLOT, 8);
    let zero = fx.ptr(0);
    fx.store(zero, slot, 0);
    fx.store(zero, slot, 8);
    lo.store_held(fx, slot, record, record_ty);
    (fx.load(PTR, slot, 0), fx.load(PTR, slot, 8))
}
