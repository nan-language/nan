//! The `text::json` format's writing and reading of each kind. A reading gives its values and
//! the code of its failure, zero for none.

use cranelift_codegen::ir::{AbiParam, InstBuilder, Signature, Value, condcodes::IntCC, types};

use super::super::super::body::Held;
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::Routine;
use super::super::super::{CALL_CONV, Glue, Lowering};
use super::super::mem::vector;
use super::super::render::encode_utf8;
use super::{failure, put_byte, put_bytes, put_literal, reader, utf8_valid};
use crate::check::ty::Scalar;

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn code(fx: &mut Fx, code: i64) -> Value {
    fx.int(types::I32, code)
}

fn succeeded(fx: &mut Fx, failed: Value) -> Value {
    fx.b.ins().icmp_imm_s(IntCC::Equal, failed, 0)
}

/// Writes an integer in decimal, with `-` before a negative signed one.
pub fn put_integer(lo: &mut Lowering, fx: &mut Fx, state: Value, value: Value, signed: bool) {
    let magnitude = match signed {
        true => {
            let negative = fx.b.ins().icmp_imm_s(IntCC::SignedLessThan, value, 0);
            fx.when(negative, |fx| put_literal(lo, fx, state, b"-"));
            let flipped = fx.b.ins().ineg(value);
            fx.b.ins().select(negative, flipped, value)
        }
        false => value,
    };
    let digits = fx.slot(20, 1);
    let end = fx.ptr(20);
    let first = fx.repeat(&[magnitude, end], |fx, values| {
        let (rest, at) = (values[0], values[1]);
        let ten = fx.ptr(10);
        let digit = fx.b.ins().urem(rest, ten);
        let digit = fx.b.ins().iadd_imm_s(digit, i64::from(b'0'));
        let digit = fx.b.ins().ireduce(types::I8, digit);
        let at = fx.b.ins().iadd_imm_s(at, -1);
        let address = fx.b.ins().iadd(digits, at);
        fx.store(digit, address, 0);
        let rest = fx.b.ins().udiv(rest, ten);
        let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, rest, 0);
        (more, vec![rest, at])
    })[1];
    let start = fx.b.ins().iadd(digits, first);
    let count = fx.b.ins().isub(end, first);
    put_bytes(lo, fx, state, start, count);
}

/// Writes a float as its rendering, `NaN`, `inf` and `-inf` as JSON strings.
pub fn put_float(lo: &mut Lowering, fx: &mut Fx, state: Value, scalar: Scalar, value: Value) {
    let buffer = fx.slot(40, 8);
    let name = match scalar {
        Scalar::F32 => "f32_text",
        _ => "f64_text",
    };
    let len = lo
        .platform_call(fx, name, &[Held::Scalar(value), Held::Address(buffer)])
        .result
        .scalar();
    let first = fx.load(types::I8, buffer, 0);
    let second = fx.load(types::I8, buffer, 1);
    let nan = fx.b.ins().icmp_imm_s(IntCC::Equal, first, i64::from(b'N'));
    let inf = fx.b.ins().icmp_imm_s(IntCC::Equal, first, i64::from(b'i'));
    let minus = fx.b.ins().icmp_imm_s(IntCC::Equal, first, i64::from(b'-'));
    let minus_inf = fx.b.ins().icmp_imm_s(IntCC::Equal, second, i64::from(b'i'));
    let minus_inf = fx.b.ins().band(minus, minus_inf);
    let special = fx.b.ins().bor(nan, inf);
    let special = fx.b.ins().bor(special, minus_inf);
    fx.when(special, |fx| put_literal(lo, fx, state, b"\""));
    put_bytes(lo, fx, state, buffer, len);
    fx.when(special, |fx| put_literal(lo, fx, state, b"\""));
}

/// Writes text as a JSON string, escaping as JSON encoding states.
pub fn put_string(lo: &mut Lowering, fx: &mut Fx, state: Value, start: Value, len: Value) {
    put_literal(lo, fx, state, b"\"");
    let zero = fx.ptr(0);
    fx.each(zero, len, |fx, index| {
        let at = fx.b.ins().iadd(start, index);
        let byte = fx.load(types::I8, at, 0);
        let byte = fx.b.ins().uextend(types::I64, byte);
        let mut handled = fx.int(types::I8, 0);
        for (raw, escaped) in [
            (b'"', b"\\\"".as_slice()),
            (b'\\', b"\\\\"),
            (b'\n', b"\\n"),
            (b'\r', b"\\r"),
            (b'\t', b"\\t"),
            (8, b"\\b"),
            (12, b"\\f"),
        ] {
            let this = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(raw));
            fx.when(this, |fx| put_literal(lo, fx, state, escaped));
            handled = fx.b.ins().bor(handled, this);
        }
        let control = fx.b.ins().icmp_imm_s(IntCC::UnsignedLessThan, byte, 0x20);
        let unicode = fx.b.ins().bxor_imm_s(handled, 1);
        let unicode = fx.b.ins().band(unicode, control);
        fx.when(unicode, |fx| {
            put_literal(lo, fx, state, b"\\u00");
            for shift in [4, 0] {
                let nibble = fx.b.ins().ushr_imm_s(byte, shift);
                let nibble = fx.b.ins().band_imm_s(nibble, 0xF);
                let letter =
                    fx.b.ins()
                        .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, nibble, 10);
                let digit = fx.b.ins().iadd_imm_s(nibble, i64::from(b'0'));
                let lower = fx.b.ins().iadd_imm_s(nibble, i64::from(b'a') - 10);
                let hex = fx.b.ins().select(letter, lower, digit);
                put_byte(fx, state, hex);
            }
        });
        let plain = fx.b.ins().bor(handled, control);
        let plain = fx.b.ins().bxor_imm_s(plain, 1);
        fx.when(plain, |fx| put_byte(fx, state, byte));
    });
    put_literal(lo, fx, state, b"\"");
}

/// Writes a literal text as a JSON string.
pub fn put_literal_string(lo: &mut Lowering, fx: &mut Fx, state: Value, bytes: &[u8]) {
    let mut quoted = vec![b'"'];
    for byte in bytes {
        match byte {
            b'"' => quoted.extend_from_slice(b"\\\""),
            b'\\' => quoted.extend_from_slice(b"\\\\"),
            b'\n' => quoted.extend_from_slice(b"\\n"),
            b'\r' => quoted.extend_from_slice(b"\\r"),
            b'\t' => quoted.extend_from_slice(b"\\t"),
            8 => quoted.extend_from_slice(b"\\b"),
            12 => quoted.extend_from_slice(b"\\f"),
            byte if *byte < 0x20 => quoted.extend_from_slice(format!("\\u{:04x}", byte).as_bytes()),
            byte => quoted.push(*byte),
        }
    }
    quoted.push(b'"');
    put_literal(lo, fx, state, &quoted);
}

/// Writes bytes as a JSON string of their base64 with padding.
pub fn put_base64(lo: &mut Lowering, fx: &mut Fx, state: Value, start: Value, len: Value) {
    put_literal(lo, fx, state, b"\"");
    let alphabet = lo.view_literal(fx, BASE64).address();
    let alphabet = fx.load(PTR, alphabet, 0);
    let three = fx.ptr(3);
    let groups = fx.b.ins().iadd_imm_s(len, 2);
    let groups = fx.b.ins().udiv(groups, three);
    let zero = fx.ptr(0);
    fx.each(zero, groups, |fx, group| {
        let first = fx.b.ins().imul_imm_s(group, 3);
        let mut bits = fx.ptr(0);
        for index in 0..3 {
            let at = fx.b.ins().iadd_imm_s(first, index);
            let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, at, len);
            let address = fx.b.ins().iadd(start, at);
            let byte = fx.choose(
                inside,
                &[types::I64],
                |fx| {
                    let byte = fx.load(types::I8, address, 0);
                    vec![fx.b.ins().uextend(types::I64, byte)]
                },
                |fx| vec![fx.ptr(0)],
            )[0];
            let shifted = fx.b.ins().ishl_imm_s(byte, 16 - 8 * index);
            bits = fx.b.ins().bor(bits, shifted);
        }
        let present = fx.b.ins().isub(len, first);
        for index in 0..4i64 {
            let written =
                fx.b.ins()
                    .icmp_imm_s(IntCC::SignedLessThanOrEqual, present, index - 1);
            fx.choose_in(
                lo,
                written,
                &[],
                |lo, fx| {
                    put_literal(lo, fx, state, b"=");
                    Vec::new()
                },
                |_, fx| {
                    let sextet = fx.b.ins().ushr_imm_s(bits, 18 - 6 * index);
                    let sextet = fx.b.ins().band_imm_s(sextet, 0x3F);
                    let at = fx.b.ins().iadd(alphabet, sextet);
                    let letter = fx.load(types::I8, at, 0);
                    put_byte(fx, state, letter);
                    Vec::new()
                },
            );
        }
    });
    put_literal(lo, fx, state, b"\"");
}

/// Passes over whitespace.
pub fn space(fx: &mut Fx, state: Value) {
    let end = fx.load(PTR, state, reader::END);
    fx.repeat(&[], |fx, _| {
        let pos = fx.load(PTR, state, reader::POS);
        let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, pos, end);
        let blank = fx.choose(
            inside,
            &[types::I8],
            |fx| {
                let byte = fx.load(types::I8, pos, 0);
                let mut blank = fx.int(types::I8, 0);
                for character in *b" \t\n\r" {
                    let this =
                        fx.b.ins()
                            .icmp_imm_s(IntCC::Equal, byte, i64::from(character));
                    blank = fx.b.ins().bor(blank, this);
                }
                vec![blank]
            },
            |fx| vec![fx.int(types::I8, 0)],
        )[0];
        fx.when(blank, |fx| {
            let next = fx.b.ins().iadd_imm_s(pos, 1);
            fx.store(next, state, reader::POS);
        });
        (blank, Vec::new())
    });
}

/// The next byte after whitespace, not passed over: `Truncated` at the end of the input.
pub fn peek(fx: &mut Fx, state: Value) -> (Value, Value) {
    space(fx, state);
    let pos = fx.load(PTR, state, reader::POS);
    let end = fx.load(PTR, state, reader::END);
    let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, pos, end);
    let parts = fx.choose(
        inside,
        &[types::I64, types::I32],
        |fx| {
            let byte = fx.load(types::I8, pos, 0);
            vec![fx.b.ins().uextend(types::I64, byte), code(fx, 0)]
        },
        |fx| vec![fx.ptr(0), code(fx, failure::TRUNCATED)],
    );
    (parts[0], parts[1])
}

/// Passes over a byte after whitespace: `Malformed` for another byte.
pub fn expect_code(lo: &mut Lowering, fx: &mut Fx, state: Value, expected: u8) -> Value {
    let (byte, failed) = peek(fx, state);
    let ok = succeeded(fx, failed);
    fx.choose_in(
        lo,
        ok,
        &[types::I32],
        |_, fx| {
            let same =
                fx.b.ins()
                    .icmp_imm_s(IntCC::Equal, byte, i64::from(expected));
            fx.when(same, |fx| {
                let pos = fx.load(PTR, state, reader::POS);
                let next = fx.b.ins().iadd_imm_s(pos, 1);
                fx.store(next, state, reader::POS);
            });
            let malformed = code(fx, failure::MALFORMED);
            let none = code(fx, 0);
            vec![fx.b.ins().select(same, none, malformed)]
        },
        |_, _| vec![failed],
    )[0]
}

/// Whether a literal word follows after whitespace, passing over it when it does.
fn literal(lo: &mut Lowering, fx: &mut Fx, state: Value, word: &[u8]) -> Value {
    space(fx, state);
    let pos = fx.load(PTR, state, reader::POS);
    let end = fx.load(PTR, state, reader::END);
    let left = fx.b.ins().isub(end, pos);
    let room =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, left, word.len() as i64);
    let found = fx.choose_in(
        lo,
        room,
        &[types::I8],
        |lo, fx| {
            let view = lo.view_literal(fx, word).address();
            let expected = fx.load(PTR, view, 0);
            let len = fx.ptr(word.len() as u64);
            let order = lo.call_routine(fx, Routine::Compare, &[pos, expected, len])[0];
            vec![fx.b.ins().icmp_imm_s(IntCC::Equal, order, 0)]
        },
        |_, fx| vec![fx.int(types::I8, 0)],
    )[0];
    fx.when(found, |fx| {
        let next = fx.b.ins().iadd_imm_s(pos, word.len() as i64);
        fx.store(next, state, reader::POS);
    });
    found
}

/// A bool: `true` or `false`, `Malformed` for anything else.
pub fn read_bool(lo: &mut Lowering, fx: &mut Fx, state: Value) -> (Value, Value) {
    let truth = literal(lo, fx, state, b"true");
    let falsity = fx.choose_in(
        lo,
        truth,
        &[types::I8],
        |_, fx| vec![fx.int(types::I8, 0)],
        |lo, fx| vec![literal(lo, fx, state, b"false")],
    )[0];
    let either = fx.b.ins().bor(truth, falsity);
    let malformed = code(fx, failure::MALFORMED);
    let none = code(fx, 0);
    (truth, fx.b.ins().select(either, none, malformed))
}

/// A number checked against the RFC 8259 grammar: its first byte's address, its end, and
/// whether it has a fraction or an exponent.
fn number(lo: &mut Lowering, fx: &mut Fx, state: Value) -> (Value, Value, Value, Value) {
    let mut signature = Signature::new(CALL_CONV);
    signature.params.push(AbiParam::new(PTR));
    signature.params.push(AbiParam::new(PTR));
    signature.returns.push(AbiParam::new(types::I32));
    let function = lo.glue_function(Glue::JsonNumber, signature, |_, fx| {
        let params = fx.entry();
        let (state, found) = (params[0], params[1]);
        space(fx, state);
        let start = fx.load(PTR, state, reader::POS);
        let end = fx.load(PTR, state, reader::END);
        let byte_at = |fx: &mut Fx, at: Value| {
            let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, at, end);
            fx.choose(
                inside,
                &[types::I64],
                |fx| {
                    let byte = fx.load(types::I8, at, 0);
                    vec![fx.b.ins().uextend(types::I64, byte)]
                },
                |fx| vec![fx.int(types::I64, -1)],
            )[0]
        };
        let is_digit = |fx: &mut Fx, byte: Value| {
            let low =
                fx.b.ins()
                    .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, byte, i64::from(b'0'));
            let high =
                fx.b.ins()
                    .icmp_imm_s(IntCC::SignedLessThanOrEqual, byte, i64::from(b'9'));
            fx.b.ins().band(low, high)
        };
        let digits = |fx: &mut Fx, from: Value| {
            fx.repeat(&[from], |fx, values| {
                let at = values[0];
                let byte = byte_at(fx, at);
                let digit = is_digit(fx, byte);
                let next = fx.b.ins().iadd_imm_s(at, 1);
                (digit, vec![fx.b.ins().select(digit, next, at)])
            })[0]
        };
        let malformed = |fx: &mut Fx| {
            let code = code(fx, failure::MALFORMED);
            fx.b.ins().return_(&[code]);
        };
        let first = byte_at(fx, start);
        let minus = fx.b.ins().icmp_imm_s(IntCC::Equal, first, i64::from(b'-'));
        let after_sign = fx.b.ins().iadd_imm_s(start, 1);
        let at = fx.b.ins().select(minus, after_sign, start);
        let lead = byte_at(fx, at);
        let zero_lead = fx.b.ins().icmp_imm_s(IntCC::Equal, lead, i64::from(b'0'));
        let lead_digit = is_digit(fx, lead);
        let not_digit = fx.b.ins().bxor_imm_s(lead_digit, 1);
        fx.when(not_digit, |fx| malformed(fx));
        let after_lead = fx.b.ins().iadd_imm_s(at, 1);
        let integral_end = digits(fx, after_lead);
        let at = fx.b.ins().select(zero_lead, after_lead, integral_end);
        let point_byte = byte_at(fx, at);
        let point =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, point_byte, i64::from(b'.'));
        let at = fx.choose(
            point,
            &[types::I64],
            |fx| {
                let from = fx.b.ins().iadd_imm_s(at, 1);
                let to = digits(fx, from);
                let none = fx.b.ins().icmp(IntCC::Equal, to, from);
                fx.when(none, |fx| malformed(fx));
                vec![to]
            },
            |_| vec![at],
        )[0];
        let exponent_byte = byte_at(fx, at);
        let lower_e =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, exponent_byte, i64::from(b'e'));
        let upper_e =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, exponent_byte, i64::from(b'E'));
        let exponent = fx.b.ins().bor(lower_e, upper_e);
        let at = fx.choose(
            exponent,
            &[types::I64],
            |fx| {
                let from = fx.b.ins().iadd_imm_s(at, 1);
                let sign = byte_at(fx, from);
                let plus = fx.b.ins().icmp_imm_s(IntCC::Equal, sign, i64::from(b'+'));
                let negative = fx.b.ins().icmp_imm_s(IntCC::Equal, sign, i64::from(b'-'));
                let signed = fx.b.ins().bor(plus, negative);
                let after = fx.b.ins().iadd_imm_s(from, 1);
                let from = fx.b.ins().select(signed, after, from);
                let to = digits(fx, from);
                let none = fx.b.ins().icmp(IntCC::Equal, to, from);
                fx.when(none, |fx| malformed(fx));
                vec![to]
            },
            |_| vec![at],
        )[0];
        fx.store(at, state, reader::POS);
        let fractional = fx.b.ins().bor(point, exponent);
        fx.store(start, found, 0);
        fx.store(at, found, 8);
        fx.store(fractional, found, 16);
        let none = code(fx, 0);
        fx.b.ins().return_(&[none]);
    });
    let found = fx.slot(24, 8);
    let reference = lo.func_ref(fx, function);
    let call = fx.b.ins().call(reference, &[state, found]);
    let failed = fx.b.inst_results(call)[0];
    let start = fx.load(PTR, found, 0);
    let end = fx.load(PTR, found, 8);
    let fractional = fx.load(types::I8, found, 16);
    (start, end, fractional, failed)
}

/// An integer without fraction or exponent, `-0` being 0, in 64 bits: `OutOfRange` outside
/// `u64`, or `i64` when signed.
pub fn read_integer(lo: &mut Lowering, fx: &mut Fx, state: Value, signed: bool) -> (Value, Value) {
    let (start, end, fractional, failed) = number(lo, fx, state);
    let ok = succeeded(fx, failed);
    let parts = fx.choose_in(
        lo,
        ok,
        &[types::I64, types::I32],
        |_, fx| {
            let first = fx.load(types::I8, start, 0);
            let negative = fx.b.ins().icmp_imm_s(IntCC::Equal, first, i64::from(b'-'));
            let after = fx.b.ins().iadd_imm_s(start, 1);
            let from = fx.b.ins().select(negative, after, start);
            let zero = fx.ptr(0);
            let no = fx.int(types::I8, 0);
            let accumulated = fx.repeat(&[from, zero, no], |fx, values| {
                let (at, value, overflowed) = (values[0], values[1], values[2]);
                let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, at, end);
                let next = fx.choose(
                    inside,
                    &[types::I64, types::I64, types::I8],
                    |fx| {
                        let byte = fx.load(types::I8, at, 0);
                        let digit = fx.b.ins().uextend(types::I64, byte);
                        let digit = fx.b.ins().iadd_imm_s(digit, -i64::from(b'0'));
                        let ten = fx.ptr(10);
                        let (scaled, over_mul) = fx.b.ins().umul_overflow(value, ten);
                        let (sum, over_add) = fx.b.ins().uadd_overflow(scaled, digit);
                        let over = fx.b.ins().bor(over_mul, over_add);
                        let overflowed = fx.b.ins().bor(overflowed, over);
                        vec![fx.b.ins().iadd_imm_s(at, 1), sum, overflowed]
                    },
                    |_| vec![at, value, overflowed],
                );
                (inside, next)
            });
            let (magnitude, overflowed) = (accumulated[1], accumulated[2]);
            let out_of_range = match signed {
                true => {
                    let most_negative = fx.int(types::I64, i64::MIN);
                    let most_positive = fx.int(types::I64, i64::MAX);
                    let limit = fx.b.ins().select(negative, most_negative, most_positive);
                    let beyond =
                        fx.b.ins()
                            .icmp(IntCC::UnsignedGreaterThan, magnitude, limit);
                    fx.b.ins().bor(overflowed, beyond)
                }
                false => {
                    let nonzero = fx.b.ins().icmp_imm_s(IntCC::NotEqual, magnitude, 0);
                    let below = fx.b.ins().band(negative, nonzero);
                    fx.b.ins().bor(overflowed, below)
                }
            };
            let negated = fx.b.ins().ineg(magnitude);
            let value = match signed {
                true => fx.b.ins().select(negative, negated, magnitude),
                false => magnitude,
            };
            let malformed = code(fx, failure::MALFORMED);
            let range = code(fx, failure::OUT_OF_RANGE);
            let none = code(fx, 0);
            let checked = fx.b.ins().select(out_of_range, range, none);
            vec![value, fx.b.ins().select(fractional, malformed, checked)]
        },
        |_, fx| vec![fx.ptr(0), failed],
    );
    (parts[0], parts[1])
}

/// A float: any number rounded to f64, or one of the strings `NaN`, `inf` and `-inf`.
pub fn read_float(lo: &mut Lowering, fx: &mut Fx, state: Value) -> (Value, Value) {
    let (byte, failed) = peek(fx, state);
    let ok = succeeded(fx, failed);
    let quoted = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'"'));
    let parts = fx.choose_in(
        lo,
        ok,
        &[types::F64, types::I32],
        |lo, fx| {
            fx.choose_in(
                lo,
                quoted,
                &[types::F64, types::I32],
                |lo, fx| {
                    let buffer = fx.slot(8, 1);
                    let capacity = fx.ptr(8);
                    let (len, failed) = string(lo, fx, state, buffer, capacity);
                    let mut value = fx.b.ins().f64const(0.0);
                    let mut known = fx.int(types::I8, 0);
                    for (word, float) in [
                        (b"NaN".as_slice(), f64::NAN),
                        (b"inf", f64::INFINITY),
                        (b"-inf", f64::NEG_INFINITY),
                    ] {
                        let same = super::bytes_equal(lo, fx, buffer, len, word);
                        let constant = fx.b.ins().f64const(float);
                        value = fx.b.ins().select(same, constant, value);
                        known = fx.b.ins().bor(known, same);
                    }
                    let ok = succeeded(fx, failed);
                    let malformed = code(fx, failure::MALFORMED);
                    let none = code(fx, 0);
                    let checked = fx.b.ins().select(known, none, malformed);
                    vec![value, fx.b.ins().select(ok, checked, failed)]
                },
                |lo, fx| {
                    let (start, end, _, failed) = number(lo, fx, state);
                    let ok = succeeded(fx, failed);
                    fx.choose_in(
                        lo,
                        ok,
                        &[types::F64, types::I32],
                        |lo, fx| {
                            let len = fx.b.ins().isub(end, start);
                            let size = fx.b.ins().iadd_imm_s(len, 1);
                            let text = lo.record_alloc(fx, size);
                            let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, text, 0);
                            fx.choose_in(
                                lo,
                                missing,
                                &[types::F64, types::I32],
                                |_, fx| {
                                    vec![fx.b.ins().f64const(0.0), code(fx, failure::OUT_OF_MEMORY)]
                                },
                                |lo, fx| {
                                    lo.call_routine(fx, Routine::Copy, &[text, start, len]);
                                    let none = fx.ptr(0);
                                    let value =
                                        lo.call_libc(fx, "strtod", &[text, none], &[types::F64])[0];
                                    lo.record_free(fx, text);
                                    vec![value, code(fx, 0)]
                                },
                            )
                        },
                        |_, fx| vec![fx.b.ins().f64const(0.0), failed],
                    )
                },
            )
        },
        |_, fx| vec![fx.b.ins().f64const(0.0), failed],
    );
    (parts[0], parts[1])
}

/// A char: a JSON string of exactly one char, its scalar value in 64 bits.
pub fn read_char(lo: &mut Lowering, fx: &mut Fx, state: Value) -> (Value, Value) {
    let buffer = fx.slot(8, 1);
    let capacity = fx.ptr(8);
    let (len, failed) = string(lo, fx, state, buffer, capacity);
    one_char(fx, buffer, len, failed)
}

/// The scalar value of text read into `buffer` with the code `failed`, which must be exactly
/// one char: the code, `Malformed` when it is not.
pub fn one_char(fx: &mut Fx, buffer: Value, len: Value, failed: Value) -> (Value, Value) {
    let lead = fx.load(types::I8, buffer, 0);
    let lead = fx.b.ins().uextend(types::I64, lead);
    let two =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, lead, 0xC0);
    let three =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, lead, 0xE0);
    let four =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, lead, 0xF0);
    let mut width = fx.ptr(1);
    for flag in [two, three, four] {
        let step = fx.b.ins().uextend(types::I64, flag);
        width = fx.b.ins().iadd(width, step);
    }
    let one_char = fx.b.ins().icmp(IntCC::Equal, len, width);
    let mut scalar = fx.b.ins().band_imm_s(lead, 0x7F);
    let masks = [0x1F, 0x0F, 0x07];
    for (index, width_flag) in [two, three, four].iter().enumerate() {
        let masked = fx.b.ins().band_imm_s(lead, masks[index]);
        scalar = fx.b.ins().select(*width_flag, masked, scalar);
    }
    for index in 1..4u64 {
        let inside =
            fx.b.ins()
                .icmp_imm_s(IntCC::UnsignedGreaterThan, width, index as i64);
        let at = fx.offset(buffer, index);
        let byte = fx.load(types::I8, at, 0);
        let byte = fx.b.ins().uextend(types::I64, byte);
        let bits = fx.b.ins().band_imm_s(byte, 0x3F);
        let shifted = fx.b.ins().ishl_imm_s(scalar, 6);
        let joined = fx.b.ins().bor(shifted, bits);
        scalar = fx.b.ins().select(inside, joined, scalar);
    }
    let ok = succeeded(fx, failed);
    let malformed = code(fx, failure::MALFORMED);
    let none = code(fx, 0);
    let checked = fx.b.ins().select(one_char, none, malformed);
    (scalar, fx.b.ins().select(ok, checked, failed))
}

/// A JSON string, its escapes resolved: at most `capacity` of its bytes written to `buffer`
/// unless it is zero, and its whole length.
pub fn string(
    lo: &mut Lowering,
    fx: &mut Fx,
    state: Value,
    buffer: Value,
    capacity: Value,
) -> (Value, Value) {
    let mut signature = Signature::new(CALL_CONV);
    for _ in 0..3 {
        signature.params.push(AbiParam::new(PTR));
    }
    signature.returns.push(AbiParam::new(types::I64));
    signature.returns.push(AbiParam::new(types::I32));
    let function = lo.glue_function(Glue::JsonString, signature, |lo, fx| {
        let params = fx.entry();
        let (state, buffer, capacity) = (params[0], params[1], params[2]);
        let opened = expect_code(lo, fx, state, b'"');
        let zero = fx.ptr(0);
        let failed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, opened, 0);
        fx.when(failed, |fx| {
            fx.b.ins().return_(&[zero, opened]);
        });
        let end = fx.load(PTR, state, reader::END);
        let give_up = |fx: &mut Fx, len: Value, failure: i64| {
            let code = code(fx, failure);
            fx.b.ins().return_(&[len, code]);
        };
        let emit = |lo: &mut Lowering, fx: &mut Fx, len: Value, from: Value, count: Value| {
            let writing = fx.b.ins().icmp_imm_s(IntCC::NotEqual, buffer, 0);
            let room = fx.b.ins().icmp(IntCC::UnsignedLessThan, len, capacity);
            let copying = fx.b.ins().band(writing, room);
            fx.when(copying, |fx| {
                let left = fx.b.ins().isub(capacity, len);
                let kept = fx.b.ins().umin(left, count);
                let at = fx.b.ins().iadd(buffer, len);
                lo.call_routine(fx, Routine::Copy, &[at, from, kept]);
            });
            fx.b.ins().iadd(len, count)
        };
        fx.repeat(&[zero], |fx, values| {
            let len = values[0];
            let segment = fx.load(PTR, state, reader::POS);
            let stop = fx.repeat(&[segment], |fx, values| {
                let at = values[0];
                let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, at, end);
                let plain = fx.choose(
                    inside,
                    &[types::I8],
                    |fx| {
                        let byte = fx.load(types::I8, at, 0);
                        let quote = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'"'));
                        let escape = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'\\'));
                        let special = fx.b.ins().bor(quote, escape);
                        vec![fx.b.ins().bxor_imm_s(special, 1)]
                    },
                    |fx| vec![fx.int(types::I8, 0)],
                )[0];
                let next = fx.b.ins().iadd_imm_s(at, 1);
                (plain, vec![fx.b.ins().select(plain, next, at)])
            })[0];
            let truncated = fx.b.ins().icmp(IntCC::Equal, stop, end);
            fx.when(truncated, |fx| give_up(fx, len, failure::TRUNCATED));
            let count = fx.b.ins().isub(stop, segment);
            let valid = utf8_valid(lo, fx, segment, count);
            let invalid = fx.b.ins().bxor_imm_s(valid, 1);
            fx.when(invalid, |fx| give_up(fx, len, failure::INVALID_TEXT));
            let zero = fx.ptr(0);
            fx.each(zero, count, |fx, index| {
                let at = fx.b.ins().iadd(segment, index);
                let byte = fx.load(types::I8, at, 0);
                let control = fx.b.ins().icmp_imm_s(IntCC::UnsignedLessThan, byte, 0x20);
                fx.when(control, |fx| give_up(fx, len, failure::MALFORMED));
            });
            let len = emit(lo, fx, len, segment, count);
            let byte = fx.load(types::I8, stop, 0);
            let after = fx.b.ins().iadd_imm_s(stop, 1);
            fx.store(after, state, reader::POS);
            let closed = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'"'));
            fx.when(closed, |fx| {
                let none = code(fx, 0);
                fx.b.ins().return_(&[len, none]);
            });
            let at_end = fx.b.ins().icmp(IntCC::Equal, after, end);
            fx.when(at_end, |fx| give_up(fx, len, failure::TRUNCATED));
            let escape = fx.load(types::I8, after, 0);
            let escape = fx.b.ins().uextend(types::I64, escape);
            let past = fx.b.ins().iadd_imm_s(after, 1);
            fx.store(past, state, reader::POS);
            let char_buffer = fx.slot(4, 1);
            let mut scalar = fx.int(types::I64, -1);
            for (letter, meaning) in [
                (b'"', b'"'),
                (b'\\', b'\\'),
                (b'/', b'/'),
                (b'b', 8),
                (b'f', 12),
                (b'n', b'\n'),
                (b'r', b'\r'),
                (b't', b'\t'),
            ] {
                let this =
                    fx.b.ins()
                        .icmp_imm_s(IntCC::Equal, escape, i64::from(letter));
                let meaning = fx.int(types::I64, i64::from(meaning));
                scalar = fx.b.ins().select(this, meaning, scalar);
            }
            let unicode = fx.b.ins().icmp_imm_s(IntCC::Equal, escape, i64::from(b'u'));
            let scalar = fx.choose_in(
                lo,
                unicode,
                &[types::I64],
                |_, fx| {
                    let high = hex4(fx, state, len, &give_up);
                    let leading = in_range(fx, high, 0xD800, 0xDBFF);
                    let trailing = in_range(fx, high, 0xDC00, 0xDFFF);
                    fx.when(trailing, |fx| give_up(fx, len, failure::INVALID_TEXT));
                    let joined = fx.choose(
                        leading,
                        &[types::I64],
                        |fx| {
                            let pos = fx.load(PTR, state, reader::POS);
                            let left = fx.b.ins().isub(end, pos);
                            let short = fx.b.ins().icmp_imm_s(IntCC::UnsignedLessThan, left, 2);
                            fx.when(short, |fx| give_up(fx, len, failure::TRUNCATED));
                            let first = fx.load(types::I8, pos, 0);
                            let second = fx.load(types::I8, pos, 1);
                            let slash =
                                fx.b.ins().icmp_imm_s(IntCC::Equal, first, i64::from(b'\\'));
                            let u = fx.b.ins().icmp_imm_s(IntCC::Equal, second, i64::from(b'u'));
                            let paired = fx.b.ins().band(slash, u);
                            let unpaired = fx.b.ins().bxor_imm_s(paired, 1);
                            fx.when(unpaired, |fx| give_up(fx, len, failure::INVALID_TEXT));
                            let next = fx.b.ins().iadd_imm_s(pos, 2);
                            fx.store(next, state, reader::POS);
                            let low = hex4(fx, state, len, &give_up);
                            let low_ok = in_range(fx, low, 0xDC00, 0xDFFF);
                            let bad = fx.b.ins().bxor_imm_s(low_ok, 1);
                            fx.when(bad, |fx| give_up(fx, len, failure::INVALID_TEXT));
                            let high_bits = fx.b.ins().iadd_imm_s(high, -0xD800);
                            let high_bits = fx.b.ins().ishl_imm_s(high_bits, 10);
                            let low_bits = fx.b.ins().iadd_imm_s(low, -0xDC00);
                            let joined = fx.b.ins().iadd(high_bits, low_bits);
                            vec![fx.b.ins().iadd_imm_s(joined, 0x1_0000)]
                        },
                        |_| vec![high],
                    )[0];
                    vec![joined]
                },
                |_, _| vec![scalar],
            )[0];
            let unknown = fx.b.ins().icmp_imm_s(IntCC::Equal, scalar, -1);
            fx.when(unknown, |fx| give_up(fx, len, failure::MALFORMED));
            let narrow = fx.b.ins().ireduce(types::I32, scalar);
            let count = encode_utf8(fx, narrow, char_buffer);
            let len = emit(lo, fx, len, char_buffer, count);
            (fx.int(types::I8, 1), vec![len])
        });
        let none = fx.ptr(0);
        let truncated = code(fx, failure::TRUNCATED);
        fx.b.ins().return_(&[none, truncated]);
    });
    let reference = lo.func_ref(fx, function);
    let call = fx.b.ins().call(reference, &[state, buffer, capacity]);
    let results = fx.b.inst_results(call).to_vec();
    (results[0], results[1])
}

fn in_range(fx: &mut Fx, value: Value, low: i64, high: i64) -> Value {
    let above =
        fx.b.ins()
            .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, value, low);
    let below =
        fx.b.ins()
            .icmp_imm_s(IntCC::SignedLessThanOrEqual, value, high);
    fx.b.ins().band(above, below)
}

/// Four hexadecimal digits after `\u`, giving up with `Truncated` or `Malformed`.
fn hex4(fx: &mut Fx, state: Value, len: Value, give_up: &impl Fn(&mut Fx, Value, i64)) -> Value {
    let pos = fx.load(PTR, state, reader::POS);
    let end = fx.load(PTR, state, reader::END);
    let left = fx.b.ins().isub(end, pos);
    let short = fx.b.ins().icmp_imm_s(IntCC::UnsignedLessThan, left, 4);
    fx.when(short, |fx| give_up(fx, len, failure::TRUNCATED));
    let mut value = fx.ptr(0);
    for index in 0..4u64 {
        let at = fx.offset(pos, index);
        let byte = fx.load(types::I8, at, 0);
        let byte = fx.b.ins().uextend(types::I64, byte);
        let digit = in_range(fx, byte, i64::from(b'0'), i64::from(b'9'));
        let lower = in_range(fx, byte, i64::from(b'a'), i64::from(b'f'));
        let upper = in_range(fx, byte, i64::from(b'A'), i64::from(b'F'));
        let hex = fx.b.ins().bor(digit, lower);
        let hex = fx.b.ins().bor(hex, upper);
        let not_hex = fx.b.ins().bxor_imm_s(hex, 1);
        fx.when(not_hex, |fx| give_up(fx, len, failure::MALFORMED));
        let from_digit = fx.b.ins().iadd_imm_s(byte, -i64::from(b'0'));
        let from_lower = fx.b.ins().iadd_imm_s(byte, 10 - i64::from(b'a'));
        let from_upper = fx.b.ins().iadd_imm_s(byte, 10 - i64::from(b'A'));
        let nibble = fx.b.ins().select(lower, from_lower, from_digit);
        let nibble = fx.b.ins().select(upper, from_upper, nibble);
        let shifted = fx.b.ins().ishl_imm_s(value, 4);
        value = fx.b.ins().bor(shifted, nibble);
    }
    let next = fx.b.ins().iadd_imm_s(pos, 4);
    fx.store(next, state, reader::POS);
    value
}

/// The length of the JSON string that follows, its escapes resolved, the reader staying put.
pub fn measure_string(lo: &mut Lowering, fx: &mut Fx, state: Value) -> (Value, Value) {
    let pos = fx.load(PTR, state, reader::POS);
    let none = fx.ptr(0);
    let (len, failed) = string(lo, fx, state, none, none);
    let ok = succeeded(fx, failed);
    fx.when(ok, |fx| fx.store(pos, state, reader::POS));
    (len, failed)
}

/// Writes the JSON string that follows, measured before as `len` bytes, to `destination`.
pub fn write_string(lo: &mut Lowering, fx: &mut Fx, state: Value, destination: Value, len: Value) {
    string(lo, fx, state, destination, len);
}

/// A record member's key, at most `capacity` bytes of it written to `buffer`, then its colon.
pub fn read_key(
    lo: &mut Lowering,
    fx: &mut Fx,
    state: Value,
    buffer: Value,
    capacity: Value,
) -> (Value, Value) {
    let (len, failed) = string(lo, fx, state, buffer, capacity);
    let ok = succeeded(fx, failed);
    let failed = fx.choose_in(
        lo,
        ok,
        &[types::I32],
        |lo, fx| vec![expect_code(lo, fx, state, b':')],
        |_, _| vec![failed],
    )[0];
    (len, failed)
}

/// A variant's key, at most `capacity` bytes of it written to `buffer`, and whether a payload
/// follows: a string alone, or an object whose one member is the key.
pub fn read_variant(
    lo: &mut Lowering,
    fx: &mut Fx,
    state: Value,
    buffer: Value,
    capacity: Value,
) -> (Value, Value, Value) {
    let (byte, failed) = peek(fx, state);
    let ok = succeeded(fx, failed);
    let quoted = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'"'));
    let parts = fx.choose_in(
        lo,
        ok,
        &[types::I64, types::I8, types::I32],
        |lo, fx| {
            fx.choose_in(
                lo,
                quoted,
                &[types::I64, types::I8, types::I32],
                |lo, fx| {
                    let (len, failed) = string(lo, fx, state, buffer, capacity);
                    vec![len, fx.int(types::I8, 0), failed]
                },
                |lo, fx| {
                    let opened = expect_code(lo, fx, state, b'{');
                    let ok = succeeded(fx, opened);
                    fx.choose_in(
                        lo,
                        ok,
                        &[types::I64, types::I8, types::I32],
                        |lo, fx| {
                            let (len, failed) = read_key(lo, fx, state, buffer, capacity);
                            vec![len, fx.int(types::I8, 1), failed]
                        },
                        |_, fx| vec![fx.ptr(0), fx.int(types::I8, 0), opened],
                    )
                },
            )
        },
        |_, fx| vec![fx.ptr(0), fx.int(types::I8, 0), failed],
    );
    (parts[0], parts[1], parts[2])
}

/// Whether the element at `index` of an array or object follows: none once the closing byte
/// comes, which is passed over; a comma before each element after the first.
pub fn element_follows(
    lo: &mut Lowering,
    fx: &mut Fx,
    state: Value,
    index: Value,
    close: u8,
) -> (Value, Value) {
    let (byte, failed) = peek(fx, state);
    let ok = succeeded(fx, failed);
    let parts = fx.choose_in(
        lo,
        ok,
        &[types::I8, types::I32],
        |lo, fx| {
            let closing = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(close));
            fx.choose_in(
                lo,
                closing,
                &[types::I8, types::I32],
                |_, fx| {
                    let pos = fx.load(PTR, state, reader::POS);
                    let next = fx.b.ins().iadd_imm_s(pos, 1);
                    fx.store(next, state, reader::POS);
                    vec![fx.int(types::I8, 0), code(fx, 0)]
                },
                |lo, fx| {
                    let later = fx.b.ins().icmp_imm_s(IntCC::NotEqual, index, 0);
                    let failed = fx.choose_in(
                        lo,
                        later,
                        &[types::I32],
                        |lo, fx| vec![expect_code(lo, fx, state, b',')],
                        |_, fx| vec![code(fx, 0)],
                    )[0];
                    vec![fx.int(types::I8, 1), failed]
                },
            )
        },
        |_, fx| vec![fx.int(types::I8, 0), failed],
    );
    (parts[0], parts[1])
}

/// Passes over any JSON value, nested no deeper than decoding reads.
pub fn skip_value(lo: &mut Lowering, fx: &mut Fx, state: Value) -> Value {
    let mut signature = Signature::new(CALL_CONV);
    signature.params.push(AbiParam::new(PTR));
    signature.returns.push(AbiParam::new(types::I32));
    let function = lo.glue_function(Glue::JsonSkip, signature, |lo, fx| {
        let params = fx.entry();
        let state = params[0];
        let (byte, failed) = peek(fx, state);
        let stop = |fx: &mut Fx, failed: Value| {
            let bad = fx.b.ins().icmp_imm_s(IntCC::NotEqual, failed, 0);
            fx.when(bad, |fx| {
                fx.b.ins().return_(&[failed]);
            });
        };
        stop(fx, failed);
        let none = code(fx, 0);
        let quote = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'"'));
        fx.when(quote, |fx| {
            let zero = fx.ptr(0);
            let (_, failed) = string(lo, fx, state, zero, zero);
            fx.b.ins().return_(&[failed]);
        });
        for (open, close) in [(b'{', b'}'), (b'[', b']')] {
            let this = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(open));
            fx.when(this, |fx| {
                super::nest(fx, state);
                let pos = fx.load(PTR, state, reader::POS);
                let next = fx.b.ins().iadd_imm_s(pos, 1);
                fx.store(next, state, reader::POS);
                let zero = fx.ptr(0);
                fx.repeat(&[zero], |fx, values| {
                    let index = values[0];
                    let (follows, failed) = element_follows(lo, fx, state, index, close);
                    stop(fx, failed);
                    fx.when(follows, |fx| {
                        if open == b'{' {
                            let zero = fx.ptr(0);
                            let (_, failed) = read_key(lo, fx, state, zero, zero);
                            stop(fx, failed);
                        }
                        let failed = skip_value(lo, fx, state);
                        stop(fx, failed);
                    });
                    let next = fx.b.ins().iadd_imm_s(index, 1);
                    (follows, vec![next])
                });
                super::unnest(fx, state);
                fx.b.ins().return_(&[none]);
            });
        }
        for word in [b"true".as_slice(), b"false", b"null"] {
            let found = literal(lo, fx, state, word);
            fx.when(found, |fx| {
                fx.b.ins().return_(&[none]);
            });
        }
        let (_, _, _, failed) = number(lo, fx, state);
        fx.b.ins().return_(&[failed]);
    });
    let reference = lo.func_ref(fx, function);
    let call = fx.b.ins().call(reference, &[state]);
    fx.b.inst_results(call)[0]
}

/// Bytes: a JSON string of their base64 exactly as encoding writes it, `Malformed` for any
/// other, read into bytes drawn through the allocator at `target`, which holds empty bytes.
pub fn read_base64(lo: &mut Lowering, fx: &mut Fx, state: Value, target: Value) -> Value {
    let (len, failed) = measure_string(lo, fx, state);
    let ok = succeeded(fx, failed);
    fx.choose_in(
        lo,
        ok,
        &[types::I32],
        |lo, fx| {
            let one = fx.ptr(1);
            let size = fx.b.ins().umax(len, one);
            let text = lo.record_alloc(fx, size);
            let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, text, 0);
            fx.choose_in(
                lo,
                missing,
                &[types::I32],
                |_, fx| vec![code(fx, failure::OUT_OF_MEMORY)],
                |lo, fx| {
                    write_string(lo, fx, state, text, len);
                    let mut signature = Signature::new(CALL_CONV);
                    for _ in 0..3 {
                        signature.params.push(AbiParam::new(PTR));
                    }
                    signature.returns.push(AbiParam::new(types::I32));
                    let function = lo.glue_function(Glue::Base64, signature, |lo, fx| {
                        let params = fx.entry();
                        let failed = decode_base64(lo, fx, params[0], params[1], params[2]);
                        fx.b.ins().return_(&[failed]);
                    });
                    let reference = lo.func_ref(fx, function);
                    let call = fx.b.ins().call(reference, &[text, len, target]);
                    let failed = fx.b.inst_results(call)[0];
                    lo.record_free(fx, text);
                    vec![failed]
                },
            )
        },
        |_, _| vec![failed],
    )[0]
}

/// The body of a function decoding base64 text with padding into the bytes at `target`:
/// `Malformed`, leaving them empty, unless the text is exactly what encoding writes: whole
/// groups, padding only ending the last, no bits set past the bytes it holds.
fn decode_base64(lo: &mut Lowering, fx: &mut Fx, text: Value, len: Value, target: Value) -> Value {
    let malformed = |fx: &mut Fx| {
        let failed = code(fx, failure::MALFORMED);
        fx.b.ins().return_(&[failed]);
    };
    let ragged = fx.b.ins().band_imm_s(len, 3);
    let ragged = fx.b.ins().icmp_imm_s(IntCC::NotEqual, ragged, 0);
    fx.when(ragged, |fx| malformed(fx));
    let sextet_of = |fx: &mut Fx, byte: Value| {
        let upper = in_range(fx, byte, i64::from(b'A'), i64::from(b'Z'));
        let lower = in_range(fx, byte, i64::from(b'a'), i64::from(b'z'));
        let digit = in_range(fx, byte, i64::from(b'0'), i64::from(b'9'));
        let plus = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'+'));
        let slash = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'/'));
        let mut value = fx.int(types::I64, -1);
        for (flag, base) in [
            (upper, i64::from(b'A')),
            (lower, i64::from(b'a') - 26),
            (digit, i64::from(b'0') - 52),
        ] {
            let decoded = fx.b.ins().iadd_imm_s(byte, -base);
            value = fx.b.ins().select(flag, decoded, value);
        }
        let plus_value = fx.int(types::I64, 62);
        let slash_value = fx.int(types::I64, 63);
        let value = fx.b.ins().select(plus, plus_value, value);
        fx.b.ins().select(slash, slash_value, value)
    };
    let groups = fx.b.ins().ushr_imm_s(len, 2);
    let nonempty = fx.b.ins().icmp_imm_s(IntCC::NotEqual, len, 0);
    let last_padding = fx.choose(
        nonempty,
        &[types::I64],
        |fx| {
            let end = fx.b.ins().iadd(text, len);
            let last_at = fx.b.ins().iadd_imm_s(end, -1);
            let before_at = fx.b.ins().iadd_imm_s(end, -2);
            let last = fx.load(types::I8, last_at, 0);
            let before = fx.load(types::I8, before_at, 0);
            let last = fx.b.ins().icmp_imm_s(IntCC::Equal, last, i64::from(b'='));
            let before = fx.b.ins().icmp_imm_s(IntCC::Equal, before, i64::from(b'='));
            let both = fx.b.ins().band(last, before);
            let count = fx.b.ins().uextend(types::I64, last);
            let extra = fx.b.ins().uextend(types::I64, both);
            vec![fx.b.ins().iadd(count, extra)]
        },
        |fx| vec![fx.ptr(0)],
    )[0];
    let whole = fx.b.ins().imul_imm_s(groups, 3);
    let size = fx.b.ins().isub(whole, last_padding);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, size, 0);
    fx.when(present, |fx| {
        let drawn = vector::resize(lo, fx, target, size, super::BYTE);
        let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, drawn, 0);
        fx.when(failed, |fx| {
            let failed = code(fx, failure::OUT_OF_MEMORY);
            fx.b.ins().return_(&[failed]);
        });
    });
    let data = fx.load(PTR, target, vector::DATA);
    let undo = |lo: &mut Lowering, fx: &mut Fx| {
        vector::free_storage(lo, fx, target);
        let empty = fx.ptr(1);
        fx.store(empty, target, vector::DATA);
        let zero = fx.ptr(0);
        fx.store(zero, target, vector::CAPACITY);
        malformed(fx);
    };
    let zero = fx.ptr(0);
    fx.each(zero, groups, |fx, group| {
        let at = fx.b.ins().ishl_imm_s(group, 2);
        let chars = fx.b.ins().iadd(text, at);
        let final_group = fx.b.ins().iadd_imm_s(group, 1);
        let final_group = fx.b.ins().icmp(IntCC::Equal, final_group, groups);
        let padding = fx.b.ins().select(final_group, last_padding, zero);
        let mut bits = fx.ptr(0);
        for index in 0..4u64 {
            let byte = fx.load(types::I8, chars, index);
            let byte = fx.b.ins().uextend(types::I64, byte);
            let padded =
                fx.b.ins()
                    .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, padding, 4 - index as i64);
            let sextet = sextet_of(fx, byte);
            let invalid = fx.b.ins().icmp_imm_s(IntCC::Equal, sextet, -1);
            let wrong = fx.b.ins().bxor_imm_s(padded, 1);
            let wrong = fx.b.ins().band(wrong, invalid);
            fx.when(wrong, |fx| undo(lo, fx));
            let sextet = fx.b.ins().select(padded, zero, sextet);
            let shifted = fx.b.ins().ishl_imm_s(sextet, 18 - 6 * index as i64);
            bits = fx.b.ins().bor(bits, shifted);
        }
        let one_pad = fx.b.ins().icmp_imm_s(IntCC::Equal, padding, 1);
        let two_pad = fx.b.ins().icmp_imm_s(IntCC::Equal, padding, 2);
        let low_eight = fx.b.ins().band_imm_s(bits, 0xFF);
        let low_sixteen = fx.b.ins().band_imm_s(bits, 0xFFFF);
        let stray_one = fx.b.ins().icmp_imm_s(IntCC::NotEqual, low_eight, 0);
        let stray_one = fx.b.ins().band(one_pad, stray_one);
        let stray_two = fx.b.ins().icmp_imm_s(IntCC::NotEqual, low_sixteen, 0);
        let stray_two = fx.b.ins().band(two_pad, stray_two);
        let stray = fx.b.ins().bor(stray_one, stray_two);
        fx.when(stray, |fx| undo(lo, fx));
        let out = fx.b.ins().imul_imm_s(group, 3);
        for index in 0..3u64 {
            let room =
                fx.b.ins()
                    .icmp_imm_s(IntCC::SignedLessThan, padding, 3 - index as i64);
            fx.when(room, |fx| {
                let byte = fx.b.ins().ushr_imm_s(bits, 16 - 8 * index as i64);
                let byte = fx.b.ins().ireduce(types::I8, byte);
                let at = fx.b.ins().iadd(data, out);
                fx.store(byte, at, index);
            });
        }
    });
    fx.store(size, target, vector::LEN);
    code(fx, 0)
}
