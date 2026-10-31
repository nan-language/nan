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
