//! Sequences: arrays, slices and their cursors, texts and char cursors, ranges, the supplied
//! members of `Option`, and the content-blind operations of `Untrusted`.
//!
//! A slice, text or slice cursor is the address of its first element and its length; an array
//! cursor is its position and the array.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::body::Held;
use super::super::fx::{Fx, PTR};
use super::super::layout::Shape;
use super::super::types::Wrap;
use super::super::{Lowering, data::const_arg};
use super::{Supply, element_at};
use crate::check::ty::{Scalar, Ty};

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    let owner = supply.path.rsplit_once('.').map_or("", |(owner, _)| owner);
    match owner {
        "core::Array" => array(lo, fx, supply),
        "core::Slice" | "core::SliceMut" | "core::Str" => slice(lo, fx, supply),
        "core::SliceCursor" | "core::SliceCursorMut" => slice_next(lo, fx, supply),
        "core::ArrayCursor" => array_next(lo, fx, supply),
        "core::CharCursor" => char_next(lo, fx, supply),
        "core::Range" => range_next(lo, fx, supply),
        "core::Option" => option(lo, fx, supply),
        "core::Untrusted" => untrusted(lo, fx, supply),
        _ => return false,
    }
    true
}

/// The element type of a sequence type: an array's, a slice's, or `u8` for a text.
fn element(lo: &Lowering, ty: &Ty) -> Ty {
    if lo.ck.is_std(ty, "core::Str") {
        return Ty::Scalar(Scalar::U8);
    }
    ty.type_arg(0).cloned().expect("an element type")
}

/// Writes a view of `len` elements from `start` at an address.
fn write_view(fx: &mut Fx, address: Value, start: Value, len: Value) {
    fx.store(start, address, 0);
    fx.store(len, address, 8);
}

fn array(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let array_ty = supply.owner.clone().expect("an array type");
    let element = element(lo, &array_ty);
    let count = const_arg(&array_ty.args()[1]) as u64;
    match supply.member() {
        "filled" => {
            let sret = supply.sret.expect("an array's address");
            let value = supply.param(0);
            let start = fx.ptr(0);
            let end = fx.ptr(count);
            let size = lo.layout(&element).size;
            fx.each(start, end, |fx, index| {
                let offset = fx.b.ins().imul_imm_s(index, size as i64);
                let at = fx.b.ins().iadd(sret, offset);
                lo.store_held(fx, at, value, &element);
            });
            supply.done(lo, fx);
        }
        "at" | "at_mut" => {
            let base = supply.param(0).address();
            let index = supply.param(1).scalar();
            let at = element_at(lo, fx, &element, base, index);
            supply.ret(lo, fx, Held::Address(at));
        }
        "view" | "view_mut" | "cursor" | "cursor_mut" => {
            let base = supply.param(0).address();
            let sret = supply.sret.expect("a view's address");
            let len = fx.ptr(count);
            write_view(fx, sret, base, len);
            supply.done(lo, fx);
        }
        "into_cursor" => {
            let source = supply.param(0).address();
            let sret = supply.sret.expect("a cursor's address");
            let cursor = supply.abi.result.clone();
            let position = lo.layouts.field(lo.ck, &cursor, None, "position").offset;
            let array_at = lo.layouts.field(lo.ck, &cursor, None, "array").offset;
            let zero = fx.ptr(0);
            fx.store(zero, sret, position);
            let target = fx.offset(sret, array_at);
            let layout = lo.layout(&array_ty);
            lo.copy(fx, target, source, layout.size, layout.align);
            supply.done(lo, fx);
        }
        member => panic!("no lowering of core::Array.{member}"),
    }
}

/// `prefix` and `split_at` of `len` elements of a type from `start`: the views of the parts,
/// none past the end.
fn parts(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, element: &Ty, start: Value, len: Value) {
    let at = supply.param(1).scalar();
    let fits = fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, at, len);
    let sret = supply.sret.expect("an option's address");
    let option = supply.abi.result.clone();
    if supply.member().starts_with("prefix") {
        lo.write_option_with(fx, &option, sret, fits, |_, fx, payload| {
            write_view(fx, payload, start, at);
        });
    } else {
        let parts = option.type_arg(0).cloned().expect("the parts type");
        let head = lo.layouts.field(lo.ck, &parts, None, "head").offset;
        let tail = lo.layouts.field(lo.ck, &parts, None, "tail").offset;
        let tail_start = element_at(lo, fx, element, start, at);
        let tail_len = fx.b.ins().isub(len, at);
        lo.write_option_with(fx, &option, sret, fits, |_, fx, payload| {
            let head_at = fx.offset(payload, head);
            write_view(fx, head_at, start, at);
            let tail_at = fx.offset(payload, tail);
            write_view(fx, tail_at, tail_start, tail_len);
        });
    }
    supply.done(lo, fx);
}

fn slice(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let slice_ty = supply.owner.clone().expect("a slice type");
    let element = element(lo, &slice_ty);
    let view = supply.param(0).address();
    let start = fx.load(PTR, view, 0);
    let len = fx.load(PTR, view, 8);
    let member = supply.member().to_owned();
    match member.as_str() {
        "len" | "byte_len" => supply.ret(lo, fx, Held::Scalar(len)),
        "at" | "at_mut" => {
            let index = supply.param(1).scalar();
            let at = element_at(lo, fx, &element, start, index);
            supply.ret(lo, fx, Held::Address(at));
        }
        "as_bytes" | "view" | "cursor" | "cursor_mut" => {
            let sret = supply.sret.expect("a view's address");
            write_view(fx, sret, start, len);
            supply.done(lo, fx);
        }
        "prefix" | "prefix_mut" | "split_at" | "split_at_mut" => {
            parts(lo, fx, supply, &element, start, len);
        }
        "slice_bytes" => {
            let (from, to) = (supply.param(1).scalar(), supply.param(2).scalar());
            let ordered = fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, from, to);
            let inside = fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, to, len);
            let valid = fx.b.ins().band(ordered, inside);
            let at_from = boundary(fx, start, len, from);
            let at_to = boundary(fx, start, len, to);
            let valid = fx.b.ins().band(valid, at_from);
            let valid = fx.b.ins().band(valid, at_to);
            let sret = supply.sret.expect("an option's address");
            let option = supply.abi.result.clone();
            let begin = fx.b.ins().iadd(start, from);
            let count = fx.b.ins().isub(to, from);
            lo.write_option_with(fx, &option, sret, valid, |_, fx, payload| {
                write_view(fx, payload, begin, count);
            });
            supply.done(lo, fx);
        }
        "fill" => {
            let value = supply.param(1);
            let zero = fx.ptr(0);
            let size = lo.layout(&element).size;
            fx.each(zero, len, |fx, index| {
                let offset = fx.b.ins().imul_imm_s(index, size as i64);
                let at = fx.b.ins().iadd(start, offset);
                lo.store_held(fx, at, value, &element);
            });
            supply.done(lo, fx);
        }
        "copy_from" => {
            let source = supply.param(1).address();
            let from = fx.load(PTR, source, 0);
            let count = fx.load(PTR, source, 8);
            let size = lo.layout(&element).size;
            let bytes = fx.b.ins().imul_imm_s(count, size as i64);
            lo.call_routine(
                fx,
                super::super::runtime::Routine::Copy,
                &[start, from, bytes],
            );
            supply.done(lo, fx);
        }
        member if member.starts_with("write_u") => {
            let (_, big) = integer_access(&member["write_".len()..]);
            let at = supply.param(1).scalar();
            let value = supply.param(2).scalar();
            let address = fx.b.ins().iadd(start, at);
            fx.store_ordered(value, address, 0, true, big);
            supply.done(lo, fx);
        }
        member if member.starts_with('u') => {
            let (width, big) = integer_access(member);
            let at = supply.param(1).scalar();
            let address = fx.b.ins().iadd(start, at);
            let ty = match width {
                2 => types::I16,
                4 => types::I32,
                _ => types::I64,
            };
            let value = fx.load_ordered(ty, address, 0, true, big);
            supply.ret(lo, fx, Held::Scalar(value));
        }
        other => panic!("no lowering of {}.{other}", supply.path),
    }
}

/// Whether byte `at` of a text of `len` bytes starts a char or ends the text.
fn boundary(fx: &mut Fx, start: Value, len: Value, at: Value) -> Value {
    let at_end = fx.b.ins().icmp(IntCC::Equal, at, len);
    let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, at, len);
    let result = fx.choose(
        inside,
        &[types::I8],
        |fx| {
            let address = fx.b.ins().iadd(start, at);
            let byte = fx.load(types::I8, address, 0);
            let high = fx.b.ins().band_imm_s(byte, 0xC0);
            vec![fx.b.ins().icmp_imm_s(IntCC::NotEqual, high, 0x80)]
        },
        |fx| vec![fx.int(types::I8, 0)],
    )[0];
    fx.b.ins().bor(at_end, result)
}

/// The width in bytes and byte order, big-endian when `true`, of an integer reader such as
/// `u32_le`.
fn integer_access(name: &str) -> (u64, bool) {
    let (width, order) = name.split_once('_').expect("an integer access");
    let width = match width {
        "u16" => 2,
        "u32" => 4,
        _ => 8,
    };
    (width, order == "be")
}

/// Writes `Some` of the element at `element` (its address held when the option is a wrapped
/// part and the element a part), or `None`.
fn write_option_element(
    lo: &mut Lowering,
    fx: &mut Fx,
    option: &Ty,
    address: Value,
    present: Value,
    element: Value,
    wrapped: Option<Wrap>,
) {
    let element_ty = option
        .type_arg(0)
        .cloned()
        .expect("an option's payload type");
    let held_as_address = wrapped.is_some_and(|wrap| lo.is_part(&element_ty, wrap));
    let layout_ty = match wrapped {
        Some(wrap) => lo.wrapped_type(option, wrap),
        None => option.clone(),
    };
    lo.write_option_with(fx, &layout_ty, address, present, |lo, fx, payload| {
        if held_as_address {
            fx.store(element, payload, 0);
        } else {
            let held = lo.held_at(fx, &element_ty, element);
            lo.store_held(fx, payload, held, &element_ty);
        }
    });
}

fn slice_next(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let cursor_ty = supply.owner.clone().expect("a cursor type");
    let element_ty = element(lo, &cursor_ty);
    let cursor = supply.param(0).address();
    let start = fx.load(PTR, cursor, 0);
    let len = fx.load(PTR, cursor, 8);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, len, 0);
    let size = lo.layout(&element_ty).size;
    fx.when(present, |fx| {
        let next = fx.b.ins().iadd_imm_s(start, size as i64);
        let rest = fx.b.ins().iadd_imm_s(len, -1);
        write_view(fx, cursor, next, rest);
    });
    let sret = supply.sret.expect("an option's address");
    let option = lo.std_ty("core", "Option", vec![element_ty.clone()]);
    write_option_element(lo, fx, &option, sret, present, start, supply.wrap);
    supply.done(lo, fx);
}

fn array_next(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let cursor_ty = supply.owner.clone().expect("a cursor type");
    let element_ty = element(lo, &cursor_ty);
    let count = const_arg(&cursor_ty.args()[1]) as i64;
    let cursor = supply.param(0).address();
    let position_at = lo.layouts.field(lo.ck, &cursor_ty, None, "position").offset;
    let array_at = lo.layouts.field(lo.ck, &cursor_ty, None, "array").offset;
    let position = fx.load(PTR, cursor, position_at);
    let present =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedLessThan, position, count);
    let array = fx.offset(cursor, array_at);
    let element = element_at(lo, fx, &element_ty, array, position);
    let next = fx.b.ins().iadd_imm_s(position, 1);
    let kept = fx.b.ins().select(present, next, position);
    fx.store(kept, cursor, position_at);
    let sret = supply.sret.expect("an option's address");
    let option = supply.abi.result.clone();
    write_option_element(lo, fx, &option, sret, present, element, None);
    supply.done(lo, fx);
}

/// `CharCursor.next`: decodes the char its bytes start with, which are valid UTF-8.
fn char_next(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let cursor = supply.param(0).address();
    let start = fx.load(PTR, cursor, 0);
    let len = fx.load(PTR, cursor, 8);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, len, 0);
    let decoded = fx.choose(
        present,
        &[types::I32, PTR],
        |fx| {
            let lead = fx.load(types::I8, start, 0);
            let lead = fx.b.ins().uextend(types::I32, lead);
            let count = fx.b.ins().iconst(PTR, 1);
            let two =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, lead, 0xC0);
            let three =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, lead, 0xE0);
            let four =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, lead, 0xF0);
            let two_count = fx.b.ins().iconst(PTR, 2);
            let three_count = fx.b.ins().iconst(PTR, 3);
            let four_count = fx.b.ins().iconst(PTR, 4);
            let count = fx.b.ins().select(two, two_count, count);
            let count = fx.b.ins().select(three, three_count, count);
            let count = fx.b.ins().select(four, four_count, count);
            let mask = fx.b.ins().iconst(types::I32, 0x7F);
            let mask2 = fx.b.ins().iconst(types::I32, 0x1F);
            let mask3 = fx.b.ins().iconst(types::I32, 0x0F);
            let mask4 = fx.b.ins().iconst(types::I32, 0x07);
            let mask = fx.b.ins().select(two, mask2, mask);
            let mask = fx.b.ins().select(three, mask3, mask);
            let mask = fx.b.ins().select(four, mask4, mask);
            let first = fx.b.ins().band(lead, mask);
            let one = fx.ptr(1);
            let value = fx.repeat(&[first, one], |fx, state| {
                let (value, at) = (state[0], state[1]);
                let more = fx.b.ins().icmp(IntCC::UnsignedLessThan, at, count);
                let next = fx.choose(
                    more,
                    &[types::I32, PTR],
                    |fx| {
                        let address = fx.b.ins().iadd(start, at);
                        let byte = fx.load(types::I8, address, 0);
                        let byte = fx.b.ins().uextend(types::I32, byte);
                        let bits = fx.b.ins().band_imm_s(byte, 0x3F);
                        let shifted = fx.b.ins().ishl_imm_s(value, 6);
                        let value = fx.b.ins().bor(shifted, bits);
                        let at = fx.b.ins().iadd_imm_s(at, 1);
                        vec![value, at]
                    },
                    |_| vec![value, at],
                );
                (more, next)
            })[0];
            let rest = fx.b.ins().isub(len, count);
            let next = fx.b.ins().iadd(start, count);
            write_view(fx, cursor, next, rest);
            vec![value, count]
        },
        |fx| vec![fx.int(types::I32, 0), fx.ptr(0)],
    );
    let sret = supply.sret.expect("an option's address");
    let option = supply.abi.result.clone();
    lo.write_option(fx, &option, sret, present, Held::Scalar(decoded[0]));
    supply.done(lo, fx);
}

/// `Range.next`: the low bound, then the range past it.
fn range_next(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let range_ty = supply.owner.clone().expect("a range type");
    let element_ty = element(lo, &range_ty);
    let scalar = element_ty.scalar().expect("a scalar range");
    let range = supply.param(0).address();
    let low_at = lo.layouts.field(lo.ck, &range_ty, None, "low").offset;
    let high_at = lo.layouts.field(lo.ck, &range_ty, None, "high").offset;
    let inclusive_at = lo.layouts.field(lo.ck, &range_ty, None, "inclusive").offset;
    let ty = super::super::layout::scalar_type(scalar);
    let low = fx.load(ty, range, low_at);
    let high = fx.load(ty, range, high_at);
    let inclusive = fx.load(types::I8, range, inclusive_at);
    let (less, less_or_equal) = match scalar.is_signed() {
        true => (IntCC::SignedLessThan, IntCC::SignedLessThanOrEqual),
        false => (IntCC::UnsignedLessThan, IntCC::UnsignedLessThanOrEqual),
    };
    let below = fx.b.ins().icmp(less, low, high);
    let at_most = fx.b.ins().icmp(less_or_equal, low, high);
    let inclusive = fx.b.ins().icmp_imm_s(IntCC::NotEqual, inclusive, 0);
    let reaches = fx.b.ins().band(inclusive, at_most);
    let present = fx.b.ins().bor(below, reaches);
    fx.when(present, |fx| {
        let next = fx.b.ins().iadd_imm_s(low, 1);
        fx.choose(
            below,
            &[],
            |fx| {
                fx.store(next, range, low_at);
                Vec::new()
            },
            |fx| {
                let closed = fx.int(types::I8, 0);
                fx.store(closed, range, inclusive_at);
                Vec::new()
            },
        );
    });
    let sret = supply.sret.expect("an option's address");
    let option = supply.abi.result.clone();
    lo.write_option(fx, &option, sret, present, Held::Scalar(low));
    supply.done(lo, fx);
}

fn option(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let option = supply.owner.clone().expect("an option type");
    let payload = option.type_arg(0).cloned().expect("a payload type");
    let address = supply.param(0).address();
    let present = lo.variant_is(fx, address, &option, "Some");
    let payload_at = match &lo.layout(&option).shape {
        Shape::Enum(shape) => shape.variants[0]
            .fields
            .first()
            .map_or(0, |field| field.offset),
        _ => 0,
    };
    match supply.member() {
        "value_or" => {
            let value = fx.offset(address, payload_at);
            let fallback = supply.param(1);
            match lo.owned_repr(&payload) {
                super::super::abi::Repr::Scalar(ty) => {
                    let held = fx.load(ty, value, 0);
                    let other = match fallback {
                        Held::Scalar(value) => value,
                        Held::Address(at) => fx.load(ty, at, 0),
                        Held::Empty => held,
                    };
                    let chosen = fx.b.ins().select(present, held, other);
                    supply.ret(lo, fx, Held::Scalar(chosen));
                }
                super::super::abi::Repr::Address => {
                    let other = fallback.address();
                    let chosen = fx.b.ins().select(present, value, other);
                    supply.ret(lo, fx, Held::Address(chosen));
                }
                super::super::abi::Repr::Empty => supply.done(lo, fx),
            }
        }
        "extract" => {
            let layout = lo.layout(&option);
            let sret = supply.sret.expect("an option's address");
            lo.copy(fx, sret, address, layout.size, layout.align);
            let none = lo.none_value(fx, &option);
            lo.store_held(fx, address, none, &option);
            supply.done(lo, fx);
        }
        member => panic!("no lowering of core::Option.{member}"),
    }
}

fn untrusted(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let inner = supply
        .owner
        .as_ref()
        .and_then(|owner| owner.type_arg(0).cloned())
        .expect("an Untrusted's content type");
    match supply.member() {
        "of" | "inspect" => {
            supply.ret(lo, fx, supply.param(0));
        }
        "into_cleared" => {
            let address = supply.param(0).address();
            let (start, len) = content_bytes(lo, fx, &inner, address);
            let zero = fx.ptr(0);
            lo.call_routine(
                fx,
                super::super::runtime::Routine::Fill,
                &[start, zero, len],
            );
            supply.ret(lo, fx, supply.param(0));
        }
        "byte_len" => {
            let address = supply.param(0).address();
            let (_, len) = content_bytes(lo, fx, &inner, address);
            supply.ret(lo, fx, Held::Scalar(len));
        }
        "view" | "view_mut" => {
            let address = supply.param(0).address();
            let (start, len) = content_bytes(lo, fx, &inner, address);
            let sret = supply.sret.expect("a view's address");
            write_view(fx, sret, start, len);
            supply.done(lo, fx);
        }
        "prefix" | "split_at" => {
            let address = supply.param(0).address();
            let (start, len) = content_bytes(lo, fx, &inner, address);
            parts(lo, fx, supply, &Ty::Scalar(Scalar::U8), start, len);
        }
        "append" | "try_append" => super::mem::lists::append_untrusted(lo, fx, supply),
        "copy_prefix" | "try_copy_prefix" => {
            let address = supply.param(0).address();
            let (start, _) = content_bytes(lo, fx, &inner, address);
            super::mem::lists::copy_prefix(lo, fx, supply, start);
        }
        member => panic!("no lowering of core::Untrusted.{member}"),
    }
}

/// The address and length in bytes of the contents of a value holding bytes: a slice, text,
/// array of bytes, list or byte string.
pub fn content_bytes(lo: &mut Lowering, fx: &mut Fx, ty: &Ty, address: Value) -> (Value, Value) {
    if lo.ck.is_std(ty, "core::Array") {
        let element = ty.type_arg(0).cloned().expect("an element type");
        let count = const_arg(&ty.args()[1]) as u64;
        let size = lo.layout(&element).size;
        return (address, fx.ptr(count * size));
    }
    let start = fx.load(PTR, address, 0);
    let len = fx.load(PTR, address, 8);
    (start, len)
}
