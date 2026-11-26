//! Capability kinds: minters, reaches and derivations, the operations of crossing kinds through the
//! handles holding their capabilities, and the handles of the capabilities a space's runtime holds
//! for its kernel's with the revocation of what it minted for them. A minter is the holding of its
//! state's record, which is a shared value's whose count holds one for the minter and one for each
//! reach or derivation in flight, and which the outermost domain reaches. An entry of a kind holds
//! that record, its own record, which the capability table keeps given its address and size, and
//! the kind's method table for `sys::Crossing`; a reach pins the state under the table lock, which
//! copies the record out, and calls its body outside it.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC};

use super::super::Lowering;
use super::super::body::Held;
use super::super::fx::{Fx, PTR};
use super::super::runtime::tables::{self, kind};
use super::super::runtime::{Routine, holdings, state};
use super::Supply;
use super::codecs::protocols::handle;
use super::mem::lists::fn_param;
use super::mem::stores::{contents_at, new_shared, release_shared, transfer_shared};
use super::served;
use super::sys::fail_with;
use crate::check::ty::{Ty, no_args};

/// The fewest bytes of the slot a pin copies a record into: the two words a record its entry's
/// state words hold fills.
const RECORD_SLOT: u64 = 16;

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    match supply.path.as_str() {
        "sys::Minter.of" => of(lo, fx, supply),
        "sys::Minter.mint" => {
            let minter = holdings::payload(fx, supply.param(0).address());
            let (record, size) = record_at(lo, fx, supply.param(1), &supply.type_arg(2));
            let table = crossing_table(lo, fx, &supply.type_arg(0));
            let handle = lo.call_routine(fx, Routine::KindMint, &[minter, record, size, table])[0];
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
        "sys::Handle.crosses" => {
            let word = fx.load(PTR, supply.param(0).address(), handle::HANDLE);
            let (live, _, table) = tables::crossing(lo, fx, word);
            let crossing = fx.b.ins().icmp_imm_s(IntCC::NotEqual, table, 0);
            let crosses = fx.b.ins().band(live, crossing);
            supply.ret(lo, fx, Held::Scalar(crosses));
        }
        "sys::Handle.invoke" | "sys::Handle.call" => crossed_operation(lo, fx, supply),
        "nanos::crossing_handle" => crossing_handle(lo, fx, supply),
        "nanos::handle_word" => {
            let word = fx.load(PTR, supply.param(0).address(), handle::HANDLE);
            supply.ret(lo, fx, Held::Scalar(word));
        }
        "nanos::revoke" => {
            let handle = fx.load(PTR, supply.param(0).address(), 0);
            lo.call_routine(fx, Routine::Revoke, &[handle]);
            supply.done(lo, fx);
        }
        "nanos::handle_hash" => {
            let hash = fx.offset(supply.param(0).address(), handle::HASH);
            supply.ret(lo, fx, Held::Address(hash));
        }
        _ => return false,
    }
    true
}

/// The method table of a kind for `sys::Crossing`, zero for a kind that does not cross.
fn crossing_table(lo: &mut Lowering, fx: &mut Fx, kind: &Ty) -> Value {
    let crossing = lo.ck.db.std_item("sys", "Crossing").expect("sys::Crossing");
    if !lo.ck.implements(kind, crossing) {
        return fx.ptr(0);
    }
    let (table, _) = lo.dyn_table(kind, crossing, &no_args());
    let table = lo.data_ref(fx, table);
    fx.b.ins().symbol_value(PTR, table)
}

/// `handle.invoke(operation, args, alloc)` or `call`: the method of its name of the kind through
/// which the capability `handle` holds crosses, its failure becoming the operation's;
/// `Revoked.Stale` for a stale capability and `Refused` for one that does not cross.
fn crossed_operation(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let word = fx.load(PTR, supply.param(0).address(), handle::HANDLE);
    let (live, service, table) = tables::crossing(lo, fx, word);
    let stale = fx.b.ins().bxor_imm_s(live, 1);
    fx.when(stale, |fx| fail_with(lo, fx, supply, "Revoked"));
    let refused = fx.b.ins().icmp_imm_s(IntCC::Equal, table, 0);
    fx.when(refused, |fx| fail_with(lo, fx, supply, "Refused"));
    let member = supply.member();
    let called = served::call(
        lo,
        fx,
        "Crossing",
        member,
        (service, table),
        &supply.params[1..],
    );
    supply.forward_failure(lo, fx, &called);
    supply.ret(lo, fx, called.result);
}

/// `nanos::crossing_handle(service, hash)`: a handle holding a capability of the crossing kind
/// whose handle hash is `hash`, served by the kind `service` through `sys::Crossing`, its entry
/// deriving from `service`'s; `Revoked.Stale` when `service` is stale.
fn crossing_handle(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let service_ty = supply.type_arg(0);
    let service = fx.load(PTR, supply.param(0).address(), 0);
    let crossing = lo.ck.db.std_item("sys", "Crossing").expect("sys::Crossing");
    let (table, _) = lo.dyn_table(&service_ty, crossing, &no_args());
    let table = lo.data_ref(fx, table);
    let table = fx.b.ins().symbol_value(PTR, table);
    let derived = tables::served(lo, fx, kind::CROSSED, service, table);
    let stale = fx.b.ins().icmp_imm_s(IntCC::Equal, derived, -1);
    fx.when(stale, |fx| fail_with(lo, fx, supply, "Revoked"));
    let sret = supply.sret.expect("a handle's address");
    fx.store(derived, sret, handle::HANDLE);
    let at = fx.offset(sret, handle::HASH);
    lo.copy(fx, at, supply.param(1).address(), 33, 1);
    supply.done(lo, fx);
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
    let record = pin_slot(lo, fx, &supply.type_arg(2));
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
    let record = pin_slot(lo, fx, &record_ty);
    let minter = lo.call_routine(fx, Routine::KindPin, &[handle, record])[0];
    let stale = fx.b.ins().icmp_imm_s(IntCC::Equal, minter, 0);
    fx.when(stale, |fx| fail_with(lo, fx, supply, "Revoked"));
    let called = call_pinned(lo, fx, supply, &state_ty, minter, record);
    unpin(lo, fx, &state_ty, minter);
    let (narrowed, size) = record_at(lo, fx, called.result, &record_ty);
    let derived = lo.call_routine(fx, Routine::KindDerive, &[handle, narrowed, size])[0];
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

/// The address of a slot holding a record held one way or another, and its size.
fn record_at(lo: &mut Lowering, fx: &mut Fx, record: Held, record_ty: &Ty) -> (Value, Value) {
    let layout = lo.layout(record_ty);
    let slot = fx.slot(layout.size, layout.align);
    lo.store_held(fx, slot, record, record_ty);
    (slot, fx.ptr(layout.size))
}

/// A slot a pin copies a record of a type into.
fn pin_slot(lo: &mut Lowering, fx: &mut Fx, record_ty: &Ty) -> Value {
    let layout = lo.layout(record_ty);
    fx.slot(layout.size.max(RECORD_SLOT), layout.align.max(8))
}
