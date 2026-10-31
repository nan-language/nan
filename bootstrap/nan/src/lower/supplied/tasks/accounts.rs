//! Executors and their accounts: `task::Executor.coarse`, `split` and `serial`, which derive an
//! entry from an executor's, and the members of `task::Account`, which owns the entry `split` or
//! `serial` derived. An executor's share is one word: its weight in the low 32 bits, its class in
//! the next 8 and its reservation in parts per million above them.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::super::Lowering;
use super::super::super::body::Held;
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::tables::{self, entry};
use super::super::super::runtime::{Routine, state};
use super::super::Supply;
use super::super::sys;
use crate::check::ty::Ty;

/// The bit positions of a share's class and reservation in its word.
const CLASS_SHIFT: i64 = 32;
const RESERVE_SHIFT: i64 = 40;

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    match supply.path.as_str() {
        "task::Executor.coarse" => sys::coarse(lo, fx, supply, tables::EXECUTOR_RESOLUTION),
        "task::Executor.split" => derive(lo, fx, supply, |lo, fx, record| {
            let share_ty = supply.abi.params[1].0.ty().clone();
            let share = supply.param(1);
            let address = match share {
                Held::Address(address) => address,
                other => {
                    let layout = lo.layout(&share_ty);
                    let slot = fx.slot(layout.size, layout.align);
                    lo.store_held(fx, slot, other, &share_ty);
                    slot
                }
            };
            let packed = pack_share(lo, fx, &share_ty, address);
            fx.store(packed, record, tables::EXECUTOR_SHARE);
        }),
        "task::Executor.serial" => derive(lo, fx, supply, |_, fx, record| {
            let seed = supply.param(1).scalar();
            fx.store(seed, record, entry::SEED);
        }),
        "task::Account.executor" => {
            let handle = fx.load(PTR, supply.param(0).address(), 0);
            let sret = supply.sret.expect("an executor's address");
            fx.store(handle, sret, 0);
            supply.done(lo, fx);
        }
        "task::Account.used_ns" => {
            let handle = fx.load(PTR, supply.param(0).address(), 0);
            let state_address = lo.state(fx);
            let lock = fx.offset(state_address, state::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let record = tables::live_entry(lo, fx, handle);
            let live = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
            let used = fx.choose(
                live,
                &[PTR],
                |fx| vec![fx.load(PTR, record, entry::USED)],
                |fx| vec![fx.ptr(0)],
            )[0];
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            supply.ret(lo, fx, Held::Scalar(used));
        }
        "task::Account.close" => {
            let handle = fx.load(PTR, supply.param(0).address(), 0);
            lo.call_routine(fx, Routine::Revoke, &[handle]);
            supply.done(lo, fx);
        }
        path => unreachable!("an executor operation {path}"),
    }
}

/// Returns the account owning an entry derived from the executor at the operation's first
/// parameter, which `fill` completes under the lock before any task reads it; for `serial`, the
/// entry names itself as its tasks' serial executor. An account of a stale executor holds that
/// executor's handle.
fn derive(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    fill: impl FnOnce(&mut Lowering, &mut Fx, Value),
) {
    let handle = fx.load(PTR, supply.param(0).address(), 0);
    let word = fx.ptr(tables::EXECUTOR_RESOLUTION);
    let zero = fx.ptr(0);
    let derived = lo.call_routine(fx, Routine::Derive, &[handle, word, zero, zero, zero])[0];
    let live = fx.b.ins().icmp_imm_s(IntCC::NotEqual, derived, -1);
    fx.when(live, |fx| {
        let state_address = lo.state(fx);
        let lock = fx.offset(state_address, state::LOCK);
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let record = tables::entry_at(lo, fx, derived);
        if supply.path == "task::Executor.serial" {
            fx.store(derived, record, entry::SERIAL);
        }
        fill(lo, fx, record);
        lo.call_routine(fx, Routine::Unlock, &[lock]);
    });
    let owned = fx.b.ins().select(live, derived, handle);
    let sret = supply.sret.expect("an account's address");
    fx.store(owned, sret, 0);
    supply.done(lo, fx);
}

/// The word of a `task::Share` at an address.
fn pack_share(lo: &mut Lowering, fx: &mut Fx, share_ty: &Ty, address: Value) -> Value {
    let [weight, class, reserve] = share_fields(lo, share_ty);
    let weight = fx.load(types::I32, address, weight);
    let weight = fx.b.ins().uextend(PTR, weight);
    let class = fx.load(types::I8, address, class);
    let class = fx.b.ins().uextend(PTR, class);
    let class = fx.b.ins().ishl_imm_s(class, CLASS_SHIFT);
    let reserve = fx.load(types::I32, address, reserve);
    let reserve = fx.b.ins().uextend(PTR, reserve);
    let reserve = fx.b.ins().ishl_imm_s(reserve, RESERVE_SHIFT);
    let packed = fx.b.ins().bor(weight, class);
    fx.b.ins().bor(packed, reserve)
}

/// Writes the `task::Share` of a word at an address.
pub fn unpack_share(lo: &mut Lowering, fx: &mut Fx, share_ty: &Ty, packed: Value, address: Value) {
    let [weight_at, class_at, reserve_at] = share_fields(lo, share_ty);
    let weight = fx.b.ins().ireduce(types::I32, packed);
    fx.store(weight, address, weight_at);
    let class = fx.b.ins().ushr_imm_s(packed, CLASS_SHIFT);
    let class = fx.b.ins().ireduce(types::I8, class);
    fx.store(class, address, class_at);
    let reserve = fx.b.ins().ushr_imm_s(packed, RESERVE_SHIFT);
    let reserve = fx.b.ins().ireduce(types::I32, reserve);
    fx.store(reserve, address, reserve_at);
}

/// The offsets of a `task::Share`'s weight, class and reservation.
fn share_fields(lo: &mut Lowering, share_ty: &Ty) -> [u64; 3] {
    ["weight", "class", "reserve_ppm"]
        .map(|name| lo.layouts.field(lo.ck, share_ty, None, name).offset)
}
