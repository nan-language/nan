//! Secret values: each member of `core::Secret` lowered to one fixed sequence of integer
//! instructions, without a branch, a division or an address computed from its operands, but for
//! the shifts' check of their public amount; over a lane type, its lanes' loads and stores at its
//! operands' addresses.

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
    let chosen = (supply.member() == "select").then(|| supply.type_arg(1));
    if let Some(Ty::Item(item, _)) = chosen.or_else(|| Some(supply.type_arg(0))) {
        let lanes = lo.ck.db.canonical(item);
        lower_lanes(lo, fx, supply, &lanes);
        return true;
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
                true => {
                    fx.b.ins()
                        .sextend(scalar_type(scalar, lo.machine()), value(0))
                }
                false => {
                    fx.b.ins()
                        .uextend(scalar_type(scalar, lo.machine()), value(0))
                }
            }
        }
        "truncate" => super::super::place::resize(fx, value(0), scalar_type(scalar, lo.machine())),
        _ => return false,
    };
    supply.ret(lo, fx, Held::Scalar(held));
    true
}

/// Defines a member of `core::Secret` over an integer lane type, its values held in memory: the
/// lanes computed with vector instructions on a host target and one by one on a machine target,
/// the equality of all lanes over the two 64-bit halves, and `select` through a mask of the
/// whole value.
fn lower_lanes(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, lanes: &str) {
    let (ty, signed) = super::simd::vector(lanes).expect("an integer lane type");
    let member = supply.member();
    if member == "of" || member == "reveal" {
        supply.ret(lo, fx, supply.param(0));
        return;
    }
    if member == "eq" || member == "ne" {
        let (a, b) = (supply.param(0).address(), supply.param(1).address());
        let mut differing = Vec::new();
        for half in [0, 8] {
            let left = fx.load(types::I64, a, half);
            let right = fx.load(types::I64, b, half);
            differing.push(fx.b.ins().bxor(left, right));
        }
        let either = fx.b.ins().bor(differing[0], differing[1]);
        let cc = match member {
            "eq" => IntCC::Equal,
            _ => IntCC::NotEqual,
        };
        let held = fx.b.ins().icmp_imm_s(cc, either, 0);
        supply.ret(lo, fx, Held::Scalar(held));
        return;
    }
    let sret = supply.sret.expect("a lane value's address");
    if member == "select" {
        let chosen = supply.param(0).scalar();
        let wide = fx.b.ins().uextend(types::I64, chosen);
        let mask = fx.b.ins().ineg(wide);
        let (a, b) = (supply.param(1).address(), supply.param(2).address());
        if lo.machine() {
            for half in [0, 8] {
                let left = fx.load(types::I64, a, half);
                let right = fx.load(types::I64, b, half);
                let kept = fx.b.ins().band(left, mask);
                let rest = fx.b.ins().band_not(right, mask);
                let selected = fx.b.ins().bor(kept, rest);
                fx.store(selected, sret, half);
            }
        } else {
            let masks = fx.b.ins().splat(types::I64X2, mask);
            let left = fx.b.ins().load(types::I64X2, super::simd::lanes(), a, 0);
            let right = fx.b.ins().load(types::I64X2, super::simd::lanes(), b, 0);
            let selected = fx.b.ins().bitselect(masks, left, right);
            fx.b.ins().store(super::simd::lanes(), selected, sret, 0);
        }
        supply.ret(lo, fx, Held::Address(sret));
        return;
    }
    let amount = matches!(member, "shl" | "shr").then(|| {
        let amount = supply.param(1).scalar();
        let wide = super::super::body::widen(fx, amount);
        let bits = i64::from(ty.lane_type().bits());
        let too_far =
            fx.b.ins()
                .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, wide, bits);
        fx.when(too_far, |fx| {
            super::super::runtime::report::raise_kind(lo, fx, "trap.shift")
        });
        amount
    });
    let a = supply.param(0).address();
    let b = (amount.is_none() && member != "bit_not").then(|| supply.param(1).address());
    let lane = ty.lane_type();
    let (count, width, value_ty) = match lo.machine() {
        true => (ty.lane_count(), lane.bytes(), lane),
        false => (1, 16, ty),
    };
    for index in 0..count {
        let offset = u64::from(index * width);
        let left =
            fx.b.ins()
                .load(value_ty, super::simd::lanes(), a, offset as i32);
        let right = b.map(|b| {
            fx.b.ins()
                .load(value_ty, super::simd::lanes(), b, offset as i32)
        });
        let ins = fx.b.ins();
        let result = match (member, right, amount) {
            ("bit_not", _, _) => ins.bnot(left),
            ("shl", _, Some(amount)) => ins.ishl(left, amount),
            ("shr", _, Some(amount)) if signed => ins.sshr(left, amount),
            ("shr", _, Some(amount)) => ins.ushr(left, amount),
            ("wrapping_add", Some(right), _) => ins.iadd(left, right),
            ("wrapping_sub", Some(right), _) => ins.isub(left, right),
            ("wrapping_mul", Some(right), _) => ins.imul(left, right),
            ("bit_and", Some(right), _) => ins.band(left, right),
            ("bit_or", Some(right), _) => ins.bor(left, right),
            ("bit_xor", Some(right), _) => ins.bxor(left, right),
            (other, _, _) => panic!("no lowering of core::Secret.{other} over {lanes}"),
        };
        fx.b.ins()
            .store(super::simd::lanes(), result, sret, offset as i32);
    }
    supply.ret(lo, fx, Held::Address(sret));
}
