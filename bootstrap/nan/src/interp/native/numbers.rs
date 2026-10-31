//! The numbers the interpreter supplies: the members core declares for scalars and the named and
//! bit conversions.

use std::rc::Rc;

use super::super::value::{Val, fits, wrap};
use super::super::{End, Exec, Flow, Interpreter, Site};
use crate::check::db::ItemId;
use crate::check::lowlevel::{BitsKind, LayoutShape};
use crate::check::sig::SigKind;
use crate::check::ty::{Scalar, Ty};
use crate::nir::{BitConversion, BitsOwner, Builtin, Conversion, Span};

/// The members core declares for a scalar type without a body.
pub fn scalar_member(
    interp: &mut Interpreter,
    scalar: Scalar,
    member: &str,
    args: &[Val],
    at: (ItemId, Span),
) -> Exec<Flow> {
    if scalar.is_float() {
        return float_member(interp, member, args).map(Flow::Return);
    }
    let a = interp.deref(&args[0]).int();
    let b = || interp.deref(&args[1]).int();
    let value = match member {
        "wrapping_add" => Val::Int(wrap(a + b(), scalar)),
        "wrapping_sub" => Val::Int(wrap(a - b(), scalar)),
        "wrapping_mul" => Val::Int(wrap(a.wrapping_mul(b()), scalar)),
        "wrapping_shl" | "checked_shl" | "shr_arith" => {
            let amount = b();
            let fits_width = amount < i128::from(scalar.bits());
            match member {
                "checked_shl" => {
                    let option = interp.ty_item("core", "Option");
                    let shifted = a << amount.min(127);
                    match fits_width && fits(shifted, scalar) {
                        true => Val::some(option, Val::Int(shifted)),
                        false => Val::none(option),
                    }
                }
                _ if !fits_width => return Err(interp.trap("trap.shift", at)),
                "wrapping_shl" => Val::Int(wrap(a << amount, scalar)),
                _ => Val::Int(a >> amount),
            }
        }
        "div_trunc" | "rem_trunc" => {
            let divisor = b();
            if divisor == 0 {
                return Err(interp.trap("trap.division-by-zero", at));
            }
            let quotient = a / divisor;
            match member {
                "div_trunc" if !fits(quotient, scalar) => {
                    return Err(interp.trap("trap.overflow", at));
                }
                "div_trunc" => Val::Int(quotient),
                _ => Val::Int(a - divisor * quotient),
            }
        }
        "count_ones" => Val::Int(i128::from(bits_of(a, scalar).count_ones())),
        "leading_zeros" => {
            let bits = bits_of(a, scalar);
            Val::Int(i128::from(bits.leading_zeros()) - i128::from(64 - scalar.bits()))
        }
        "trailing_zeros" => {
            let bits = bits_of(a, scalar);
            Val::Int(i128::from(bits.trailing_zeros().min(scalar.bits())))
        }
        "wide_mul" => {
            let wide = interp.ty_item("core", "WideProduct");
            let (high, low) = match scalar.is_signed() {
                true => {
                    let product = a * b();
                    let low = product.rem_euclid(1i128 << 64);
                    ((product - low) >> 64, low)
                }
                false => {
                    let product = (a as u128) * (b() as u128);
                    ((product >> 64) as i128, i128::from(product as u64))
                }
            };
            Val::Struct(wide, Rc::new(vec![Val::Int(high), Val::Int(low)]))
        }
        _ => return Err(End::Missing),
    };
    Ok(Flow::Return(value))
}

/// The members core declares for a float type without a body.
fn float_member(interp: &Interpreter, member: &str, args: &[Val]) -> Exec<Val> {
    let values: Vec<Val> = args.iter().map(|arg| interp.deref(arg)).collect();
    Ok(match (member, &values[..]) {
        ("sqrt", [Val::F32(a)]) => canonical(Val::F32(a.sqrt())),
        ("sqrt", [Val::F64(a)]) => canonical(Val::F64(a.sqrt())),
        ("fused_mul_add", [Val::F32(a), Val::F32(b), Val::F32(c)]) => {
            canonical(Val::F32(a.mul_add(*b, *c)))
        }
        ("fused_mul_add", [Val::F64(a), Val::F64(b), Val::F64(c)]) => {
            canonical(Val::F64(a.mul_add(*b, *c)))
        }
        _ => return Err(End::Missing),
    })
}

/// A float with a NaN replaced by the canonical NaN of its type.
pub fn canonical(value: Val) -> Val {
    match value {
        Val::F32(value) if value.is_nan() => Val::F32(f32::from_bits(0x7FC0_0000)),
        Val::F64(value) if value.is_nan() => Val::F64(f64::from_bits(0x7FF8_0000_0000_0000)),
        other => other,
    }
}

/// The bits of an integer of a scalar type, two's complement within its width.
fn bits_of(value: i128, scalar: Scalar) -> u64 {
    (value & ((1i128 << scalar.bits()) - 1)) as u64
}

/// A named or bit conversion.
pub fn builtin(
    interp: &mut Interpreter,
    builtin: Builtin,
    args: Vec<Val>,
    at: (ItemId, Span),
    site: Site,
) -> Exec<Flow> {
    let value = interp.deref(&args[0]);
    let option_item = interp.ty_item("core", "Option");
    let option = || option_item;
    let result = match builtin {
        Builtin::Conversion(conversion, target) => match conversion {
            Conversion::Widen => widened(&value, target),
            Conversion::Fit => match fitted(&value, target) {
                Some(fitted) => fitted,
                None => return Err(interp.conversion_trap(site, at)),
            },
            Conversion::CheckedFit => match fitted(&value, target) {
                Some(fitted) => Val::some(option(), fitted),
                None => Val::none(option()),
            },
            Conversion::Truncate => Val::Int(wrap(value.int(), target)),
            Conversion::Saturate => {
                let (low, high) = target.range();
                match value {
                    Val::Int(value) => Val::Int(value.clamp(low, high)),
                    Val::F32(_) | Val::F64(_) => {
                        let float = as_f64(&value);
                        if float.is_nan() {
                            Val::Int(0)
                        } else {
                            Val::Int((float.trunc() as i128).clamp(low, high))
                        }
                    }
                    other => panic!("saturate of {other:?}"),
                }
            }
            Conversion::Nearest => nearest(&value, target),
        },
        Builtin::Bits(conversion, BitsOwner::Scalar(scalar)) => match conversion {
            BitConversion::To => Val::Int(i128::from(scalar_bits(&value, scalar))),
            BitConversion::From => from_bits(value.int() as u64, scalar)
                .unwrap_or_else(|| panic!("from_bits of a niche")),
            BitConversion::CheckedFrom => match from_bits(value.int() as u64, scalar) {
                Some(value) => Val::some(option(), value),
                None => Val::none(option()),
            },
        },
        Builtin::Bits(conversion, BitsOwner::Layout(layout)) => {
            if interp.ck.layout_shape(layout).is_none() {
                return Err(End::Missing);
            }
            match conversion {
                BitConversion::To => Val::Int(i128::from(layout_bits(interp, layout, &value))),
                BitConversion::From => layout_value(interp, layout, value.int() as u64)
                    .unwrap_or_else(|| panic!("from_bits of a niche")),
                BitConversion::CheckedFrom => {
                    match layout_value(interp, layout, value.int() as u64) {
                        Some(value) => Val::some(option(), value),
                        None => Val::none(option()),
                    }
                }
            }
        }
        Builtin::Intrinsic(_) => unreachable!("an intrinsic is no conversion"),
    };
    Ok(Flow::Return(result))
}

fn as_f64(value: &Val) -> f64 {
    match value {
        Val::F32(value) => f64::from(*value),
        Val::F64(value) => *value,
        Val::Int(value) => *value as f64,
        other => panic!("a number, not {other:?}"),
    }
}

/// `T.widen(x)`: x as a value of T.
fn widened(value: &Val, target: Scalar) -> Val {
    match (value, target) {
        (Val::Int(value), Scalar::F32) => Val::F32(*value as f32),
        (Val::Int(value), Scalar::F64) => Val::F64(*value as f64),
        (Val::F32(value), Scalar::F64) => Val::F64(f64::from(*value)),
        (Val::Int(value), _) => Val::Int(*value),
        (other, _) => other.clone(),
    }
}

/// `T.fit(x)`: x when exactly a value of T.
fn fitted(value: &Val, target: Scalar) -> Option<Val> {
    match (value, target) {
        (Val::Int(value), Scalar::F32) => {
            let float = *value as f32;
            (float as i128 == *value).then_some(Val::F32(float))
        }
        (Val::Int(value), Scalar::F64) => {
            let float = *value as f64;
            (float as i128 == *value).then_some(Val::F64(float))
        }
        (Val::Int(value), _) => fits(*value, target).then_some(Val::Int(*value)),
        (Val::F64(value), Scalar::F32) => {
            let narrowed = *value as f32;
            (f64::from(narrowed) == *value || value.is_nan()).then_some(Val::F32(narrowed))
        }
        (Val::F32(_) | Val::F64(_), _) => {
            let float = as_f64(value);
            let integral = float.is_finite() && float.trunc() == float;
            (integral && fits(float as i128, target)).then_some(Val::Int(float as i128))
        }
        _ => None,
    }
}

/// `T.nearest(x)`: x rounded to T, ties to even, clamped for an integer T.
fn nearest(value: &Val, target: Scalar) -> Val {
    let float = as_f64(value);
    match target {
        Scalar::F32 => Val::F32(float as f32),
        Scalar::F64 => Val::F64(float),
        _ if float.is_nan() => Val::Int(0),
        _ => {
            let (low, high) = target.range();
            Val::Int((float.round_ties_even() as i128).clamp(low, high))
        }
    }
}

/// The bit pattern of a scalar value.
pub(super) fn scalar_bits(value: &Val, scalar: Scalar) -> u64 {
    match value {
        Val::F32(value) => u64::from(value.to_bits()),
        Val::F64(value) => value.to_bits(),
        Val::Bool(value) => u64::from(*value),
        Val::Char(c) => u64::from(u32::from(*c)),
        Val::Int(value) => bits_of(*value, scalar),
        other => panic!("the bits of {other:?}"),
    }
}

/// The value of a scalar type with a bit pattern, none for a niche.
pub(super) fn from_bits(bits: u64, scalar: Scalar) -> Option<Val> {
    Some(match scalar {
        Scalar::F32 => Val::F32(f32::from_bits(bits as u32)),
        Scalar::F64 => Val::F64(f64::from_bits(bits)),
        Scalar::Bool => match bits {
            0 => Val::Bool(false),
            1 => Val::Bool(true),
            _ => return None,
        },
        Scalar::Char => Val::Char(char::from_u32(bits as u32)?),
        _ => Val::Int(wrap(i128::from(bits), scalar)),
    })
}

/// The bits value of an integer layout's value.
pub fn layout_bits(interp: &Interpreter, layout: ItemId, value: &Val) -> u64 {
    match interp.ck.layout_shape(layout).expect("an integer layout") {
        LayoutShape::Tags(values) => match value {
            Val::Variant(_, index, _) => values[*index as usize],
            other => panic!("a tag value, not {other:?}"),
        },
        LayoutShape::Fields(fields) => {
            let types = layout_field_types(interp, layout);
            fields
                .iter()
                .zip(value.fields())
                .zip(types)
                .map(|((field, value), ty)| {
                    let bits = match (field.kind, &ty) {
                        (BitsKind::Typed, Ty::Item(nested, _)) => {
                            layout_bits(interp, *nested, value)
                        }
                        _ => value.int() as u64,
                    };
                    match field.kind {
                        BitsKind::Rest => bits,
                        _ => bits << field.low,
                    }
                })
                .fold(0, |all, bits| all | bits)
        }
    }
}

/// The value of an integer layout with a bits value, none for a niche.
pub(super) fn layout_value(interp: &Interpreter, layout: ItemId, bits: u64) -> Option<Val> {
    match interp.ck.layout_shape(layout).expect("an integer layout") {
        LayoutShape::Tags(values) => {
            let index = values.iter().position(|value| *value == bits)?;
            Some(Val::Variant(layout, index as u32, Rc::new(Vec::new())))
        }
        LayoutShape::Fields(fields) => {
            let types = layout_field_types(interp, layout);
            let mut covered = 0u64;
            let mut values = Vec::new();
            for (field, ty) in fields.iter().zip(&types) {
                let mask = match field.width {
                    64.. => u64::MAX,
                    width => (1u64 << width) - 1,
                };
                if field.kind != BitsKind::Rest {
                    covered |= mask << field.low;
                }
                let raw = (bits >> field.low) & mask;
                values.push(match (field.kind, ty) {
                    (BitsKind::Bool, _) => Val::Bool(raw == 1),
                    (BitsKind::Typed, Ty::Item(nested, _)) => layout_value(interp, *nested, raw)?,
                    (BitsKind::Rest, _) => Val::Int(0),
                    _ => Val::Int(i128::from(raw)),
                });
            }
            for (field, value) in fields.iter().zip(values.iter_mut()) {
                if field.kind == BitsKind::Rest {
                    *value = Val::Int(i128::from(bits & !covered));
                }
            }
            Some(Val::Struct(layout, Rc::new(values)))
        }
    }
}

/// The types of a layout's fields, in declaration order.
fn layout_field_types(interp: &Interpreter, layout: ItemId) -> Vec<Ty> {
    match &interp.ck.sig(layout).kind {
        SigKind::Layout { fields, .. } => fields.iter().map(|field| field.ty.clone()).collect(),
        _ => Vec::new(),
    }
}
