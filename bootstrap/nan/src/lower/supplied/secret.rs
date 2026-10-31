//! Secret values: each member of `core::Secret` lowered to one fixed sequence of integer
//! instructions, without a branch, a division or an address computed from its operands, but for
//! the shifts' check of their public amount.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{InstBuilder, types};

use super::super::Lowering;
use super::super::body::Held;
use super::super::fx::Fx;
use super::super::layout::scalar_type;
use super::Supply;
use crate::check::ty::{Scalar, Ty};

/// Defines a member of `core::Secret`.
pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    if !supply.path.starts_with("core::Secret.") {
        return false;
    }
    let Ty::Scalar(scalar) = supply.type_arg(0) else {
        panic!("a Secret of a scalar in {}", supply.path);
    };
    let value = |index: usize| supply.param(index).scalar();
    let signed = scalar.is_signed();
    let compare = |fx: &mut Fx, cc: IntCC| fx.b.ins().icmp(cc, value(0), value(1));
    let held = match supply.member() {
        "of" | "reveal" => value(0),
        "wrapping_add" => fx.b.ins().iadd(value(0), value(1)),
        "wrapping_sub" => fx.b.ins().isub(value(0), value(1)),
        "wrapping_mul" => fx.b.ins().imul(value(0), value(1)),
        "mul_high" if signed => fx.b.ins().smulhi(value(0), value(1)),
        "mul_high" => fx.b.ins().umulhi(value(0), value(1)),
        "bit_and" => fx.b.ins().band(value(0), value(1)),
        "bit_or" => fx.b.ins().bor(value(0), value(1)),
        "bit_xor" => fx.b.ins().bxor(value(0), value(1)),
        "bit_not" if scalar == Scalar::Bool => fx.b.ins().bxor_imm_s(value(0), 1),
        "bit_not" => fx.b.ins().bnot(value(0)),
        "shl" | "shr" => {
            let amount = super::super::body::widen(fx, value(1));
            let too_far = fx.b.ins().icmp_imm_s(
                IntCC::UnsignedGreaterThanOrEqual,
                amount,
                i64::from(scalar.bits()),
            );
            fx.when(too_far, |fx| {
                super::super::runtime::report::raise_kind(lo, fx, "trap.shift")
            });
            match (supply.member(), signed) {
                ("shl", _) => fx.b.ins().ishl(value(0), value(1)),
                (_, true) => fx.b.ins().sshr(value(0), value(1)),
                _ => fx.b.ins().ushr(value(0), value(1)),
            }
        }
        "rotate_left" => {
            let width = fx.int(types::I32, i64::from(scalar.bits()));
            let turn = fx.b.ins().isub(width, value(1));
            fx.b.ins().rotr(value(0), turn)
        }
        "rotate_right" => fx.b.ins().rotr(value(0), value(1)),
        "eq" => compare(fx, IntCC::Equal),
        "ne" => compare(fx, IntCC::NotEqual),
        "lt" if signed => compare(fx, IntCC::SignedLessThan),
        "lt" => compare(fx, IntCC::UnsignedLessThan),
        "le" if signed => compare(fx, IntCC::SignedLessThanOrEqual),
        "le" => compare(fx, IntCC::UnsignedLessThanOrEqual),
        "gt" if signed => compare(fx, IntCC::SignedGreaterThan),
        "gt" => compare(fx, IntCC::UnsignedGreaterThan),
        "ge" if signed => compare(fx, IntCC::SignedGreaterThanOrEqual),
        "ge" => compare(fx, IntCC::UnsignedGreaterThanOrEqual),
        "select" => fx.b.ins().select(value(0), value(1), value(2)),
        "widen" => {
            let Ty::Scalar(source) = supply.type_arg(1) else {
                panic!("a Secret of a scalar widened");
            };
            match source.is_signed() {
                true => fx.b.ins().sextend(scalar_type(scalar), value(0)),
                false => fx.b.ins().uextend(scalar_type(scalar), value(0)),
            }
        }
        "truncate" => super::super::place::resize(fx, value(0), scalar_type(scalar)),
        _ => return false,
    };
    supply.ret(lo, fx, Held::Scalar(held));
    true
}
