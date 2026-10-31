//! The codecs `mem::wire::Codec`, with its schemas and erased decoders, `text::json::Codec` and
//! `text::construction::Codec`. Per format and type, an encoding function writes a value through
//! a writer and a decoding function reads one through a reader, laid out as the kinds of
//! `core::Encoder` and `core::Decoder` are: a struct as a record of its fields in ascending byte
//! order of their keys, an enum value as a variant, and each std type as its encoding states.
//! The calls of a decoder are a format too.
//!
//! A writer is the buffer bytes go to, zero while measuring, and the count of bytes written. A
//! reader is the address of the next byte, or the decoder's address, the end of the input and
//! the nesting depth. A decoding function gives zero, or one plus the index of the
//! `core::DecodeError` variant it fails with, having released what it read.

pub mod calls;
pub mod construction;
mod json;
mod wire;

use cranelift_codegen::ir::{AbiParam, InstBuilder, Signature, Value, condcodes::IntCC, types};
use cranelift_module::FuncId;

use super::super::body::Held;
use super::super::call::{Called, Invoked};
use super::super::data::const_arg;
use super::super::fx::{Fx, PTR};
use super::super::runtime::Routine;
use super::super::{CALL_CONV, Glue, Instance, Lowering};
use super::Supply;
use super::mem::maps::Entries;
use super::mem::vector::{self, Element};
use crate::check::db::{ItemId, TypeKey};
use crate::check::sig::{KEPT, Payload, SigKind};
use crate::check::ty::{Arg, Scalar, Subst, Ty};
use crate::nir::{Callable, FnRef, Role};

/// A format values encode in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Wire,
    Json,
}

/// A format values decode from, or the calls of a decoder of a type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DecodeFormat {
    Wire,
    Json,
    Construction,
    Calls(Ty),
}

pub mod writer {
    pub const BUFFER: u64 = 0;
    pub const LEN: u64 = 8;
    pub const SIZE: u64 = 16;
}

pub mod reader {
    pub const POS: u64 = 0;
    pub const END: u64 = 8;
    pub const DEPTH: u64 = 16;
    pub const SIZE: u64 = 24;
}

/// The codes of decoding failures: one plus the index of each `core::DecodeError` variant.
pub mod failure {
    pub const TRUNCATED: i64 = 1;
    pub const MALFORMED: i64 = 2;
    pub const TRAILING: i64 = 3;
    pub const TOO_DEEP: i64 = 4;
    pub const UNKNOWN_KEY: i64 = 5;
    pub const DUPLICATE_KEY: i64 = 6;
    pub const MISSING_FIELD: i64 = 7;
    pub const UNKNOWN_VARIANT: i64 = 8;
    pub const OUT_OF_RANGE: i64 = 9;
    pub const INVALID_TEXT: i64 = 10;
    pub const INVALID: i64 = 11;
    pub const OUT_OF_MEMORY: i64 = 12;
    pub const UNKNOWN_SCHEMA: i64 = 13;
}

/// The `core::DecodeError` variants, in order.
const DECODE_ERRORS: [&str; 13] = [
    "Truncated",
    "Malformed",
    "Trailing",
    "TooDeep",
    "UnknownKey",
    "DuplicateKey",
    "MissingField",
    "UnknownVariant",
    "OutOfRange",
    "InvalidText",
    "Invalid",
    "OutOfMemory",
    "UnknownSchema",
];

/// The nesting of sequences, maps, records and variants past which decoding fails.
const MOST_DEPTH: i64 = 128;

const BYTE: Element = Element { size: 1, align: 1 };

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    let owner = supply.path.rsplit_once('.').map_or("", |(owner, _)| owner);
    let (encoding, decoding) = match owner {
        "mem::wire::Codec" => (Some(Format::Wire), DecodeFormat::Wire),
        "text::json::Codec" => (Some(Format::Json), DecodeFormat::Json),
        "text::construction::Codec" => (None, DecodeFormat::Construction),
        _ => return false,
    };
    match (supply.member(), encoding) {
        ("encode" | "try_encode", Some(format)) => encode(lo, fx, supply, format),
        ("decode", _) => decode(lo, fx, supply, &decoding),
        ("canonical", _) => canonical(lo, fx, supply),
        ("schema_hash", _) => {
            let hash = schema_hash(lo, fx, &supply.type_arg(0));
            let held = lo.held_at(fx, &supply.abi.result, hash);
            supply.ret(lo, fx, held);
        }
        ("schema", _) => schema(lo, fx, supply),
        ("erased", _) => erased(lo, fx, supply),
        ("decode_from", _) => decode_from(lo, fx, supply),
        ("encode_as", _) => encode_as(lo, fx, supply),
        (member, _) => panic!("no lowering of {owner}.{member}"),
    }
    true
}

/// `Codec.encode(value, mut out)` and `try_encode`.
fn encode(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, format: Format) {
    let ty = supply.type_arg(0);
    let value = super::tasks::address_of(lo, fx, &ty, supply.param(0));
    let out = supply.param(1).address();
    append(lo, fx, supply, format, &ty, value, out);
    supply.done(lo, fx);
}

/// Appends the encoding of the value of `ty` at `value` to the bytes at `out`: the encoding
/// measured, room made for exactly it, then written there.
fn append(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    format: Format,
    ty: &Ty,
    value: Value,
    out: Value,
) {
    let function = encoder(lo, format, ty);
    let zero = fx.ptr(0);
    let size = measure(lo, fx, function, value);
    let len = fx.load(PTR, out, vector::LEN);
    let (needed, overflow) = fx.b.ins().uadd_overflow(len, size);
    let drawn = fx.choose_in(
        lo,
        overflow,
        &[types::I8],
        |_, fx| vec![fx.int(types::I8, 0)],
        |lo, fx| vec![vector::reserve(lo, fx, out, needed, BYTE)],
    )[0];
    let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, drawn, 0);
    fx.when(failed, |fx| supply.exhausted(lo, fx, &[]));
    let data = fx.load(PTR, out, vector::DATA);
    let target = fx.b.ins().iadd(data, len);
    let state = fx.slot(writer::SIZE, 8);
    fx.store(target, state, writer::BUFFER);
    fx.store(zero, state, writer::LEN);
    call(lo, fx, function, &[value, state]);
    fx.store(needed, out, vector::LEN);
}

/// The bytes an encoding function writes for a value.
fn measure(lo: &mut Lowering, fx: &mut Fx, function: FuncId, value: Value) -> Value {
    let state = fx.slot(writer::SIZE, 8);
    let zero = fx.ptr(0);
    fx.store(zero, state, writer::BUFFER);
    fx.store(zero, state, writer::LEN);
    call(lo, fx, function, &[value, state]);
    fx.load(PTR, state, writer::LEN)
}

fn call(lo: &mut Lowering, fx: &mut Fx, function: FuncId, args: &[Value]) -> Vec<Value> {
    let reference = lo.func_ref(fx, function);
    let call = fx.b.ins().call(reference, args);
    fx.b.inst_results(call).to_vec()
}

/// `Codec.decode(raw, alloc)`: one value read, failing with `Trailing` on what is left.
fn decode(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, format: &DecodeFormat) {
    let ty = supply.type_arg(0);
    let layout = lo.layout(&ty);
    let target = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    let code = read(lo, fx, supply, 0, format, &ty, target);
    returned(lo, fx, supply, &ty, target, code);
}

/// Reads one value of `ty` at `target` from the bytes of the operation's Untrusted slice, its
/// parameter `raw`, drawing through the allocator of the parameter after it: the code of a
/// decoding function, `Trailing` on what is left after the value, which is then released.
fn read(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    raw: usize,
    format: &DecodeFormat,
    ty: &Ty,
    target: Value,
) -> Value {
    let alloc = supply.param(raw + 1).address();
    let raw = supply.param(raw).address();
    let start = fx.load(PTR, raw, 0);
    let len = fx.load(PTR, raw, 8);
    let alloc = fx.load(PTR, alloc, 0);
    let state = fx.slot(reader::SIZE, 8);
    fx.store(start, state, reader::POS);
    let end = fx.b.ins().iadd(start, len);
    fx.store(end, state, reader::END);
    let zero = fx.ptr(0);
    fx.store(zero, state, reader::DEPTH);
    let function = decoder(lo, format, ty);
    let code = call(lo, fx, function, &[state, alloc, target])[0];
    let read = fx.b.ins().icmp_imm_s(IntCC::Equal, code, 0);
    fx.choose_in(
        lo,
        read,
        &[types::I32],
        |lo, fx| {
            if *format != DecodeFormat::Wire {
                json::space(fx, state);
            }
            let pos = fx.load(PTR, state, reader::POS);
            let left = fx.b.ins().icmp(IntCC::NotEqual, pos, end);
            fx.when(left, |fx| lo.release(fx, ty, target));
            let trailing = fx.int(types::I32, failure::TRAILING);
            let none = fx.int(types::I32, 0);
            vec![fx.b.ins().select(left, trailing, none)]
        },
        |_, _| vec![code],
    )[0]
}

/// `Codec.canonical(raw, alloc)`: one value read as `decode` reads it, then released once its
/// encoding is written to new bytes, failing with `OutOfMemory` when they cannot be drawn.
fn canonical(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let ty = supply.type_arg(0);
    let layout = lo.layout(&ty);
    let value = fx.slot(layout.size, layout.align);
    let code = read(lo, fx, supply, 0, &DecodeFormat::Wire, &ty, value);
    let result = supply.abi.result.clone();
    let bytes = supply.sret.unwrap_or_else(|| {
        let layout = lo.layout(&result);
        fx.slot(layout.size, layout.align)
    });
    let read = fx.b.ins().icmp_imm_s(IntCC::Equal, code, 0);
    let code = fx.choose_in(
        lo,
        read,
        &[types::I32],
        |lo, fx| {
            let function = encoder(lo, Format::Wire, &ty);
            let size = measure(lo, fx, function, value);
            let alloc = fx.load(PTR, supply.param(1).address(), 0);
            vector::empty(fx, bytes, alloc, BYTE);
            let drawn = vector::resize(lo, fx, bytes, size, BYTE);
            let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, drawn, 0);
            fx.when(failed, |fx| lo.release(fx, &ty, value));
            fx.choose_in(
                lo,
                failed,
                &[types::I32],
                |_, fx| vec![fx.int(types::I32, failure::OUT_OF_MEMORY)],
                |lo, fx| {
                    let state = fx.slot(writer::SIZE, 8);
                    let data = fx.load(PTR, bytes, vector::DATA);
                    fx.store(data, state, writer::BUFFER);
                    let zero = fx.ptr(0);
                    fx.store(zero, state, writer::LEN);
                    call(lo, fx, function, &[value, state]);
                    fx.store(size, bytes, vector::LEN);
                    lo.release(fx, &ty, value);
                    vec![fx.int(types::I32, 0)]
                },
            )
        },
        |_, _| vec![code],
    )[0];
    returned(lo, fx, supply, &result, bytes, code);
}

/// The address of the schema hash of a type, as read-only data.
fn schema_hash(lo: &mut Lowering, fx: &mut Fx, ty: &Ty) -> Value {
    let hash = lo.ck.schema(ty).hash();
    let data = lo.bytes_data(&hash.0);
    lo.data_address(fx, data)
}

/// Whether the schema hash at `wanted` is the one at `hash`.
fn same_hash(lo: &mut Lowering, fx: &mut Fx, wanted: Value, hash: Value) -> Value {
    let len = fx.ptr(33);
    let order = lo.call_routine(fx, Routine::Compare, &[wanted, hash, len])[0];
    fx.b.ins().icmp_imm_s(IntCC::Equal, order, 0)
}

/// The migrations of a type in one direction, each with its frozen shape.
fn migrations_of(lo: &Lowering, ty: &Ty, forward: bool) -> Vec<(ItemId, Ty)> {
    let Ty::Item(item, _) = ty else {
        return Vec::new();
    };
    lo.ck
        .migrations(*item)
        .into_iter()
        .filter(|migration| migration.forward == forward)
        .map(|migration| (migration.function, Ty::item(migration.shape, Vec::new())))
        .collect()
}

/// Calls a migration on a value, with the allocator `alloc` when it takes one.
fn migrate(lo: &mut Lowering, fx: &mut Fx, function: ItemId, value: Held, alloc: Held) -> Called {
    let instance = Instance::Nir {
        item: function,
        role: Role::Body,
        index: 0,
        args: Vec::new().into(),
    };
    let abi = lo.abi(&instance);
    let mut args = vec![value];
    if abi.params.len() == 2 {
        args.push(alloc);
    }
    let id = lo.instance(instance);
    lo.invoke(fx, Invoked::Direct(id), &abi, &args)
}

/// `Codec.decode_from(schema, raw, alloc)`: one value read as `decode` reads it under the type's
/// own schema hash, or one of a frozen shape that a forward migration converts, failing with
/// `UnknownSchema` under another.
fn decode_from(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let ty = supply.type_arg(0);
    let layout = lo.layout(&ty);
    let target = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    let wanted = supply.param(0).address();
    let code = fx.slot(4, 4);
    let unknown = fx.int(types::I32, failure::UNKNOWN_SCHEMA);
    fx.store(unknown, code, 0);
    let own = schema_hash(lo, fx, &ty);
    let same = same_hash(lo, fx, wanted, own);
    fx.when(same, |fx| {
        let read = read(lo, fx, supply, 1, &DecodeFormat::Wire, &ty, target);
        fx.store(read, code, 0);
    });
    for (function, shape) in migrations_of(lo, &ty, true) {
        let hash = schema_hash(lo, fx, &shape);
        let same = same_hash(lo, fx, wanted, hash);
        fx.when(same, |fx| {
            let layout = lo.layout(&shape);
            let old = fx.slot(layout.size, layout.align);
            let read = read(lo, fx, supply, 1, &DecodeFormat::Wire, &shape, old);
            let ok = fx.b.ins().icmp_imm_s(IntCC::Equal, read, 0);
            let result = fx.choose_in(
                lo,
                ok,
                &[types::I32],
                |lo, fx| {
                    let held = lo.held_at(fx, &shape, old);
                    let called = migrate(lo, fx, function, held, supply.param(2));
                    lo.store_held(fx, target, called.result, &ty);
                    let none = fx.int(types::I32, 0);
                    let Some(failed) = called.failed else {
                        return vec![none];
                    };
                    let exhausted = fx.int(types::I32, failure::OUT_OF_MEMORY);
                    let failed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, failed, 0);
                    vec![fx.b.ins().select(failed, exhausted, none)]
                },
                |_, _| vec![read],
            )[0];
            fx.store(result, code, 0);
        });
    }
    let code = fx.load(types::I32, code, 0);
    returned(lo, fx, supply, &ty, target, code);
}

/// `Codec.encode_as(value, schema, mut out, alloc)`: the value appended as `encode` appends it
/// under its type's own schema hash, or the encoding of an inverse's conversion of it under a
/// frozen shape's; `false` under another.
fn encode_as(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let ty = supply.type_arg(0);
    let value = super::tasks::address_of(lo, fx, &ty, supply.param(0));
    let wanted = supply.param(1).address();
    let out = supply.param(2).address();
    let written = fx.slot(1, 1);
    let no = fx.int(types::I8, 0);
    fx.store(no, written, 0);
    let own = schema_hash(lo, fx, &ty);
    let same = same_hash(lo, fx, wanted, own);
    fx.when(same, |fx| {
        append(lo, fx, supply, Format::Wire, &ty, value, out);
        let yes = fx.int(types::I8, 1);
        fx.store(yes, written, 0);
    });
    for (function, shape) in migrations_of(lo, &ty, false) {
        let hash = schema_hash(lo, fx, &shape);
        let same = same_hash(lo, fx, wanted, hash);
        fx.when(same, |fx| {
            let held = lo.held_at(fx, &ty, value);
            let called = migrate(lo, fx, function, held, supply.param(3));
            if let Some(failed) = called.failed {
                let failed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, failed, 0);
                fx.when(failed, |fx| supply.exhausted(lo, fx, &[]));
            }
            let old = super::tasks::address_of(lo, fx, &shape, called.result);
            append(lo, fx, supply, Format::Wire, &shape, old, out);
            lo.release(fx, &shape, old);
            let yes = fx.int(types::I8, 1);
            fx.store(yes, written, 0);
        });
    }
    let written = fx.load(types::I8, written, 0);
    supply.ret(lo, fx, Held::Scalar(written));
}

/// `Codec.schema(alloc)`: the type's schema tree, decoded from its `mem::wire` encoding.
fn schema(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let tree = lo.ck.schema(&supply.type_arg(0)).wire();
    let data = lo.bytes_data(&tree);
    let start = lo.data_address(fx, data);
    let alloc = fx.load(PTR, supply.param(0).address(), 0);
    let state = fx.slot(reader::SIZE, 8);
    fx.store(start, state, reader::POS);
    let end = fx.b.ins().iadd_imm_s(start, tree.len() as i64);
    fx.store(end, state, reader::END);
    let zero = fx.ptr(0);
    fx.store(zero, state, reader::DEPTH);
    let result = supply.abi.result.clone();
    let layout = lo.layout(&result);
    let target = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    let function = decoder(lo, &DecodeFormat::Wire, &result);
    let code = call(lo, fx, function, &[state, alloc, target])[0];
    let failed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, code, 0);
    fx.when(failed, |fx| supply.exhausted(lo, fx, &[]));
    let held = lo.held_at(fx, &result, target);
    supply.ret(lo, fx, held);
}

/// `Codec.erased()`: the type's schema hash and `Codec.canonical` of it as a function value.
fn erased(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let result = supply.abi.result.clone();
    let layout = lo.layout(&result);
    let target = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    let hash = schema_hash(lo, fx, &supply.type_arg(0));
    let at = lo.layouts.field(lo.ck, &result, None, "schema").offset;
    let schema = fx.b.ins().iadd_imm_s(target, at as i64);
    lo.copy(fx, schema, hash, 33, 1);
    let codec = lo
        .ck
        .db
        .std_item("mem::wire", "Codec")
        .expect("mem::wire::Codec");
    let canonical = lo.ck.db.members(TypeKey::Item(codec), "canonical")[0];
    let function = FnRef {
        callable: Callable::Item {
            item: canonical,
            role: Role::Body,
            index: 0,
        },
        args: supply.args.clone(),
    };
    let value = lo.function_value(fx, &function).address();
    let at = lo.layouts.field(lo.ck, &result, None, "canonical").offset;
    let field = fx.b.ins().iadd_imm_s(target, at as i64);
    lo.copy(fx, field, value, 16, 8);
    let held = lo.held_at(fx, &result, target);
    supply.ret(lo, fx, held);
}

/// Returns the value a decoding function read at `target`, or fails with the
/// `core::DecodeError` its code names.
fn returned(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, ty: &Ty, target: Value, code: Value) {
    let failed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, code, 0);
    fx.when(failed, |fx| {
        let fails = supply.abi.fails.clone().expect("a decode's failure type");
        let layout = lo.layout(&fails);
        let error = fx.slot(layout.size, layout.align);
        for (index, name) in DECODE_ERRORS.iter().enumerate() {
            let this = fx.b.ins().icmp_imm_s(IntCC::Equal, code, index as i64 + 1);
            let fails = fails.clone();
            fx.when(this, |fx| {
                let value = lo.variant_value(fx, &fails, name, &[], None);
                lo.store_held(fx, error, value, &fails);
            });
        }
        let held = lo.held_at(fx, &fails, error);
        let target = supply.failure.expect("a failure address");
        lo.store_held(fx, target, held, &fails);
        supply.fail(fx);
    });
    let held = lo.held_at(fx, ty, target);
    supply.ret(lo, fx, held);
}

/// How a type encodes.
#[derive(Clone)]
enum Kind {
    Bool,
    Unsigned(Scalar),
    Signed(Scalar),
    Float(Scalar),
    Char,
    /// Text, inline in a fixed text of at most the given bytes.
    Text(Option<u64>),
    Bytes,
    Sequence(Ty, Form),
    Map(Ty, Ty),
    /// The contents of a box, or of a shared value.
    Contents(Ty, bool),
    Id,
    Record(Record),
    Variant(Vec<Case>),
}

/// The element storage of a sequence.
#[derive(Clone, Copy)]
enum Form {
    /// An array of exactly so many elements.
    Array(u64),
    List,
    Set,
    /// A bounded list of at most so many elements.
    Bounded(u64),
}

/// A record: its fields with their keys, types and offsets, in declaration order, whether it
/// decodes as open, the aliases of its keys, the struct it builds, whose predicates hold of
/// every value decoded, and the offset and type of a flexible struct's kept fields.
#[derive(Clone)]
struct Record {
    fields: Vec<(String, Ty, u64)>,
    open: bool,
    aliases: Vec<(Vec<u8>, Vec<u8>)>,
    owner: Option<(crate::check::db::ItemId, Vec<Arg>)>,
    kept: Option<(u64, Ty)>,
}

/// A variant of an enum: its key and payload.
#[derive(Clone)]
struct Case {
    key: String,
    payload: CasePayload,
}

#[derive(Clone)]
enum CasePayload {
    None,
    Single(Ty),
    Labelled(Record),
}

fn kind_of(lo: &mut Lowering, ty: &Ty) -> Kind {
    match ty {
        Ty::Scalar(scalar) => match scalar {
            Scalar::Bool => Kind::Bool,
            Scalar::Char => Kind::Char,
            Scalar::F32 | Scalar::F64 => Kind::Float(*scalar),
            _ if scalar.is_signed() => Kind::Signed(*scalar),
            _ => Kind::Unsigned(*scalar),
        },
        Ty::Unit => Kind::Record(Record {
            fields: Vec::new(),
            open: false,
            aliases: Vec::new(),
            owner: None,
            kept: None,
        }),
        Ty::Item(item, args) => {
            let arg = |index: usize| {
                args.get(index)
                    .and_then(Arg::ty)
                    .cloned()
                    .expect("a type argument")
            };
            let count = |index: usize| const_arg(&args[index]) as u64;
            match lo.ck.db.canonical(*item).as_str() {
                "core::Array" => Kind::Sequence(arg(0), Form::Array(count(1))),
                "text::String" => Kind::Text(None),
                "text::Fixed" => Kind::Text(Some(count(0))),
                "mem::Bytes" => Kind::Bytes,
                "mem::List" => Kind::Sequence(arg(0), Form::List),
                "mem::Set" => Kind::Sequence(arg(0), Form::Set),
                "mem::Bounded" => Kind::Sequence(arg(0), Form::Bounded(count(1))),
                "mem::Map" => Kind::Map(arg(0), arg(1)),
                "mem::Box" => Kind::Contents(arg(0), false),
                "mem::Shared" => Kind::Contents(arg(0), true),
                "mem::Id" => Kind::Id,
                _ => declared(lo, *item, args),
            }
        }
        other => panic!("an encoded value of {other:?}"),
    }
}

/// How a struct or enum deriving `Encode` encodes.
fn declared(lo: &mut Lowering, item: crate::check::db::ItemId, args: &[Arg]) -> Kind {
    let sig = lo.ck.sig(item);
    let subst = Subst::of(sig.generics.iter().map(|def| def.param), args);
    let ty = Ty::Item(item, args.to_vec().into());
    let open = lo.ck.db.ast(item).is_open();
    let aliases: Vec<(Vec<u8>, Vec<u8>)> = lo.ck.key_aliases(item).into_iter().collect();
    match &sig.kind {
        SigKind::Struct { fields } => {
            let fields = fields
                .iter()
                .filter(|field| !field.is_kept())
                .map(|field| {
                    let offset = lo.layouts.field(lo.ck, &ty, None, &field.name).offset;
                    (field.name.clone(), subst.ty(&field.ty), offset)
                })
                .collect();
            let kept = lo.ck.kept_type(item).map(|kept| {
                let offset = lo.layouts.field(lo.ck, &ty, None, KEPT).offset;
                (offset, kept)
            });
            Kind::Record(Record {
                fields,
                open,
                aliases,
                owner: Some((item, args.to_vec())),
                kept,
            })
        }
        SigKind::Enum { variants } => Kind::Variant(
            variants
                .iter()
                .map(|variant| Case {
                    key: variant.name.clone(),
                    payload: match &variant.payload {
                        Payload::None => CasePayload::None,
                        Payload::Single(payload) => CasePayload::Single(subst.ty(payload)),
                        Payload::Labelled(labels) => CasePayload::Labelled(Record {
                            fields: labels
                                .iter()
                                .map(|(name, payload)| {
                                    let offset = lo
                                        .layouts
                                        .field(lo.ck, &ty, Some(&variant.name), name)
                                        .offset;
                                    (name.clone(), subst.ty(payload), offset)
                                })
                                .collect(),
                            open: false,
                            aliases: Vec::new(),
                            owner: None,
                            kept: None,
                        }),
                    },
                })
                .collect(),
        ),
        other => panic!("an Encode type of kind {other:?}"),
    }
}

/// A type without its Untrusted wrappers, which an Untrusted value is laid out and encoded as.
fn unwrapped(lo: &Lowering, ty: &Ty) -> Ty {
    let mut ty = ty;
    while lo.ck.is_std(ty, "core::Untrusted") {
        ty = ty.type_arg(0).expect("an Untrusted type argument");
    }
    ty.clone()
}

/// The encoding function of a type in a format: given the value's address and a writer.
fn encoder(lo: &mut Lowering, format: Format, ty: &Ty) -> FuncId {
    let mut signature = Signature::new(CALL_CONV);
    signature.params.push(AbiParam::new(PTR));
    signature.params.push(AbiParam::new(PTR));
    let ty = unwrapped(lo, ty);
    lo.glue_function(
        Glue::Encode(format, ty.clone()),
        signature,
        move |lo, fx| {
            let params = fx.entry();
            let (value, writer_state) = (params[0], params[1]);
            let kind = kind_of(lo, &ty);
            encode_kind(lo, fx, format, &ty, &kind, value, writer_state);
            fx.b.ins().return_(&[]);
        },
    )
}

/// Writes one byte.
pub fn put_byte(fx: &mut Fx, state: Value, byte: Value) {
    let buffer = fx.load(PTR, state, writer::BUFFER);
    let len = fx.load(PTR, state, writer::LEN);
    let writing = fx.b.ins().icmp_imm_s(IntCC::NotEqual, buffer, 0);
    fx.when(writing, |fx| {
        let at = fx.b.ins().iadd(buffer, len);
        let byte = match fx.b.func.dfg.value_type(byte) {
            types::I8 => byte,
            _ => fx.b.ins().ireduce(types::I8, byte),
        };
        fx.store(byte, at, 0);
    });
    let len = fx.b.ins().iadd_imm_s(len, 1);
    fx.store(len, state, writer::LEN);
}

/// Writes a byte range.
pub fn put_bytes(lo: &mut Lowering, fx: &mut Fx, state: Value, start: Value, count: Value) {
    let buffer = fx.load(PTR, state, writer::BUFFER);
    let len = fx.load(PTR, state, writer::LEN);
    let writing = fx.b.ins().icmp_imm_s(IntCC::NotEqual, buffer, 0);
    fx.when(writing, |fx| {
        let at = fx.b.ins().iadd(buffer, len);
        lo.call_routine(fx, Routine::Copy, &[at, start, count]);
    });
    let len = fx.b.ins().iadd(len, count);
    fx.store(len, state, writer::LEN);
}

/// Writes the bytes of a literal.
pub fn put_literal(lo: &mut Lowering, fx: &mut Fx, state: Value, bytes: &[u8]) {
    if let [byte] = bytes {
        let byte = fx.int(types::I8, i64::from(*byte));
        put_byte(fx, state, byte);
        return;
    }
    let view = lo.view_literal(fx, bytes).address();
    let start = fx.load(PTR, view, 0);
    let count = fx.ptr(bytes.len() as u64);
    put_bytes(lo, fx, state, start, count);
}

/// The text a value of a text type holds: its first byte's address and its length.
fn text_of(
    lo: &mut Lowering,
    fx: &mut Fx,
    ty: &Ty,
    fixed: Option<u64>,
    value: Value,
) -> (Value, Value) {
    match fixed {
        None => (
            fx.load(PTR, value, vector::DATA),
            fx.load(PTR, value, vector::LEN),
        ),
        Some(_) => {
            let len_at = lo.layouts.field(lo.ck, ty, None, "len").offset;
            let bytes_at = lo.layouts.field(lo.ck, ty, None, "bytes").offset;
            (fx.offset(value, bytes_at), fx.load(PTR, value, len_at))
        }
    }
}

/// The elements of a sequence value: the first one's address and their count.
fn elements_of(
    lo: &mut Lowering,
    fx: &mut Fx,
    ty: &Ty,
    form: Form,
    value: Value,
) -> (Value, Value) {
    match form {
        Form::Array(count) => (value, fx.ptr(count)),
        Form::List | Form::Set => (
            fx.load(PTR, value, vector::DATA),
            fx.load(PTR, value, vector::LEN),
        ),
        Form::Bounded(_) => {
            let len_at = lo.layouts.field(lo.ck, ty, None, "len").offset;
            let items_at = lo.layouts.field(lo.ck, ty, None, "items").offset;
            (fx.offset(value, items_at), fx.load(PTR, value, len_at))
        }
    }
}

/// The address of the contents of a box or shared value at an address.
fn contents_of(lo: &mut Lowering, fx: &mut Fx, ty: &Ty, value: Value) -> Value {
    let pointer = fx.load(PTR, value, 0);
    super::mem::stores::contents_address(lo, fx, ty, pointer)
}

/// The fields of a record in ascending byte order of their keys.
fn by_key(record: &Record) -> Vec<(String, Ty, u64)> {
    let mut fields = record.fields.clone();
    fields.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    fields
}

fn encode_kind(
    lo: &mut Lowering,
    fx: &mut Fx,
    format: Format,
    ty: &Ty,
    kind: &Kind,
    value: Value,
    state: Value,
) {
    match kind {
        Kind::Bool => {
            let flag = fx.load(types::I8, value, 0);
            match format {
                Format::Wire => put_byte(fx, state, flag),
                Format::Json => {
                    fx.choose_in(
                        lo,
                        flag,
                        &[],
                        |lo, fx| {
                            put_literal(lo, fx, state, b"true");
                            Vec::new()
                        },
                        |lo, fx| {
                            put_literal(lo, fx, state, b"false");
                            Vec::new()
                        },
                    );
                }
            }
        }
        Kind::Unsigned(_) | Kind::Signed(_) | Kind::Char => {
            let scalar = match kind {
                Kind::Unsigned(scalar) | Kind::Signed(scalar) => *scalar,
                _ => Scalar::Char,
            };
            let loaded = fx.load(
                super::super::layout::scalar_type(scalar, lo.machine()),
                value,
                0,
            );
            let signed = matches!(kind, Kind::Signed(_));
            let wide = match (signed, scalar.bits() < 64) {
                (true, true) => fx.b.ins().sextend(types::I64, loaded),
                (false, true) => fx.b.ins().uextend(types::I64, loaded),
                _ => loaded,
            };
            match (format, kind) {
                (Format::Json, Kind::Char) => {
                    let buffer = fx.slot(4, 1);
                    let len = super::render::encode_utf8(fx, loaded, buffer);
                    json::put_string(lo, fx, state, buffer, len);
                }
                (Format::Json, _) => json::put_integer(lo, fx, state, wide, signed),
                (Format::Wire, _) => {
                    let value = match signed {
                        true => {
                            let doubled = fx.b.ins().ishl_imm_s(wide, 1);
                            let sign = fx.b.ins().sshr_imm_s(wide, 63);
                            fx.b.ins().bxor(doubled, sign)
                        }
                        false => wide,
                    };
                    wire::put_uvar(fx, state, value);
                }
            }
        }
        Kind::Float(scalar) => {
            let loaded = fx.load(
                super::super::layout::scalar_type(*scalar, lo.machine()),
                value,
                0,
            );
            match format {
                Format::Wire => {
                    let wide = match scalar {
                        Scalar::F32 => super::floats::promote(fx, loaded),
                        _ => loaded,
                    };
                    let bits = super::numbers::to_bits(fx, wide);
                    let slot = fx.slot(8, 8);
                    fx.store(bits, slot, 0);
                    let eight = fx.ptr(8);
                    put_bytes(lo, fx, state, slot, eight);
                }
                Format::Json => json::put_float(lo, fx, state, loaded),
            }
        }
        Kind::Text(fixed) => {
            let (start, len) = text_of(lo, fx, ty, *fixed, value);
            match format {
                Format::Wire => wire::put_text(lo, fx, state, start, len),
                Format::Json => json::put_string(lo, fx, state, start, len),
            }
        }
        Kind::Bytes => {
            let start = fx.load(PTR, value, vector::DATA);
            let len = fx.load(PTR, value, vector::LEN);
            match format {
                Format::Wire => wire::put_text(lo, fx, state, start, len),
                Format::Json => json::put_base64(lo, fx, state, start, len),
            }
        }
        Kind::Sequence(element, form) => {
            let (first, count) = elements_of(lo, fx, ty, *form, value);
            let shape = Element::of(lo, element);
            let function = encoder(lo, format, element);
            match format {
                Format::Wire => wire::put_uvar(fx, state, count),
                Format::Json => put_literal(lo, fx, state, b"["),
            }
            let zero = fx.ptr(0);
            fx.each(zero, count, |fx, index| {
                if format == Format::Json {
                    let later = fx.b.ins().icmp_imm_s(IntCC::NotEqual, index, 0);
                    fx.when(later, |fx| put_literal(lo, fx, state, b","));
                }
                let at = shape.at(fx, first, index);
                call(lo, fx, function, &[at, state]);
            });
            if format == Format::Json {
                put_literal(lo, fx, state, b"]");
            }
        }
        Kind::Map(key, value_ty) => {
            let entries = Entries::of_map(lo, key.clone(), value_ty.clone());
            let value_at = lo
                .layouts
                .field(lo.ck, &entries.entry, None, "value")
                .offset;
            let first = fx.load(PTR, value, vector::DATA);
            let count = fx.load(PTR, value, vector::LEN);
            let key_function = encoder(lo, format, key);
            let value_function = encoder(lo, format, value_ty);
            match format {
                Format::Wire => wire::put_uvar(fx, state, count),
                Format::Json => put_literal(lo, fx, state, b"["),
            }
            let zero = fx.ptr(0);
            fx.each(zero, count, |fx, index| {
                let entry = entries.shape.at(fx, first, index);
                let key_address = fx.offset(entry, entries.key_at);
                let value_address = fx.offset(entry, value_at);
                if format == Format::Json {
                    let later = fx.b.ins().icmp_imm_s(IntCC::NotEqual, index, 0);
                    fx.when(later, |fx| put_literal(lo, fx, state, b","));
                    put_literal(lo, fx, state, b"[");
                }
                call(lo, fx, key_function, &[key_address, state]);
                if format == Format::Json {
                    put_literal(lo, fx, state, b",");
                }
                call(lo, fx, value_function, &[value_address, state]);
                if format == Format::Json {
                    put_literal(lo, fx, state, b"]");
                }
            });
            if format == Format::Json {
                put_literal(lo, fx, state, b"]");
            }
        }
        Kind::Contents(inner, _) => {
            let contents = contents_of(lo, fx, ty, value);
            let function = encoder(lo, format, inner);
            call(lo, fx, function, &[contents, state]);
        }
        Kind::Id => {
            let index = fx.load(types::I32, value, 0);
            let index = fx.b.ins().uextend(types::I64, index);
            match format {
                Format::Wire => wire::put_uvar(fx, state, index),
                Format::Json => json::put_integer(lo, fx, state, index, false),
            }
        }
        Kind::Record(record) => encode_record(lo, fx, format, record, value, state),
        Kind::Variant(cases) => {
            let index = lo.variant_index(fx, ty, value);
            for (position, case) in cases.iter().enumerate() {
                let this = fx.b.ins().icmp_imm_s(IntCC::Equal, index, position as i64);
                fx.when(this, |fx| {
                    encode_case(lo, fx, format, ty, case, value, state)
                });
            }
        }
    }
}

/// Writes a record: per field, in ascending byte order of keys, its key and value; in the wire
/// format each value framed by its byte length, a flexible struct's kept fields among them.
fn encode_record(
    lo: &mut Lowering,
    fx: &mut Fx,
    format: Format,
    record: &Record,
    value: Value,
    state: Value,
) {
    let fields = by_key(record);
    let kept = match (format, &record.kept) {
        (Format::Wire, Some((offset, kept_ty))) => {
            let at = fx.offset(value, *offset);
            Some(wire::kept_cursor(lo, fx, kept_ty, at))
        }
        _ => None,
    };
    match format {
        Format::Wire => {
            let mut count = fx.ptr(fields.len() as u64);
            if let Some(cursor) = kept {
                let held = wire::kept_count(lo, fx, cursor);
                count = fx.b.ins().iadd(count, held);
            }
            wire::put_uvar(fx, state, count);
        }
        Format::Json => put_literal(lo, fx, state, b"{"),
    }
    for (position, (key, field_ty, offset)) in fields.iter().enumerate() {
        let field = fx.offset(value, *offset);
        let function = encoder(lo, format, field_ty);
        match format {
            Format::Wire => {
                if let Some(cursor) = kept {
                    wire::put_kept(lo, fx, state, cursor, Some(key.as_bytes()));
                }
                wire::put_literal_text(lo, fx, state, key.as_bytes());
                let size = measure(lo, fx, function, field);
                wire::put_uvar(fx, state, size);
            }
            Format::Json => {
                if position > 0 {
                    put_literal(lo, fx, state, b",");
                }
                json::put_literal_string(lo, fx, state, key.as_bytes());
                put_literal(lo, fx, state, b":");
            }
        }
        call(lo, fx, function, &[field, state]);
    }
    if let Some(cursor) = kept {
        wire::put_kept(lo, fx, state, cursor, None);
    }
    if format == Format::Json {
        put_literal(lo, fx, state, b"}");
    }
}

/// Writes a variant of an enum value: its key, then its payload when it has one.
fn encode_case(
    lo: &mut Lowering,
    fx: &mut Fx,
    format: Format,
    ty: &Ty,
    case: &Case,
    value: Value,
    state: Value,
) {
    let key = case.key.as_bytes();
    if let CasePayload::None = case.payload {
        match format {
            Format::Wire => {
                wire::put_literal_text(lo, fx, state, key);
                put_literal(lo, fx, state, &[0]);
            }
            Format::Json => json::put_literal_string(lo, fx, state, key),
        }
        return;
    }
    match format {
        Format::Wire => {
            wire::put_literal_text(lo, fx, state, key);
            put_literal(lo, fx, state, &[1]);
        }
        Format::Json => {
            put_literal(lo, fx, state, b"{");
            json::put_literal_string(lo, fx, state, key);
            put_literal(lo, fx, state, b":");
        }
    }
    match &case.payload {
        CasePayload::Single(payload_ty) => {
            let payload = lo.payload_address(fx, ty, &case.key, "", value);
            let function = encoder(lo, format, payload_ty);
            if format == Format::Wire {
                let size = measure(lo, fx, function, payload);
                wire::put_uvar(fx, state, size);
            }
            call(lo, fx, function, &[payload, state]);
        }
        CasePayload::Labelled(record) => {
            if format == Format::Wire {
                let counting = fx.slot(writer::SIZE, 8);
                let zero = fx.ptr(0);
                fx.store(zero, counting, writer::BUFFER);
                fx.store(zero, counting, writer::LEN);
                encode_record(lo, fx, format, record, value, counting);
                let size = fx.load(PTR, counting, writer::LEN);
                wire::put_uvar(fx, state, size);
            }
            encode_record(lo, fx, format, record, value, state);
        }
        CasePayload::None => unreachable!("a payload"),
    }
    if format == Format::Json {
        put_literal(lo, fx, state, b"}");
    }
}

/// The decoding function of a type in a format: given a reader, the allocator handle values
/// draw through and the address the value goes to, the code of its failure, zero for none.
fn decoder(lo: &mut Lowering, format: &DecodeFormat, ty: &Ty) -> FuncId {
    let mut signature = Signature::new(CALL_CONV);
    for _ in 0..3 {
        signature.params.push(AbiParam::new(PTR));
    }
    signature.returns.push(AbiParam::new(types::I32));
    let ty = unwrapped(lo, ty);
    let format = format.clone();
    lo.glue_function(
        Glue::Decode(format.clone(), ty.clone()),
        signature,
        move |lo, fx| {
            let params = fx.entry();
            let decoding = Decoding {
                format,
                reader: params[0],
                alloc: params[1],
            };
            let kind = kind_of(lo, &ty);
            decode_kind(lo, fx, &decoding, &ty, &kind, params[2]);
            let none = fx.int(types::I32, 0);
            fx.b.ins().return_(&[none]);
        },
    )
}

/// A decoding function's reader and allocator handle, in a format.
struct Decoding {
    format: DecodeFormat,
    reader: Value,
    alloc: Value,
}

/// Returns a failure code from a decoding function.
fn fail(fx: &mut Fx, code: i64) {
    let code = fx.int(types::I32, code);
    fx.b.ins().return_(&[code]);
}

/// Returns a failure code from a decoding function when it is nonzero, first running `undo`.
fn check(lo: &mut Lowering, fx: &mut Fx, code: Value, undo: impl FnOnce(&mut Lowering, &mut Fx)) {
    let failed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, code, 0);
    fx.when(failed, |fx| {
        undo(lo, fx);
        fx.b.ins().return_(&[code]);
    });
}

/// Calls a type's decoding function on a target address, giving its code.
fn decode_into(
    lo: &mut Lowering,
    fx: &mut Fx,
    decoding: &Decoding,
    ty: &Ty,
    target: Value,
) -> Value {
    let function = decoder(lo, &decoding.format, ty);
    call(lo, fx, function, &[decoding.reader, decoding.alloc, target])[0]
}

/// Enters a sequence, map, record or variant, failing with `TooDeep` past the most nesting.
pub fn nest(fx: &mut Fx, reader_state: Value) {
    let depth = fx.load(PTR, reader_state, reader::DEPTH);
    let depth = fx.b.ins().iadd_imm_s(depth, 1);
    fx.store(depth, reader_state, reader::DEPTH);
    let deep =
        fx.b.ins()
            .icmp_imm_s(IntCC::SignedGreaterThan, depth, MOST_DEPTH);
    fx.when(deep, |fx| fail(fx, failure::TOO_DEEP));
}

/// Leaves a sequence, map, record or variant.
pub fn unnest(fx: &mut Fx, reader_state: Value) {
    let depth = fx.load(PTR, reader_state, reader::DEPTH);
    let depth = fx.b.ins().iadd_imm_s(depth, -1);
    fx.store(depth, reader_state, reader::DEPTH);
}

/// Whether bytes are well-formed UTF-8: shortest-form sequences of 1 to 4 bytes encoding
/// scalar values, with no surrogate and nothing above U+10FFFF.
pub fn utf8_valid(lo: &mut Lowering, fx: &mut Fx, start: Value, len: Value) -> Value {
    let mut signature = Signature::new(CALL_CONV);
    signature.params.push(AbiParam::new(PTR));
    signature.params.push(AbiParam::new(PTR));
    signature.returns.push(AbiParam::new(types::I8));
    let function = lo.glue_function(Glue::Utf8Valid, signature, |_, fx| {
        let params = fx.entry();
        let (start, len) = (params[0], params[1]);
        let invalid = |fx: &mut Fx| {
            let no = fx.int(types::I8, 0);
            fx.b.ins().return_(&[no]);
        };
        let byte_at = |fx: &mut Fx, at: Value| {
            let address = fx.b.ins().iadd(start, at);
            let byte = fx.load(types::I8, address, 0);
            fx.b.ins().uextend(types::I64, byte)
        };
        let zero = fx.ptr(0);
        fx.repeat(&[zero], |fx, values| {
            let at = values[0];
            let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, at, len);
            let next = fx.choose(
                inside,
                &[types::I64],
                |fx| {
                    let lead = byte_at(fx, at);
                    let ascii = fx.b.ins().icmp_imm_s(IntCC::UnsignedLessThan, lead, 0x80);
                    let two = in_range(fx, lead, 0xC2, 0xDF);
                    let three = in_range(fx, lead, 0xE0, 0xEF);
                    let four = in_range(fx, lead, 0xF0, 0xF4);
                    let known = fx.b.ins().bor(ascii, two);
                    let known = fx.b.ins().bor(known, three);
                    let known = fx.b.ins().bor(known, four);
                    let unknown = fx.b.ins().bxor_imm_s(known, 1);
                    fx.when(unknown, |fx| invalid(fx));
                    let mut width = fx.ptr(1);
                    for flag in [two, three, four] {
                        let step = fx.b.ins().uextend(types::I64, flag);
                        width = fx.b.ins().iadd(width, step);
                    }
                    let end = fx.b.ins().iadd(at, width);
                    let past = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, end, len);
                    fx.when(past, |fx| invalid(fx));
                    for index in 1..4i64 {
                        let used =
                            fx.b.ins()
                                .icmp_imm_s(IntCC::SignedGreaterThan, width, index);
                        fx.when(used, |fx| {
                            let position = fx.b.ins().iadd_imm_s(at, index);
                            let byte = byte_at(fx, position);
                            let (low, high) = match index {
                                1 => {
                                    let e0 = fx.b.ins().icmp_imm_s(IntCC::Equal, lead, 0xE0);
                                    let ed = fx.b.ins().icmp_imm_s(IntCC::Equal, lead, 0xED);
                                    let f0 = fx.b.ins().icmp_imm_s(IntCC::Equal, lead, 0xF0);
                                    let f4 = fx.b.ins().icmp_imm_s(IntCC::Equal, lead, 0xF4);
                                    let raised = fx.b.ins().bor(e0, f0);
                                    let lowered = fx.b.ins().bor(ed, f4);
                                    let base = fx.int(types::I64, 0x80);
                                    let top = fx.int(types::I64, 0xBF);
                                    let low_e0 = fx.int(types::I64, 0xA0);
                                    let low_f0 = fx.int(types::I64, 0x90);
                                    let high_ed = fx.int(types::I64, 0x9F);
                                    let high_f4 = fx.int(types::I64, 0x8F);
                                    let low_raised = fx.b.ins().select(e0, low_e0, low_f0);
                                    let low = fx.b.ins().select(raised, low_raised, base);
                                    let high_lowered = fx.b.ins().select(ed, high_ed, high_f4);
                                    let high = fx.b.ins().select(lowered, high_lowered, top);
                                    (low, high)
                                }
                                _ => (fx.int(types::I64, 0x80), fx.int(types::I64, 0xBF)),
                            };
                            let below = fx.b.ins().icmp(IntCC::UnsignedLessThan, byte, low);
                            let above = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, byte, high);
                            let wrong = fx.b.ins().bor(below, above);
                            fx.when(wrong, |fx| invalid(fx));
                        });
                    }
                    vec![end]
                },
                |_| vec![at],
            )[0];
            (inside, vec![next])
        });
        let yes = fx.int(types::I8, 1);
        fx.b.ins().return_(&[yes]);
    });
    call(lo, fx, function, &[start, len])[0]
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

fn decode_kind(
    lo: &mut Lowering,
    fx: &mut Fx,
    decoding: &Decoding,
    ty: &Ty,
    kind: &Kind,
    target: Value,
) {
    let reader_state = decoding.reader;
    match kind {
        Kind::Bool => {
            let (flag, failed) = match &decoding.format {
                DecodeFormat::Wire => wire::read_bool(fx, reader_state),
                DecodeFormat::Json => json::read_bool(lo, fx, reader_state),
                DecodeFormat::Construction => construction::read_bool(lo, fx, reader_state),
                DecodeFormat::Calls(decoder) => {
                    let (flag, failed) =
                        calls::read(lo, fx, decoder, reader_state, "read_bool", &[]);
                    (flag.scalar(), failed)
                }
            };
            check(lo, fx, failed, |_, _| {});
            fx.store(flag, target, 0);
        }
        Kind::Unsigned(scalar) | Kind::Signed(scalar) => {
            let signed = matches!(kind, Kind::Signed(_));
            let value = read_integer(lo, fx, decoding, signed);
            let (low, high) = scalar.range();
            let inside = match signed {
                true => {
                    let above =
                        fx.b.ins()
                            .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, value, low as i64);
                    let below =
                        fx.b.ins()
                            .icmp_imm_s(IntCC::SignedLessThanOrEqual, value, high as i64);
                    fx.b.ins().band(above, below)
                }
                false => {
                    fx.b.ins()
                        .icmp_imm_s(IntCC::UnsignedLessThanOrEqual, value, high as u64 as i64)
                }
            };
            let outside = fx.b.ins().bxor_imm_s(inside, 1);
            fx.when(outside, |fx| fail(fx, failure::OUT_OF_RANGE));
            let narrowed = super::super::place::resize(
                fx,
                value,
                super::super::layout::scalar_type(*scalar, lo.machine()),
            );
            fx.store(narrowed, target, 0);
        }
        Kind::Float(scalar) => {
            let float = super::super::layout::scalar_type(Scalar::F64, lo.machine());
            let (value, failed) = match &decoding.format {
                DecodeFormat::Wire => wire::read_float(fx, reader_state, float),
                DecodeFormat::Json => json::read_float(lo, fx, reader_state, float),
                DecodeFormat::Construction => construction::read_float(lo, fx, reader_state, float),
                DecodeFormat::Calls(decoder) => {
                    let (value, failed) =
                        calls::read(lo, fx, decoder, reader_state, "read_float", &[]);
                    (value.scalar(), failed)
                }
            };
            check(lo, fx, failed, |_, _| {});
            let value = match scalar {
                Scalar::F32 => super::floats::demote(fx, value),
                _ => value,
            };
            fx.store(value, target, 0);
        }
        Kind::Char => {
            let scalar = match &decoding.format {
                DecodeFormat::Wire => {
                    let value = read_integer(lo, fx, decoding, false);
                    let most = fx.int(types::I64, i64::from(u32::MAX));
                    fx.b.ins().umin(value, most)
                }
                DecodeFormat::Json => {
                    let (scalar, failed) = json::read_char(lo, fx, reader_state);
                    check(lo, fx, failed, |_, _| {});
                    scalar
                }
                DecodeFormat::Construction => {
                    let (scalar, failed) = construction::read_char(lo, fx, reader_state);
                    check(lo, fx, failed, |_, _| {});
                    scalar
                }
                DecodeFormat::Calls(decoder) => {
                    let scalar =
                        calls::read_scalar(lo, fx, decoder, reader_state, "read_char", |_, _| {});
                    fx.b.ins().uextend(types::I64, scalar)
                }
            };
            let surrogate = in_range(fx, scalar, 0xD800, 0xDFFF);
            let beyond =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedGreaterThan, scalar, 0x10_FFFF);
            let invalid = fx.b.ins().bor(surrogate, beyond);
            fx.when(invalid, |fx| fail(fx, failure::INVALID));
            let scalar = fx.b.ins().ireduce(types::I32, scalar);
            fx.store(scalar, target, 0);
        }
        Kind::Text(fixed) => decode_text(lo, fx, decoding, ty, *fixed, target),
        Kind::Bytes => {
            vector::empty(fx, target, decoding.alloc, BYTE);
            match &decoding.format {
                DecodeFormat::Wire => {
                    let (start, len, failed) = wire::read_framed(lo, fx, reader_state);
                    check(lo, fx, failed, |_, _| {});
                    let failed = fill(lo, fx, target, start, len);
                    check(lo, fx, failed, |_, _| {});
                }
                DecodeFormat::Json => {
                    let failed = json::read_base64(lo, fx, reader_state, target);
                    check(lo, fx, failed, |_, _| {});
                }
                DecodeFormat::Construction => {
                    let literal = construction::Literal::Bytes;
                    let (len, failed) =
                        construction::measure_literal(lo, fx, reader_state, literal);
                    check(lo, fx, failed, |_, _| {});
                    draw_measured(lo, fx, decoding, Measured::Literal(literal), len, target);
                }
                DecodeFormat::Calls(decoder) => {
                    calls::decode_text(lo, fx, decoder, reader_state, ty, None, false, target)
                }
            }
        }
        Kind::Sequence(element, form) => {
            decode_sequence(lo, fx, decoding, ty, element, *form, target)
        }
        Kind::Map(key, value) => decode_map(lo, fx, decoding, key, value, target),
        Kind::Contents(inner, shared) => {
            let layout = lo.layout(inner);
            let slot = fx.slot(layout.size, layout.align);
            let failed = decode_into(lo, fx, decoding, inner, slot);
            check(lo, fx, failed, |_, _| {});
            let held = lo.held_at(fx, inner, slot);
            let made = match shared {
                true => super::mem::stores::new_shared(lo, fx, decoding.alloc, inner, held),
                false => {
                    let size = fx.ptr(layout.size.max(1));
                    let align = fx.ptr(layout.align.max(1));
                    let address =
                        lo.call_routine(fx, Routine::Alloc, &[decoding.alloc, size, align])[0];
                    let drawn = fx.b.ins().icmp_imm_s(IntCC::NotEqual, address, 0);
                    fx.when(drawn, |fx| lo.store_held(fx, address, held, inner));
                    address
                }
            };
            let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, made, 0);
            fx.when(missing, |fx| {
                lo.release(fx, inner, slot);
                fail(fx, failure::OUT_OF_MEMORY);
            });
            fx.store(made, target, 0);
        }
        Kind::Id => {
            let value = read_integer(lo, fx, decoding, false);
            let outside =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedGreaterThan, value, i64::from(u32::MAX));
            fx.when(outside, |fx| fail(fx, failure::OUT_OF_RANGE));
            let index = fx.b.ins().ireduce(types::I32, value);
            fx.store(index, target, 0);
        }
        Kind::Record(record) => {
            decode_record(lo, fx, decoding, record, false, target);
            if let Some((item, args)) = &record.owner {
                check_predicates(lo, fx, ty, *item, args, target);
            }
        }
        Kind::Variant(cases) => decode_variant(lo, fx, decoding, ty, cases, target),
    }
}

/// An unsigned or signed value in 64 bits, returning from the decoding function on failure.
fn read_integer(lo: &mut Lowering, fx: &mut Fx, decoding: &Decoding, signed: bool) -> Value {
    let (value, failed) = match &decoding.format {
        DecodeFormat::Wire => wire::read_integer(lo, fx, decoding.reader, signed),
        DecodeFormat::Json => json::read_integer(lo, fx, decoding.reader, signed),
        DecodeFormat::Construction => construction::read_integer(lo, fx, decoding.reader, signed),
        DecodeFormat::Calls(decoder) => {
            let name = match signed {
                true => "read_signed",
                false => "read_unsigned",
            };
            let (value, failed) = calls::read(lo, fx, decoder, decoding.reader, name, &[]);
            (value.scalar(), failed)
        }
    };
    check(lo, fx, failed, |_, _| {});
    value
}

/// Copies bytes into the empty bytes or string at `target`, drawn for exactly them: the code of
/// exhaustion, zero otherwise.
fn fill(lo: &mut Lowering, fx: &mut Fx, target: Value, start: Value, len: Value) -> Value {
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, len, 0);
    fx.choose_in(
        lo,
        present,
        &[types::I32],
        |lo, fx| {
            let drawn = vector::resize(lo, fx, target, len, BYTE);
            fx.choose_in(
                lo,
                drawn,
                &[types::I32],
                |lo, fx| {
                    let data = fx.load(PTR, target, vector::DATA);
                    lo.call_routine(fx, Routine::Copy, &[data, start, len]);
                    fx.store(len, target, vector::LEN);
                    vec![fx.int(types::I32, 0)]
                },
                |_, fx| vec![fx.int(types::I32, failure::OUT_OF_MEMORY)],
            )
        },
        |_, fx| vec![fx.int(types::I32, 0)],
    )[0]
}

/// Fails with `Invalid`, releasing the struct value at `target`, unless each of its predicates
/// holds.
fn check_predicates(
    lo: &mut Lowering,
    fx: &mut Fx,
    ty: &Ty,
    item: crate::check::db::ItemId,
    args: &[Arg],
    target: Value,
) {
    for index in 0..lo.ck.predicate_count(item) {
        let instance = Instance::Nir {
            item,
            role: Role::Predicate,
            index,
            args: args.to_vec().into(),
        };
        let abi = lo.abi(&instance);
        let id = lo.instance(instance);
        let held = lo.held_at(fx, ty, target);
        let holds = lo
            .invoke(fx, super::super::call::Invoked::Direct(id), &abi, &[held])
            .result
            .scalar();
        let broken = fx.b.ins().icmp_imm_s(IntCC::Equal, holds, 0);
        let ty = ty.clone();
        fx.when(broken, |fx| {
            lo.release(fx, &ty, target);
            fail(fx, failure::INVALID);
        });
    }
}

/// Reads text, its bytes checked as UTF-8, into a string drawn through the allocator or a fixed
/// text of at most so many bytes.
fn decode_text(
    lo: &mut Lowering,
    fx: &mut Fx,
    decoding: &Decoding,
    ty: &Ty,
    fixed: Option<u64>,
    target: Value,
) {
    let reader_state = decoding.reader;
    let (source, len) = match &decoding.format {
        DecodeFormat::Wire => {
            let (start, len, failed) = wire::read_framed(lo, fx, reader_state);
            check(lo, fx, failed, |_, _| {});
            let valid = utf8_valid(lo, fx, start, len);
            let invalid = fx.b.ins().bxor_imm_s(valid, 1);
            fx.when(invalid, |fx| fail(fx, failure::INVALID_TEXT));
            (Measured::Input(start), len)
        }
        DecodeFormat::Json => {
            let (len, failed) = json::measure_string(lo, fx, reader_state);
            check(lo, fx, failed, |_, _| {});
            (Measured::Json, len)
        }
        DecodeFormat::Construction => {
            let literal = construction::Literal::Text;
            let (len, failed) = construction::measure_literal(lo, fx, reader_state, literal);
            check(lo, fx, failed, |_, _| {});
            (Measured::Literal(literal), len)
        }
        DecodeFormat::Calls(decoder) => {
            if fixed.is_none() {
                vector::empty(fx, target, decoding.alloc, BYTE);
            }
            calls::decode_text(lo, fx, decoder, reader_state, ty, fixed, true, target);
            return;
        }
    };
    match fixed {
        Some(capacity) => {
            let over =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedGreaterThan, len, capacity as i64);
            fx.when(over, |fx| fail(fx, failure::OUT_OF_RANGE));
            let len_at = lo.layouts.field(lo.ck, ty, None, "len").offset;
            let bytes_at = lo.layouts.field(lo.ck, ty, None, "bytes").offset;
            fx.store(len, target, len_at);
            let destination = fx.offset(target, bytes_at);
            write_measured(lo, fx, reader_state, source, destination, len);
        }
        None => {
            vector::empty(fx, target, decoding.alloc, BYTE);
            match source {
                Measured::Input(start) => {
                    let failed = fill(lo, fx, target, start, len);
                    check(lo, fx, failed, |_, _| {});
                }
                _ => draw_measured(lo, fx, decoding, source, len, target),
            }
        }
    }
}

/// Where the bytes of text read come from: the input, or a JSON string or construction
/// literal that follows, measured before.
#[derive(Clone, Copy)]
enum Measured {
    Input(Value),
    Json,
    Literal(construction::Literal),
}

/// Writes the `len` bytes of read text to `destination`.
fn write_measured(
    lo: &mut Lowering,
    fx: &mut Fx,
    reader_state: Value,
    source: Measured,
    destination: Value,
    len: Value,
) {
    match source {
        Measured::Input(start) => {
            lo.call_routine(fx, Routine::Copy, &[destination, start, len]);
        }
        Measured::Json => json::write_string(lo, fx, reader_state, destination, len),
        Measured::Literal(literal) => {
            construction::write_literal(lo, fx, reader_state, literal, destination, len)
        }
    }
}

/// Reads the `len` bytes of read text into the empty string or bytes at `target`, drawn for
/// exactly them.
fn draw_measured(
    lo: &mut Lowering,
    fx: &mut Fx,
    decoding: &Decoding,
    source: Measured,
    len: Value,
    target: Value,
) {
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, len, 0);
    fx.when(present, |fx| {
        let drawn = vector::resize(lo, fx, target, len, BYTE);
        let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, drawn, 0);
        fx.when(failed, |fx| fail(fx, failure::OUT_OF_MEMORY));
    });
    let data = fx.load(PTR, target, vector::DATA);
    write_measured(lo, fx, decoding.reader, source, data, len);
    fx.store(len, target, vector::LEN);
}

/// Whether the element at `index` of a sequence, map or record follows, returning from the
/// decoding function after `undo` on failure: in the wire format while fewer than its count
/// have been read; in the text formats until the closing byte, which is passed over, a comma
/// before each element after the first, and in construction text never when `count` is zero.
#[allow(clippy::too_many_arguments)]
fn element_follows(
    lo: &mut Lowering,
    fx: &mut Fx,
    decoding: &Decoding,
    index: Value,
    count: Value,
    close: u8,
    undo: impl FnOnce(&mut Lowering, &mut Fx),
) -> Value {
    match &decoding.format {
        DecodeFormat::Wire => fx.b.ins().icmp(IntCC::UnsignedLessThan, index, count),
        DecodeFormat::Json => {
            let (follows, failed) = json::element_follows(lo, fx, decoding.reader, index, close);
            check(lo, fx, failed, undo);
            follows
        }
        DecodeFormat::Construction => {
            let open = fx.b.ins().icmp_imm_s(IntCC::NotEqual, count, 0);
            let parts = fx.choose_in(
                lo,
                open,
                &[types::I8, types::I32],
                |lo, fx| {
                    let (follows, failed) =
                        json::element_follows(lo, fx, decoding.reader, index, close);
                    vec![follows, failed]
                },
                |_, fx| vec![fx.int(types::I8, 0), fx.int(types::I32, 0)],
            );
            check(lo, fx, parts[1], undo);
            parts[0]
        }
        DecodeFormat::Calls(decoder) => {
            calls::read_scalar(lo, fx, decoder, decoding.reader, "next_follows", undo)
        }
    }
}

/// Opens a sequence, map or record: the wire format's count, a text format's opening byte, or
/// the decoder's call `begin`, returning from the decoding function on failure.
fn open(lo: &mut Lowering, fx: &mut Fx, decoding: &Decoding, opening: u8, begin: &str) -> Value {
    match &decoding.format {
        DecodeFormat::Wire => {
            let (count, failed) = wire::read_count(lo, fx, decoding.reader);
            check(lo, fx, failed, |_, _| {});
            count
        }
        DecodeFormat::Json => {
            let failed = json::expect_code(lo, fx, decoding.reader, opening);
            check(lo, fx, failed, |_, _| {});
            fx.ptr(0)
        }
        DecodeFormat::Construction => {
            let failed = json::expect_code(lo, fx, decoding.reader, opening);
            check(lo, fx, failed, |_, _| {});
            fx.ptr(1)
        }
        DecodeFormat::Calls(decoder) => {
            let (_, failed) = calls::read(lo, fx, decoder, decoding.reader, begin, &[]);
            check(lo, fx, failed, |_, _| {});
            fx.ptr(0)
        }
    }
}

/// Ends a sequence, map, record or variant read through a decoder's calls with its `end`,
/// returning from the decoding function after `undo` on failure; a format ends each as its
/// last part is read.
fn end(
    lo: &mut Lowering,
    fx: &mut Fx,
    decoding: &Decoding,
    undo: impl FnOnce(&mut Lowering, &mut Fx),
) {
    if let DecodeFormat::Calls(decoder) = &decoding.format {
        let (_, failed) = calls::read(lo, fx, decoder, decoding.reader, "end", &[]);
        check(lo, fx, failed, undo);
    }
}

/// Reads a sequence: an array of exactly its element count, a list, a set whose elements
/// ascend strictly, or a bounded list of at most its capacity. Elements past an array's or a
/// bounded list's room are read and released, the count checked once every element is read.
fn decode_sequence(
    lo: &mut Lowering,
    fx: &mut Fx,
    decoding: &Decoding,
    ty: &Ty,
    element: &Ty,
    form: Form,
    target: Value,
) {
    let reader_state = decoding.reader;
    nest(fx, reader_state);
    let count = open(lo, fx, decoding, b'[', "begin_sequence");
    let shape = Element::of(lo, element);
    let element_layout = lo.layout(element);
    let scratch = fx.slot(element_layout.size, element_layout.align);
    let (storage, len_at, room) = match form {
        Form::Array(count) => (target, None, Some(count)),
        Form::Bounded(capacity) => {
            let len_at = lo.layouts.field(lo.ck, ty, None, "len").offset;
            let items_at = lo.layouts.field(lo.ck, ty, None, "items").offset;
            let zero = fx.ptr(0);
            fx.store(zero, target, len_at);
            (fx.offset(target, items_at), Some(len_at), Some(capacity))
        }
        Form::List | Form::Set => {
            vector::empty(fx, target, decoding.alloc, shape);
            (target, None, None)
        }
    };
    let release_read = |lo: &mut Lowering, fx: &mut Fx, read: Value| {
        let first = match form {
            Form::List | Form::Set => fx.load(PTR, target, vector::DATA),
            _ => storage,
        };
        let kept = match room {
            Some(room) => {
                let room = fx.ptr(room);
                fx.b.ins().umin(read, room)
            }
            None => read,
        };
        if lo.needs_release(element) {
            let zero = fx.ptr(0);
            fx.each(zero, kept, |fx, index| {
                let at = shape.at(fx, first, index);
                lo.release(fx, element, at);
            });
        }
        if let Form::List | Form::Set = form {
            vector::free_storage(lo, fx, target);
        }
    };
    let zero = fx.ptr(0);
    let read = fx.repeat(&[zero], |fx, state| {
        let index = state[0];
        let follows = element_follows(lo, fx, decoding, index, count, b']', |lo, fx| {
            release_read(lo, fx, index)
        });
        fx.when(follows, |fx| {
            let slot = match (form, room) {
                (Form::List | Form::Set, _) => {
                    let needed = fx.b.ins().iadd_imm_s(index, 1);
                    let drawn = vector::reserve(lo, fx, target, needed, shape);
                    let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, drawn, 0);
                    fx.when(failed, |fx| {
                        release_read(lo, fx, index);
                        fail(fx, failure::OUT_OF_MEMORY);
                    });
                    let data = fx.load(PTR, target, vector::DATA);
                    shape.at(fx, data, index)
                }
                (_, Some(room)) => {
                    let inside =
                        fx.b.ins()
                            .icmp_imm_s(IntCC::UnsignedLessThan, index, room as i64);
                    let at = shape.at(fx, storage, index);
                    fx.b.ins().select(inside, at, scratch)
                }
                _ => unreachable!("an array or bounded list has room"),
            };
            let failed = decode_into(lo, fx, decoding, element, slot);
            check(lo, fx, failed, |lo, fx| release_read(lo, fx, index));
            if let Some(room) = room {
                let outside =
                    fx.b.ins()
                        .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, index, room as i64);
                fx.when(outside, |fx| lo.release(fx, element, scratch));
            }
            let next = fx.b.ins().iadd_imm_s(index, 1);
            match (form, len_at, room) {
                (Form::List | Form::Set, _, _) => fx.store(next, target, vector::LEN),
                (_, Some(len_at), Some(room)) => {
                    let room = fx.ptr(room);
                    let kept = fx.b.ins().umin(next, room);
                    fx.store(kept, target, len_at);
                }
                _ => {}
            }
        });
        let next = fx.b.ins().iadd_imm_s(index, 1);
        let next = fx.b.ins().select(follows, next, index);
        (follows, vec![next])
    })[0];
    end(lo, fx, decoding, |lo, fx| release_read(lo, fx, read));
    unnest(fx, reader_state);
    match form {
        Form::Array(count) => {
            let wrong = fx.b.ins().icmp_imm_s(IntCC::NotEqual, read, count as i64);
            fx.when(wrong, |fx| {
                release_read(lo, fx, read);
                fail(fx, failure::MALFORMED);
            });
        }
        Form::Bounded(capacity) => {
            let over =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedGreaterThan, read, capacity as i64);
            fx.when(over, |fx| {
                release_read(lo, fx, read);
                fail(fx, failure::OUT_OF_RANGE);
            });
        }
        Form::Set => {
            let data = fx.load(PTR, target, vector::DATA);
            ascending(lo, fx, element, shape, data, 0, read, |lo, fx| {
                release_read(lo, fx, read);
            });
        }
        Form::List => {}
    }
}

/// Fails with `DuplicateKey` on two equal neighbouring keys and with `Malformed` on a key less
/// than the one before it, among `count` entries from `first` whose keys lie at `key_at`,
/// first running `undo`.
#[allow(clippy::too_many_arguments)]
fn ascending(
    lo: &mut Lowering,
    fx: &mut Fx,
    key: &Ty,
    shape: Element,
    first: Value,
    key_at: u64,
    count: Value,
    undo: impl Fn(&mut Lowering, &mut Fx),
) {
    let one = fx.ptr(1);
    fx.each(one, count, |fx, index| {
        let previous = fx.b.ins().iadd_imm_s(index, -1);
        let earlier = shape.at(fx, first, previous);
        let earlier = fx.offset(earlier, key_at);
        let later = shape.at(fx, first, index);
        let later = fx.offset(later, key_at);
        let order = lo.order(fx, key, earlier, later);
        let equal = fx.b.ins().icmp_imm_s(IntCC::Equal, order, 1);
        fx.when(equal, |fx| {
            undo(lo, fx);
            fail(fx, failure::DUPLICATE_KEY);
        });
        let descending = fx.b.ins().icmp_imm_s(IntCC::Equal, order, 2);
        fx.when(descending, |fx| {
            undo(lo, fx);
            fail(fx, failure::MALFORMED);
        });
    });
}

/// Reads a map: its entries, each a key then a value, whose keys ascend strictly.
fn decode_map(
    lo: &mut Lowering,
    fx: &mut Fx,
    decoding: &Decoding,
    key: &Ty,
    value: &Ty,
    target: Value,
) {
    let reader_state = decoding.reader;
    let entries = Entries::of_map(lo, key.clone(), value.clone());
    let value_at = lo
        .layouts
        .field(lo.ck, &entries.entry, None, "value")
        .offset;
    let construction = decoding.format == DecodeFormat::Construction;
    let (opening, close) = match construction {
        true => (b'{', b'}'),
        false => (b'[', b']'),
    };
    nest(fx, reader_state);
    let count = open(lo, fx, decoding, opening, "begin_map");
    vector::empty(fx, target, decoding.alloc, entries.shape);
    let entry_ty = entries.entry.clone();
    let release_read = |lo: &mut Lowering, fx: &mut Fx| {
        let zero = fx.ptr(0);
        vector::release(lo, fx, target, &entry_ty, zero);
    };
    let json = decoding.format == DecodeFormat::Json;
    let zero = fx.ptr(0);
    let read = fx.repeat(&[zero], |fx, state| {
        let index = state[0];
        let follows = element_follows(lo, fx, decoding, index, count, close, |lo, fx| {
            release_read(lo, fx)
        });
        fx.when(follows, |fx| {
            let needed = fx.b.ins().iadd_imm_s(index, 1);
            let drawn = vector::reserve(lo, fx, target, needed, entries.shape);
            let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, drawn, 0);
            fx.when(failed, |fx| {
                release_read(lo, fx);
                fail(fx, failure::OUT_OF_MEMORY);
            });
            let data = fx.load(PTR, target, vector::DATA);
            let entry = entries.shape.at(fx, data, index);
            if json {
                let failed = json::expect_code(lo, fx, reader_state, b'[');
                check(lo, fx, failed, |lo, fx| release_read(lo, fx));
            }
            let key_slot = fx.offset(entry, entries.key_at);
            let failed = decode_into(lo, fx, decoding, key, key_slot);
            check(lo, fx, failed, |lo, fx| release_read(lo, fx));
            let release_key = |lo: &mut Lowering, fx: &mut Fx| {
                lo.release(fx, key, key_slot);
                release_read(lo, fx);
            };
            if json || construction {
                let separator = if json { b',' } else { b':' };
                let failed = json::expect_code(lo, fx, reader_state, separator);
                check(lo, fx, failed, release_key);
            }
            let value_slot = fx.offset(entry, value_at);
            let failed = decode_into(lo, fx, decoding, value, value_slot);
            check(lo, fx, failed, release_key);
            fx.store(needed, target, vector::LEN);
            if json {
                let failed = json::expect_code(lo, fx, reader_state, b']');
                check(lo, fx, failed, |lo, fx| release_read(lo, fx));
            }
        });
        let next = fx.b.ins().iadd_imm_s(index, 1);
        let next = fx.b.ins().select(follows, next, index);
        (follows, vec![next])
    })[0];
    end(lo, fx, decoding, |lo, fx| release_read(lo, fx));
    unnest(fx, reader_state);
    let data = fx.load(PTR, target, vector::DATA);
    ascending(
        lo,
        fx,
        key,
        entries.shape,
        data,
        entries.key_at,
        read,
        |lo, fx| release_read(lo, fx),
    );
}

/// The longest key or alias of a record, plus one: a longer key names none of its fields.
fn key_room(record: &Record) -> u64 {
    let longest = record
        .fields
        .iter()
        .map(|(key, _, _)| key.len())
        .chain(record.aliases.iter().map(|(old, _)| old.len()))
        .max()
        .unwrap_or(0);
    longest as u64 + 1
}

/// Reads a record, or a variant's labelled payload, into the fields at their offsets from
/// `target`, as the decoding rules state for unknown, repeated and missing keys; on failure
/// every field read is released.
fn decode_record(
    lo: &mut Lowering,
    fx: &mut Fx,
    decoding: &Decoding,
    record: &Record,
    labelled: bool,
    target: Value,
) {
    let reader_state = decoding.reader;
    let count = record.fields.len();
    let read = fx.slot(count.max(1) as u64, 1);
    let zero = fx.ptr(0);
    let size = fx.ptr(count.max(1) as u64);
    lo.call_routine(fx, Routine::Fill, &[read, zero, size]);
    let fields = record.fields.clone();
    let kept = (record.kept.is_some() && decoding.format == DecodeFormat::Wire).then(|| {
        let kept = fx.slot(vector::SIZE, 8);
        vector::empty(fx, kept, decoding.alloc, BYTE);
        kept
    });
    let release_read = |lo: &mut Lowering, fx: &mut Fx| {
        if let Some(kept) = kept {
            vector::free_storage(lo, fx, kept);
        }
        for (position, (_, field_ty, offset)) in fields.iter().enumerate() {
            if !lo.needs_release(field_ty) {
                continue;
            }
            let flag = fx.load(types::I8, read, position as u64);
            fx.when(flag, |fx| {
                let at = fx.offset(target, *offset);
                lo.release(fx, field_ty, at);
            });
        }
    };
    nest(fx, reader_state);
    let room = key_room(record);
    let key_buffer = fx.slot(room, 1);
    let left = match (&decoding.format, labelled) {
        (DecodeFormat::Construction, true) => fx.ptr(1),
        (DecodeFormat::Construction, false) => {
            let (parens, failed) = construction::open_record(lo, fx, reader_state);
            check(lo, fx, failed, |_, _| {});
            fx.b.ins().uextend(PTR, parens)
        }
        _ => open(lo, fx, decoding, b'{', "begin_record"),
    };
    let close = match &decoding.format {
        DecodeFormat::Construction => b')',
        _ => b'}',
    };
    let last = fx.slot(16, 8);
    let none = fx.int(PTR, -1);
    fx.store(none, last, 8);
    let zero = fx.ptr(0);
    fx.repeat(&[zero], |fx, state| {
        let index = state[0];
        let follows = element_follows(lo, fx, decoding, index, left, close, release_read);
        fx.when(follows, |fx| {
            let entry = kept.map(|_| fx.load(PTR, reader_state, reader::POS));
            let (matched, value_end) = match &decoding.format {
                DecodeFormat::Wire => {
                    let (start, len, end, failed) = wire::read_key(lo, fx, reader_state, last);
                    check(lo, fx, failed, release_read);
                    (match_key(lo, fx, record, start, len), Some(end))
                }
                DecodeFormat::Json => {
                    let capacity = fx.ptr(room);
                    let (len, failed) = json::read_key(lo, fx, reader_state, key_buffer, capacity);
                    check(lo, fx, failed, release_read);
                    (match_key(lo, fx, record, key_buffer, len), None)
                }
                DecodeFormat::Construction => {
                    let capacity = fx.ptr(room);
                    let (len, failed) =
                        construction::read_key(lo, fx, reader_state, key_buffer, capacity);
                    check(lo, fx, failed, release_read);
                    (match_key(lo, fx, record, key_buffer, len), None)
                }
                DecodeFormat::Calls(decoder) => {
                    let names: Vec<&[u8]> = record
                        .fields
                        .iter()
                        .map(|(key, _, _)| key.as_bytes())
                        .collect();
                    let matched = calls::read_key(
                        lo,
                        fx,
                        decoder,
                        reader_state,
                        "read_key",
                        &names,
                        &record.aliases,
                        release_read,
                    );
                    (matched, None)
                }
            };
            let unknown = fx.b.ins().icmp_imm_s(IntCC::Equal, matched, -1);
            fx.when(unknown, |fx| {
                if !record.open {
                    release_read(lo, fx);
                    fail(fx, failure::UNKNOWN_KEY);
                    return;
                }
                match value_end {
                    Some(end) => {
                        if let (Some(kept), Some(entry)) = (kept, entry) {
                            let len = fx.load(PTR, kept, vector::LEN);
                            let size = fx.b.ins().isub(end, entry);
                            let needed = fx.b.ins().iadd(len, size);
                            let drawn = vector::reserve(lo, fx, kept, needed, BYTE);
                            let exhausted = fx.b.ins().icmp_imm_s(IntCC::Equal, drawn, 0);
                            fx.when(exhausted, |fx| {
                                release_read(lo, fx);
                                fail(fx, failure::OUT_OF_MEMORY);
                            });
                            let data = fx.load(PTR, kept, vector::DATA);
                            let at = fx.b.ins().iadd(data, len);
                            lo.call_routine(fx, Routine::Copy, &[at, entry, size]);
                            fx.store(needed, kept, vector::LEN);
                        }
                        fx.store(end, reader_state, reader::POS);
                    }
                    None => {
                        let failed = match &decoding.format {
                            DecodeFormat::Construction => {
                                construction::skip_value(lo, fx, reader_state)
                            }
                            DecodeFormat::Calls(decoder) => {
                                calls::read(lo, fx, decoder, reader_state, "skip", &[]).1
                            }
                            _ => json::skip_value(lo, fx, reader_state),
                        };
                        check(lo, fx, failed, release_read);
                    }
                }
            });
            let known = fx.b.ins().bxor_imm_s(unknown, 1);
            fx.when(known, |fx| {
                for (position, (_, field_ty, offset)) in record.fields.iter().enumerate() {
                    let this =
                        fx.b.ins()
                            .icmp_imm_s(IntCC::Equal, matched, position as i64);
                    fx.when(this, |fx| {
                        let flag = fx.load(types::I8, read, position as u64);
                        fx.when(flag, |fx| {
                            release_read(lo, fx);
                            fail(fx, failure::DUPLICATE_KEY);
                        });
                        let at = fx.offset(target, *offset);
                        let failed = decode_into(lo, fx, decoding, field_ty, at);
                        check(lo, fx, failed, release_read);
                        let one = fx.int(types::I8, 1);
                        fx.store(one, read, position as u64);
                    });
                }
                if let Some(end) = value_end {
                    let pos = fx.load(PTR, reader_state, reader::POS);
                    let short = fx.b.ins().icmp(IntCC::NotEqual, pos, end);
                    fx.when(short, |fx| {
                        release_read(lo, fx);
                        fail(fx, failure::MALFORMED);
                    });
                }
            });
        });
        let next = fx.b.ins().iadd_imm_s(index, 1);
        (follows, vec![next])
    });
    end(lo, fx, decoding, release_read);
    unnest(fx, reader_state);
    let default = lo.core_interface("Default");
    for (position, (_, field_ty, offset)) in record.fields.iter().enumerate() {
        let flag = fx.load(types::I8, read, position as u64);
        let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, flag, 0);
        let defaults = record.open && lo.ck.conformance(field_ty, default).is_some();
        fx.when(missing, |fx| {
            if !defaults {
                release_read(lo, fx);
                fail(fx, failure::MISSING_FIELD);
                return;
            }
            let at = fx.offset(target, *offset);
            let made = lo.call_method(fx, field_ty, "Default", Vec::new(), &[]);
            lo.store_held(fx, at, made, field_ty);
            let one = fx.int(types::I8, 1);
            fx.store(one, read, position as u64);
        });
    }
    if let Some((offset, kept_ty)) = &record.kept {
        let at = fx.offset(target, *offset);
        let empty = lo.variant_value(fx, kept_ty, "Empty", &[], None);
        lo.store_held(fx, at, empty, kept_ty);
        if let Some(kept) = kept {
            let len = fx.load(PTR, kept, vector::LEN);
            let held = fx.b.ins().icmp_imm_s(IntCC::NotEqual, len, 0);
            fx.when(held, |fx| {
                let fields = lo.variant_value(fx, kept_ty, "Fields", &[Held::Address(kept)], None);
                lo.store_held(fx, at, fields, kept_ty);
            });
        }
    }
}

/// The position among a record's fields of the one a key or one of its aliases names, -1 for
/// none.
fn match_key(lo: &mut Lowering, fx: &mut Fx, record: &Record, start: Value, len: Value) -> Value {
    let mut matched = fx.int(PTR, -1);
    for (position, (key, _, _)) in record.fields.iter().enumerate() {
        let mut names = vec![key.as_bytes().to_vec()];
        names.extend(
            record
                .aliases
                .iter()
                .filter(|(_, current)| current.as_slice() == key.as_bytes())
                .map(|(old, _)| old.clone()),
        );
        for name in names {
            let same = bytes_equal(lo, fx, start, len, &name);
            let position = fx.int(PTR, position as i64);
            matched = fx.b.ins().select(same, position, matched);
        }
    }
    matched
}

/// Whether a byte range holds exactly a literal's bytes.
fn bytes_equal(lo: &mut Lowering, fx: &mut Fx, start: Value, len: Value, literal: &[u8]) -> Value {
    let same_len =
        fx.b.ins()
            .icmp_imm_s(IntCC::Equal, len, literal.len() as i64);
    if literal.is_empty() {
        return same_len;
    }
    fx.choose_in(
        lo,
        same_len,
        &[types::I8],
        |lo, fx| {
            let view = lo.view_literal(fx, literal).address();
            let expected = fx.load(PTR, view, 0);
            let order = lo.call_routine(fx, Routine::Compare, &[start, expected, len])[0];
            vec![fx.b.ins().icmp_imm_s(IntCC::Equal, order, 0)]
        },
        |_, fx| vec![fx.int(types::I8, 0)],
    )[0]
}

/// Reads a variant of an enum into `target`: its key, then its payload when the variant has
/// one, which must follow exactly then.
fn decode_variant(
    lo: &mut Lowering,
    fx: &mut Fx,
    decoding: &Decoding,
    ty: &Ty,
    cases: &[Case],
    target: Value,
) {
    let reader_state = decoding.reader;
    let record = Record {
        fields: cases
            .iter()
            .map(|case| (case.key.clone(), Ty::Unit, 0))
            .collect(),
        open: false,
        aliases: match ty {
            Ty::Item(item, _) => lo.ck.key_aliases(*item).into_iter().collect(),
            _ => Vec::new(),
        },
        owner: None,
        kept: None,
    };
    nest(fx, reader_state);
    let room = key_room(&record);
    let key_buffer = fx.slot(room, 1);
    let (matched, text_payload) = match &decoding.format {
        DecodeFormat::Wire => {
            let (start, len, failed) = wire::read_framed(lo, fx, reader_state);
            check(lo, fx, failed, |_, _| {});
            (match_key(lo, fx, &record, start, len), None)
        }
        DecodeFormat::Json => {
            let capacity = fx.ptr(room);
            let (len, payload, failed) =
                json::read_variant(lo, fx, reader_state, key_buffer, capacity);
            check(lo, fx, failed, |_, _| {});
            (match_key(lo, fx, &record, key_buffer, len), Some(payload))
        }
        DecodeFormat::Construction => {
            let capacity = fx.ptr(room);
            let (len, payload, failed) =
                construction::read_variant(lo, fx, reader_state, key_buffer, capacity);
            check(lo, fx, failed, |_, _| {});
            (match_key(lo, fx, &record, key_buffer, len), Some(payload))
        }
        DecodeFormat::Calls(decoder) => {
            let names: Vec<&[u8]> = cases.iter().map(|case| case.key.as_bytes()).collect();
            let matched = calls::read_key(
                lo,
                fx,
                decoder,
                reader_state,
                "read_variant",
                &names,
                &record.aliases,
                |_, _| {},
            );
            (matched, None)
        }
    };
    let unknown = fx.b.ins().icmp_imm_s(IntCC::Equal, matched, -1);
    fx.when(unknown, |fx| fail(fx, failure::UNKNOWN_VARIANT));
    let (follows, payload_end) = match &decoding.format {
        DecodeFormat::Wire => {
            let (follows, end, failed) = wire::read_payload_flag(lo, fx, reader_state);
            check(lo, fx, failed, |_, _| {});
            (follows, Some(end))
        }
        DecodeFormat::Calls(decoder) => {
            let follows =
                calls::read_scalar(lo, fx, decoder, reader_state, "payload_follows", |_, _| {});
            (follows, None)
        }
        _ => (text_payload.expect("a text variant's form"), None),
    };
    for (position, case) in cases.iter().enumerate() {
        let this =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, matched, position as i64);
        fx.when(this, |fx| {
            let expected = !matches!(case.payload, CasePayload::None);
            let mismatch = match expected {
                true => fx.b.ins().bxor_imm_s(follows, 1),
                false => follows,
            };
            fx.when(mismatch, |fx| fail(fx, failure::MALFORMED));
            match &case.payload {
                CasePayload::None => {}
                CasePayload::Single(payload_ty) => {
                    let payload = lo.payload_address(fx, ty, &case.key, "", target);
                    let failed = decode_into(lo, fx, decoding, payload_ty, payload);
                    check(lo, fx, failed, |_, _| {});
                }
                CasePayload::Labelled(labelled) => {
                    decode_record(lo, fx, decoding, labelled, true, target)
                }
            }
            let release_payload = |lo: &mut Lowering, fx: &mut Fx| match &case.payload {
                CasePayload::Single(payload_ty) => {
                    let payload = lo.payload_address(fx, ty, &case.key, "", target);
                    lo.release(fx, payload_ty, payload);
                }
                CasePayload::Labelled(labelled) => {
                    for (_, field_ty, offset) in &labelled.fields {
                        let at = fx.offset(target, *offset);
                        lo.release(fx, field_ty, at);
                    }
                }
                CasePayload::None => {}
            };
            let closing = match (&decoding.format, &case.payload) {
                (DecodeFormat::Json, _) => Some(b'}'),
                (DecodeFormat::Construction, CasePayload::Single(_)) => Some(b')'),
                _ => None,
            };
            if expected {
                if let Some(end) = payload_end {
                    let pos = fx.load(PTR, reader_state, reader::POS);
                    let short = fx.b.ins().icmp(IntCC::NotEqual, pos, end);
                    fx.when(short, |fx| {
                        release_payload(lo, fx);
                        fail(fx, failure::MALFORMED);
                    });
                }
                if let Some(closing) = closing {
                    let failed = json::expect_code(lo, fx, reader_state, closing);
                    check(lo, fx, failed, release_payload);
                }
            }
            end(lo, fx, decoding, release_payload);
            lo.write_variant_tag(fx, ty, target, &case.key);
        });
    }
    unnest(fx, reader_state);
}
