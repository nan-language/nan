//! `hw::ExtendedState`, the address of the frame holding its save area: `of` writes the initial
//! state there and `into_frame` gives the frame back, `vector` and `set_vector` read and write a
//! vector register of the saved state, and `with` runs its body with the running task owning
//! that state; and the save area an optional one holds, which the user path owns.

use cranelift_codegen::ir::{InstBuilder, Value};

use super::super::super::Lowering;
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::Routine;
use super::super::Supply;
use super::super::mem::lists::fn_param;
use super::cpu::{word, word_result};
use super::forward;
use crate::check::ty::Ty;

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let base = word(fx, supply.param(0));
    match supply.member() {
        "of" => {
            lo.initial_extended(fx, base);
            word_result(lo, fx, supply, base);
        }
        "into_frame" => word_result(lo, fx, supply, base),
        "vector" => {
            let index = supply.param(1).scalar();
            let index = fx.b.ins().uextend(PTR, index);
            let (at, held) = lo.extended_vector(fx, base, index);
            let zero = fx.ptr(0);
            let value = fx.slot(16, 8);
            for half in [0, 8] {
                let bits = fx.load(PTR, at, half);
                let bits = fx.b.ins().select(held, bits, zero);
                fx.store(bits, value, half);
            }
            let held = lo.held_at(fx, &supply.abi.result, value);
            supply.ret(lo, fx, held);
        }
        "set_vector" => {
            let index = supply.param(1).scalar();
            let index = fx.b.ins().uextend(PTR, index);
            let value_ty = supply.abi.params[2].0.ty().clone();
            let value = fx.slot(16, 8);
            lo.store_held(fx, value, supply.param(2), &value_ty);
            lo.hold_extended_vectors(fx, base);
            let (at, _) = lo.extended_vector(fx, base, index);
            for half in [0, 8] {
                let bits = fx.load(PTR, value, half);
                fx.store(bits, at, half);
            }
            supply.done(lo, fx);
        }
        "with" => {
            let previous = lo.call_routine(fx, Routine::Extended, &[base])[0];
            let body_ty = fn_param(supply, 1);
            let called = lo.call_value(fx, &body_ty, supply.param(1).address(), &[]);
            lo.call_routine(fx, Routine::Extended, &[previous]);
            forward(lo, fx, supply, called);
        }
        member => panic!("no lowering of hw::ExtendedState.{member}"),
    }
}

/// The save area of the `Option[hw::ExtendedState]` at an address, zero for `None`.
pub fn optional_area(lo: &mut Lowering, fx: &mut Fx, ty: &Ty, address: Value) -> Value {
    let present = lo.variant_is(fx, address, ty, "Some");
    let payload = lo.payload_address(fx, ty, "Some", "", address);
    let base = fx.load(PTR, payload, 0);
    let zero = fx.ptr(0);
    fx.b.ins().select(present, base, zero)
}
