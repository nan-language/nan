//! The `mem::wire` format's writing and reading of each kind. A reading gives its values and the
//! code of its failure, zero for none.

use cranelift_codegen::ir::{
    AbiParam, InstBuilder, MemFlagsData, Signature, Type, Value, condcodes::IntCC, types,
};

use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::Routine;
use super::super::super::{CALL_CONV, Glue, Lowering};
use super::super::mem::vector;
use super::{failure, put_byte, put_bytes, reader};
use crate::check::ty::Ty;

/// Writes a uvar: 7 bits per byte, least significant group first.
pub fn put_uvar(fx: &mut Fx, state: Value, value: Value) {
    fx.repeat(&[value], |fx, values| {
        let rest = values[0];
        let group = fx.b.ins().band_imm_s(rest, 0x7F);
        let later = fx.b.ins().ushr_imm_s(rest, 7);
        let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, later, 0);
        let flagged = fx.b.ins().bor_imm_s(group, 0x80);
        let byte = fx.b.ins().select(more, flagged, group);
        put_byte(fx, state, byte);
        (more, vec![later])
    });
}

/// Writes text or bytes: the byte length as a uvar, then the bytes.
pub fn put_text(lo: &mut Lowering, fx: &mut Fx, state: Value, start: Value, len: Value) {
    put_uvar(fx, state, len);
    put_bytes(lo, fx, state, start, len);
}

/// Writes a literal text: its byte length as a uvar, then its bytes.
pub fn put_literal_text(lo: &mut Lowering, fx: &mut Fx, state: Value, bytes: &[u8]) {
    let len = fx.ptr(bytes.len() as u64);
    put_uvar(fx, state, len);
    super::put_literal(lo, fx, state, bytes);
}

/// The bytes left in the input.
fn left(fx: &mut Fx, state: Value) -> Value {
    let pos = fx.load(PTR, state, reader::POS);
    let end = fx.load(PTR, state, reader::END);
    fx.b.ins().isub(end, pos)
}

fn code(fx: &mut Fx, code: i64) -> Value {
    fx.int(types::I32, code)
}

/// The next byte: `Truncated` at the end of the input.
pub fn read_byte(fx: &mut Fx, state: Value) -> (Value, Value) {
    let pos = fx.load(PTR, state, reader::POS);
    let end = fx.load(PTR, state, reader::END);
    let present = fx.b.ins().icmp(IntCC::UnsignedLessThan, pos, end);
    let parts = fx.choose(
        present,
        &[types::I64, types::I32],
        |fx| {
            let byte = fx.load(types::I8, pos, 0);
            let byte = fx.b.ins().uextend(types::I64, byte);
            let next = fx.b.ins().iadd_imm_s(pos, 1);
            fx.store(next, state, reader::POS);
            vec![byte, code(fx, 0)]
        },
        |fx| vec![fx.ptr(0), code(fx, failure::TRUNCATED)],
    );
    (parts[0], parts[1])
}

/// A uvar: `Malformed` when it is not minimal or exceeds 2^64 − 1.
pub fn read_uvar(lo: &mut Lowering, fx: &mut Fx, state: Value) -> (Value, Value) {
    let mut signature = Signature::new(CALL_CONV);
    signature.params.push(AbiParam::new(PTR));
    signature.returns.push(AbiParam::new(types::I64));
    signature.returns.push(AbiParam::new(types::I32));
    let function = lo.glue_function(Glue::WireUvar, signature, |_, fx| {
        let params = fx.entry();
        let state = params[0];
        let zero = fx.ptr(0);
        fx.repeat(&[zero, zero], |fx, values| {
            let (index, value) = (values[0], values[1]);
            let (byte, failed) = read_byte(fx, state);
            let truncated = fx.b.ins().icmp_imm_s(IntCC::NotEqual, failed, 0);
            fx.when(truncated, |fx| {
                fx.b.ins().return_(&[value, failed]);
            });
            let last_group = fx.b.ins().icmp_imm_s(IntCC::Equal, index, 9);
            let too_large = fx.b.ins().icmp_imm_s(IntCC::UnsignedGreaterThan, byte, 1);
            let overflow = fx.b.ins().band(last_group, too_large);
            fx.when(overflow, |fx| {
                let malformed = code(fx, failure::MALFORMED);
                fx.b.ins().return_(&[value, malformed]);
            });
            let group = fx.b.ins().band_imm_s(byte, 0x7F);
            let shift = fx.b.ins().imul_imm_s(index, 7);
            let shifted = fx.b.ins().ishl(group, shift);
            let value = fx.b.ins().bor(value, shifted);
            let continued = fx.b.ins().band_imm_s(byte, 0x80);
            let last = fx.b.ins().icmp_imm_s(IntCC::Equal, continued, 0);
            fx.when(last, |fx| {
                let empty = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, 0);
                let later = fx.b.ins().icmp_imm_s(IntCC::NotEqual, index, 0);
                let padded = fx.b.ins().band(empty, later);
                let malformed = code(fx, failure::MALFORMED);
                let ok = code(fx, 0);
                let result = fx.b.ins().select(padded, malformed, ok);
                fx.b.ins().return_(&[value, result]);
            });
            let next = fx.b.ins().iadd_imm_s(index, 1);
            let more = fx.b.ins().icmp_imm_s(IntCC::UnsignedLessThan, next, 10);
            (more, vec![next, value])
        });
        let zero = fx.ptr(0);
        let malformed = code(fx, failure::MALFORMED);
        fx.b.ins().return_(&[zero, malformed]);
    });
    let reference = lo.func_ref(fx, function);
    let call = fx.b.ins().call(reference, &[state]);
    let results = fx.b.inst_results(call).to_vec();
    (results[0], results[1])
}

/// A count of elements, fields or entries: `Truncated` when it exceeds the bytes left, each
/// taking at least one.
pub fn read_count(lo: &mut Lowering, fx: &mut Fx, state: Value) -> (Value, Value) {
    let (count, failed) = read_uvar(lo, fx, state);
    let left = left(fx, state);
    let beyond = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, count, left);
    let truncated = code(fx, failure::TRUNCATED);
    let ok = fx.b.ins().icmp_imm_s(IntCC::Equal, failed, 0);
    let truncated = fx.b.ins().select(beyond, truncated, failed);
    let failed = fx.b.ins().select(ok, truncated, failed);
    (count, failed)
}

/// The end of a value of a byte length read here: `Truncated` when it lies past the input.
pub fn read_length_end(lo: &mut Lowering, fx: &mut Fx, state: Value) -> (Value, Value) {
    let (len, failed) = read_uvar(lo, fx, state);
    let left = left(fx, state);
    let beyond = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, len, left);
    let truncated = code(fx, failure::TRUNCATED);
    let ok = fx.b.ins().icmp_imm_s(IntCC::Equal, failed, 0);
    let truncated = fx.b.ins().select(beyond, truncated, failed);
    let failed = fx.b.ins().select(ok, truncated, failed);
    let pos = fx.load(PTR, state, reader::POS);
    (fx.b.ins().iadd(pos, len), failed)
}

/// Text or bytes: their first byte's address and length, the input passing over them.
pub fn read_framed(lo: &mut Lowering, fx: &mut Fx, state: Value) -> (Value, Value, Value) {
    let (end, failed) = read_length_end(lo, fx, state);
    let ok = fx.b.ins().icmp_imm_s(IntCC::Equal, failed, 0);
    let pos = fx.load(PTR, state, reader::POS);
    let len = fx.b.ins().isub(end, pos);
    fx.when(ok, |fx| fx.store(end, state, reader::POS));
    (pos, len, failed)
}

/// A record field's key and the end of its value: `DuplicateKey` for a key equal to the last
/// one, kept at `last` as its address and length (-1 before the first), and `Malformed` for
/// a key less than it.
pub fn read_key(
    lo: &mut Lowering,
    fx: &mut Fx,
    state: Value,
    last: Value,
) -> (Value, Value, Value, Value) {
    let (start, len, failed) = read_framed(lo, fx, state);
    let ok = fx.b.ins().icmp_imm_s(IntCC::Equal, failed, 0);
    let parts = fx.choose_in(
        lo,
        ok,
        &[types::I64, types::I32],
        |lo, fx| {
            let (end, failed) = read_length_end(lo, fx, state);
            let ok = fx.b.ins().icmp_imm_s(IntCC::Equal, failed, 0);
            let order = fx.choose_in(
                lo,
                ok,
                &[types::I32],
                |lo, fx| vec![compare_last(lo, fx, last, start, len)],
                |_, _| vec![failed],
            )[0];
            vec![end, order]
        },
        |_, fx| vec![fx.ptr(0), failed],
    );
    (start, len, parts[0], parts[1])
}

/// Compares a key with the last one kept, failing on an equal or lesser key, and keeps it.
fn compare_last(lo: &mut Lowering, fx: &mut Fx, last: Value, start: Value, len: Value) -> Value {
    let last_start = fx.load(PTR, last, 0);
    let last_len = fx.load(PTR, last, 8);
    fx.store(start, last, 0);
    fx.store(len, last, 8);
    let first = fx.b.ins().icmp_imm_s(IntCC::Equal, last_len, -1);
    fx.choose_in(
        lo,
        first,
        &[types::I32],
        |_, fx| vec![code(fx, 0)],
        |lo, fx| {
            let shorter = fx.b.ins().umin(len, last_len);
            let order = lo.call_routine(fx, Routine::Compare, &[start, last_start, shorter])[0];
            let less = fx.b.ins().icmp_imm_s(IntCC::SignedLessThan, order, 0);
            let tie = fx.b.ins().icmp_imm_s(IntCC::Equal, order, 0);
            let shorter_key = fx.b.ins().icmp(IntCC::UnsignedLessThan, len, last_len);
            let same_len = fx.b.ins().icmp(IntCC::Equal, len, last_len);
            let less_by_len = fx.b.ins().band(tie, shorter_key);
            let less = fx.b.ins().bor(less, less_by_len);
            let equal = fx.b.ins().band(tie, same_len);
            let malformed = code(fx, failure::MALFORMED);
            let duplicate = code(fx, failure::DUPLICATE_KEY);
            let ok = code(fx, 0);
            let result = fx.b.ins().select(less, malformed, ok);
            vec![fx.b.ins().select(equal, duplicate, result)]
        },
    )[0]
}

/// Whether a variant's payload follows, and its end when it does: `Malformed` on a byte other
/// than 0 and 1.
pub fn read_payload_flag(lo: &mut Lowering, fx: &mut Fx, state: Value) -> (Value, Value, Value) {
    let (byte, failed) = read_byte(fx, state);
    let ok = fx.b.ins().icmp_imm_s(IntCC::Equal, failed, 0);
    let parts = fx.choose_in(
        lo,
        ok,
        &[types::I8, types::I64, types::I32],
        |lo, fx| {
            let absent = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, 0);
            let present = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, 1);
            fx.choose_in(
                lo,
                present,
                &[types::I8, types::I64, types::I32],
                |lo, fx| {
                    let (end, failed) = read_length_end(lo, fx, state);
                    vec![fx.int(types::I8, 1), end, failed]
                },
                |_, fx| {
                    let malformed = code(fx, failure::MALFORMED);
                    let ok = code(fx, 0);
                    let result = fx.b.ins().select(absent, ok, malformed);
                    vec![fx.int(types::I8, 0), fx.ptr(0), result]
                },
            )
        },
        |_, fx| vec![fx.int(types::I8, 0), fx.ptr(0), failed],
    );
    (parts[0], parts[1], parts[2])
}

/// A cursor over the kept fields of a flexible struct, the value of type `kept` at an address:
/// a reader over the bytes they are written as, empty for none.
pub fn kept_cursor(lo: &mut Lowering, fx: &mut Fx, kept: &Ty, address: Value) -> Value {
    let cursor = fx.slot(reader::SIZE, 8);
    let zero = fx.ptr(0);
    fx.store(zero, cursor, reader::POS);
    fx.store(zero, cursor, reader::END);
    let held = lo.variant_is(fx, address, kept, "Fields");
    let bytes = lo.payload_address(fx, kept, "Fields", "", address);
    fx.when(held, |fx| {
        let data = fx.load(PTR, bytes, vector::DATA);
        let len = fx.load(PTR, bytes, vector::LEN);
        let end = fx.b.ins().iadd(data, len);
        fx.store(data, cursor, reader::POS);
        fx.store(end, cursor, reader::END);
    });
    cursor
}

/// The key of the kept field at a cursor: its first byte's address and its length, the
/// cursor passing over the field.
fn kept_field(lo: &mut Lowering, fx: &mut Fx, cursor: Value) -> (Value, Value) {
    let (key_len, _) = read_uvar(lo, fx, cursor);
    let key = fx.load(PTR, cursor, reader::POS);
    let after = fx.b.ins().iadd(key, key_len);
    fx.store(after, cursor, reader::POS);
    let (value_len, _) = read_uvar(lo, fx, cursor);
    let value = fx.load(PTR, cursor, reader::POS);
    let end = fx.b.ins().iadd(value, value_len);
    fx.store(end, cursor, reader::POS);
    (key, key_len)
}

/// The number of kept fields a cursor has left.
pub fn kept_count(lo: &mut Lowering, fx: &mut Fx, cursor: Value) -> Value {
    let walk = fx.slot(reader::SIZE, 8);
    let pos = fx.load(PTR, cursor, reader::POS);
    let end = fx.load(PTR, cursor, reader::END);
    fx.store(pos, walk, reader::POS);
    fx.store(end, walk, reader::END);
    let zero = fx.ptr(0);
    fx.repeat(&[zero], |fx, state| {
        let count = state[0];
        let pos = fx.load(PTR, walk, reader::POS);
        let more = fx.b.ins().icmp(IntCC::UnsignedLessThan, pos, end);
        let next = fx.choose_in(
            lo,
            more,
            &[types::I64],
            |lo, fx| {
                kept_field(lo, fx, walk);
                vec![fx.b.ins().iadd_imm_s(count, 1)]
            },
            |_, _| vec![count],
        )[0];
        (more, vec![next])
    })[0]
}

/// Writes the kept fields a cursor has left whose keys precede `before` in byte order, or all
/// of them, the cursor passing over each written.
pub fn put_kept(
    lo: &mut Lowering,
    fx: &mut Fx,
    state: Value,
    cursor: Value,
    before: Option<&[u8]>,
) {
    let zero = fx.ptr(0);
    fx.repeat(&[zero], |fx, unchanged| {
        let pos = fx.load(PTR, cursor, reader::POS);
        let end = fx.load(PTR, cursor, reader::END);
        let more = fx.b.ins().icmp(IntCC::UnsignedLessThan, pos, end);
        let written = fx.choose_in(
            lo,
            more,
            &[types::I8],
            |lo, fx| {
                let (key, len) = kept_field(lo, fx, cursor);
                let earlier = match before {
                    Some(literal) => key_precedes(lo, fx, key, len, literal),
                    None => fx.int(types::I8, 1),
                };
                let after = fx.load(PTR, cursor, reader::POS);
                fx.when(earlier, |fx| {
                    let size = fx.b.ins().isub(after, pos);
                    put_bytes(lo, fx, state, pos, size);
                });
                let later = fx.b.ins().bxor_imm_s(earlier, 1);
                fx.when(later, |fx| fx.store(pos, cursor, reader::POS));
                vec![earlier]
            },
            |_, fx| vec![fx.int(types::I8, 0)],
        )[0];
        (written, vec![unchanged[0]])
    });
}

/// Whether a key precedes a literal one in byte order.
fn key_precedes(lo: &mut Lowering, fx: &mut Fx, key: Value, len: Value, literal: &[u8]) -> Value {
    let view = lo.view_literal(fx, literal).address();
    let expected = fx.load(PTR, view, 0);
    let literal_len = fx.ptr(literal.len() as u64);
    let shorter = fx.b.ins().umin(len, literal_len);
    let order = lo.call_routine(fx, Routine::Compare, &[key, expected, shorter])[0];
    let less = fx.b.ins().icmp_imm_s(IntCC::SignedLessThan, order, 0);
    let tie = fx.b.ins().icmp_imm_s(IntCC::Equal, order, 0);
    let shorter_key = fx.b.ins().icmp(IntCC::UnsignedLessThan, len, literal_len);
    let less_by_len = fx.b.ins().band(tie, shorter_key);
    fx.b.ins().bor(less, less_by_len)
}

/// A bool: `Malformed` on a byte other than 0 and 1.
pub fn read_bool(fx: &mut Fx, state: Value) -> (Value, Value) {
    let (byte, failed) = read_byte(fx, state);
    let valid =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedLessThanOrEqual, byte, 1);
    let malformed = code(fx, failure::MALFORMED);
    let ok = fx.b.ins().icmp_imm_s(IntCC::Equal, failed, 0);
    let checked = fx.b.ins().select(valid, failed, malformed);
    let failed = fx.b.ins().select(ok, checked, failed);
    (fx.b.ins().ireduce(types::I8, byte), failed)
}

/// An unsigned value, or a signed one from its zigzag uvar, in 64 bits.
pub fn read_integer(lo: &mut Lowering, fx: &mut Fx, state: Value, signed: bool) -> (Value, Value) {
    let (value, failed) = read_uvar(lo, fx, state);
    if !signed {
        return (value, failed);
    }
    let half = fx.b.ins().ushr_imm_s(value, 1);
    let low = fx.b.ins().band_imm_s(value, 1);
    let mask = fx.b.ins().ineg(low);
    (fx.b.ins().bxor(half, mask), failed)
}

/// A float: the 8 bytes of an f64 bit pattern, little-endian, as a value of `float`, the type
/// holding an f64.
pub fn read_float(fx: &mut Fx, state: Value, float: Type) -> (Value, Value) {
    let pos = fx.load(PTR, state, reader::POS);
    let left = left(fx, state);
    let enough =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, left, 8);
    let parts = fx.choose(
        enough,
        &[float, types::I32],
        |fx| {
            let bits =
                fx.b.ins()
                    .load(types::I64, MemFlagsData::new().with_notrap(), pos, 0);
            let next = fx.b.ins().iadd_imm_s(pos, 8);
            fx.store(next, state, reader::POS);
            vec![
                super::super::numbers::from_bits(fx, bits, float),
                code(fx, 0),
            ]
        },
        |fx| {
            vec![
                super::super::super::body::zero(fx, float),
                code(fx, failure::TRUNCATED),
            ]
        },
    );
    (parts[0], parts[1])
}
