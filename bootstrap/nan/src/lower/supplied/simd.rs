//! Lane types: `store` of the lane types of `core::simd`, a copy of the lanes on every target,
//! and the members a host target computes with vector instructions in place of their text, which
//! a machine target lowers lane by lane as it is written. Lane values are held in memory and
//! loaded and stored whole, with no alignment assumed beyond their lanes'.

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Type, Value, types};

use super::super::Lowering;
use super::super::abi::Abi;
use super::super::body::Held;
use super::super::fx::{Fx, PTR};
use super::{Supply, entry};
use crate::check::db::ItemId;
use crate::check::ty::Args;

/// The lane types and mask types with the vector type holding them, by name in `core::simd`;
/// `F16x8` is computed lane by lane on every target.
const VECTORS: [(&str, Type); 13] = [
    ("U8x16", types::I8X16),
    ("I8x16", types::I8X16),
    ("Mask8x16", types::I8X16),
    ("U16x8", types::I16X8),
    ("I16x8", types::I16X8),
    ("Mask16x8", types::I16X8),
    ("U32x4", types::I32X4),
    ("I32x4", types::I32X4),
    ("Mask32x4", types::I32X4),
    ("U64x2", types::I64X2),
    ("I64x2", types::I64X2),
    ("Mask64x2", types::I64X2),
    ("F32x4", types::F32X4),
];

/// The members computed with vector instructions on a host target.
const VECTORIZED: [&str; 30] = [
    "splat",
    "load",
    "wrapping_add",
    "wrapping_sub",
    "wrapping_mul",
    "saturating_add",
    "saturating_sub",
    "bit_and",
    "bit_or",
    "bit_xor",
    "bit_not",
    "shl",
    "shr",
    "lanes_eq",
    "lanes_lt",
    "lanes_gt",
    "select",
    "add",
    "sub",
    "mul",
    "div",
    "sqrt",
    "min",
    "max",
    "from_i32x4",
    "to_i32x4",
    "dot",
    "any",
    "all",
    "bitmask",
];

/// The vector type of a lane or mask type named by its canonical path, and whether its lanes are
/// signed.
pub fn vector(owner: &str) -> Option<(Type, bool)> {
    let name = owner.strip_prefix("core::simd::")?;
    let (_, ty) = VECTORS.iter().find(|(lanes, _)| *lanes == name)?;
    Some((*ty, name.starts_with('I')))
}

/// The flags of an access to lanes: unable to fault, aligned only as their lanes are.
pub fn lanes() -> MemFlagsData {
    MemFlagsData::new().with_notrap()
}

/// Defines `store` of a lane type: its lanes copied to the elements from `at`.
pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    if !supply.path.starts_with("core::simd::") || supply.member() != "store" {
        return false;
    }
    let lanes = supply.param(0).address();
    let view = supply.param(1).address();
    let start = fx.load(PTR, view, 0);
    let element = lo.layout(&supply.abi.params[0].0.ty().clone()).align;
    let offset =
        fx.b.ins()
            .imul_imm_s(supply.param(2).scalar(), element as i64);
    let at = fx.b.ins().iadd(start, offset);
    lo.copy(fx, at, lanes, 16, element);
    supply.done(lo, fx);
    true
}

/// Whether a host target computes a member of `core::simd`, named by its canonical path, with
/// vector instructions in place of its text.
pub fn vectorized(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(owner, member)| vector(owner).is_some() && VECTORIZED.contains(&member))
}

/// Defines a member `vectorized` accepts.
pub fn lower_vector(lo: &mut Lowering, fx: &mut Fx, abi: &Abi, item: ItemId, args: &Args) {
    let (sret, failure, params) = entry(fx, abi);
    let path = lo.ck.db.canonical(item);
    let (owner, member) = path.rsplit_once('.').expect("a member path");
    let (ty, signed) = vector(owner).expect("a lane type");
    let supply = Supply {
        abi,
        args: args.clone(),
        path: path.clone(),
        owner: None,
        sret,
        failure,
        params,
        wrap: None,
    };
    let operand = |fx: &mut Fx, index: usize, ty: Type| {
        let address = supply.param(index).address();
        fx.b.ins().load(ty, lanes(), address, 0)
    };
    let int_ty = ty.as_int();
    let compare = |fx: &mut Fx, signed_cc: IntCC, unsigned_cc: IntCC| {
        let (a, b) = (operand(fx, 0, ty), operand(fx, 1, ty));
        match ty {
            types::F32X4 => {
                let cc = match signed_cc {
                    IntCC::Equal => FloatCC::Equal,
                    IntCC::SignedLessThan => FloatCC::LessThan,
                    _ => FloatCC::GreaterThan,
                };
                fx.b.ins().fcmp(cc, a, b)
            }
            _ if signed => fx.b.ins().icmp(signed_cc, a, b),
            _ => fx.b.ins().icmp(unsigned_cc, a, b),
        }
    };
    let binary = |fx: &mut Fx| (operand(fx, 0, ty), operand(fx, 1, ty));
    let result = match member {
        "splat" => fx.b.ins().splat(ty, supply.param(0).scalar()),
        "load" => {
            let view = supply.param(0).address();
            let start = fx.load(PTR, view, 0);
            let lane = i64::from(ty.lane_type().bytes());
            let offset = fx.b.ins().imul_imm_s(supply.param(1).scalar(), lane);
            let at = fx.b.ins().iadd(start, offset);
            fx.b.ins().load(ty, lanes(), at, 0)
        }
        "wrapping_add" => {
            let (a, b) = binary(fx);
            fx.b.ins().iadd(a, b)
        }
        "wrapping_sub" => {
            let (a, b) = binary(fx);
            fx.b.ins().isub(a, b)
        }
        "wrapping_mul" => {
            let (a, b) = binary(fx);
            fx.b.ins().imul(a, b)
        }
        "saturating_add" => {
            let (a, b) = binary(fx);
            match signed {
                true => fx.b.ins().sadd_sat(a, b),
                false => fx.b.ins().uadd_sat(a, b),
            }
        }
        "saturating_sub" => {
            let (a, b) = binary(fx);
            match signed {
                true => fx.b.ins().ssub_sat(a, b),
                false => fx.b.ins().usub_sat(a, b),
            }
        }
        "bit_and" => {
            let (a, b) = binary(fx);
            fx.b.ins().band(a, b)
        }
        "bit_or" => {
            let (a, b) = binary(fx);
            fx.b.ins().bor(a, b)
        }
        "bit_xor" => {
            let (a, b) = binary(fx);
            fx.b.ins().bxor(a, b)
        }
        "bit_not" => {
            let a = operand(fx, 0, ty);
            fx.b.ins().bnot(a)
        }
        "shl" => {
            let a = operand(fx, 0, ty);
            fx.b.ins().ishl(a, supply.param(1).scalar())
        }
        "shr" if signed => {
            let a = operand(fx, 0, ty);
            fx.b.ins().sshr(a, supply.param(1).scalar())
        }
        "shr" => {
            let a = operand(fx, 0, ty);
            fx.b.ins().ushr(a, supply.param(1).scalar())
        }
        "lanes_eq" => compare(fx, IntCC::Equal, IntCC::Equal),
        "lanes_lt" => compare(fx, IntCC::SignedLessThan, IntCC::UnsignedLessThan),
        "lanes_gt" => compare(fx, IntCC::SignedGreaterThan, IntCC::UnsignedGreaterThan),
        "select" => {
            let mask = operand(fx, 0, int_ty);
            let (if_true, if_false) = (operand(fx, 1, int_ty), operand(fx, 2, int_ty));
            fx.b.ins().bitselect(mask, if_true, if_false)
        }
        "add" => {
            let (a, b) = binary(fx);
            fx.b.ins().fadd(a, b)
        }
        "sub" => {
            let (a, b) = binary(fx);
            fx.b.ins().fsub(a, b)
        }
        "mul" => {
            let (a, b) = binary(fx);
            fx.b.ins().fmul(a, b)
        }
        "div" => {
            let (a, b) = binary(fx);
            fx.b.ins().fdiv(a, b)
        }
        "min" => {
            let (a, b) = binary(fx);
            fx.b.ins().fmin(a, b)
        }
        "max" => {
            let (a, b) = binary(fx);
            fx.b.ins().fmax(a, b)
        }
        "sqrt" => {
            let a = operand(fx, 0, ty);
            fx.b.ins().sqrt(a)
        }
        "from_i32x4" => {
            let value = operand(fx, 0, types::I32X4);
            fx.b.ins().fcvt_from_sint(types::F32X4, value)
        }
        "to_i32x4" => {
            let value = operand(fx, 0, ty);
            fx.b.ins().fcvt_to_sint_sat(types::I32X4, value)
        }
        "dot" => {
            let (a, b) = binary(fx);
            let acc = operand(fx, 2, types::I32X4);
            dot(fx, a, b, acc)
        }
        "any" => {
            let a = operand(fx, 0, ty);
            let any = fx.b.ins().vany_true(a);
            supply.ret(lo, fx, Held::Scalar(any));
            return;
        }
        "all" => {
            let a = operand(fx, 0, ty);
            let all = fx.b.ins().vall_true(a);
            supply.ret(lo, fx, Held::Scalar(all));
            return;
        }
        "bitmask" => {
            let a = operand(fx, 0, ty);
            let bits = fx.b.ins().vhigh_bits(types::I16, a);
            supply.ret(lo, fx, Held::Scalar(bits));
            return;
        }
        other => panic!("no vector lowering of {owner}.{other}"),
    };
    let sret = supply.sret.expect("a lane value's address");
    fx.b.ins().store(lanes(), result, sret, 0);
    supply.ret(lo, fx, Held::Address(sret));
}

/// `acc` plus, lane by lane, the sums of the four products of the signed bytes of `a` and `b`
/// each lane spans, in the form a dot product instruction computes.
fn dot(fx: &mut Fx, a: Value, b: Value, acc: Value) -> Value {
    let low_a = fx.b.ins().swiden_low(a);
    let low_b = fx.b.ins().swiden_low(b);
    let low = fx.b.ins().imul(low_a, low_b);
    let high_a = fx.b.ins().swiden_high(a);
    let high_b = fx.b.ins().swiden_high(b);
    let high = fx.b.ins().imul(high_a, high_b);
    let mut pairs = [low, high].map(|products| {
        let first = fx.b.ins().swiden_low(products);
        let second = fx.b.ins().swiden_high(products);
        fx.b.ins().iadd_pairwise(first, second)
    });
    pairs[0] = fx.b.ins().iadd_pairwise(pairs[0], pairs[1]);
    fx.b.ins().iadd(pairs[0], acc)
}
