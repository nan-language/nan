//! The reading of construction text, the form derived `Render` writes, for each kind. A reading
//! gives its values and the code of its failure, zero for none; whitespace, element lists and
//! their closing bytes read as in JSON.

use cranelift_codegen::ir::{
    AbiParam, InstBuilder, Signature, Type, Value, condcodes::IntCC, types,
};

use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::Routine;
use super::super::super::runtime::report::decode_utf8;
use super::super::super::{CALL_CONV, Glue, Lowering};
use super::super::render::{encode_utf8, rejected};
use super::json::{element_follows, expect_code, one_char, peek, space};
use super::{bytes_equal, failure, in_range, nest, reader, unnest, utf8_valid};

/// The literals of construction text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Literal {
    Char,
    Text,
    Bytes,
}

impl Literal {
    fn quote(self) -> u8 {
        match self {
            Literal::Char => b'\'',
            Literal::Text | Literal::Bytes => b'"',
        }
    }
}

fn code(fx: &mut Fx, code: i64) -> Value {
    fx.int(types::I32, code)
}

fn succeeded(fx: &mut Fx, failed: Value) -> Value {
    fx.b.ins().icmp_imm_s(IntCC::Equal, failed, 0)
}

/// The byte at an address before `end`, -1 at it.
fn byte_at(fx: &mut Fx, at: Value, end: Value) -> Value {
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
}

fn is_digit(fx: &mut Fx, byte: Value) -> Value {
    in_range(fx, byte, i64::from(b'0'), i64::from(b'9'))
}

/// Whether a byte may begin a `WORD`: a letter or `_`.
fn word_start(fx: &mut Fx, byte: Value) -> Value {
    let lower = in_range(fx, byte, i64::from(b'a'), i64::from(b'z'));
    let upper = in_range(fx, byte, i64::from(b'A'), i64::from(b'Z'));
    let underscore = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'_'));
    let letter = fx.b.ins().bor(lower, upper);
    fx.b.ins().bor(letter, underscore)
}

/// Whether a byte follows after whitespace, passing over it when it does.
fn follows(fx: &mut Fx, state: Value, byte: u8) -> Value {
    space(fx, state);
    let pos = fx.load(PTR, state, reader::POS);
    let end = fx.load(PTR, state, reader::END);
    let next = byte_at(fx, pos, end);
    let found = fx.b.ins().icmp_imm_s(IntCC::Equal, next, i64::from(byte));
    fx.when(found, |fx| {
        let after = fx.b.ins().iadd_imm_s(pos, 1);
        fx.store(after, state, reader::POS);
    });
    found
}

/// A `WORD` after whitespace: at most `capacity` of its bytes written to `buffer` unless it is
/// zero, and its whole length.
fn word(
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
    let function = lo.glue_function(Glue::ConstructionWord, signature, |lo, fx| {
        let params = fx.entry();
        let (state, buffer, capacity) = (params[0], params[1], params[2]);
        let zero = fx.ptr(0);
        let (first, failed) = peek(fx, state);
        let bad = fx.b.ins().icmp_imm_s(IntCC::NotEqual, failed, 0);
        fx.when(bad, |fx| {
            fx.b.ins().return_(&[zero, failed]);
        });
        let starts = word_start(fx, first);
        let other = fx.b.ins().bxor_imm_s(starts, 1);
        fx.when(other, |fx| {
            let malformed = code(fx, failure::MALFORMED);
            fx.b.ins().return_(&[zero, malformed]);
        });
        let start = fx.load(PTR, state, reader::POS);
        let end = fx.load(PTR, state, reader::END);
        let stop = fx.repeat(&[start], |fx, values| {
            let at = values[0];
            let byte = byte_at(fx, at, end);
            let letter = word_start(fx, byte);
            let digit = is_digit(fx, byte);
            let part = fx.b.ins().bor(letter, digit);
            let next = fx.b.ins().iadd_imm_s(at, 1);
            (part, vec![fx.b.ins().select(part, next, at)])
        })[0];
        fx.store(stop, state, reader::POS);
        let len = fx.b.ins().isub(stop, start);
        let writing = fx.b.ins().icmp_imm_s(IntCC::NotEqual, buffer, 0);
        fx.when(writing, |fx| {
            let kept = fx.b.ins().umin(len, capacity);
            lo.call_routine(fx, Routine::Copy, &[buffer, start, kept]);
        });
        let none = code(fx, 0);
        fx.b.ins().return_(&[len, none]);
    });
    let reference = lo.func_ref(fx, function);
    let call = fx.b.ins().call(reference, &[state, buffer, capacity]);
    let results = fx.b.inst_results(call).to_vec();
    (results[0], results[1])
}

/// A bool: `true` or `false`, `Malformed` for any other word.
pub fn read_bool(lo: &mut Lowering, fx: &mut Fx, state: Value) -> (Value, Value) {
    let buffer = fx.slot(8, 1);
    let capacity = fx.ptr(8);
    let (len, failed) = word(lo, fx, state, buffer, capacity);
    let truth = bytes_equal(lo, fx, buffer, len, b"true");
    let falsity = bytes_equal(lo, fx, buffer, len, b"false");
    let either = fx.b.ins().bor(truth, falsity);
    let ok = succeeded(fx, failed);
    let malformed = code(fx, failure::MALFORMED);
    let none = code(fx, 0);
    let checked = fx.b.ins().select(either, none, malformed);
    (truth, fx.b.ins().select(ok, checked, failed))
}

/// The number after whitespace, checked against its grammar: its first byte's address, its
/// end, and whether it has a fraction or an exponent; its digits may hold `_`.
fn number(lo: &mut Lowering, fx: &mut Fx, state: Value) -> (Value, Value, Value, Value) {
    let mut signature = Signature::new(CALL_CONV);
    signature.params.push(AbiParam::new(PTR));
    signature.params.push(AbiParam::new(PTR));
    signature.returns.push(AbiParam::new(types::I32));
    let function = lo.glue_function(Glue::ConstructionNumber, signature, |_, fx| {
        let params = fx.entry();
        let (state, found) = (params[0], params[1]);
        let (_, failed) = peek(fx, state);
        let bad = fx.b.ins().icmp_imm_s(IntCC::NotEqual, failed, 0);
        fx.when(bad, |fx| {
            fx.b.ins().return_(&[failed]);
        });
        let start = fx.load(PTR, state, reader::POS);
        let end = fx.load(PTR, state, reader::END);
        let malformed = |fx: &mut Fx| {
            let code = code(fx, failure::MALFORMED);
            fx.b.ins().return_(&[code]);
        };
        let digits = |fx: &mut Fx, from: Value| {
            let first = byte_at(fx, from, end);
            let digit = is_digit(fx, first);
            let none = fx.b.ins().bxor_imm_s(digit, 1);
            fx.when(none, |fx| malformed(fx));
            fx.repeat(&[from], |fx, values| {
                let at = values[0];
                let byte = byte_at(fx, at, end);
                let digit = is_digit(fx, byte);
                let underscore = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'_'));
                let after = fx.b.ins().iadd_imm_s(at, 1);
                let following = byte_at(fx, after, end);
                let joined = is_digit(fx, following);
                let separated = fx.b.ins().band(underscore, joined);
                let step = fx.b.ins().bor(digit, separated);
                (step, vec![fx.b.ins().select(step, after, at)])
            })[0]
        };
        let first = byte_at(fx, start, end);
        let minus = fx.b.ins().icmp_imm_s(IntCC::Equal, first, i64::from(b'-'));
        let after_sign = fx.b.ins().iadd_imm_s(start, 1);
        let at = fx.b.ins().select(minus, after_sign, start);
        let lead = byte_at(fx, at, end);
        let zero_lead = fx.b.ins().icmp_imm_s(IntCC::Equal, lead, i64::from(b'0'));
        let at = fx.choose(
            zero_lead,
            &[types::I64],
            |fx| vec![fx.b.ins().iadd_imm_s(at, 1)],
            |fx| vec![digits(fx, at)],
        )[0];
        let point_byte = byte_at(fx, at, end);
        let point =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, point_byte, i64::from(b'.'));
        let at = fx.choose(
            point,
            &[types::I64],
            |fx| {
                let from = fx.b.ins().iadd_imm_s(at, 1);
                vec![digits(fx, from)]
            },
            |_| vec![at],
        )[0];
        let exponent_byte = byte_at(fx, at, end);
        let exponent =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, exponent_byte, i64::from(b'e'));
        let at = fx.choose(
            exponent,
            &[types::I64],
            |fx| {
                let from = fx.b.ins().iadd_imm_s(at, 1);
                let sign = byte_at(fx, from, end);
                let negative = fx.b.ins().icmp_imm_s(IntCC::Equal, sign, i64::from(b'-'));
                let after = fx.b.ins().iadd_imm_s(from, 1);
                let from = fx.b.ins().select(negative, after, from);
                let to = fx.repeat(&[from], |fx, values| {
                    let at = values[0];
                    let byte = byte_at(fx, at, end);
                    let digit = is_digit(fx, byte);
                    let next = fx.b.ins().iadd_imm_s(at, 1);
                    (digit, vec![fx.b.ins().select(digit, next, at)])
                })[0];
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
                        let byte = fx.b.ins().uextend(types::I64, byte);
                        let underscore = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'_'));
                        let digit = fx.b.ins().iadd_imm_s(byte, -i64::from(b'0'));
                        let ten = fx.ptr(10);
                        let (scaled, over_mul) = fx.b.ins().umul_overflow(value, ten);
                        let (sum, over_add) = fx.b.ins().uadd_overflow(scaled, digit);
                        let over = fx.b.ins().bor(over_mul, over_add);
                        let over = fx.b.ins().bor(overflowed, over);
                        vec![
                            fx.b.ins().iadd_imm_s(at, 1),
                            fx.b.ins().select(underscore, value, sum),
                            fx.b.ins().select(underscore, overflowed, over),
                        ]
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

/// A float: `NaN`, `inf`, `-inf`, or a number rounded to f64, as a value of `float`, the type
/// holding an f64.
pub fn read_float(lo: &mut Lowering, fx: &mut Fx, state: Value, float: Type) -> (Value, Value) {
    let mut signature = Signature::new(CALL_CONV);
    signature.params.push(AbiParam::new(PTR));
    signature.params.push(AbiParam::new(PTR));
    signature.returns.push(AbiParam::new(types::I32));
    let function = lo.glue_function(Glue::ConstructionFloat, signature, |lo, fx| {
        let params = fx.entry();
        let (state, out) = (params[0], params[1]);
        let stop = |fx: &mut Fx, failed: Value| {
            let bad = fx.b.ins().icmp_imm_s(IntCC::NotEqual, failed, 0);
            fx.when(bad, |fx| {
                fx.b.ins().return_(&[failed]);
            });
        };
        let (first, failed) = peek(fx, state);
        stop(fx, failed);
        let pos = fx.load(PTR, state, reader::POS);
        let end = fx.load(PTR, state, reader::END);
        let after = fx.b.ins().iadd_imm_s(pos, 1);
        let second = byte_at(fx, after, end);
        let minus = fx.b.ins().icmp_imm_s(IntCC::Equal, first, i64::from(b'-'));
        let letter = fx.b.ins().icmp_imm_s(IntCC::Equal, second, i64::from(b'i'));
        let negative = fx.b.ins().band(minus, letter);
        fx.when(negative, |fx| fx.store(after, state, reader::POS));
        let (first, _) = peek(fx, state);
        let starts = word_start(fx, first);
        let named = fx.b.ins().bor(negative, starts);
        fx.when(named, |fx| {
            let buffer = fx.slot(8, 1);
            let capacity = fx.ptr(8);
            let (len, failed) = word(lo, fx, state, buffer, capacity);
            stop(fx, failed);
            let mut bits = fx.int(types::I64, 0);
            let mut known = fx.int(types::I8, 0);
            for (spelling, below, float) in [
                (b"NaN".as_slice(), false, f64::NAN),
                (b"inf", false, f64::INFINITY),
                (b"inf", true, f64::NEG_INFINITY),
            ] {
                let same = bytes_equal(lo, fx, buffer, len, spelling);
                let sign = match below {
                    true => negative,
                    false => fx.b.ins().bxor_imm_s(negative, 1),
                };
                let same = fx.b.ins().band(same, sign);
                let constant = fx.int(types::I64, float.to_bits() as i64);
                bits = fx.b.ins().select(same, constant, bits);
                known = fx.b.ins().bor(known, same);
            }
            fx.store(bits, out, 0);
            let malformed = code(fx, failure::MALFORMED);
            let none = code(fx, 0);
            let checked = fx.b.ins().select(known, none, malformed);
            fx.b.ins().return_(&[checked]);
        });
        let (start, end, _, failed) = number(lo, fx, state);
        stop(fx, failed);
        let len = fx.b.ins().isub(end, start);
        let bits = super::super::floats::parse(lo, fx, start, len);
        fx.store(bits, out, 0);
        let none = code(fx, 0);
        fx.b.ins().return_(&[none]);
    });
    let out = fx.slot(8, 8);
    let reference = lo.func_ref(fx, function);
    let call = fx.b.ins().call(reference, &[state, out]);
    let failed = fx.b.inst_results(call)[0];
    (fx.load(float, out, 0), failed)
}

/// A literal after whitespace, its escapes resolved: at most `capacity` of its bytes written
/// to `buffer` unless it is zero, and its whole length.
fn literal(
    lo: &mut Lowering,
    fx: &mut Fx,
    state: Value,
    kind: Literal,
    (buffer, capacity): (Value, Value),
) -> (Value, Value) {
    let mut signature = Signature::new(CALL_CONV);
    for _ in 0..3 {
        signature.params.push(AbiParam::new(PTR));
    }
    signature.returns.push(AbiParam::new(types::I64));
    signature.returns.push(AbiParam::new(types::I32));
    let function = lo.glue_function(Glue::ConstructionLiteral(kind), signature, |lo, fx| {
        let params = fx.entry();
        let (state, buffer, capacity) = (params[0], params[1], params[2]);
        let end = fx.load(PTR, state, reader::END);
        let zero = fx.ptr(0);
        let give_up = |fx: &mut Fx, failure: i64| {
            let code = code(fx, failure);
            fx.b.ins().return_(&[zero, code]);
        };
        let opening = match kind {
            Literal::Bytes => b'b',
            _ => kind.quote(),
        };
        let opened = expect_code(lo, fx, state, opening);
        let failed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, opened, 0);
        fx.when(failed, |fx| {
            fx.b.ins().return_(&[zero, opened]);
        });
        if kind == Literal::Bytes {
            let pos = fx.load(PTR, state, reader::POS);
            let quote = byte_at(fx, pos, end);
            let other =
                fx.b.ins()
                    .icmp_imm_s(IntCC::NotEqual, quote, i64::from(b'"'));
            fx.when(other, |fx| give_up(fx, failure::MALFORMED));
            let after = fx.b.ins().iadd_imm_s(pos, 1);
            fx.store(after, state, reader::POS);
        }
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
        let next_byte = |fx: &mut Fx| {
            let pos = fx.load(PTR, state, reader::POS);
            let at_end = fx.b.ins().icmp(IntCC::Equal, pos, end);
            fx.when(at_end, |fx| give_up(fx, failure::TRUNCATED));
            let byte = fx.load(types::I8, pos, 0);
            let after = fx.b.ins().iadd_imm_s(pos, 1);
            fx.store(after, state, reader::POS);
            (pos, fx.b.ins().uextend(types::I64, byte))
        };
        let hex = |fx: &mut Fx, least: usize, most: usize| {
            let mut value = fx.ptr(0);
            let mut done = fx.int(types::I8, 0);
            for index in 0..most {
                let active = fx.b.ins().bxor_imm_s(done, 1);
                let parts = fx.choose(
                    active,
                    &[types::I64, types::I8],
                    |fx| {
                        let pos = fx.load(PTR, state, reader::POS);
                        let at_end = fx.b.ins().icmp(IntCC::Equal, pos, end);
                        fx.when(at_end, |fx| give_up(fx, failure::TRUNCATED));
                        let byte = fx.load(types::I8, pos, 0);
                        let byte = fx.b.ins().uextend(types::I64, byte);
                        let digit = in_range(fx, byte, i64::from(b'0'), i64::from(b'9'));
                        let lower = in_range(fx, byte, i64::from(b'a'), i64::from(b'f'));
                        let upper = in_range(fx, byte, i64::from(b'A'), i64::from(b'F'));
                        let letter = fx.b.ins().bor(lower, upper);
                        let is_hex = fx.b.ins().bor(digit, letter);
                        if index < least {
                            let other = fx.b.ins().bxor_imm_s(is_hex, 1);
                            fx.when(other, |fx| give_up(fx, failure::MALFORMED));
                        }
                        let from_digit = fx.b.ins().iadd_imm_s(byte, -i64::from(b'0'));
                        let from_lower = fx.b.ins().iadd_imm_s(byte, 10 - i64::from(b'a'));
                        let from_upper = fx.b.ins().iadd_imm_s(byte, 10 - i64::from(b'A'));
                        let nibble = fx.b.ins().select(lower, from_lower, from_digit);
                        let nibble = fx.b.ins().select(upper, from_upper, nibble);
                        let shifted = fx.b.ins().ishl_imm_s(value, 4);
                        let joined = fx.b.ins().bor(shifted, nibble);
                        fx.when(is_hex, |fx| {
                            let after = fx.b.ins().iadd_imm_s(pos, 1);
                            fx.store(after, state, reader::POS);
                        });
                        let other = fx.b.ins().bxor_imm_s(is_hex, 1);
                        vec![fx.b.ins().select(is_hex, joined, value), other]
                    },
                    |_| vec![value, done],
                );
                value = parts[0];
                done = parts[1];
            }
            value
        };
        let scratch = fx.slot(4, 4);
        fx.repeat(&[zero], |fx, values| {
            let len = values[0];
            let (pos, byte) = next_byte(fx);
            let closed =
                fx.b.ins()
                    .icmp_imm_s(IntCC::Equal, byte, i64::from(kind.quote()));
            fx.when(closed, |fx| {
                let none = code(fx, 0);
                fx.b.ins().return_(&[len, none]);
            });
            let escape = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'\\'));
            let len = fx.choose_in(
                lo,
                escape,
                &[types::I64],
                |lo, fx| {
                    let (_, letter) = next_byte(fx);
                    let mut scalar = fx.int(types::I64, -1);
                    for (name, meaning) in [
                        (b'\\', b'\\'),
                        (b'"', b'"'),
                        (b'\'', b'\''),
                        (b'n', b'\n'),
                        (b'r', b'\r'),
                        (b't', b'\t'),
                        (b'0', 0),
                    ] {
                        let this = fx.b.ins().icmp_imm_s(IntCC::Equal, letter, i64::from(name));
                        let meaning = fx.int(types::I64, i64::from(meaning));
                        scalar = fx.b.ins().select(this, meaning, scalar);
                    }
                    let byte_escape = fx.b.ins().icmp_imm_s(IntCC::Equal, letter, i64::from(b'x'));
                    let mut scalar = fx.choose(
                        byte_escape,
                        &[types::I64],
                        |fx| {
                            let value = hex(fx, 2, 2);
                            if kind != Literal::Bytes {
                                let wide =
                                    fx.b.ins()
                                        .icmp_imm_s(IntCC::UnsignedGreaterThan, value, 0x7F);
                                fx.when(wide, |fx| give_up(fx, failure::MALFORMED));
                            }
                            vec![value]
                        },
                        |_| vec![scalar],
                    )[0];
                    if kind != Literal::Bytes {
                        let unicode = fx.b.ins().icmp_imm_s(IntCC::Equal, letter, i64::from(b'u'));
                        scalar = fx.choose(
                            unicode,
                            &[types::I64],
                            |fx| {
                                let (_, open) = next_byte(fx);
                                let other =
                                    fx.b.ins()
                                        .icmp_imm_s(IntCC::NotEqual, open, i64::from(b'{'));
                                fx.when(other, |fx| give_up(fx, failure::MALFORMED));
                                let value = hex(fx, 1, 6);
                                let (_, close) = next_byte(fx);
                                let other =
                                    fx.b.ins()
                                        .icmp_imm_s(IntCC::NotEqual, close, i64::from(b'}'));
                                fx.when(other, |fx| give_up(fx, failure::MALFORMED));
                                vec![value]
                            },
                            |_| vec![scalar],
                        )[0];
                    }
                    let unknown = fx.b.ins().icmp_imm_s(IntCC::Equal, scalar, -1);
                    fx.when(unknown, |fx| give_up(fx, failure::MALFORMED));
                    let count = match kind {
                        Literal::Bytes => {
                            let narrow = fx.b.ins().ireduce(types::I8, scalar);
                            fx.store(narrow, scratch, 0);
                            fx.ptr(1)
                        }
                        _ => {
                            let surrogate = in_range(fx, scalar, 0xD800, 0xDFFF);
                            let beyond = fx.b.ins().icmp_imm_s(
                                IntCC::UnsignedGreaterThan,
                                scalar,
                                0x10_FFFF,
                            );
                            let invalid = fx.b.ins().bor(surrogate, beyond);
                            fx.when(invalid, |fx| give_up(fx, failure::INVALID_TEXT));
                            encode_utf8(fx, scalar, scratch)
                        }
                    };
                    vec![emit(lo, fx, len, scratch, count)]
                },
                |lo, fx| {
                    let count = match kind {
                        Literal::Bytes => {
                            let printable = in_range(fx, byte, 0x20, 0x7E);
                            let other = fx.b.ins().bxor_imm_s(printable, 1);
                            fx.when(other, |fx| give_up(fx, failure::MALFORMED));
                            fx.ptr(1)
                        }
                        _ => {
                            let ascii = fx.b.ins().icmp_imm_s(IntCC::UnsignedLessThan, byte, 0x80);
                            let two = in_range(fx, byte, 0xC2, 0xDF);
                            let three = in_range(fx, byte, 0xE0, 0xEF);
                            let four = in_range(fx, byte, 0xF0, 0xF4);
                            let known = fx.b.ins().bor(ascii, two);
                            let known = fx.b.ins().bor(known, three);
                            let known = fx.b.ins().bor(known, four);
                            let unknown = fx.b.ins().bxor_imm_s(known, 1);
                            fx.when(unknown, |fx| give_up(fx, failure::INVALID_TEXT));
                            let mut width = fx.ptr(1);
                            for flag in [two, three, four] {
                                let step = fx.b.ins().uextend(types::I64, flag);
                                width = fx.b.ins().iadd(width, step);
                            }
                            let stop = fx.b.ins().iadd(pos, width);
                            let past = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, stop, end);
                            fx.when(past, |fx| give_up(fx, failure::TRUNCATED));
                            let valid = utf8_valid(lo, fx, pos, width);
                            let invalid = fx.b.ins().bxor_imm_s(valid, 1);
                            fx.when(invalid, |fx| give_up(fx, failure::INVALID_TEXT));
                            let (scalar, _) = decode_utf8(fx, pos, byte);
                            let control =
                                fx.b.ins().icmp_imm_s(IntCC::UnsignedLessThan, scalar, 0x20);
                            let outside = rejected(lo, fx, scalar);
                            let outside = fx.b.ins().bor(control, outside);
                            fx.when(outside, |fx| give_up(fx, failure::MALFORMED));
                            fx.store(stop, state, reader::POS);
                            width
                        }
                    };
                    vec![emit(lo, fx, len, pos, count)]
                },
            )[0];
            (fx.int(types::I8, 1), vec![len])
        });
        let truncated = code(fx, failure::TRUNCATED);
        fx.b.ins().return_(&[zero, truncated]);
    });
    let reference = lo.func_ref(fx, function);
    let call = fx.b.ins().call(reference, &[state, buffer, capacity]);
    let results = fx.b.inst_results(call).to_vec();
    (results[0], results[1])
}

/// The length of the literal that follows, its escapes resolved, the reader staying put.
pub fn measure_literal(
    lo: &mut Lowering,
    fx: &mut Fx,
    state: Value,
    kind: Literal,
) -> (Value, Value) {
    let pos = fx.load(PTR, state, reader::POS);
    let none = fx.ptr(0);
    let (len, failed) = literal(lo, fx, state, kind, (none, none));
    let ok = succeeded(fx, failed);
    fx.when(ok, |fx| fx.store(pos, state, reader::POS));
    (len, failed)
}

/// Writes the literal that follows, measured before as `len` bytes, to `destination`.
pub fn write_literal(
    lo: &mut Lowering,
    fx: &mut Fx,
    state: Value,
    kind: Literal,
    destination: Value,
    len: Value,
) {
    literal(lo, fx, state, kind, (destination, len));
}

/// A char: a char literal of exactly one char, its scalar value in 64 bits.
pub fn read_char(lo: &mut Lowering, fx: &mut Fx, state: Value) -> (Value, Value) {
    let buffer = fx.slot(8, 1);
    let capacity = fx.ptr(8);
    let (len, failed) = literal(lo, fx, state, Literal::Char, (buffer, capacity));
    one_char(fx, buffer, len, failed)
}

/// Opens a record: its name, then whether `(` follows.
pub fn open_record(lo: &mut Lowering, fx: &mut Fx, state: Value) -> (Value, Value) {
    let none = fx.ptr(0);
    let (_, failed) = word(lo, fx, state, none, none);
    let ok = succeeded(fx, failed);
    let parens = fx.choose(
        ok,
        &[types::I8],
        |fx| vec![follows(fx, state, b'(')],
        |fx| vec![fx.int(types::I8, 0)],
    )[0];
    (parens, failed)
}

/// A record field's key, at most `capacity` bytes of it written to `buffer`, then its colon.
pub fn read_key(
    lo: &mut Lowering,
    fx: &mut Fx,
    state: Value,
    buffer: Value,
    capacity: Value,
) -> (Value, Value) {
    let (len, failed) = word(lo, fx, state, buffer, capacity);
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

/// A variant's key after its optional name and `.`, at most `capacity` bytes of it written to
/// `buffer`, and whether a payload follows: whether `(` comes next.
pub fn read_variant(
    lo: &mut Lowering,
    fx: &mut Fx,
    state: Value,
    buffer: Value,
    capacity: Value,
) -> (Value, Value, Value) {
    let (len, failed) = word(lo, fx, state, buffer, capacity);
    let ok = succeeded(fx, failed);
    let parts = fx.choose_in(
        lo,
        ok,
        &[types::I64, types::I8, types::I32],
        |lo, fx| {
            let named = follows(fx, state, b'.');
            let key = fx.choose_in(
                lo,
                named,
                &[types::I64, types::I32],
                |lo, fx| {
                    let (len, failed) = word(lo, fx, state, buffer, capacity);
                    vec![len, failed]
                },
                |_, fx| vec![len, code(fx, 0)],
            );
            let (len, failed) = (key[0], key[1]);
            let ok = succeeded(fx, failed);
            let payload = fx.choose(
                ok,
                &[types::I8],
                |fx| vec![follows(fx, state, b'(')],
                |fx| vec![fx.int(types::I8, 0)],
            )[0];
            vec![len, payload, failed]
        },
        |_, fx| vec![len, fx.int(types::I8, 0), failed],
    );
    (parts[0], parts[1], parts[2])
}

/// Passes over any value, nested no deeper than decoding reads.
pub fn skip_value(lo: &mut Lowering, fx: &mut Fx, state: Value) -> Value {
    let mut signature = Signature::new(CALL_CONV);
    signature.params.push(AbiParam::new(PTR));
    signature.returns.push(AbiParam::new(types::I32));
    let function = lo.glue_function(Glue::ConstructionSkip, signature, |lo, fx| {
        let state = fx.entry()[0];
        let stop = |fx: &mut Fx, failed: Value| {
            let bad = fx.b.ins().icmp_imm_s(IntCC::NotEqual, failed, 0);
            fx.when(bad, |fx| {
                fx.b.ins().return_(&[failed]);
            });
        };
        let (byte, failed) = peek(fx, state);
        stop(fx, failed);
        let none = code(fx, 0);
        let pos = fx.load(PTR, state, reader::POS);
        let end = fx.load(PTR, state, reader::END);
        let after = fx.b.ins().iadd_imm_s(pos, 1);
        let second = byte_at(fx, after, end);
        let text = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'"'));
        fx.when(text, |fx| {
            let zero = fx.ptr(0);
            let (_, failed) = literal(lo, fx, state, Literal::Text, (zero, zero));
            fx.b.ins().return_(&[failed]);
        });
        let char_quote = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'\''));
        fx.when(char_quote, |fx| {
            let (_, failed) = read_char(lo, fx, state);
            fx.b.ins().return_(&[failed]);
        });
        let b = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'b'));
        let quoted = fx.b.ins().icmp_imm_s(IntCC::Equal, second, i64::from(b'"'));
        let bytes = fx.b.ins().band(b, quoted);
        fx.when(bytes, |fx| {
            let zero = fx.ptr(0);
            let (_, failed) = literal(lo, fx, state, Literal::Bytes, (zero, zero));
            fx.b.ins().return_(&[failed]);
        });
        for (open, close) in [(b'[', b']'), (b'{', b'}')] {
            let this = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(open));
            fx.when(this, |fx| {
                nest(fx, state);
                fx.store(after, state, reader::POS);
                let zero = fx.ptr(0);
                fx.repeat(&[zero], |fx, values| {
                    let index = values[0];
                    let (follows, failed) = element_follows(lo, fx, state, index, close);
                    stop(fx, failed);
                    fx.when(follows, |fx| {
                        let failed = skip_value(lo, fx, state);
                        stop(fx, failed);
                        if open == b'{' {
                            let failed = expect_code(lo, fx, state, b':');
                            stop(fx, failed);
                            let failed = skip_value(lo, fx, state);
                            stop(fx, failed);
                        }
                    });
                    let next = fx.b.ins().iadd_imm_s(index, 1);
                    (follows, vec![next])
                });
                unnest(fx, state);
                fx.b.ins().return_(&[none]);
            });
        }
        let starts = word_start(fx, byte);
        fx.when(starts, |fx| {
            let zero = fx.ptr(0);
            let (_, failed) = word(lo, fx, state, zero, zero);
            stop(fx, failed);
            let named = follows(fx, state, b'.');
            fx.when(named, |fx| {
                let (_, failed) = word(lo, fx, state, zero, zero);
                stop(fx, failed);
            });
            let opened = follows(fx, state, b'(');
            let bare = fx.b.ins().bxor_imm_s(opened, 1);
            fx.when(bare, |fx| {
                fx.b.ins().return_(&[none]);
            });
            nest(fx, state);
            fx.repeat(&[zero], |fx, values| {
                let index = values[0];
                let (follows_item, failed) = element_follows(lo, fx, state, index, b')');
                stop(fx, failed);
                fx.when(follows_item, |fx| {
                    let at = fx.load(PTR, state, reader::POS);
                    let (_, failed) = word(lo, fx, state, zero, zero);
                    let worded = succeeded(fx, failed);
                    let labelled = fx.choose(
                        worded,
                        &[types::I8],
                        |fx| vec![follows(fx, state, b':')],
                        |fx| vec![fx.int(types::I8, 0)],
                    )[0];
                    let unlabelled = fx.b.ins().bxor_imm_s(labelled, 1);
                    fx.when(unlabelled, |fx| fx.store(at, state, reader::POS));
                    let failed = skip_value(lo, fx, state);
                    stop(fx, failed);
                });
                let next = fx.b.ins().iadd_imm_s(index, 1);
                (follows_item, vec![next])
            });
            unnest(fx, state);
            fx.b.ins().return_(&[none]);
        });
        let minus = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'-'));
        let letter = fx.b.ins().icmp_imm_s(IntCC::Equal, second, i64::from(b'i'));
        let infinite = fx.b.ins().band(minus, letter);
        fx.when(infinite, |fx| {
            fx.store(after, state, reader::POS);
            let buffer = fx.slot(8, 1);
            let capacity = fx.ptr(8);
            let (len, failed) = word(lo, fx, state, buffer, capacity);
            stop(fx, failed);
            let same = bytes_equal(lo, fx, buffer, len, b"inf");
            let malformed = code(fx, failure::MALFORMED);
            let checked = fx.b.ins().select(same, none, malformed);
            fx.b.ins().return_(&[checked]);
        });
        let (_, _, _, failed) = number(lo, fx, state);
        fx.b.ins().return_(&[failed]);
    });
    let reference = lo.func_ref(fx, function);
    let call = fx.b.ins().call(reference, &[state]);
    fx.b.inst_results(call)[0]
}
