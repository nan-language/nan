//! Lists and bytes over vectors, bounded lists holding their elements inline, and their owning
//! cursors.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::super::Lowering;
use super::super::super::body::Held;
use super::super::super::data::const_arg;
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::Routine;
use super::super::Supply;
use super::vector::{self, Element};
use crate::check::ty::{Scalar, Ty};

/// A member of `mem::List[T]` or `mem::Bytes`, whose elements have type `element`.
pub fn list(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, element: &Ty) {
    let shape = Element::of(lo, element);
    match supply.member() {
        "empty" => {
            let sret = supply.sret.expect("a list's address");
            let alloc = fx.load(PTR, supply.param(0).address(), 0);
            vector::empty(fx, sret, alloc, shape);
            supply.done(lo, fx);
        }
        "zeroed" | "try_zeroed" => {
            let sret = supply.sret.expect("a list's address");
            let len = supply.param(0).scalar();
            let alloc = fx.load(PTR, supply.param(1).address(), 0);
            vector::empty(fx, sret, alloc, shape);
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, len, 0);
            fx.when(present, |fx| {
                let drawn = vector::resize(lo, fx, sret, len, shape);
                let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, drawn, 0);
                fx.when(failed, |fx| supply.exhausted(lo, fx, &[]));
                let data = fx.load(PTR, sret, vector::DATA);
                let zero = fx.ptr(0);
                lo.call_routine(fx, Routine::Fill, &[data, zero, len]);
                fx.store(len, sret, vector::LEN);
            });
            supply.done(lo, fx);
        }
        "len" | "capacity" => {
            let field = match supply.member() {
                "len" => vector::LEN,
                _ => vector::CAPACITY,
            };
            let value = fx.load(PTR, supply.param(0).address(), field);
            supply.ret(lo, fx, Held::Scalar(value));
        }
        "push" | "try_push" => {
            let address = supply.param(0).address();
            let value = supply.param(1);
            let len = fx.load(PTR, address, vector::LEN);
            let needed = fx.b.ins().iadd_imm_s(len, 1);
            let room = vector::reserve(lo, fx, address, needed, shape);
            let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, room, 0);
            fx.when(failed, |fx| {
                supply.exhausted(lo, fx, &[(value, element.clone())])
            });
            vector::push_reserved(lo, fx, address, value, element);
            supply.done(lo, fx);
        }
        "append" | "try_append" => {
            let address = supply.param(0).address();
            let data = supply.param(1).address();
            let start = fx.load(PTR, data, 0);
            let count = fx.load(PTR, data, 8);
            append(lo, fx, supply, address, start, count);
            supply.done(lo, fx);
        }
        "insert" | "try_insert" => {
            let address = supply.param(0).address();
            let index = supply.param(1).scalar();
            let value = supply.param(2);
            let len = fx.load(PTR, address, vector::LEN);
            let needed = fx.b.ins().iadd_imm_s(len, 1);
            let room = vector::reserve(lo, fx, address, needed, shape);
            let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, room, 0);
            fx.when(failed, |fx| {
                supply.exhausted(lo, fx, &[(value, element.clone())])
            });
            let at = vector::open_gap(lo, fx, address, index, shape);
            lo.store_held(fx, at, value, element);
            supply.done(lo, fx);
        }
        "reserve" | "try_reserve" => {
            let address = supply.param(0).address();
            let additional = supply.param(1).scalar();
            let len = fx.load(PTR, address, vector::LEN);
            let (needed, overflow) = fx.b.ins().uadd_overflow(len, additional);
            let room = fx.choose(
                overflow,
                &[types::I8],
                |fx| vec![fx.int(types::I8, 0)],
                |fx| vec![vector::reserve(lo, fx, address, needed, shape)],
            )[0];
            let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, room, 0);
            fx.when(failed, |fx| supply.exhausted(lo, fx, &[]));
            supply.done(lo, fx);
        }
        "pop" => {
            let address = supply.param(0).address();
            let len = fx.load(PTR, address, vector::LEN);
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, len, 0);
            let option = supply.abi.result.clone();
            let layout = lo.layout(&option);
            let result = supply
                .sret
                .unwrap_or_else(|| fx.slot(layout.size, layout.align));
            let element = element.clone();
            lo.write_option_with(fx, &option, result, present, |lo, fx, payload| {
                let last = fx.b.ins().iadd_imm_s(len, -1);
                let data = fx.load(PTR, address, vector::DATA);
                let at = shape.at(fx, data, last);
                let held = lo.held_at(fx, &element, at);
                lo.store_held(fx, payload, held, &element);
                fx.store(last, address, vector::LEN);
            });
            let held = lo.held_at(fx, &option, result);
            supply.ret(lo, fx, held);
        }
        "remove" => {
            let address = supply.param(0).address();
            let index = supply.param(1).scalar();
            let layout = lo.layout(element);
            let result = supply
                .sret
                .unwrap_or_else(|| fx.slot(layout.size, layout.align));
            let data = fx.load(PTR, address, vector::DATA);
            let at = shape.at(fx, data, index);
            let held = lo.held_at(fx, element, at);
            lo.store_held(fx, result, held, element);
            vector::close_gap(lo, fx, address, index, shape);
            let held = lo.held_at(fx, element, result);
            supply.ret(lo, fx, held);
        }
        "truncate" => {
            let address = supply.param(0).address();
            let len = supply.param(1).scalar();
            let current = fx.load(PTR, address, vector::LEN);
            let shorter = fx.b.ins().icmp(IntCC::UnsignedLessThan, len, current);
            fx.when(shorter, |fx| {
                vector::release_elements(lo, fx, address, element, len);
                fx.store(len, address, vector::LEN);
            });
            supply.done(lo, fx);
        }
        "at" | "at_mut" => {
            let address = supply.param(0).address();
            let index = supply.param(1).scalar();
            let data = fx.load(PTR, address, vector::DATA);
            let at = shape.at(fx, data, index);
            supply.ret(lo, fx, Held::Address(at));
        }
        "view" | "view_mut" | "cursor_mut" => {
            let sret = supply.sret.expect("a view's address");
            vector::write_view(fx, sret, supply.param(0).address());
            supply.done(lo, fx);
        }
        "into_cursor" => {
            let sret = supply.sret.expect("a cursor's address");
            let source = supply.param(0).address();
            lo.copy(fx, sret, source, 32, 8);
            let zero = fx.ptr(0);
            fx.store(zero, sret, 32);
            supply.done(lo, fx);
        }
        "retain" => {
            let address = supply.param(0).address();
            let keep = supply.param(1).address();
            let fn_ty = fn_param(supply, 1);
            let element = element.clone();
            vector::retain(lo, fx, address, &element.clone(), |lo, fx, at| {
                let held = lo.held_at(fx, &element, at);
                lo.call_value(fx, &fn_ty, keep, &[held]).result.scalar()
            });
            supply.done(lo, fx);
        }
        "sort_by" => {
            sort_by(lo, fx, supply, element);
            supply.done(lo, fx);
        }
        member => panic!("no lowering of {}.{member}", supply.path),
    }
}

/// The function type of a parameter.
pub fn fn_param(supply: &Supply, index: usize) -> crate::check::ty::FnTy {
    match supply.abi.params[index].0.ty() {
        Ty::Fn(fn_ty) => (**fn_ty).clone(),
        other => panic!("a function parameter, found {other:?}"),
    }
}

/// Appends `count` bytes from `start` to the bytes at an address, as `append` and `try_append`
/// do.
pub fn append(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    address: Value,
    start: Value,
    count: Value,
) {
    let shape = Element { size: 1, align: 1 };
    let len = fx.load(PTR, address, vector::LEN);
    let (needed, overflow) = fx.b.ins().uadd_overflow(len, count);
    let room = fx.choose(
        overflow,
        &[types::I8],
        |fx| vec![fx.int(types::I8, 0)],
        |fx| vec![vector::reserve(lo, fx, address, needed, shape)],
    )[0];
    let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, room, 0);
    fx.when(failed, |fx| supply.exhausted(lo, fx, &[]));
    let data = fx.load(PTR, address, vector::DATA);
    let end = fx.b.ins().iadd(data, len);
    lo.call_routine(fx, Routine::Copy, &[end, start, count]);
    fx.store(needed, address, vector::LEN);
}

/// `sort_by`: the stable binary insertion sort its section states, calling `order` with the
/// element moved and an element before it.
fn sort_by(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, element: &Ty) {
    let address = supply.param(0).address();
    let order = supply.param(1).address();
    let fn_ty = fn_param(supply, 1);
    let shape = Element::of(lo, element);
    let data = fx.load(PTR, address, vector::DATA);
    let len = fx.load(PTR, address, vector::LEN);
    let moved = fx.slot(shape.size, shape.align);
    let one = fx.ptr(1);
    fx.each(one, len, |fx, index| {
        let at = shape.at(fx, data, index);
        let size = fx.ptr(shape.size);
        lo.call_routine(fx, Routine::Copy, &[moved, at, size]);
        let zero = fx.ptr(0);
        let bounds = fx.repeat(&[zero, index], |fx, state| {
            let (low, high) = (state[0], state[1]);
            let open = fx.b.ins().icmp(IntCC::UnsignedLessThan, low, high);
            let next = fx.choose(
                open,
                &[PTR, PTR],
                |fx| {
                    let sum = fx.b.ins().iadd(low, high);
                    let middle = fx.b.ins().ushr_imm_s(sum, 1);
                    let candidate = shape.at(fx, data, middle);
                    let left = lo.held_at(fx, element, moved);
                    let right = lo.held_at(fx, element, candidate);
                    let ordering = lo.call_value(fx, &fn_ty, order, &[left, right]).result;
                    let ordering = lo.ordering_index(fx, ordering);
                    let less = fx.b.ins().icmp_imm_s(IntCC::Equal, ordering, 0);
                    let above = fx.b.ins().iadd_imm_s(middle, 1);
                    let low = fx.b.ins().select(less, low, above);
                    let high = fx.b.ins().select(less, middle, high);
                    vec![low, high]
                },
                |_| vec![low, high],
            );
            (open, next)
        });
        let low = bounds[0];
        let target = shape.at(fx, data, low);
        let after = fx.b.ins().iadd_imm_s(target, shape.size as i64);
        let between = fx.b.ins().isub(index, low);
        let between = shape.bytes(fx, between);
        lo.call_routine(fx, Routine::Copy, &[after, target, between]);
        let size = fx.ptr(shape.size);
        lo.call_routine(fx, Routine::Copy, &[target, moved, size]);
    });
}

/// A member of `mem::Bounded[T, N]`: its length, then its elements inline.
pub fn bounded(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let bounded_ty = supply.owner.clone().expect("a bounded list's type");
    let element = supply.type_arg(0);
    let shape = Element::of(lo, &element);
    let len_at = lo.layouts.field(lo.ck, &bounded_ty, None, "len").offset;
    let items_at = lo.layouts.field(lo.ck, &bounded_ty, None, "items").offset;
    match supply.member() {
        "empty" => {
            let sret = supply.sret.expect("a bounded list's address");
            let zero = fx.ptr(0);
            fx.store(zero, sret, len_at);
            supply.done(lo, fx);
        }
        "len" => {
            let len = fx.load(PTR, supply.param(0).address(), len_at);
            supply.ret(lo, fx, Held::Scalar(len));
        }
        "is_full" => {
            let len = fx.load(PTR, supply.param(0).address(), len_at);
            let bound = const_arg(&bounded_ty.args()[1]);
            let full = fx.b.ins().icmp_imm_s(IntCC::Equal, len, bound as i64);
            supply.ret(lo, fx, Held::Scalar(full));
        }
        "push" => {
            let address = supply.param(0).address();
            let len = fx.load(PTR, address, len_at);
            let items = fx.offset(address, items_at);
            let at = shape.at(fx, items, len);
            lo.store_held(fx, at, supply.param(1), &element);
            let len = fx.b.ins().iadd_imm_s(len, 1);
            fx.store(len, address, len_at);
            supply.done(lo, fx);
        }
        "pop" => {
            let address = supply.param(0).address();
            let len = fx.load(PTR, address, len_at);
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, len, 0);
            let option = supply.abi.result.clone();
            let layout = lo.layout(&option);
            let result = supply
                .sret
                .unwrap_or_else(|| fx.slot(layout.size, layout.align));
            lo.write_option_with(fx, &option, result, present, |lo, fx, payload| {
                let last = fx.b.ins().iadd_imm_s(len, -1);
                let items = fx.offset(address, items_at);
                let at = shape.at(fx, items, last);
                let held = lo.held_at(fx, &element, at);
                lo.store_held(fx, payload, held, &element);
                fx.store(last, address, len_at);
            });
            let held = lo.held_at(fx, &option, result);
            supply.ret(lo, fx, held);
        }
        "at" | "at_mut" => {
            let address = supply.param(0).address();
            let index = supply.param(1).scalar();
            let items = fx.offset(address, items_at);
            let at = shape.at(fx, items, index);
            supply.ret(lo, fx, Held::Address(at));
        }
        "view" | "view_mut" | "cursor_mut" => {
            let address = supply.param(0).address();
            let sret = supply.sret.expect("a view's address");
            let items = fx.offset(address, items_at);
            let len = fx.load(PTR, address, len_at);
            fx.store(items, sret, 0);
            fx.store(len, sret, 8);
            supply.done(lo, fx);
        }
        "into_cursor" => {
            let source = supply.param(0).address();
            let sret = supply.sret.expect("a cursor's address");
            let cursor = supply.abi.result.clone();
            let position_at = lo.layouts.field(lo.ck, &cursor, None, "position").offset;
            let held_at = lo.layouts.field(lo.ck, &cursor, None, "bounded").offset;
            let zero = fx.ptr(0);
            fx.store(zero, sret, position_at);
            let target = fx.offset(sret, held_at);
            let layout = lo.layout(&bounded_ty);
            lo.copy(fx, target, source, layout.size, layout.align);
            supply.done(lo, fx);
        }
        member => panic!("no lowering of mem::Bounded.{member}"),
    }
}

/// `next` of an owning cursor over a vector: its storage, length, capacity, allocator and
/// position. The element moves out.
pub fn vector_next(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, element: &Ty) {
    let cursor = supply.param(0).address();
    let shape = Element::of(lo, element);
    let len = fx.load(PTR, cursor, vector::LEN);
    let position = fx.load(PTR, cursor, 32);
    let present = fx.b.ins().icmp(IntCC::UnsignedLessThan, position, len);
    let option = supply.abi.result.clone();
    let layout = lo.layout(&option);
    let result = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    let element = element.clone();
    lo.write_option_with(fx, &option, result, present, |lo, fx, payload| {
        let data = fx.load(PTR, cursor, vector::DATA);
        let at = shape.at(fx, data, position);
        let held = lo.held_at(fx, &element, at);
        lo.store_held(fx, payload, held, &element);
        let next = fx.b.ins().iadd_imm_s(position, 1);
        fx.store(next, cursor, 32);
    });
    let held = lo.held_at(fx, &option, result);
    supply.ret(lo, fx, held);
}

/// Releases an owning cursor over a vector: the elements from its position, then the storage.
pub fn release_cursor(lo: &mut Lowering, fx: &mut Fx, address: Value, element: &Ty) {
    let position = fx.load(PTR, address, 32);
    vector::release(lo, fx, address, element, position);
}

/// `next` of a bounded list's cursor: its position, then the bounded list. The element moves
/// out.
pub fn bounded_next(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let cursor_ty = supply.owner.clone().expect("a cursor type");
    let element = supply.type_arg(0);
    let shape = Element::of(lo, &element);
    let cursor = supply.param(0).address();
    let position_at = lo.layouts.field(lo.ck, &cursor_ty, None, "position").offset;
    let bounded_at = lo.layouts.field(lo.ck, &cursor_ty, None, "bounded").offset;
    let bounded_ty = lo.layouts.field(lo.ck, &cursor_ty, None, "bounded").ty;
    let len_at = lo.layouts.field(lo.ck, &bounded_ty, None, "len").offset + bounded_at;
    let items_at = lo.layouts.field(lo.ck, &bounded_ty, None, "items").offset + bounded_at;
    let position = fx.load(PTR, cursor, position_at);
    let len = fx.load(PTR, cursor, len_at);
    let present = fx.b.ins().icmp(IntCC::UnsignedLessThan, position, len);
    let option = supply.abi.result.clone();
    let layout = lo.layout(&option);
    let result = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    lo.write_option_with(fx, &option, result, present, |lo, fx, payload| {
        let items = fx.offset(cursor, items_at);
        let at = shape.at(fx, items, position);
        let held = lo.held_at(fx, &element, at);
        lo.store_held(fx, payload, held, &element);
        let next = fx.b.ins().iadd_imm_s(position, 1);
        fx.store(next, cursor, position_at);
    });
    let held = lo.held_at(fx, &option, result);
    supply.ret(lo, fx, held);
}

/// `Untrusted.append` and `try_append`: bytes appended to untrusted bytes.
pub fn append_untrusted(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let address = supply.param(0).address();
    let data = supply.param(1).address();
    let start = fx.load(PTR, data, 0);
    let count = fx.load(PTR, data, 8);
    append(lo, fx, supply, address, start, count);
    supply.done(lo, fx);
}

/// `Untrusted.copy_prefix` and `try_copy_prefix`: new untrusted bytes holding a prefix of the
/// content's bytes.
pub fn copy_prefix(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, start: Value) {
    let count = supply.param(1).scalar();
    let alloc = fx.load(PTR, supply.param(2).address(), 0);
    let sret = supply.sret.expect("a bytes' address");
    let shape = Element { size: 1, align: 1 };
    vector::empty(fx, sret, alloc, shape);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, count, 0);
    fx.when(present, |fx| {
        let drawn = vector::resize(lo, fx, sret, count, shape);
        let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, drawn, 0);
        fx.when(failed, |fx| supply.exhausted(lo, fx, &[]));
        let data = fx.load(PTR, sret, vector::DATA);
        lo.call_routine(fx, Routine::Copy, &[data, start, count]);
        fx.store(count, sret, vector::LEN);
    });
    supply.done(lo, fx);
}

/// The element type of `mem::Bytes`.
pub fn byte() -> Ty {
    Ty::Scalar(Scalar::U8)
}
