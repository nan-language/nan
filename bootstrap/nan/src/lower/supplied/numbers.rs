//! Numbers: the named conversions and bit conversions, lowered where they are called, and the
//! scalar members std supplies: wrapping arithmetic, bit counts, truncating division and
//! arithmetic shifts, wide products, square roots, fused multiply-adds and total float order.

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Type, Value, types};

use super::super::body::{Body, Held};
use super::super::fx::Fx;
use super::super::layout::{Shape, scalar_type};
use super::super::trap::{Form, Operands};
use super::super::{Lowering, abi::Repr};
use super::Supply;
use crate::check::db::TypeKey;
use crate::check::ty::{Scalar, Ty};
use crate::nir::{
    BitConversion, BitsOwner, BlockId, Callable, Conversion, FnRef, NTy, Operand, Value as NirValue,
};

/// Checks, at a call outside std of a scalar's division or shift method, the conditions on its
/// operands that the method's rules trap on, so that the report names the call and shows its
/// receiver and argument.
pub fn operand_checks(body: &mut Body, fn_ref: &FnRef, operands: &[Operand]) {
    let Callable::Item { item, .. } = fn_ref.callable else {
        return;
    };
    let db = &body.lo.ck.db;
    let Some(TypeKey::Scalar(scalar)) = db.owners.get(&item).cloned() else {
        return;
    };
    if db.is_std(body.item) || !db.is_std(item) {
        return;
    }
    let path = db.canonical(item);
    let kinds: &[&str] = match path.rsplit_once('.').map_or("", |(_, member)| member) {
        "div_trunc" | "div_floor" => &["trap.division-by-zero", "trap.overflow"],
        "rem_trunc" | "mod_floor" => &["trap.division-by-zero"],
        "shr_arith" | "wrapping_shl" => &["trap.shift"],
        _ => return,
    };
    let values: Vec<(Operand, Held, NTy)> = operands
        .iter()
        .map(|operand| {
            let (held, nty) = body.operand(operand);
            (operand.clone(), held, nty)
        })
        .collect();
    let [(_, left, left_nty), (_, right, right_nty)] = values.as_slice() else {
        return;
    };
    let repr = body.lo.owned_repr(left_nty.ty());
    let value = body.adapt(*left, left_nty, repr).scalar();
    let repr = body.lo.owned_repr(right_nty.ty());
    let other = body.adapt(*right, right_nty, repr).scalar();
    for kind in kinds {
        let holds = match *kind {
            "trap.division-by-zero" => body.fx.b.ins().icmp_imm_s(IntCC::NotEqual, other, 0),
            "trap.overflow" => {
                let minimum = scalar.range().0 as i64;
                let is_minimum = body.fx.b.ins().icmp_imm_s(IntCC::Equal, value, minimum);
                let minus_one = body.fx.b.ins().icmp_imm_s(IntCC::Equal, other, -1);
                let overflows = body.fx.b.ins().band(is_minimum, minus_one);
                body.fx.b.ins().bxor_imm_s(overflows, 1)
            }
            _ => {
                let amount = super::super::body::widen(body.fx, other);
                let width = i64::from(scalar.bits());
                body.fx
                    .b
                    .ins()
                    .icmp_imm_s(IntCC::UnsignedLessThan, amount, width)
            }
        };
        body.trap_unless(holds, kind, Operands::Values(Form::Call, values.clone()));
    }
}

/// A named conversion of an operand to a scalar type.
pub fn conversion(
    body: &mut Body,
    conversion: Conversion,
    target: Scalar,
    operand: &Operand,
    block: BlockId,
    position: usize,
) -> Held {
    let (held, nty) = body.operand(operand);
    let source = nty.ty().scalar().expect("a scalar converted");
    let repr = body.lo.owned_repr(nty.ty());
    let value = body.adapt(held, &nty, repr).scalar();
    let target_ty = scalar_type(target);
    let checked = body.checked(block, position);
    match conversion {
        Conversion::Widen => Held::Scalar(widen(body.fx, value, source, target)),
        Conversion::Truncate => {
            Held::Scalar(super::super::place::resize(body.fx, value, target_ty))
        }
        Conversion::Saturate => Held::Scalar(saturate(body.fx, value, source, target)),
        Conversion::Nearest => Held::Scalar(nearest(body.fx, value, source, target)),
        Conversion::Fit | Conversion::CheckedFit => {
            let (converted, fits) = fit(body.fx, value, source, target);
            if conversion == Conversion::Fit {
                if checked {
                    let operands = Operands::Values(
                        Form::Call,
                        vec![(operand.clone(), Held::Scalar(value), nty)],
                    );
                    body.trap_unless(fits, "trap.conversion", operands);
                }
                return Held::Scalar(converted);
            }
            let option = body.lo.std_ty("core", "Option", vec![Ty::Scalar(target)]);
            option_of(body, &option, fits, Held::Scalar(converted))
        }
    }
}

/// An `Option` of a type: `Some(value)` when `present`, else `None`.
pub fn option_of(body: &mut Body, option: &Ty, present: Value, value: Held) -> Held {
    let layout = body.lo.layout(option);
    let slot = body.fx.slot(layout.size, layout.align);
    body.lo.write_option(body.fx, option, slot, present, value);
    body.lo.held_at(body.fx, option, slot)
}

fn widen(fx: &mut Fx, value: Value, source: Scalar, target: Scalar) -> Value {
    let ty = scalar_type(target);
    match (source.is_float(), target.is_float()) {
        (false, false) => match (source.is_signed(), source.bits() < target.bits()) {
            (true, true) => fx.b.ins().sextend(ty, value),
            (false, true) => fx.b.ins().uextend(ty, value),
            _ => value,
        },
        (false, true) => {
            let wide = integer64(fx, value, source);
            match source.is_signed() {
                true => fx.b.ins().fcvt_from_sint(ty, wide),
                false => fx.b.ins().fcvt_from_uint(ty, wide),
            }
        }
        (true, true) => fx.b.ins().fpromote(ty, value),
        (true, false) => unreachable!("no float widens to an integer"),
    }
}

/// An integer register widened to 64 bits by its own sign.
fn integer64(fx: &mut Fx, value: Value, scalar: Scalar) -> Value {
    if scalar.bits() == 64 {
        return value;
    }
    match scalar.is_signed() {
        true => fx.b.ins().sextend(types::I64, value),
        false => fx.b.ins().uextend(types::I64, value),
    }
}

/// A conversion that keeps the value exactly, with whether it did.
fn fit(fx: &mut Fx, value: Value, source: Scalar, target: Scalar) -> (Value, Value) {
    let ty = scalar_type(target);
    match (source.is_float(), target.is_float()) {
        (false, false) => {
            let (low, high) = target.range();
            let wide = integer64(fx, value, source);
            let fits = in_range(fx, wide, source.is_signed(), low, high);
            (super::super::place::resize(fx, value, ty), fits)
        }
        (false, true) => {
            let wide = integer64(fx, value, source);
            let converted = match source.is_signed() {
                true => fx.b.ins().fcvt_from_sint(ty, wide),
                false => fx.b.ins().fcvt_from_uint(ty, wide),
            };
            let exact = float_to_int_exact(fx, converted, wide, source);
            (converted, exact)
        }
        (true, false) => {
            let integral = trunc(fx, value);
            let whole = fx.b.ins().fcmp(FloatCC::Equal, integral, value);
            let (low, high) = target.range();
            let float_ty = fx.b.func.dfg.value_type(value);
            let low_bound = float_const(fx, float_ty, low as f64);
            let above_low =
                fx.b.ins()
                    .fcmp(FloatCC::GreaterThanOrEqual, value, low_bound);
            let high_limit = float_const(fx, float_ty, (high as f64) + 1.0);
            let below_high = fx.b.ins().fcmp(FloatCC::LessThan, value, high_limit);
            let fits = fx.b.ins().band(whole, above_low);
            let fits = fx.b.ins().band(fits, below_high);
            let converted = match target.is_signed() {
                true => fx.b.ins().fcvt_to_sint_sat(ty, value),
                false => fx.b.ins().fcvt_to_uint_sat(ty, value),
            };
            (converted, fits)
        }
        (true, true) => {
            let demoted = fx.b.ins().fdemote(ty, value);
            let back = fx.b.ins().fpromote(types::F64, demoted);
            let same = fx.b.ins().fcmp(FloatCC::Equal, back, value);
            let unordered = fx.b.ins().fcmp(FloatCC::Unordered, value, value);
            let fits = fx.b.ins().bor(same, unordered);
            (demoted, fits)
        }
    }
}

/// Whether a float converted from the 64-bit integer `original` of `source` converts back to
/// it exactly.
fn float_to_int_exact(fx: &mut Fx, converted: Value, original: Value, source: Scalar) -> Value {
    let float_ty = fx.b.func.dfg.value_type(converted);
    let (low, high) = source.range();
    let low_bound = float_const(fx, float_ty, low as f64);
    let high_limit = float_const(fx, float_ty, (high as f64) + 1.0);
    let above =
        fx.b.ins()
            .fcmp(FloatCC::GreaterThanOrEqual, converted, low_bound);
    let below = fx.b.ins().fcmp(FloatCC::LessThan, converted, high_limit);
    let back = match source.is_signed() {
        true => fx.b.ins().fcvt_to_sint_sat(types::I64, converted),
        false => fx.b.ins().fcvt_to_uint_sat(types::I64, converted),
    };
    let same = fx.b.ins().icmp(IntCC::Equal, back, original);
    let inside = fx.b.ins().band(above, below);
    fx.b.ins().band(inside, same)
}

fn float_const(fx: &mut Fx, ty: Type, value: f64) -> Value {
    match ty {
        types::F32 => fx.b.ins().f32const(value as f32),
        _ => fx.b.ins().f64const(value),
    }
}

/// Whether a 64-bit integer, read as signed or unsigned, lies in `low..=high`.
fn in_range(fx: &mut Fx, value: Value, signed: bool, low: i128, high: i128) -> Value {
    let (min, max) = if signed {
        (i128::from(i64::MIN), i128::from(i64::MAX))
    } else {
        (0, i128::from(u64::MAX))
    };
    let (at_least, at_most) = if signed {
        (
            IntCC::SignedGreaterThanOrEqual,
            IntCC::SignedLessThanOrEqual,
        )
    } else {
        (
            IntCC::UnsignedGreaterThanOrEqual,
            IntCC::UnsignedLessThanOrEqual,
        )
    };
    let above = match low <= min {
        true => fx.int(types::I8, 1),
        false => fx.b.ins().icmp_imm_s(at_least, value, low as i64),
    };
    let below = match high >= max {
        true => fx.int(types::I8, 1),
        false => fx.b.ins().icmp_imm_s(at_most, value, high as i64),
    };
    fx.b.ins().band(above, below)
}

/// A float rounded toward zero to an integral value, without the C library: through a 64-bit
/// integer below the magnitude at which the format holds no fraction, the sign kept.
pub fn trunc(fx: &mut Fx, value: Value) -> Value {
    let ty = fx.b.func.dfg.value_type(value);
    let limit = float_const(fx, ty, 4_503_599_627_370_496.0);
    let magnitude = fx.b.ins().fabs(value);
    let small = fx.b.ins().fcmp(FloatCC::LessThan, magnitude, limit);
    let whole = fx.b.ins().fcvt_to_sint_sat(types::I64, value);
    let back = fx.b.ins().fcvt_from_sint(ty, whole);
    let signed = fx.b.ins().fcopysign(back, value);
    fx.b.ins().select(small, signed, value)
}

fn saturate(fx: &mut Fx, value: Value, source: Scalar, target: Scalar) -> Value {
    let ty = scalar_type(target);
    if source.is_float() {
        return match target.is_signed() {
            true => fx.b.ins().fcvt_to_sint_sat(ty, value),
            false => fx.b.ins().fcvt_to_uint_sat(ty, value),
        };
    }
    let (low, high) = target.range();
    let wide = integer64(fx, value, source);
    let fits = in_range(fx, wide, source.is_signed(), low, high);
    let negative = if source.is_signed() {
        fx.b.ins().icmp_imm_s(IntCC::SignedLessThan, wide, 0)
    } else {
        fx.int(types::I8, 0)
    };
    let low_value = fx.b.ins().iconst(ty, low as i64);
    let high_value = fx.b.ins().iconst(ty, high as i64);
    let bound = fx.b.ins().select(negative, low_value, high_value);
    let kept = super::super::place::resize(fx, value, ty);
    fx.b.ins().select(fits, kept, bound)
}

fn nearest(fx: &mut Fx, value: Value, source: Scalar, target: Scalar) -> Value {
    let ty = scalar_type(target);
    match (source.is_float(), target.is_float()) {
        (false, true) => {
            let wide = integer64(fx, value, source);
            match source.is_signed() {
                true => fx.b.ins().fcvt_from_sint(ty, wide),
                false => fx.b.ins().fcvt_from_uint(ty, wide),
            }
        }
        (true, true) => fx.b.ins().fdemote(ty, value),
        (true, false) => {
            let rounded = round_even(fx, value);
            match target.is_signed() {
                true => fx.b.ins().fcvt_to_sint_sat(ty, rounded),
                false => fx.b.ins().fcvt_to_uint_sat(ty, rounded),
            }
        }
        (false, false) => unreachable!("nearest takes no integer to an integer"),
    }
}

/// A float rounded to an integral value, ties to even, by adding and subtracting the power of
/// two at which the format holds no fraction.
fn round_even(fx: &mut Fx, value: Value) -> Value {
    let ty = fx.b.func.dfg.value_type(value);
    let magic = match ty {
        types::F32 => fx.b.ins().f32const(8_388_608.0),
        _ => fx.b.ins().f64const(4_503_599_627_370_496.0),
    };
    let magnitude = fx.b.ins().fabs(value);
    let large =
        fx.b.ins()
            .fcmp(FloatCC::GreaterThanOrEqual, magnitude, magic);
    let shifted = fx.b.ins().fadd(magnitude, magic);
    let rounded = fx.b.ins().fsub(shifted, magic);
    let signed = fx.b.ins().fcopysign(rounded, value);
    fx.b.ins().select(large, value, signed)
}

/// A bit conversion of an operand.
pub fn bits(
    body: &mut Body,
    conversion: BitConversion,
    owner: BitsOwner,
    operand: &Operand,
    result: Option<NirValue>,
) -> Held {
    let (held, nty) = body.operand(operand);
    let repr = body.lo.owned_repr(nty.ty());
    let value = body.adapt(held, &nty, repr);
    let owner_ty = match owner {
        BitsOwner::Scalar(scalar) => Ty::Scalar(scalar),
        BitsOwner::Layout(item) => Ty::item(item, Vec::new()),
    };
    let result_ty = result
        .map(|result| body.nty(result).ty().clone())
        .unwrap_or(Ty::Unit);
    match conversion {
        BitConversion::To => Held::Scalar(to_bits(body.fx, value.scalar())),
        BitConversion::From => {
            let target = body.lo.owned_repr(&owner_ty);
            match target {
                Repr::Scalar(ty) => Held::Scalar(from_bits(body.fx, value.scalar(), ty)),
                _ => value,
            }
        }
        BitConversion::CheckedFrom => {
            let target = match body.lo.owned_repr(&owner_ty) {
                Repr::Scalar(ty) => ty,
                _ => panic!("a checked bit conversion to a value in memory"),
            };
            let converted = from_bits(body.fx, value.scalar(), target);
            let valid = valid_bits(body, &owner_ty, value.scalar());
            option_of(body, &result_ty, valid, Held::Scalar(converted))
        }
    }
}

fn to_bits(fx: &mut Fx, value: Value) -> Value {
    match fx.b.func.dfg.value_type(value) {
        types::F32 => fx.b.ins().bitcast(types::I32, MemFlagsData::new(), value),
        types::F64 => fx.b.ins().bitcast(types::I64, MemFlagsData::new(), value),
        _ => value,
    }
}

fn from_bits(fx: &mut Fx, value: Value, target: Type) -> Value {
    match target {
        types::F32 | types::F64 => fx.b.ins().bitcast(target, MemFlagsData::new(), value),
        _ => value,
    }
}

/// Whether bits form a value of a type: not one of its niches.
fn valid_bits(body: &mut Body, ty: &Ty, bits: Value) -> Value {
    match ty {
        Ty::Scalar(Scalar::Bool) => {
            body.fx
                .b
                .ins()
                .icmp_imm_s(IntCC::UnsignedLessThanOrEqual, bits, 1)
        }
        Ty::Scalar(Scalar::Char) => {
            let below = body
                .fx
                .b
                .ins()
                .icmp_imm_s(IntCC::UnsignedLessThan, bits, 0x11_0000);
            let surrogate_low =
                body.fx
                    .b
                    .ins()
                    .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, bits, 0xD800);
            let surrogate_high =
                body.fx
                    .b
                    .ins()
                    .icmp_imm_s(IntCC::UnsignedLessThanOrEqual, bits, 0xDFFF);
            let surrogate = body.fx.b.ins().band(surrogate_low, surrogate_high);
            let not_surrogate = body.fx.b.ins().bxor_imm_s(surrogate, 1);
            body.fx.b.ins().band(below, not_surrogate)
        }
        Ty::Item(..) => {
            let layout = body.lo.layout(ty);
            match &layout.niche {
                Some(niche) => {
                    let mut valid = body.fx.int(types::I8, 1);
                    if let super::super::layout::NicheKind::Except(values) = &niche.kind {
                        let field = if niche.bit == 0 {
                            bits
                        } else {
                            body.fx.b.ins().ushr_imm_s(bits, i64::from(niche.bit))
                        };
                        let field = if niche.width >= 64 {
                            field
                        } else {
                            body.fx
                                .b
                                .ins()
                                .band_imm_s(field, ((1u128 << niche.width) - 1) as i64)
                        };
                        valid = body.fx.int(types::I8, 0);
                        for value in values.iter() {
                            let same =
                                body.fx
                                    .b
                                    .ins()
                                    .icmp_imm_s(IntCC::Equal, field, *value as i64);
                            valid = body.fx.b.ins().bor(valid, same);
                        }
                    }
                    valid
                }
                None => body.fx.int(types::I8, 1),
            }
        }
        _ => body.fx.int(types::I8, 1),
    }
}

/// Defines a scalar member std supplies.
pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    let Some(Ty::Scalar(scalar)) = supply.owner.clone() else {
        if supply.path == "core::TotalF64.cmp" || supply.path == "core::TotalF32.cmp" {
            total_cmp(lo, fx, supply);
            return true;
        }
        return false;
    };
    let value = |index: usize| supply.param(index).scalar();
    let held = match supply.member() {
        "wrapping_add" => Held::Scalar(fx.b.ins().iadd(value(0), value(1))),
        "wrapping_sub" => Held::Scalar(fx.b.ins().isub(value(0), value(1))),
        "wrapping_mul" => Held::Scalar(fx.b.ins().imul(value(0), value(1))),
        "count_ones" => count(fx, value(0), "popcnt"),
        "leading_zeros" => count(fx, value(0), "clz"),
        "trailing_zeros" => count(fx, value(0), "ctz"),
        "div_trunc" | "rem_trunc" => division(lo, fx, supply, scalar),
        "shr_arith" | "wrapping_shl" | "checked_shl" => shift(lo, fx, supply, scalar),
        "wide_mul" => {
            wide_mul(lo, fx, supply, scalar);
            return true;
        }
        "sqrt" => Held::Scalar(fx.b.ins().sqrt(value(0))),
        "fused_mul_add" => Held::Scalar(super::floats::fused_mul_add(
            lo,
            fx,
            value(0),
            value(1),
            value(2),
        )),
        _ => return false,
    };
    supply.ret(lo, fx, held);
    true
}

fn count(fx: &mut Fx, value: Value, op: &str) -> Held {
    let counted = match op {
        "popcnt" => fx.b.ins().popcnt(value),
        "clz" => fx.b.ins().clz(value),
        _ => fx.b.ins().ctz(value),
    };
    Held::Scalar(super::super::place::resize(fx, counted, types::I32))
}

fn division(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, scalar: Scalar) -> Held {
    let (a, b) = (supply.param(0).scalar(), supply.param(1).scalar());
    let ty = scalar_type(scalar);
    let zero_divisor = fx.b.ins().icmp_imm_s(IntCC::Equal, b, 0);
    fx.when(zero_divisor, |fx| {
        super::super::runtime::report::raise_kind(lo, fx, "trap.division-by-zero")
    });
    let minimum = fx.b.ins().iconst(ty, scalar.range().0 as i64);
    let is_minimum = fx.b.ins().icmp(IntCC::Equal, a, minimum);
    let minus_one = fx.b.ins().icmp_imm_s(IntCC::Equal, b, -1);
    let overflow = fx.b.ins().band(is_minimum, minus_one);
    if supply.member() == "div_trunc" {
        fx.when(overflow, |fx| {
            super::super::runtime::report::raise_kind(lo, fx, "trap.overflow")
        });
        return Held::Scalar(fx.b.ins().sdiv(a, b));
    }
    let one = fx.b.ins().iconst(ty, 1);
    let divisor = fx.b.ins().select(overflow, one, b);
    let remainder = fx.b.ins().srem(a, divisor);
    let zero = fx.b.ins().iconst(ty, 0);
    Held::Scalar(fx.b.ins().select(overflow, zero, remainder))
}

fn shift(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, scalar: Scalar) -> Held {
    let (a, amount) = (supply.param(0).scalar(), supply.param(1).scalar());
    let width = i64::from(scalar.bits());
    let wide = super::super::body::widen(fx, amount);
    let too_far =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, wide, width);
    match supply.member() {
        "checked_shl" => {
            let shifted = fx.b.ins().ishl(a, amount);
            let back = fx.b.ins().sshr(shifted, amount);
            let kept = fx.b.ins().icmp(IntCC::Equal, back, a);
            let in_width = fx.b.ins().bxor_imm_s(too_far, 1);
            let valid = fx.b.ins().band(kept, in_width);
            let option = supply.abi.result.clone();
            let slot = supply.sret.expect("an option's address");
            lo.write_option(fx, &option, slot, valid, Held::Scalar(shifted));
            Held::Address(slot)
        }
        member => {
            fx.when(too_far, |fx| {
                super::super::runtime::report::raise_kind(lo, fx, "trap.shift")
            });
            Held::Scalar(match member {
                "shr_arith" => fx.b.ins().sshr(a, amount),
                _ => fx.b.ins().ishl(a, amount),
            })
        }
    }
}

/// Writes `Some(value)` or `None` of an option of a scalar at an address.
fn wide_mul(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, scalar: Scalar) {
    let (a, b) = (supply.param(0).scalar(), supply.param(1).scalar());
    let high = match scalar.is_signed() {
        true => fx.b.ins().smulhi(a, b),
        false => fx.b.ins().umulhi(a, b),
    };
    let low = fx.b.ins().imul(a, b);
    let result = supply.abi.result.clone();
    let sret = supply.sret.expect("a wide product's address");
    let high_at = lo.layouts.field(lo.ck, &result, None, "high").offset;
    let low_at = lo.layouts.field(lo.ck, &result, None, "low").offset;
    fx.store(high, sret, high_at);
    fx.store(low, sret, low_at);
    supply.done(lo, fx);
}

/// `TotalF64.cmp` and `TotalF32.cmp`: the order of the bit patterns as totalOrder gives it.
fn total_cmp(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let left = supply.param(0).address();
    let right = supply.param(1).address();
    let float = if supply.path == "core::TotalF32.cmp" {
        types::I32
    } else {
        types::I64
    };
    let a = fx.load(float, left, 0);
    let b = fx.load(float, right, 0);
    let flip = |fx: &mut Fx, bits: Value| {
        let sign = fx.b.ins().sshr_imm_s(bits, i64::from(float.bits() - 1));
        let mask = fx.b.ins().ushr_imm_s(sign, 1);
        fx.b.ins().bxor(bits, mask)
    };
    let a = flip(fx, a);
    let b = flip(fx, b);
    let less = fx.b.ins().icmp(IntCC::SignedLessThan, a, b);
    let greater = fx.b.ins().icmp(IntCC::SignedGreaterThan, a, b);
    let ordering = lo.ordering(fx, less, greater);
    supply.ret(lo, fx, ordering);
}

impl Lowering<'_, '_> {
    /// A `core::Ordering` value from whether the left side is less or greater.
    pub fn ordering(&mut self, fx: &mut Fx, less: Value, greater: Value) -> Held {
        let ordering = self.std_ty("core", "Ordering", Vec::new());
        let layout = self.layout(&ordering);
        let two = fx.int(types::I8, 2);
        let one = fx.int(types::I8, 1);
        let zero = fx.int(types::I8, 0);
        let above = fx.b.ins().select(greater, two, one);
        let index = fx.b.ins().select(less, zero, above);
        match &layout.shape {
            Shape::Scalar { .. } => Held::Scalar(index),
            _ => {
                let slot = fx.slot(layout.size, layout.align);
                fx.store(index, slot, 0);
                Held::Address(slot)
            }
        }
    }
}
