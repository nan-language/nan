//! Rendering: renderers and their budgets, the renderings of scalars, std types and derived
//! `Render`, `text::fmt` templates, and `Dyn` views.
//!
//! A renderer is the address of its state: the buffer kept bytes go to (zero while measuring),
//! the render budget, the bytes the full rendering holds so far, the bytes kept, the position
//! in the full rendering where keeping starts, and the most bytes kept, at most the budget.
//! Finishing a rendering elides what passed the budget.
//!
//! A `Dyn[I]` value is the address of the value and of its method table: the release of the
//! value's type, its size and alignment, then each of `I`'s methods.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};
use cranelift_module::{DataDescription, Linkage, Module};

use super::super::abi::{Abi, Repr};
use super::super::body::{Body, Held};
use super::super::fx::{Fx, PTR};
use super::super::runtime::Routine;
use super::super::{Instance, Lowering};
use super::Supply;
use crate::check::sig::{Payload, SigKind};
use crate::check::ty::{Args, Scalar, Subst, Ty};
use crate::nir::{Literal, Operand};

/// The bytes of the buffer the host platform writes a float's text into.
const FLOAT_TEXT: u64 = 40;

mod renderer {
    pub const BUFFER: u64 = 0;
    pub const BUDGET: u64 = 8;
    pub const WRITTEN: u64 = 16;
    pub const KEPT: u64 = 24;
    pub const SKIP: u64 = 32;
    pub const LIMIT: u64 = 40;
    pub const SIZE: u64 = 48;
}

/// A renderer value over a new state, keeping bytes in `buffer` (zero to measure).
pub fn new_renderer(fx: &mut Fx, buffer: Value, budget: Value) -> Value {
    let zero = fx.ptr(0);
    window_renderer(fx, buffer, budget, (zero, budget))
}

/// A renderer value over a new state with a budget, keeping in `buffer` at most `limit` bytes
/// of the full rendering from position `skip` on.
pub fn window_renderer(
    fx: &mut Fx,
    buffer: Value,
    budget: Value,
    (skip, limit): (Value, Value),
) -> Value {
    let state = fx.slot(renderer::SIZE, 8);
    fx.store(buffer, state, renderer::BUFFER);
    fx.store(budget, state, renderer::BUDGET);
    let zero = fx.ptr(0);
    fx.store(zero, state, renderer::WRITTEN);
    fx.store(zero, state, renderer::KEPT);
    fx.store(skip, state, renderer::SKIP);
    fx.store(limit, state, renderer::LIMIT);
    let value = fx.slot(8, 8);
    fx.store(state, value, 0);
    value
}

/// The bytes the full rendering of a renderer holds so far.
pub fn written(fx: &mut Fx, renderer_value: Value) -> Value {
    let state = fx.load(PTR, renderer_value, 0);
    fx.load(PTR, state, renderer::WRITTEN)
}

/// Ends a rendering: when it passed its budget, keeps the longest prefix ending at a char
/// boundary that leaves room for `...` cut to the budget, then that marker. Gives the length.
pub fn finish(lo: &mut Lowering, fx: &mut Fx, renderer_value: Value) -> Value {
    let state = fx.load(PTR, renderer_value, 0);
    let buffer = fx.load(PTR, state, renderer::BUFFER);
    let budget = fx.load(PTR, state, renderer::BUDGET);
    let written = fx.load(PTR, state, renderer::WRITTEN);
    let kept = fx.load(PTR, state, renderer::KEPT);
    let elided = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, written, budget);
    fx.choose(
        elided,
        &[PTR],
        |fx| {
            let three = fx.ptr(3);
            let small = fx.b.ins().icmp(IntCC::UnsignedLessThan, budget, three);
            let marker = fx.b.ins().select(small, budget, three);
            let room = fx.b.ins().isub(budget, marker);
            let fits = fx.b.ins().icmp(IntCC::UnsignedLessThan, kept, room);
            let end = fx.b.ins().select(fits, kept, room);
            let end = fx.repeat(&[end], |fx, state| {
                let end = state[0];
                let positive = fx.b.ins().icmp_imm_s(IntCC::NotEqual, end, 0);
                let continuation = fx.choose(
                    positive,
                    &[types::I8],
                    |fx| {
                        let at = fx.b.ins().iadd(buffer, end);
                        let byte = fx.load(types::I8, at, 0);
                        let high = fx.b.ins().band_imm_s(byte, 0xC0);
                        vec![fx.b.ins().icmp_imm_s(IntCC::Equal, high, 0x80)]
                    },
                    |fx| vec![fx.int(types::I8, 0)],
                )[0];
                let back = fx.b.ins().iadd_imm_s(end, -1);
                let next = fx.b.ins().select(continuation, back, end);
                (continuation, vec![next])
            })[0];
            let dots = lo.view_literal(fx, b"...").address();
            let dots_start = fx.load(PTR, dots, 0);
            let target = fx.b.ins().iadd(buffer, end);
            lo.call_routine(fx, Routine::Copy, &[target, dots_start, marker]);
            vec![fx.b.ins().iadd(end, marker)]
        },
        |_| vec![kept],
    )[0]
}

/// Renders a value of a type within a budget into a buffer on the stack: its address and the
/// rendering's length.
pub fn render_value(
    lo: &mut Lowering,
    fx: &mut Fx,
    ty: &Ty,
    value: Held,
    budget: u64,
) -> (Value, Value) {
    let buffer = fx.slot(budget.max(1), 8);
    let limit = fx.ptr(budget);
    let renderer_value = new_renderer(fx, buffer, limit);
    lo.call_method(
        fx,
        ty,
        "Render",
        Vec::new(),
        &[value, Held::Address(renderer_value)],
    );
    let len = finish(lo, fx, renderer_value);
    (buffer, len)
}

/// Appends bytes to a rendering: the bytes within its limit from its skip position on are
/// kept, all are counted.
pub fn append(lo: &mut Lowering, fx: &mut Fx, renderer_value: Value, text: Value, len: Value) {
    let state = fx.load(PTR, renderer_value, 0);
    let buffer = fx.load(PTR, state, renderer::BUFFER);
    let limit = fx.load(PTR, state, renderer::LIMIT);
    let written = fx.load(PTR, state, renderer::WRITTEN);
    let kept = fx.load(PTR, state, renderer::KEPT);
    let skip = fx.load(PTR, state, renderer::SKIP);
    let total = fx.b.ins().iadd(written, len);
    fx.store(total, state, renderer::WRITTEN);
    let ahead = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, skip, written);
    let gap = fx.b.ins().isub(skip, written);
    let zero = fx.ptr(0);
    let dropped = fx.b.ins().select(ahead, gap, zero);
    let dropped = fx.b.ins().umin(dropped, len);
    let text = fx.b.ins().iadd(text, dropped);
    let len = fx.b.ins().isub(len, dropped);
    let room = fx.b.ins().isub(limit, kept);
    let fits = fx.b.ins().icmp(IntCC::UnsignedLessThan, len, room);
    let count = fx.b.ins().select(fits, len, room);
    let has_room = fx.b.ins().icmp(IntCC::UnsignedLessThan, kept, limit);
    let keeping = fx.b.ins().icmp_imm_s(IntCC::NotEqual, buffer, 0);
    let copying = fx.b.ins().band(has_room, keeping);
    fx.when(copying, |fx| {
        let target = fx.b.ins().iadd(buffer, kept);
        lo.call_routine(fx, Routine::Copy, &[target, text, count]);
    });
    let count = fx.b.ins().select(has_room, count, zero);
    let kept = fx.b.ins().iadd(kept, count);
    fx.store(kept, state, renderer::KEPT);
}

/// Appends a literal text to a rendering.
fn append_text(lo: &mut Lowering, fx: &mut Fx, renderer_value: Value, text: &str) {
    if text.is_empty() {
        return;
    }
    let view = lo.view_literal(fx, text.as_bytes()).address();
    let start = fx.load(PTR, view, 0);
    let len = fx.ptr(text.len() as u64);
    append(lo, fx, renderer_value, start, len);
}

/// Appends the decimal digits of an unsigned 64-bit value.
pub fn append_decimal(lo: &mut Lowering, fx: &mut Fx, renderer_value: Value, value: Value) {
    let digits = fx.slot(24, 8);
    let end = fx.ptr(24);
    let state = fx.repeat(&[value, end], |fx, state| {
        let (rest, at) = (state[0], state[1]);
        let ten = fx.ptr(10);
        let digit = fx.b.ins().urem(rest, ten);
        let rest = fx.b.ins().udiv(rest, ten);
        let at = fx.b.ins().iadd_imm_s(at, -1);
        let ascii = fx.b.ins().iadd_imm_s(digit, i64::from(b'0'));
        let ascii = fx.b.ins().ireduce(types::I8, ascii);
        let place = fx.b.ins().iadd(digits, at);
        fx.store(ascii, place, 0);
        let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, rest, 0);
        (more, vec![rest, at])
    });
    let start = fx.b.ins().iadd(digits, state[1]);
    let len = fx.b.ins().isub(end, state[1]);
    append(lo, fx, renderer_value, start, len);
}

/// Appends the UTF-8 encoding of a char.
fn append_char(lo: &mut Lowering, fx: &mut Fx, renderer_value: Value, c: Value) {
    let buffer = fx.slot(4, 4);
    let c = super::super::body::widen(fx, c);
    let len = encode_utf8(fx, c, buffer);
    append(lo, fx, renderer_value, buffer, len);
}

/// Writes the UTF-8 encoding of a scalar value at an address: its length.
pub fn encode_utf8(fx: &mut Fx, c: Value, buffer: Value) -> Value {
    let byte = |fx: &mut Fx, value: Value, at: u64| {
        let narrow = fx.b.ins().ireduce(types::I8, value);
        fx.store(narrow, buffer, at);
    };
    let one = fx.b.ins().icmp_imm_s(IntCC::UnsignedLessThan, c, 0x80);
    let two = fx.b.ins().icmp_imm_s(IntCC::UnsignedLessThan, c, 0x800);
    let three = fx.b.ins().icmp_imm_s(IntCC::UnsignedLessThan, c, 0x1_0000);
    fx.choose(
        one,
        &[PTR],
        |fx| {
            byte(fx, c, 0);
            vec![fx.ptr(1)]
        },
        |fx| {
            fx.choose(
                two,
                &[PTR],
                |fx| {
                    let lead = fx.b.ins().ushr_imm_s(c, 6);
                    let lead = fx.b.ins().bor_imm_s(lead, 0xC0);
                    byte(fx, lead, 0);
                    let tail = fx.b.ins().band_imm_s(c, 0x3F);
                    let tail = fx.b.ins().bor_imm_s(tail, 0x80);
                    byte(fx, tail, 1);
                    vec![fx.ptr(2)]
                },
                |fx| {
                    fx.choose(
                        three,
                        &[PTR],
                        |fx| {
                            for (at, shift, mark) in [(0, 12, 0xE0), (1, 6, 0x80), (2, 0, 0x80)] {
                                let part = fx.b.ins().ushr_imm_s(c, shift);
                                let part = if at == 0 {
                                    part
                                } else {
                                    fx.b.ins().band_imm_s(part, 0x3F)
                                };
                                let part = fx.b.ins().bor_imm_s(part, mark);
                                byte(fx, part, at);
                            }
                            vec![fx.ptr(3)]
                        },
                        |fx| {
                            for (at, shift, mark) in
                                [(0, 18, 0xF0), (1, 12, 0x80), (2, 6, 0x80), (3, 0, 0x80)]
                            {
                                let part = fx.b.ins().ushr_imm_s(c, shift);
                                let part = if at == 0 {
                                    part
                                } else {
                                    fx.b.ins().band_imm_s(part, 0x3F)
                                };
                                let part = fx.b.ins().bor_imm_s(part, mark);
                                byte(fx, part, at);
                            }
                            vec![fx.ptr(4)]
                        },
                    )
                },
            )
        },
    )[0]
}

/// The supplied members of `core::Renderer`.
pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    let Some(member) = supply.path.strip_prefix("core::Renderer.") else {
        return false;
    };
    let renderer_value = supply.param(0).address();
    match member {
        "write" => {
            let text = supply.param(1).address();
            let start = fx.load(PTR, text, 0);
            let len = fx.load(PTR, text, 8);
            append(lo, fx, renderer_value, start, len);
            supply.done(lo, fx);
        }
        "write_char" => {
            let c = supply.param(1).scalar();
            append_char(lo, fx, renderer_value, c);
            supply.done(lo, fx);
        }
        "remaining" | "is_elided" => {
            let state = fx.load(PTR, renderer_value, 0);
            let budget = fx.load(PTR, state, renderer::BUDGET);
            let written = fx.load(PTR, state, renderer::WRITTEN);
            let over = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, written, budget);
            let held = match member {
                "is_elided" => Held::Scalar(over),
                _ => {
                    let left = fx.b.ins().isub(budget, written);
                    let zero = fx.ptr(0);
                    Held::Scalar(fx.b.ins().select(over, zero, left))
                }
            };
            supply.ret(lo, fx, held);
        }
        _ => return false,
    }
    true
}

/// Defines `render` for a type: its rendering written to the renderer it receives.
pub fn define_render(lo: &mut Lowering, fx: &mut Fx, abi: &Abi, ty: &Ty) {
    let incoming = fx.entry();
    let value = match abi.params[0].1 {
        Repr::Scalar(_) => Held::Scalar(incoming[0]),
        Repr::Address => Held::Address(incoming[0]),
        Repr::Empty => Held::Empty,
    };
    let renderer_value = *incoming.last().expect("a renderer");
    render_to(lo, fx, ty, value, renderer_value);
    fx.b.ins().return_(&[]);
}

/// Writes the rendering of a value of a type to a renderer.
pub fn render_to(lo: &mut Lowering, fx: &mut Fx, ty: &Ty, value: Held, renderer_value: Value) {
    match ty {
        Ty::Unit => append_text(lo, fx, renderer_value, "Unit"),
        Ty::Scalar(scalar) => render_scalar(lo, fx, *scalar, value.scalar(), renderer_value),
        Ty::Item(item, args) => {
            let path = lo.ck.db.canonical(*item);
            match path.as_str() {
                "core::Str" | "text::String" => {
                    let address = value.address();
                    let start = fx.load(PTR, address, 0);
                    let len = fx.load(PTR, address, 8);
                    append(lo, fx, renderer_value, start, len);
                }
                "text::Fixed" => {
                    let address = value.address();
                    let len_at = lo.layouts.field(lo.ck, ty, None, "len").offset;
                    let bytes_at = lo.layouts.field(lo.ck, ty, None, "bytes").offset;
                    let len = fx.load(PTR, address, len_at);
                    let start = fx.offset(address, bytes_at);
                    append(lo, fx, renderer_value, start, len);
                }
                "core::Slice" | "core::SliceMut" | "mem::List" | "mem::Set" => {
                    let element = args[0].ty().cloned().expect("an element type");
                    let address = value.address();
                    let start = fx.load(PTR, address, 0);
                    let len = fx.load(PTR, address, 8);
                    render_elements(lo, fx, &element, start, len, renderer_value);
                }
                "core::Array" => {
                    let element = args[0].ty().cloned().expect("an element type");
                    let count = super::super::data::const_arg(&args[1]) as u64;
                    let len = fx.ptr(count);
                    render_elements(lo, fx, &element, value.address(), len, renderer_value);
                }
                "mem::Bounded" => {
                    let element = args[0].ty().cloned().expect("an element type");
                    let address = value.address();
                    let len_at = lo.layouts.field(lo.ck, ty, None, "len").offset;
                    let items_at = lo.layouts.field(lo.ck, ty, None, "items").offset;
                    let len = fx.load(PTR, address, len_at);
                    let start = fx.offset(address, items_at);
                    render_elements(lo, fx, &element, start, len, renderer_value);
                }
                "mem::Bytes" => render_bytes(lo, fx, value.address(), renderer_value),
                "mem::Map" => {
                    let key = args[0].ty().cloned().expect("a key type");
                    let entry = lo.std_ty(
                        "mem",
                        "KeyValue",
                        vec![key, args[1].ty().cloned().expect("a value type")],
                    );
                    let address = value.address();
                    let start = fx.load(PTR, address, 0);
                    let len = fx.load(PTR, address, 8);
                    render_entries(lo, fx, &entry, start, len, renderer_value);
                }
                "mem::Box" | "mem::Shared" => {
                    let inner = args[0].ty().cloned().expect("a content type");
                    let address = value.address();
                    let contents = fx.load(PTR, address, 0);
                    let contents = super::mem::stores::contents_address(lo, fx, ty, contents);
                    let held = lo.held_at(fx, &inner, contents);
                    render_via(lo, fx, &inner, held, renderer_value);
                }
                _ => render_composite(lo, fx, ty, *item, args, value, renderer_value),
            }
        }
        Ty::Dyn(_) => {
            let address = value.address();
            render_dyn(fx, address, renderer_value);
        }
        other => panic!("a rendering of {other:?}"),
    }
}

/// Renders a value through its type's `Render` conformance.
fn render_via(lo: &mut Lowering, fx: &mut Fx, ty: &Ty, value: Held, renderer_value: Value) {
    lo.call_method(
        fx,
        ty,
        "Render",
        Vec::new(),
        &[value, Held::Address(renderer_value)],
    );
}

fn render_scalar(
    lo: &mut Lowering,
    fx: &mut Fx,
    scalar: Scalar,
    value: Value,
    renderer_value: Value,
) {
    match scalar {
        Scalar::Bool => {
            let truth = fx.b.ins().icmp_imm_s(IntCC::NotEqual, value, 0);
            fx.choose_in(
                lo,
                truth,
                &[],
                |lo, fx| {
                    append_text(lo, fx, renderer_value, "true");
                    Vec::new()
                },
                |lo, fx| {
                    append_text(lo, fx, renderer_value, "false");
                    Vec::new()
                },
            );
        }
        Scalar::Char => append_char(lo, fx, renderer_value, value),
        Scalar::F32 | Scalar::F64 => {
            let buffer = fx.slot(FLOAT_TEXT, 8);
            let name = match scalar {
                Scalar::F32 => "f32_text",
                _ => "f64_text",
            };
            let args = [Held::Scalar(value), Held::Address(buffer)];
            let len = lo.platform_call(fx, name, &args).result.scalar();
            append(lo, fx, renderer_value, buffer, len);
        }
        scalar => {
            let wide = match (scalar.is_signed(), scalar.bits() < 64) {
                (true, true) => fx.b.ins().sextend(types::I64, value),
                (false, true) => fx.b.ins().uextend(types::I64, value),
                _ => value,
            };
            if scalar.is_signed() {
                let negative = fx.b.ins().icmp_imm_s(IntCC::SignedLessThan, wide, 0);
                fx.when(negative, |fx| append_text(lo, fx, renderer_value, "-"));
                let flipped = fx.b.ins().ineg(wide);
                let magnitude = fx.b.ins().select(negative, flipped, wide);
                append_decimal(lo, fx, renderer_value, magnitude);
            } else {
                append_decimal(lo, fx, renderer_value, wide);
            }
        }
    }
}

/// `[a, b]`: elements of a type from an address, separated by `, `.
fn render_elements(
    lo: &mut Lowering,
    fx: &mut Fx,
    element: &Ty,
    start: Value,
    len: Value,
    renderer_value: Value,
) {
    append_text(lo, fx, renderer_value, "[");
    let size = lo.layout(element).size;
    let zero = fx.ptr(0);
    fx.each(zero, len, |fx, index| {
        let not_first = fx.b.ins().icmp_imm_s(IntCC::NotEqual, index, 0);
        fx.when(not_first, |fx| append_text(lo, fx, renderer_value, ", "));
        let offset = fx.b.ins().imul_imm_s(index, size as i64);
        let at = fx.b.ins().iadd(start, offset);
        let held = lo.held_at(fx, element, at);
        render_via(lo, fx, element, held, renderer_value);
    });
    append_text(lo, fx, renderer_value, "]");
}

/// `{k: v, k: v}`: key-value entries from an address.
fn render_entries(
    lo: &mut Lowering,
    fx: &mut Fx,
    entry: &Ty,
    start: Value,
    len: Value,
    renderer_value: Value,
) {
    append_text(lo, fx, renderer_value, "{");
    let size = lo.layout(entry).size;
    let key = lo.layouts.field(lo.ck, entry, None, "key");
    let value = lo.layouts.field(lo.ck, entry, None, "value");
    let zero = fx.ptr(0);
    fx.each(zero, len, |fx, index| {
        let not_first = fx.b.ins().icmp_imm_s(IntCC::NotEqual, index, 0);
        fx.when(not_first, |fx| append_text(lo, fx, renderer_value, ", "));
        let offset = fx.b.ins().imul_imm_s(index, size as i64);
        let at = fx.b.ins().iadd(start, offset);
        let key_at = fx.offset(at, key.offset);
        let held = lo.held_at(fx, &key.ty, key_at);
        render_via(lo, fx, &key.ty, held, renderer_value);
        append_text(lo, fx, renderer_value, ": ");
        let value_at = fx.offset(at, value.offset);
        let held = lo.held_at(fx, &value.ty, value_at);
        render_via(lo, fx, &value.ty, held, renderer_value);
    });
    append_text(lo, fx, renderer_value, "}");
}

/// `b"..."`: printable ASCII as itself but `"` and `\`, the named escapes, `\xHH` otherwise.
fn render_bytes(lo: &mut Lowering, fx: &mut Fx, address: Value, renderer_value: Value) {
    append_text(lo, fx, renderer_value, "b\"");
    let start = fx.load(PTR, address, 0);
    let len = fx.load(PTR, address, 8);
    let zero = fx.ptr(0);
    let scratch = fx.slot(8, 8);
    fx.each(zero, len, |fx, index| {
        let at = fx.b.ins().iadd(start, index);
        let byte = fx.load(types::I8, at, 0);
        let byte = fx.b.ins().uextend(types::I64, byte);
        let mut handled = fx.int(types::I8, 0);
        for (value, text) in [
            (b'"', "\\\""),
            (b'\\', "\\\\"),
            (b'\n', "\\n"),
            (b'\r', "\\r"),
            (b'\t', "\\t"),
            (0, "\\0"),
        ] {
            let is = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(value));
            fx.when(is, |fx| append_text(lo, fx, renderer_value, text));
            handled = fx.b.ins().bor(handled, is);
        }
        let printable_low =
            fx.b.ins()
                .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, byte, 0x20);
        let printable_high =
            fx.b.ins()
                .icmp_imm_s(IntCC::UnsignedLessThanOrEqual, byte, 0x7E);
        let printable = fx.b.ins().band(printable_low, printable_high);
        let plain = fx.b.ins().bxor_imm_s(handled, 1);
        let as_itself = fx.b.ins().band(printable, plain);
        fx.when(as_itself, |fx| {
            let narrow = fx.b.ins().ireduce(types::I8, byte);
            fx.store(narrow, scratch, 0);
            let one = fx.ptr(1);
            append(lo, fx, renderer_value, scratch, one);
        });
        let not_printable = fx.b.ins().bxor_imm_s(printable, 1);
        let hex = fx.b.ins().band(not_printable, plain);
        fx.when(hex, |fx| {
            append_text(lo, fx, renderer_value, "\\x");
            for (at, shift) in [(0u64, 4i64), (1, 0)] {
                let nibble = fx.b.ins().ushr_imm_s(byte, shift);
                let nibble = fx.b.ins().band_imm_s(nibble, 0xF);
                let letter =
                    fx.b.ins()
                        .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, nibble, 10);
                let digit = fx.b.ins().iadd_imm_s(nibble, i64::from(b'0'));
                let upper = fx.b.ins().iadd_imm_s(nibble, i64::from(b'A') - 10);
                let ascii = fx.b.ins().select(letter, upper, digit);
                let narrow = fx.b.ins().ireduce(types::I8, ascii);
                fx.store(narrow, scratch, at);
            }
            let two = fx.ptr(2);
            append(lo, fx, renderer_value, scratch, two);
        });
    });
    append_text(lo, fx, renderer_value, "\"");
}

/// The derived rendering of a struct or enum: its name and fields, or `Name.Variant` and its
/// payload; the variants of `Option` and `Result` render without their type's name.
fn render_composite(
    lo: &mut Lowering,
    fx: &mut Fx,
    ty: &Ty,
    item: crate::check::db::ItemId,
    args: &Args,
    value: Held,
    renderer_value: Value,
) {
    let name = lo.ck.db.declaration(item).name().unwrap_or_default();
    let sig = lo.ck.sig(item);
    let subst = Subst::of(sig.generics.iter().map(|def| def.param), args);
    match &sig.kind {
        SigKind::Struct { fields } => {
            let address = value.address();
            append_text(lo, fx, renderer_value, &format!("{name}("));
            for (index, field) in fields.iter().enumerate() {
                if index > 0 {
                    append_text(lo, fx, renderer_value, ", ");
                }
                append_text(lo, fx, renderer_value, &format!("{}: ", field.name));
                let field_ty = subst.ty(&field.ty);
                let at = lo.layouts.field(lo.ck, ty, None, &field.name).offset;
                let field_at = fx.offset(address, at);
                let held = lo.held_at(fx, &field_ty, field_at);
                render_via(lo, fx, &field_ty, held, renderer_value);
            }
            append_text(lo, fx, renderer_value, ")");
        }
        SigKind::Enum { variants } => {
            let address = value.address();
            let index = lo.variant_index(fx, ty, address);
            let core = lo.wraps(ty);
            for (position, variant) in variants.iter().enumerate() {
                let is = fx.b.ins().icmp_imm_s(IntCC::Equal, index, position as i64);
                let variant = variant.clone();
                let subst = subst.clone();
                fx.when(is, |fx| {
                    let prefix = if core {
                        variant.name.clone()
                    } else {
                        format!("{name}.{}", variant.name)
                    };
                    append_text(lo, fx, renderer_value, &prefix);
                    match &variant.payload {
                        Payload::None => {}
                        Payload::Single(payload) => {
                            append_text(lo, fx, renderer_value, "(");
                            let payload_ty = subst.ty(payload);
                            let at = lo.layouts.field(lo.ck, ty, Some(&variant.name), "").offset;
                            let field_at = fx.offset(address, at);
                            let held = lo.held_at(fx, &payload_ty, field_at);
                            render_via(lo, fx, &payload_ty, held, renderer_value);
                            append_text(lo, fx, renderer_value, ")");
                        }
                        Payload::Labelled(fields) => {
                            append_text(lo, fx, renderer_value, "(");
                            for (index, (label, field_ty)) in fields.iter().enumerate() {
                                if index > 0 {
                                    append_text(lo, fx, renderer_value, ", ");
                                }
                                append_text(lo, fx, renderer_value, &format!("{label}: "));
                                let field_ty = subst.ty(field_ty);
                                let at = lo
                                    .layouts
                                    .field(lo.ck, ty, Some(&variant.name), label)
                                    .offset;
                                let field_at = fx.offset(address, at);
                                let held = lo.held_at(fx, &field_ty, field_at);
                                render_via(lo, fx, &field_ty, held, renderer_value);
                            }
                            append_text(lo, fx, renderer_value, ")");
                        }
                    }
                });
            }
        }
        _ => append_text(lo, fx, renderer_value, "_"),
    }
}

/// Renders a `Dyn[Render]` value through its method table.
pub fn render_dyn(fx: &mut Fx, address: Value, renderer_value: Value) {
    let data = fx.load(PTR, address, 0);
    let table = fx.load(PTR, address, 8);
    let code = fx.load(PTR, table, 24);
    let mut signature = cranelift_codegen::ir::Signature::new(super::super::CALL_CONV);
    signature
        .params
        .push(cranelift_codegen::ir::AbiParam::new(PTR));
    signature
        .params
        .push(cranelift_codegen::ir::AbiParam::new(PTR));
    let signature = fx.b.import_signature(signature);
    fx.b.ins()
        .call_indirect(signature, code, &[data, renderer_value]);
}

/// `text::fmt("template", label: value, ...)`: a `Dyn[Render]` of the template and the
/// addresses of its arguments.
pub fn template(body: &mut Body, args: &Args, operands: &[Operand]) -> Held {
    let Operand::Literal(Literal::Text(text)) = &operands[0] else {
        panic!("a template literal");
    };
    let mut names = Vec::new();
    let mut types = Vec::new();
    let record = body.fx.slot((8 * operands.len() as u64).max(8), 8);
    for (position, pair) in operands[1..].chunks(2).enumerate() {
        let Operand::Literal(Literal::Text(name)) = &pair[0] else {
            panic!("a template hole's name");
        };
        names.push(name.clone());
        let ty = args
            .get(2 + 2 * position)
            .and_then(crate::check::ty::Arg::ty)
            .cloned()
            .expect("an argument's type");
        let (held, nty) = body.operand(&pair[1]);
        let address = body.adapt(held, &nty, Repr::Address).address();
        body.fx.store(address, record, 8 * position as u64);
        types.push(ty);
    }
    let pieces = parse(text, &names);
    let glue = body.lo.template_glue(pieces, types);
    let table = body.lo.method_table(glue, None);
    let global = body.lo.data_ref(body.fx, table);
    let table_address = body.fx.b.ins().symbol_value(PTR, global);
    let value = body.fx.slot(16, 8);
    body.fx.store(record, value, 0);
    body.fx.store(table_address, value, 8);
    Held::Address(value)
}

/// A piece of a template: a literal text, or the index of the argument a hole names.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Piece {
    Text(String),
    Hole(usize),
}

fn parse(text: &str, names: &[String]) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut literal = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                literal.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                literal.push('}');
            }
            '{' => {
                if !literal.is_empty() {
                    pieces.push(Piece::Text(std::mem::take(&mut literal)));
                }
                let name: String = chars.by_ref().take_while(|c| *c != '}').collect();
                let index = names
                    .iter()
                    .position(|known| *known == name)
                    .expect("a hole's argument");
                pieces.push(Piece::Hole(index));
            }
            c => literal.push(c),
        }
    }
    if !literal.is_empty() {
        pieces.push(Piece::Text(literal));
    }
    pieces
}

impl Lowering<'_, '_> {
    /// The rendering glue of a template: receives the record of its arguments' addresses and a
    /// renderer.
    fn template_glue(&mut self, pieces: Vec<Piece>, types: Vec<Ty>) -> cranelift_module::FuncId {
        let mut signature = cranelift_codegen::ir::Signature::new(super::super::CALL_CONV);
        signature
            .params
            .push(cranelift_codegen::ir::AbiParam::new(PTR));
        signature
            .params
            .push(cranelift_codegen::ir::AbiParam::new(PTR));
        let glue = super::super::Glue::Template(pieces.clone(), types.clone());
        self.glue_function(glue, signature, |lo, fx| {
            let params = fx.entry();
            let (record, renderer_value) = (params[0], params[1]);
            for piece in &pieces {
                match piece {
                    Piece::Text(text) => append_text(lo, fx, renderer_value, text),
                    Piece::Hole(index) => {
                        let address = fx.load(PTR, record, 8 * *index as u64);
                        let held = lo.held_at(fx, &types[*index], address);
                        render_via(lo, fx, &types[*index], held, renderer_value);
                    }
                }
            }
            fx.b.ins().return_(&[]);
        })
    }

    /// The method table of a `Dyn` value: its release, its size and alignment, and its
    /// methods' code.
    pub fn method_table(
        &mut self,
        first: cranelift_module::FuncId,
        release: Option<cranelift_module::FuncId>,
    ) -> cranelift_module::DataId {
        self.glue += 1;
        let name = format!("_NXtable_{}", self.glue);
        let id = self
            .module
            .declare_data(&name, Linkage::Local, false, false)
            .expect("a method table");
        let mut description = DataDescription::new();
        description.define(vec![0u8; 32].into_boxed_slice());
        description.set_align(8);
        if let Some(release) = release {
            let reference = self.module.declare_func_in_data(release, &mut description);
            description.write_function_addr(0, reference);
        }
        let reference = self.module.declare_func_in_data(first, &mut description);
        description.write_function_addr(24, reference);
        self.module
            .define_data(id, &description)
            .expect("a method table");
        id
    }
}

/// `Dyn[I].view(x)`: the value's address with the method table of its type for `I`.
pub fn dyn_view(body: &mut Body, args: &Args, operand: &Operand) -> Held {
    let (Some(Ty::Dyn(bound)), Some(source)) = (args[0].ty().cloned(), args[1].ty().cloned())
    else {
        panic!("a Dyn view's types");
    };
    let (held, nty) = body.operand(operand);
    let address = body.adapt(held, &nty, Repr::Address).address();
    let table = body.lo.dyn_table(&source, bound.interface, &bound.args);
    let global = body.lo.data_ref(body.fx, table);
    let table_address = body.fx.b.ins().symbol_value(PTR, global);
    let value = body.fx.slot(16, 8);
    body.fx.store(address, value, 0);
    body.fx.store(table_address, value, 8);
    Held::Address(value)
}

impl Lowering<'_, '_> {
    /// The method table of a type for an interface.
    pub fn dyn_table(
        &mut self,
        ty: &Ty,
        interface: crate::check::db::ItemId,
        args: &Args,
    ) -> cranelift_module::DataId {
        let methods = match &self.ck.sig(interface).kind {
            SigKind::Interface { methods } => methods.len(),
            _ => 0,
        };
        let layout = self.layout(ty);
        self.glue += 1;
        let name = format!("_NXtable_{}", self.glue);
        let id = self
            .module
            .declare_data(&name, Linkage::Local, false, false)
            .expect("a method table");
        let mut bytes = vec![0u8; 24 + 8 * methods];
        bytes[8..16].copy_from_slice(&layout.size.to_le_bytes());
        bytes[16..24].copy_from_slice(&layout.align.to_le_bytes());
        let mut description = DataDescription::new();
        description.define(bytes.into_boxed_slice());
        description.set_align(8);
        if self.needs_release(ty) {
            let release = self.instance(Instance::Release(ty.clone()));
            let reference = self.module.declare_func_in_data(release, &mut description);
            description.write_function_addr(0, reference);
        }
        for method in 0..methods {
            let glue = self.instance(Instance::Dyn {
                ty: ty.clone(),
                interface,
                args: args.clone(),
                method: method as u32,
            });
            let reference = self.module.declare_func_in_data(glue, &mut description);
            description.write_function_addr(24 + 8 * method as u32, reference);
        }
        self.module
            .define_data(id, &description)
            .expect("a method table");
        id
    }
}
