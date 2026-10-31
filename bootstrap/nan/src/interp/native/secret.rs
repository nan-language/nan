//! The members of `core::Secret` the interpreter supplies, over the value a Secret value holds.

use std::rc::Rc;

use super::super::value::{Val, wrap};
use super::super::{End, Exec, Flow, Interpreter};
use crate::check::db::ItemId;
use crate::check::ty::{Arg, Scalar, Ty};
use crate::nir::Span;

/// A member of `core::Secret[T]` without a body.
pub fn member(
    interp: &mut Interpreter,
    member: &str,
    type_args: &[Arg],
    args: &[Val],
    at: (ItemId, Span),
) -> Exec<Flow> {
    let secret = interp.ty_item("core", "Secret");
    let held: Vec<Val> = args.iter().map(|arg| interp.deref(arg)).collect();
    let inner = |index: usize| held[index].fields()[0].clone();
    let scalar = match type_args.first().and_then(Arg::ty) {
        Some(Ty::Scalar(scalar)) => *scalar,
        Some(Ty::Item(lanes, _)) if member != "select" => {
            let value = lanes_member(interp, member, *lanes, &held, at)?;
            return Ok(Flow::Return(match member {
                "reveal" | "eq" | "ne" => value,
                _ => Val::Struct(secret, Rc::new(vec![value])),
            }));
        }
        other => panic!("a Secret of a scalar, not {other:?}"),
    };
    let a = || inner(0).int();
    let b = || inner(1).int();
    let amount = || held[1].int();
    let value = match member {
        "of" => held[0].clone(),
        "reveal" => return Ok(Flow::Return(inner(0))),
        "select" => {
            let chosen = if inner(0).bool() { 1 } else { 2 };
            return Ok(Flow::Return(held[chosen].clone()));
        }
        "wrapping_add" => Val::Int(wrap(a().wrapping_add(b()), scalar)),
        "wrapping_sub" => Val::Int(wrap(a().wrapping_sub(b()), scalar)),
        "wrapping_mul" => Val::Int(wrap(a().wrapping_mul(b()), scalar)),
        "mul_high" => Val::Int(match scalar.is_signed() {
            true => (a() * b()) >> 64,
            false => ((a() as u128 * b() as u128) >> 64) as i128,
        }),
        "bit_and" => bits(scalar, a() & b()),
        "bit_or" => bits(scalar, a() | b()),
        "bit_xor" => bits(scalar, a() ^ b()),
        "bit_not" => bits(scalar, !a()),
        "shl" | "shr" => {
            if amount() >= i128::from(scalar.bits()) {
                return Err(interp.trap("trap.shift", at));
            }
            Val::Int(match member {
                "shl" => wrap(a().wrapping_shl(amount() as u32), scalar),
                _ => a() >> amount(),
            })
        }
        "rotate_left" | "rotate_right" => {
            let width = scalar.bits();
            let mut turn = (amount() % i128::from(width)) as u32;
            if member == "rotate_right" {
                turn = (width - turn) % width;
            }
            let pattern = wrap(a(), unsigned(scalar)) as u128;
            let mask = (1u128 << width) - 1;
            let rotated = ((pattern << turn) | (pattern >> (width - turn))) & mask;
            Val::Int(wrap(rotated as i128, scalar))
        }
        "eq" => Val::Bool(a() == b()),
        "ne" => Val::Bool(a() != b()),
        "lt" => Val::Bool(a() < b()),
        "le" => Val::Bool(a() <= b()),
        "gt" => Val::Bool(a() > b()),
        "ge" => Val::Bool(a() >= b()),
        "widen" => inner(0),
        "truncate" => Val::Int(wrap(a(), scalar)),
        _ => return Err(End::Missing),
    };
    Ok(Flow::Return(Val::Struct(secret, Rc::new(vec![value]))))
}

/// The value of a scalar type, `bool` included, whose two's complement bits are those of an
/// integer within its width.
fn bits(scalar: Scalar, value: i128) -> Val {
    match scalar {
        Scalar::Bool => Val::Bool(value & 1 == 1),
        _ => Val::Int(wrap(value, scalar)),
    }
}

/// The unsigned integer type of a scalar's width.
fn unsigned(scalar: Scalar) -> Scalar {
    match scalar.bits() {
        8 => Scalar::U8,
        16 => Scalar::U16,
        32 => Scalar::U32,
        _ => Scalar::U64,
    }
}

/// A member of `core::Secret[L]` for an integer lane type `L`: lane by lane on the lanes of the
/// values it holds, `eq` and `ne` comparing every lane.
fn lanes_member(
    interp: &mut Interpreter,
    member: &str,
    lanes: ItemId,
    held: &[Val],
    at: (ItemId, Span),
) -> Exec<Val> {
    let lanes_of = |index: usize| match &held[index].fields()[0].fields()[0] {
        Val::Array(values) => values.iter().map(Val::int).collect::<Vec<i128>>(),
        other => panic!("the lanes of a lane value, not {other:?}"),
    };
    let path = interp.ck.db.canonical(lanes);
    let (signedness, shape) = path.trim_start_matches("core::simd::").split_at(1);
    let bits = shape.split('x').next().unwrap_or_default();
    let scalar = Scalar::from_name(&format!("{}{bits}", signedness.to_lowercase()))
        .expect("an integer lane type's lane scalar");
    let value = match member {
        "of" => return Ok(held[0].clone()),
        "reveal" => return Ok(held[0].fields()[0].clone()),
        "eq" => return Ok(Val::Bool(lanes_of(0) == lanes_of(1))),
        "ne" => return Ok(Val::Bool(lanes_of(0) != lanes_of(1))),
        _ => lanes_of(0),
    };
    let amount = || held[1].int();
    if matches!(member, "shl" | "shr") && amount() >= i128::from(scalar.bits()) {
        return Err(interp.trap("trap.shift", at));
    }
    let computed: Vec<Val> = value
        .iter()
        .enumerate()
        .map(|(index, &a)| {
            let b = || lanes_of(1)[index];
            Val::Int(match member {
                "wrapping_add" => wrap(a + b(), scalar),
                "wrapping_sub" => wrap(a - b(), scalar),
                "wrapping_mul" => wrap(a.wrapping_mul(b()), scalar),
                "bit_and" => wrap(a & b(), scalar),
                "bit_or" => wrap(a | b(), scalar),
                "bit_xor" => wrap(a ^ b(), scalar),
                "bit_not" => wrap(!a, scalar),
                "shl" => wrap(a << amount(), scalar),
                _ => wrap(a, scalar) >> amount(),
            })
        })
        .collect();
    Ok(Val::Struct(
        lanes,
        Rc::new(vec![Val::Array(Rc::new(computed))]),
    ))
}
