//! `T.encode(self, mut e)` and `T.decode(mut d, alloc)` of the types whose `Encode` a derive
//! writes or the toolchain supplies, through the calls of an encoder or decoder of any type.
//! Per encoder type and type, an encoding function makes the calls of `core::Encoder` a value's
//! kind states, given the value's address and the encoder's. Decoding runs the decoding
//! functions of the format of a decoder's calls, whose reader holds the decoder's address in
//! place of the input's position.

use cranelift_codegen::ir::{AbiParam, InstBuilder, Signature, Value, condcodes::IntCC, types};
use cranelift_module::FuncId;

use super::super::super::abi::Abi;
use super::super::super::body::Held;
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::Routine;
use super::super::super::{CALL_CONV, Glue, Lowering};
use super::super::Supply;
use super::super::mem::maps::Entries;
use super::super::mem::vector::{self, Element};
use super::{
    BYTE, CasePayload, DecodeFormat, Kind, Record, by_key, call, check, contents_of, decoder,
    elements_of, fail, failure, kind_of, reader, returned, text_of, unwrapped, utf8_valid,
};
use crate::check::ty::{Args, Scalar, Ty};

/// Defines `encode` or `decode` of a type, whose encoder or decoder is the type of its other
/// parameter.
pub fn through(lo: &mut Lowering, fx: &mut Fx, abi: &Abi, ty: &Ty, method: &str) {
    let (sret, failure, params) = super::super::entry(fx, abi);
    let supply = Supply {
        abi,
        args: Args::from(Vec::new()),
        path: format!("core::Encode.{method}"),
        owner: None,
        sret,
        failure,
        params,
        wrap: None,
    };
    if method == "encode" {
        let encoder_ty = abi.params[1].0.ty().clone();
        let value = super::super::tasks::address_of(lo, fx, ty, supply.param(0));
        let function = encoder(lo, &encoder_ty, ty);
        call(lo, fx, function, &[value, supply.param(1).address()]);
        supply.done(lo, fx);
        return;
    }
    let decoder_ty = abi.params[0].0.ty().clone();
    let alloc = fx.load(PTR, supply.param(1).address(), 0);
    let state = fx.slot(reader::SIZE, 8);
    let address = supply.param(0).address();
    fx.store(address, state, reader::POS);
    let zero = fx.ptr(0);
    fx.store(zero, state, reader::DEPTH);
    let layout = lo.layout(ty);
    let target = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    let function = decoder(lo, &DecodeFormat::Calls(decoder_ty), ty);
    let code = call(lo, fx, function, &[state, alloc, target])[0];
    returned(lo, fx, &supply, ty, target, code);
}

/// Calls method `name` of `core::Encoder` on the encoder at an address.
fn write(lo: &mut Lowering, fx: &mut Fx, encoder: &Ty, target: Value, name: &str, args: &[Held]) {
    let interface = lo.core_interface("Encoder");
    let method = lo
        .ck
        .interface_method(interface, name)
        .expect("an encoder method");
    let mut all = vec![Held::Address(target)];
    all.extend_from_slice(args);
    lo.call_interface(fx, encoder, interface, method as u32, Vec::new(), &all);
}

/// A view of `len` bytes from `start`, as a `Str` or slice of bytes is held.
fn view(fx: &mut Fx, start: Value, len: Value) -> Held {
    let view = fx.slot(16, 8);
    fx.store(start, view, 0);
    fx.store(len, view, 8);
    Held::Address(view)
}

/// The encoding function of a type through an encoder of a type: given the value's address and
/// the encoder's.
fn encoder(lo: &mut Lowering, encoder: &Ty, ty: &Ty) -> FuncId {
    let mut signature = Signature::new(CALL_CONV);
    signature.params.push(AbiParam::new(PTR));
    signature.params.push(AbiParam::new(PTR));
    let ty = unwrapped(lo, ty);
    let encoder = encoder.clone();
    lo.glue_function(
        Glue::EncodeCalls(encoder.clone(), ty.clone()),
        signature,
        move |lo, fx| {
            let params = fx.entry();
            let kind = kind_of(lo, &ty);
            encode_kind(lo, fx, &encoder, &ty, &kind, params[0], params[1]);
            fx.b.ins().return_(&[]);
        },
    )
}

fn encode_kind(
    lo: &mut Lowering,
    fx: &mut Fx,
    encoder_ty: &Ty,
    ty: &Ty,
    kind: &Kind,
    value: Value,
    target: Value,
) {
    let encoder = |lo: &mut Lowering, ty: &Ty| encoder(lo, encoder_ty, ty);
    match kind {
        Kind::Bool => {
            let flag = fx.load(types::I8, value, 0);
            write(
                lo,
                fx,
                encoder_ty,
                target,
                "write_bool",
                &[Held::Scalar(flag)],
            );
        }
        Kind::Unsigned(scalar) | Kind::Signed(scalar) => {
            let loaded = fx.load(super::super::super::layout::scalar_type(*scalar), value, 0);
            let signed = matches!(kind, Kind::Signed(_));
            let wide = match (signed, scalar.bits() < 64) {
                (true, true) => fx.b.ins().sextend(types::I64, loaded),
                (false, true) => fx.b.ins().uextend(types::I64, loaded),
                _ => loaded,
            };
            let name = match signed {
                true => "write_signed",
                false => "write_unsigned",
            };
            write(lo, fx, encoder_ty, target, name, &[Held::Scalar(wide)]);
        }
        Kind::Char => {
            let scalar = fx.load(types::I32, value, 0);
            write(
                lo,
                fx,
                encoder_ty,
                target,
                "write_char",
                &[Held::Scalar(scalar)],
            );
        }
        Kind::Float(scalar) => {
            let loaded = fx.load(super::super::super::layout::scalar_type(*scalar), value, 0);
            let wide = match scalar {
                Scalar::F32 => fx.b.ins().fpromote(types::F64, loaded),
                _ => loaded,
            };
            write(
                lo,
                fx,
                encoder_ty,
                target,
                "write_float",
                &[Held::Scalar(wide)],
            );
        }
        Kind::Text(fixed) => {
            let (start, len) = text_of(lo, fx, ty, *fixed, value);
            let text = view(fx, start, len);
            write(lo, fx, encoder_ty, target, "write_text", &[text]);
        }
        Kind::Bytes => {
            let start = fx.load(PTR, value, vector::DATA);
            let len = fx.load(PTR, value, vector::LEN);
            let bytes = view(fx, start, len);
            write(lo, fx, encoder_ty, target, "write_bytes", &[bytes]);
        }
        Kind::Sequence(element, form) => {
            let (first, count) = elements_of(lo, fx, ty, *form, value);
            let shape = Element::of(lo, element);
            let function = encoder(lo, element);
            write(
                lo,
                fx,
                encoder_ty,
                target,
                "begin_sequence",
                &[Held::Scalar(count)],
            );
            let zero = fx.ptr(0);
            fx.each(zero, count, |fx, index| {
                let at = shape.at(fx, first, index);
                call(lo, fx, function, &[at, target]);
            });
            write(lo, fx, encoder_ty, target, "end", &[]);
        }
        Kind::Map(key, value_ty) => {
            let entries = Entries::of_map(lo, key.clone(), value_ty.clone());
            let value_at = lo
                .layouts
                .field(lo.ck, &entries.entry, None, "value")
                .offset;
            let first = fx.load(PTR, value, vector::DATA);
            let count = fx.load(PTR, value, vector::LEN);
            let key_function = encoder(lo, key);
            let value_function = encoder(lo, value_ty);
            write(
                lo,
                fx,
                encoder_ty,
                target,
                "begin_map",
                &[Held::Scalar(count)],
            );
            let zero = fx.ptr(0);
            fx.each(zero, count, |fx, index| {
                let entry = entries.shape.at(fx, first, index);
                let key_address = fx.offset(entry, entries.key_at);
                let value_address = fx.offset(entry, value_at);
                call(lo, fx, key_function, &[key_address, target]);
                call(lo, fx, value_function, &[value_address, target]);
            });
            write(lo, fx, encoder_ty, target, "end", &[]);
        }
        Kind::Contents(inner, _) => {
            let contents = contents_of(lo, fx, ty, value);
            let function = encoder(lo, inner);
            call(lo, fx, function, &[contents, target]);
        }
        Kind::Id => {
            let index = fx.load(types::I32, value, 0);
            let index = fx.b.ins().uextend(types::I64, index);
            write(
                lo,
                fx,
                encoder_ty,
                target,
                "write_unsigned",
                &[Held::Scalar(index)],
            );
        }
        Kind::Record(record) => encode_record(lo, fx, encoder_ty, record, value, target),
        Kind::Variant(cases) => {
            let index = lo.variant_index(fx, ty, value);
            for (position, case) in cases.iter().enumerate() {
                let this = fx.b.ins().icmp_imm_s(IntCC::Equal, index, position as i64);
                fx.when(this, |fx| {
                    let key = lo.view_literal(fx, case.key.as_bytes());
                    let payload = !matches!(case.payload, CasePayload::None);
                    let follows = Held::Scalar(fx.int(types::I8, i64::from(payload)));
                    write(lo, fx, encoder_ty, target, "begin_variant", &[key, follows]);
                    match &case.payload {
                        CasePayload::None => {}
                        CasePayload::Single(payload_ty) => {
                            let payload = lo.payload_address(fx, ty, &case.key, "", value);
                            let function = encoder(lo, payload_ty);
                            call(lo, fx, function, &[payload, target]);
                        }
                        CasePayload::Labelled(record) => {
                            encode_record(lo, fx, encoder_ty, record, value, target)
                        }
                    }
                    write(lo, fx, encoder_ty, target, "end", &[]);
                });
            }
        }
    }
}

/// Writes a record: per field, in ascending byte order of keys, its key and value.
fn encode_record(
    lo: &mut Lowering,
    fx: &mut Fx,
    encoder_ty: &Ty,
    record: &Record,
    value: Value,
    target: Value,
) {
    let fields = by_key(record);
    let count = fx.ptr(fields.len() as u64);
    write(
        lo,
        fx,
        encoder_ty,
        target,
        "begin_record",
        &[Held::Scalar(count)],
    );
    for (key, field_ty, offset) in &fields {
        let field = fx.offset(value, *offset);
        let key = lo.view_literal(fx, key.as_bytes());
        write(lo, fx, encoder_ty, target, "write_key", &[key]);
        let function = encoder(lo, encoder_ty, field_ty);
        call(lo, fx, function, &[field, target]);
    }
    write(lo, fx, encoder_ty, target, "end", &[]);
}

/// Calls method `name` of `core::Decoder` on the decoder a reader holds: its result, and the
/// code of the failure it raises, zero for none and for an implementing function that cannot
/// fail.
pub fn read(
    lo: &mut Lowering,
    fx: &mut Fx,
    decoder: &Ty,
    reader_state: Value,
    name: &str,
    args: &[Held],
) -> (Held, Value) {
    let interface = lo.core_interface("Decoder");
    let method = lo
        .ck
        .interface_method(interface, name)
        .expect("a decoder method");
    let address = fx.load(PTR, reader_state, reader::POS);
    let mut all = vec![Held::Address(address)];
    all.extend_from_slice(args);
    let called = lo.call_interface(fx, decoder, interface, method as u32, Vec::new(), &all);
    let (Some(failed), Some(error)) = (called.failed, called.failure) else {
        return (called.result, fx.int(types::I32, 0));
    };
    let code = fx.choose_in(
        lo,
        failed,
        &[types::I32],
        |lo, fx| {
            let error_ty = lo.std_ty("core", "DecodeError", Vec::new());
            let index = lo.variant_index(fx, &error_ty, error);
            let code = fx.b.ins().iadd_imm_s(index, 1);
            vec![fx.b.ins().ireduce(types::I32, code)]
        },
        |_, fx| vec![fx.int(types::I32, 0)],
    )[0];
    (called.result, code)
}

/// Calls a decoder's method `name` that gives a scalar, returning from the decoding function
/// after `undo` when it fails.
pub fn read_scalar(
    lo: &mut Lowering,
    fx: &mut Fx,
    decoder: &Ty,
    reader_state: Value,
    name: &str,
    undo: impl FnOnce(&mut Lowering, &mut Fx),
) -> Value {
    let (held, failed) = read(lo, fx, decoder, reader_state, name, &[]);
    check(lo, fx, failed, undo);
    held.scalar()
}

/// The position among `names` of the field or variant the key a decoder reads through its
/// method `name` names, directly or through an alias, -1 for none: the keys it is given are the
/// names, then the old keys of the aliases of names. Returns from the decoding function after
/// `undo` when the call fails.
#[allow(clippy::too_many_arguments)]
pub fn read_key(
    lo: &mut Lowering,
    fx: &mut Fx,
    decoder: &Ty,
    reader_state: Value,
    name: &str,
    names: &[&[u8]],
    aliases: &[(Vec<u8>, Vec<u8>)],
    undo: impl FnOnce(&mut Lowering, &mut Fx),
) -> Value {
    let mut keys: Vec<(&[u8], usize)> = names.iter().copied().zip(0..).collect();
    keys.extend(aliases.iter().filter_map(|(old, current)| {
        let field = names.iter().position(|name| *name == current.as_slice())?;
        Some((old.as_slice(), field))
    }));
    let array = fx.slot(16 * keys.len().max(1) as u64, 8);
    for (index, (key, _)) in keys.iter().enumerate() {
        let literal = lo.view_literal(fx, key).address();
        let start = fx.load(PTR, literal, 0);
        let len = fx.ptr(key.len() as u64);
        fx.store(start, array, 16 * index as u64);
        fx.store(len, array, 16 * index as u64 + 8);
    }
    let count = fx.ptr(keys.len() as u64);
    let slice = view(fx, array, count);
    let (held, failed) = read(lo, fx, decoder, reader_state, name, &[slice]);
    check(lo, fx, failed, undo);
    let option = lo.std_ty("core", "Option", vec![Ty::Scalar(Scalar::Usize)]);
    let found = super::super::tasks::address_of(lo, fx, &option, held);
    let variant = lo.variant_index(fx, &option, found);
    let some = fx.b.ins().icmp_imm_s(IntCC::Equal, variant, 0);
    let payload = lo.payload_address(fx, &option, "Some", "", found);
    let index = fx.load(PTR, payload, 0);
    let mut matched = fx.int(PTR, -1);
    for (position, (_, field)) in keys.iter().enumerate() {
        let this = fx.b.ins().icmp_imm_s(IntCC::Equal, index, position as i64);
        let field = fx.int(PTR, *field as i64);
        matched = fx.b.ins().select(this, field, matched);
    }
    let none = fx.int(PTR, -1);
    fx.b.ins().select(some, matched, none)
}

/// Reads text or bytes into the empty string or bytes at `target`, drawn for exactly them, or
/// into a fixed text of at most so many bytes: the length a decoder's `begin_text` or
/// `begin_bytes` gives, then the bytes its `read_chunk` fills in, which must be well-formed
/// UTF-8 for text.
#[allow(clippy::too_many_arguments)]
pub fn decode_text(
    lo: &mut Lowering,
    fx: &mut Fx,
    decoder: &Ty,
    reader_state: Value,
    ty: &Ty,
    fixed: Option<u64>,
    text: bool,
    target: Value,
) {
    let begin = match text {
        true => "begin_text",
        false => "begin_bytes",
    };
    let len = read_scalar(lo, fx, decoder, reader_state, begin, |_, _| {});
    let start = match fixed {
        Some(capacity) => {
            let over =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedGreaterThan, len, capacity as i64);
            fx.when(over, |fx| fail(fx, failure::OUT_OF_RANGE));
            let len_at = lo.layouts.field(lo.ck, ty, None, "len").offset;
            let bytes_at = lo.layouts.field(lo.ck, ty, None, "bytes").offset;
            fx.store(len, target, len_at);
            fx.offset(target, bytes_at)
        }
        None => {
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, len, 0);
            fx.when(present, |fx| {
                let drawn = vector::resize(lo, fx, target, len, BYTE);
                let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, drawn, 0);
                fx.when(failed, |fx| fail(fx, failure::OUT_OF_MEMORY));
            });
            fx.store(len, target, vector::LEN);
            fx.load(PTR, target, vector::DATA)
        }
    };
    let release = |lo: &mut Lowering, fx: &mut Fx| {
        if fixed.is_none() {
            vector::free_storage(lo, fx, target);
        }
    };
    let zero = fx.ptr(0);
    lo.call_routine(fx, Routine::Fill, &[start, zero, len]);
    let out = view(fx, start, len);
    let (_, failed) = read(lo, fx, decoder, reader_state, "read_chunk", &[out]);
    check(lo, fx, failed, release);
    if text {
        let valid = utf8_valid(lo, fx, start, len);
        let invalid = fx.b.ins().bxor_imm_s(valid, 1);
        fx.when(invalid, |fx| {
            release(lo, fx);
            fail(fx, failure::INVALID_TEXT);
        });
    }
}
