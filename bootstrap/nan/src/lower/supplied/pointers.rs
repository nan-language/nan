//! Pointers and atomics: the members of `hw::Ptr`, memory fences, and the atomic integers and
//! flag of `core::atomic`, each atomic laid out as its value.

use cranelift_codegen::ir::{
    AtomicRmwOp, InstBuilder, MemFlagsData, Type, Value, condcodes::IntCC, types,
};

use super::super::Lowering;
use super::super::body::Held;
use super::super::fx::{Fx, PTR};
use super::super::runtime::{Routine, state};
use super::Supply;
use crate::check::ty::{Scalar, Ty};

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    let owner = supply
        .path
        .rsplit_once('.')
        .map_or(supply.path.as_str(), |(owner, _)| owner);
    match owner {
        "hw::Ptr" => pointer(lo, fx, supply),
        "hw::fence" => {
            fx.b.ins().fence();
            supply.done(lo, fx);
        }
        "hw::cpu_count" => {
            let state_address = lo.state(fx);
            let count = fx.load(PTR, state_address, state::THREAD_COUNT);
            let count = fx.b.ins().ireduce(types::I32, count);
            supply.ret(lo, fx, Held::Scalar(count));
        }
        owner if owner.starts_with("core::atomic::") => atomic(lo, fx, supply),
        _ => return false,
    }
    true
}

/// The register type of an atomic or pointee of a scalar layout.
fn register(lo: &mut Lowering, ty: &Ty) -> Type {
    match lo.layout(ty).shape {
        super::super::layout::Shape::Scalar { ty, .. } => ty,
        ref shape => panic!("an atomic access to {ty:?} laid out as {shape:?}"),
    }
}

fn pointer(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let element = supply.type_arg(0);
    let layout = lo.layout(&element);
    match supply.member() {
        "of_addr" | "addr" | "cast" => supply.ret(lo, fx, supply.param(0)),
        "of" | "of_mut" => {
            let address = match supply.param(0) {
                Held::Address(address) => address,
                held => {
                    let one = fx.ptr(1);
                    let size = fx.ptr(layout.size.max(1));
                    let kept = lo.call_libc(fx, "calloc", &[one, size], &[PTR])[0];
                    lo.store_held(fx, kept, held, &element);
                    kept
                }
            };
            supply.ret(lo, fx, Held::Scalar(address));
        }
        "of_view" | "of_view_mut" => {
            let start = fx.load(PTR, supply.param(0).address(), 0);
            supply.ret(lo, fx, Held::Scalar(start));
        }
        "is_aligned" => {
            let address = supply.param(0).scalar();
            let mask = layout.align.max(1) as i64 - 1;
            let low = fx.b.ins().band_imm_s(address, mask);
            let aligned = fx.b.ins().icmp_imm_s(IntCC::Equal, low, 0);
            supply.ret(lo, fx, Held::Scalar(aligned));
        }
        "read" => {
            let address = supply.param(0).scalar();
            let held = lo.held_at(fx, &element, address);
            supply.ret(lo, fx, held);
        }
        "write" => {
            let address = supply.param(0).scalar();
            lo.store_held(fx, address, supply.param(1), &element);
            supply.done(lo, fx);
        }
        "offset" => {
            let address = supply.param(0).scalar();
            let count = supply.param(1).scalar();
            let bytes = fx.b.ins().imul_imm_s(count, layout.size as i64);
            let moved = fx.b.ins().iadd(address, bytes);
            supply.ret(lo, fx, Held::Scalar(moved));
        }
        "copy" => {
            let (from, to) = (supply.param(0).scalar(), supply.param(1).scalar());
            let count = supply.param(2).scalar();
            let bytes = fx.b.ins().imul_imm_s(count, layout.size as i64);
            lo.call_routine(fx, Routine::Copy, &[to, from, bytes]);
            supply.done(lo, fx);
        }
        member => {
            let address = supply.param(0).scalar();
            let ty = register(lo, &element);
            let result = atomic_op(
                lo,
                fx,
                supply,
                member.trim_start_matches("atomic_"),
                address,
                ty,
                &element,
            );
            supply.ret(lo, fx, result);
        }
    }
}

/// The atomic operation `member` on the value of register type `ty` at an address, its
/// operands from the operation's second parameter on.
fn atomic_op(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    member: &str,
    address: Value,
    ty: Type,
    element: &Ty,
) -> Held {
    let flags = MemFlagsData::trusted();
    let operand = |index: usize| supply.param(index).scalar();
    match member {
        "load" => Held::Scalar(fx.b.ins().atomic_load(ty, flags, address)),
        "store" => {
            fx.b.ins().atomic_store(flags, operand(1), address);
            Held::Empty
        }
        "swap" => {
            let previous =
                fx.b.ins()
                    .atomic_rmw(ty, flags, AtomicRmwOp::Xchg, address, operand(1));
            Held::Scalar(previous)
        }
        "wrapping_add" => {
            let previous =
                fx.b.ins()
                    .atomic_rmw(ty, flags, AtomicRmwOp::Add, address, operand(1));
            Held::Scalar(previous)
        }
        "fetch_and" | "fetch_or" | "fetch_xor" => {
            let op = match member {
                "fetch_and" => AtomicRmwOp::And,
                "fetch_or" => AtomicRmwOp::Or,
                _ => AtomicRmwOp::Xor,
            };
            Held::Scalar(fx.b.ins().atomic_rmw(ty, flags, op, address, operand(1)))
        }
        "compare_swap" => {
            let (expected, replacement) = (operand(1), operand(2));
            let previous = fx.b.ins().atomic_cas(flags, address, expected, replacement);
            let swapped = fx.b.ins().icmp(IntCC::Equal, previous, expected);
            let result = supply.abi.result.clone();
            let layout = lo.layout(&result);
            let slot = supply
                .sret
                .unwrap_or_else(|| fx.slot(layout.size, layout.align));
            let ok = lo.variant_value(fx, &result, "Ok", &[Held::Scalar(previous)], false);
            let err = lo.variant_value(fx, &result, "Err", &[Held::Scalar(previous)], false);
            let chosen = fx.b.ins().select(swapped, ok.address(), err.address());
            lo.copy(fx, slot, chosen, layout.size, layout.align);
            lo.held_at(fx, &result, slot)
        }
        "fetch_add" | "fetch_sub" => {
            let amount = operand(1);
            let current = fx.b.ins().atomic_load(ty, flags, address);
            let signed = element.scalar().is_some_and(Scalar::is_signed);
            let state = fx.repeat(&[current], |fx, state| {
                let previous = state[0];
                let (next, overflow) = match (member, signed) {
                    ("fetch_add", false) => fx.b.ins().uadd_overflow(previous, amount),
                    ("fetch_add", true) => fx.b.ins().sadd_overflow(previous, amount),
                    (_, false) => fx.b.ins().usub_overflow(previous, amount),
                    (_, true) => fx.b.ins().ssub_overflow(previous, amount),
                };
                let fits = fx.b.ins().bxor_imm_s(overflow, 1);
                let seen = fx.choose(
                    fits,
                    &[ty],
                    |fx| vec![fx.b.ins().atomic_cas(flags, address, previous, next)],
                    |_| vec![previous],
                )[0];
                let raced = fx.b.ins().icmp(IntCC::NotEqual, seen, previous);
                let again = fx.b.ins().band(fits, raced);
                (again, vec![seen])
            });
            let previous = state[0];
            let (_, overflow) = match (member, signed) {
                ("fetch_add", false) => fx.b.ins().uadd_overflow(previous, amount),
                ("fetch_add", true) => fx.b.ins().sadd_overflow(previous, amount),
                (_, false) => fx.b.ins().usub_overflow(previous, amount),
                (_, true) => fx.b.ins().ssub_overflow(previous, amount),
            };
            let present = fx.b.ins().bxor_imm_s(overflow, 1);
            let option = supply.abi.result.clone();
            let layout = lo.layout(&option);
            let slot = supply
                .sret
                .unwrap_or_else(|| fx.slot(layout.size, layout.align));
            lo.write_option(fx, &option, slot, present, Held::Scalar(previous));
            lo.held_at(fx, &option, slot)
        }
        other => panic!("no lowering of the atomic operation {other}"),
    }
}

fn atomic(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let atomic_ty = supply.owner.clone().expect("an atomic type");
    let value_ty = lo.layouts.field(lo.ck, &atomic_ty, None, "value").ty;
    let ty = register(lo, &value_ty);
    match supply.member() {
        "of" => {
            let sret = supply.sret.expect("an atomic's address");
            lo.store_held(fx, sret, supply.param(0), &value_ty);
            supply.done(lo, fx);
        }
        member => {
            let address = supply.param(0).address();
            let result = atomic_op(lo, fx, supply, member, address, ty, &value_ty);
            supply.ret(lo, fx, result);
        }
    }
}
