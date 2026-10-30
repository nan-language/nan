//! The codecs the interpreter supplies, `mem::wire::Codec` and `text::json::Codec`. A value
//! encodes into the tree of kinds `core::Encoder` writes, which each format then writes out;
//! decoding reads a format through the calls of `core::Decoder`, in the order the std
//! encodings and derived decoders make them.

use std::collections::BTreeMap;
use std::rc::Rc;

use super::super::value::Val;
use super::super::{End, Exec, Flow, Interpreter, Site};
use super::collections::{conformance, ordering};
use super::render::float_text;
use super::stores::{boxed, cell_of, counted, entries, exhausted, holding, place};
use crate::check::db::ItemId;
use crate::check::sig::{Payload, SigKind};
use crate::check::ty::{Arg, Const, Scalar, Subst, Ty};
use crate::nir::Span;
use crate::syntax::ast::TypeModifier;

/// The nesting of sequences, maps, records and variants past which decoding fails.
const MAX_DEPTH: usize = 128;

/// A codec operation; none for another member.
pub fn member(
    interp: &mut Interpreter,
    owner: &str,
    member: &str,
    type_args: &[Arg],
    args: &[Val],
    at: (ItemId, Span),
    site: Site,
) -> Option<Exec<Flow>> {
    let json = match owner {
        "mem::wire::Codec" => false,
        "text::json::Codec" => true,
        _ => return None,
    };
    let ty = type_args
        .first()
        .and_then(Arg::ty)
        .cloned()
        .unwrap_or(Ty::Unit);
    Some(match member {
        "encode" | "try_encode" => {
            let tree = encoded(interp, &args[0], &ty);
            let mut out = Vec::new();
            match json {
                true => write_json(&tree, &mut out),
                false => write_wire(&tree, &mut out),
            }
            let bytes: Vec<Val> = out
                .into_iter()
                .map(|byte| Val::Int(i128::from(byte)))
                .collect();
            let target = place(&args[1]);
            let byte = Ty::Scalar(Scalar::U8);
            match super::stores::append(interp, &target, &bytes, &byte) {
                Some(()) => Ok(Flow::Return(Val::Unit)),
                None => exhausted(interp, member == "try_encode", at),
            }
        }
        "decode" => {
            let raw = interp.deref(&args[0]).fields()[0].clone();
            let input = interp.bytes(&raw);
            let cell = cell_of(interp, &args[1]);
            let mut reader: Box<dyn Reader> = match json {
                true => Box::new(JsonReader::new(input)),
                false => Box::new(WireReader::new(input)),
            };
            let mut decoding = Decoding {
                interp,
                reader: reader.as_mut(),
                cell,
                depth: 0,
                at,
                site,
            };
            let decoded = decoding.value(&ty).and_then(|value| {
                decoding.reader.finish()?;
                Ok(value)
            });
            match decoded {
                Ok(value) => Ok(Flow::Return(value)),
                Err(Failure::Decode(kind)) => {
                    let error = interp.ty_item("core", "DecodeError");
                    let index = interp.variant_index(error, kind);
                    Ok(Flow::Fail(Val::Variant(error, index, Rc::new(Vec::new()))))
                }
                Err(Failure::Run(end)) => Err(end),
            }
        }
        _ => return None,
    })
}

/// An encoded value, as the kinds of `core::Encoder` write it.
enum Tree {
    Bool(bool),
    Unsigned(u64),
    Signed(i64),
    Float(f64),
    Char(char),
    Text(Vec<u8>),
    Bytes(Vec<u8>),
    Sequence(Vec<Tree>),
    Map(Vec<(Tree, Tree)>),
    /// Fields by key, in ascending byte order of keys.
    Record(Vec<(Vec<u8>, Tree)>),
    Variant(Vec<u8>, Option<Box<Tree>>),
}

/// The encoded tree of a value of a type.
fn encoded(interp: &Interpreter, value: &Val, ty: &Ty) -> Tree {
    let value = interp.deref(value);
    match ty {
        Ty::Scalar(scalar) => match (scalar, &value) {
            (Scalar::Bool, _) => Tree::Bool(value.bool()),
            (Scalar::Char, Val::Char(c)) => Tree::Char(*c),
            (_, Val::F32(float)) => Tree::Float(f64::from(*float)),
            (_, Val::F64(float)) => Tree::Float(*float),
            _ if scalar.is_signed() => Tree::Signed(value.int() as i64),
            _ => Tree::Unsigned(value.int() as u64),
        },
        Ty::Unit => Tree::Record(Vec::new()),
        Ty::Item(item, args) => {
            let arg = |index: usize| {
                args.get(index)
                    .and_then(Arg::ty)
                    .cloned()
                    .unwrap_or(Ty::Unit)
            };
            match interp.ck.db.canonical(*item).as_str() {
                "core::Array" => Tree::Sequence(
                    value
                        .fields()
                        .iter()
                        .map(|element| encoded(interp, element, &arg(0)))
                        .collect(),
                ),
                "text::String" => Tree::Text(bytes_of(&entries(interp, &value))),
                "text::Fixed" => {
                    let len = value.fields()[1].int() as usize;
                    Tree::Text(bytes_of(&value.fields()[0].fields()[..len]))
                }
                "mem::Bytes" => Tree::Bytes(bytes_of(&entries(interp, &value))),
                "mem::List" | "mem::Set" => Tree::Sequence(
                    entries(interp, &value)
                        .iter()
                        .map(|element| encoded(interp, element, &arg(0)))
                        .collect(),
                ),
                "mem::Bounded" => Tree::Sequence(
                    value.fields()[0]
                        .fields()
                        .iter()
                        .map(|element| encoded(interp, element, &arg(0)))
                        .collect(),
                ),
                "mem::Map" => Tree::Map(
                    entries(interp, &value)
                        .iter()
                        .map(|entry| {
                            let parts = entry.fields();
                            (
                                encoded(interp, &parts[0], &arg(0)),
                                encoded(interp, &parts[1], &arg(1)),
                            )
                        })
                        .collect(),
                ),
                "mem::Box" | "mem::Shared" | "core::Untrusted" => {
                    encoded(interp, &value.fields()[0], &arg(0))
                }
                "mem::Id" => Tree::Unsigned(value.fields()[0].int() as u64),
                _ => {
                    match &interp.ck.sig(*item).kind {
                        SigKind::Struct { fields } => {
                            let subst = Subst::of(
                                interp.ck.sig(*item).generics.iter().map(|def| def.param),
                                args,
                            );
                            let named = fields.iter().zip(value.fields()).map(|(field, part)| {
                                (field.name.as_str(), part, subst.ty(&field.ty))
                            });
                            record(interp, named)
                        }
                        SigKind::Enum { variants } => {
                            let Val::Variant(_, index, payload) = &value else {
                                panic!("an enum value");
                            };
                            let subst = Subst::of(
                                interp.ck.sig(*item).generics.iter().map(|def| def.param),
                                args,
                            );
                            let variant = &variants[*index as usize];
                            let encoded_payload = match &variant.payload {
                                Payload::None => None,
                                Payload::Single(ty) => {
                                    Some(Box::new(encoded(interp, &payload[0], &subst.ty(ty))))
                                }
                                Payload::Labelled(labels) => {
                                    let named = labels.iter().zip(payload.iter()).map(
                                        |((name, ty), part)| (name.as_str(), part, subst.ty(ty)),
                                    );
                                    Some(Box::new(record(interp, named)))
                                }
                            };
                            Tree::Variant(variant.name.clone().into_bytes(), encoded_payload)
                        }
                        other => panic!("an Encode type of kind {other:?}"),
                    }
                }
            }
        }
        other => panic!("an Encode value of {other:?}"),
    }
}

/// A record of named values, in ascending byte order of their keys.
fn record<'v>(interp: &Interpreter, named: impl Iterator<Item = (&'v str, &'v Val, Ty)>) -> Tree {
    let mut fields: Vec<(Vec<u8>, Tree)> = named
        .map(|(name, value, ty)| (name.as_bytes().to_vec(), encoded(interp, value, &ty)))
        .collect();
    fields.sort_by(|left, right| left.0.cmp(&right.0));
    Tree::Record(fields)
}

fn bytes_of(values: &[Val]) -> Vec<u8> {
    values.iter().map(|byte| byte.int() as u8).collect()
}

/// Writes a uvar: LEB128, least significant group first.
fn write_uvar(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let group = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(group);
            return;
        }
        out.push(group | 0x80);
    }
}

fn write_wire(tree: &Tree, out: &mut Vec<u8>) {
    let framed = |text: &[u8], out: &mut Vec<u8>| {
        write_uvar(text.len() as u64, out);
        out.extend_from_slice(text);
    };
    match tree {
        Tree::Bool(value) => out.push(u8::from(*value)),
        Tree::Unsigned(value) => write_uvar(*value, out),
        Tree::Signed(value) => write_uvar(((value << 1) ^ (value >> 63)) as u64, out),
        Tree::Float(value) => out.extend_from_slice(&value.to_bits().to_le_bytes()),
        Tree::Char(c) => write_uvar(u64::from(u32::from(*c)), out),
        Tree::Text(bytes) | Tree::Bytes(bytes) => framed(bytes, out),
        Tree::Sequence(elements) => {
            write_uvar(elements.len() as u64, out);
            elements.iter().for_each(|element| write_wire(element, out));
        }
        Tree::Map(entries) => {
            write_uvar(entries.len() as u64, out);
            for (key, value) in entries {
                write_wire(key, out);
                write_wire(value, out);
            }
        }
        Tree::Record(fields) => {
            write_uvar(fields.len() as u64, out);
            for (key, value) in fields {
                framed(key, out);
                let mut written = Vec::new();
                write_wire(value, &mut written);
                framed(&written, out);
            }
        }
        Tree::Variant(key, payload) => {
            framed(key, out);
            match payload {
                None => out.push(0),
                Some(payload) => {
                    out.push(1);
                    let mut written = Vec::new();
                    write_wire(payload, &mut written);
                    framed(&written, out);
                }
            }
        }
    }
}

fn write_json(tree: &Tree, out: &mut Vec<u8>) {
    match tree {
        Tree::Bool(value) => out.extend_from_slice(if *value { b"true" } else { b"false" }),
        Tree::Unsigned(value) => out.extend_from_slice(value.to_string().as_bytes()),
        Tree::Signed(value) => out.extend_from_slice(value.to_string().as_bytes()),
        Tree::Float(value) if value.is_nan() => write_json_string(b"NaN", out),
        Tree::Float(value) if value.is_infinite() => {
            write_json_string(if *value > 0.0 { b"inf" } else { b"-inf" }, out);
        }
        Tree::Float(value) => out.extend_from_slice(float_text(*value, None).as_bytes()),
        Tree::Char(c) => write_json_string(c.to_string().as_bytes(), out),
        Tree::Text(bytes) => write_json_string(bytes, out),
        Tree::Bytes(bytes) => write_json_string(base64(bytes).as_bytes(), out),
        Tree::Sequence(elements) => {
            out.push(b'[');
            for (index, element) in elements.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_json(element, out);
            }
            out.push(b']');
        }
        Tree::Map(entries) => {
            out.push(b'[');
            for (index, (key, value)) in entries.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                out.push(b'[');
                write_json(key, out);
                out.push(b',');
                write_json(value, out);
                out.push(b']');
            }
            out.push(b']');
        }
        Tree::Record(fields) => {
            out.push(b'{');
            for (index, (key, value)) in fields.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_json_string(key, out);
                out.push(b':');
                write_json(value, out);
            }
            out.push(b'}');
        }
        Tree::Variant(key, None) => write_json_string(key, out),
        Tree::Variant(key, Some(payload)) => {
            out.push(b'{');
            write_json_string(key, out);
            out.push(b':');
            write_json(payload, out);
            out.push(b'}');
        }
    }
}

/// A JSON string of well-formed UTF-8 text, escaped as JSON encoding states.
fn write_json_string(text: &[u8], out: &mut Vec<u8>) {
    out.push(b'"');
    for c in String::from_utf8_lossy(text).chars() {
        match c {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            '\u{8}' => out.extend_from_slice(b"\\b"),
            '\u{C}' => out.extend_from_slice(b"\\f"),
            c if (c as u32) < 0x20 => {
                out.extend_from_slice(format!("\\u{:04x}", c as u32).as_bytes())
            }
            c => out.extend_from_slice(c.to_string().as_bytes()),
        }
    }
    out.push(b'"');
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Bytes in base64 with padding.
fn base64(bytes: &[u8]) -> String {
    let mut text = String::new();
    for chunk in bytes.chunks(3) {
        let group = chunk.iter().enumerate().fold(0u32, |group, (index, byte)| {
            group | (u32::from(*byte) << (16 - 8 * index))
        });
        for index in 0..4 {
            match index <= chunk.len() {
                true => text.push(BASE64[((group >> (18 - 6 * index)) & 0x3F) as usize] as char),
                false => text.push('='),
            }
        }
    }
    text
}

/// The bytes base64 with padding encodes, none for any text `base64` does not write.
fn from_base64(text: &[u8]) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(4) {
        return None;
    }
    let mut bytes = Vec::new();
    for chunk in text.chunks(4) {
        let padding = chunk.iter().rev().take_while(|byte| **byte == b'=').count();
        if padding > 2 {
            return None;
        }
        let mut group = 0u32;
        for (index, byte) in chunk[..4 - padding].iter().enumerate() {
            let value = BASE64.iter().position(|candidate| candidate == byte)? as u32;
            group |= value << (18 - 6 * index);
        }
        bytes.extend((0..3 - padding).map(|index| (group >> (16 - 8 * index)) as u8));
    }
    (base64(&bytes).as_bytes() == text).then_some(bytes)
}

/// Why decoding stopped: a decode error, by its variant's name, or an end of the run.
enum Failure {
    Decode(&'static str),
    Run(End),
}

impl From<End> for Failure {
    fn from(end: End) -> Failure {
        Failure::Run(end)
    }
}

type Decoded<T> = Result<T, Failure>;

fn fail<T>(kind: &'static str) -> Decoded<T> {
    Err(Failure::Decode(kind))
}

/// A format read through the calls of `core::Decoder`.
trait Reader {
    fn read_bool(&mut self) -> Decoded<bool>;
    fn read_unsigned(&mut self) -> Decoded<u64>;
    fn read_signed(&mut self) -> Decoded<i64>;
    fn read_float(&mut self) -> Decoded<f64>;
    /// A char's scalar value, as written.
    fn read_char(&mut self) -> Decoded<u32>;
    fn read_text(&mut self) -> Decoded<Vec<u8>>;
    fn read_bytes(&mut self) -> Decoded<Vec<u8>>;
    fn begin_sequence(&mut self) -> Decoded<()>;
    fn begin_map(&mut self) -> Decoded<()>;
    fn begin_record(&mut self) -> Decoded<()>;
    fn next_follows(&mut self) -> Decoded<bool>;
    /// Between a map entry's key and its value.
    fn map_value(&mut self) -> Decoded<()>;
    fn read_key(&mut self) -> Decoded<Vec<u8>>;
    fn read_variant(&mut self) -> Decoded<Vec<u8>>;
    fn payload_follows(&mut self) -> Decoded<bool>;
    /// Passes over the value of the field whose key was just read.
    fn skip(&mut self) -> Decoded<()>;
    fn end(&mut self) -> Decoded<()>;
    /// Fails on what is left after the value.
    fn finish(&mut self) -> Decoded<()>;
}

/// The decoding of one value: the reader, the cell containers draw through, and the nesting.
struct Decoding<'d, 'i, 'c, 'a> {
    interp: &'i mut Interpreter<'c, 'a>,
    reader: &'d mut dyn Reader,
    cell: u32,
    depth: usize,
    at: (ItemId, Span),
    site: Site,
}

impl Decoding<'_, '_, '_, '_> {
    /// Enters a sequence, map, record or variant.
    fn nest(&mut self) -> Decoded<()> {
        self.depth += 1;
        match self.depth > MAX_DEPTH {
            true => fail("TooDeep"),
            false => Ok(()),
        }
    }

    fn value(&mut self, ty: &Ty) -> Decoded<Val> {
        match ty {
            Ty::Scalar(scalar) => self.scalar(*scalar),
            Ty::Unit => {
                self.nest()?;
                self.reader.begin_record()?;
                if self.reader.next_follows()? {
                    let _ = self.reader.read_key()?;
                    return fail("UnknownKey");
                }
                self.reader.end()?;
                self.depth -= 1;
                Ok(Val::Unit)
            }
            Ty::Item(item, args) => self.item(*item, args),
            other => panic!("a decoded value of {other:?}"),
        }
    }

    fn scalar(&mut self, scalar: Scalar) -> Decoded<Val> {
        let in_range = |value: i128| {
            let (low, high) = scalar.range();
            (low..=high).contains(&value)
        };
        Ok(match scalar {
            Scalar::Bool => Val::Bool(self.reader.read_bool()?),
            Scalar::Char => match char::from_u32(self.reader.read_char()?) {
                Some(c) => Val::Char(c),
                None => return fail("Invalid"),
            },
            Scalar::F64 => Val::F64(self.reader.read_float()?),
            Scalar::F32 => Val::F32(self.reader.read_float()? as f32),
            _ if scalar.is_signed() => {
                let value = i128::from(self.reader.read_signed()?);
                match in_range(value) {
                    true => Val::Int(value),
                    false => return fail("OutOfRange"),
                }
            }
            _ => {
                let value = i128::from(self.reader.read_unsigned()?);
                match in_range(value) {
                    true => Val::Int(value),
                    false => return fail("OutOfRange"),
                }
            }
        })
    }

    fn item(&mut self, item: ItemId, args: &[Arg]) -> Decoded<Val> {
        let arg = |index: usize| {
            args.get(index)
                .and_then(Arg::ty)
                .cloned()
                .unwrap_or(Ty::Unit)
        };
        let count = |index: usize| match args.get(index) {
            Some(Arg::Const(Const::Int(count))) => *count as usize,
            _ => usize::MAX,
        };
        let path = self.interp.ck.db.canonical(item);
        let option = self.interp.ty_item("core", "Option");
        let result = self.interp.ty_item("core", "Result");
        Ok(match path.as_str() {
            "core::Array" => {
                let elements = self.sequence(&arg(0))?;
                if elements.len() != count(1) {
                    return fail("Malformed");
                }
                Val::Array(Rc::new(elements))
            }
            "core::Option" | "core::Result" => {
                let keys: [(&str, Option<Ty>); 2] = match path.as_str() {
                    "core::Option" => [("Some", Some(arg(0))), ("None", None)],
                    _ => [("Ok", Some(arg(0))), ("Err", Some(arg(1)))],
                };
                let enumeration = if path == "core::Option" {
                    option
                } else {
                    result
                };
                let (index, payload) = self.variant(&keys)?;
                Val::Variant(
                    enumeration,
                    index as u32,
                    Rc::new(payload.into_iter().collect()),
                )
            }
            "text::String" | "mem::Bytes" => {
                let bytes = match path.as_str() {
                    "text::String" => {
                        let text = self.reader.read_text()?;
                        if std::str::from_utf8(&text).is_err() {
                            return fail("InvalidText");
                        }
                        text
                    }
                    _ => self.reader.read_bytes()?,
                };
                let values = bytes
                    .into_iter()
                    .map(|byte| Val::Int(i128::from(byte)))
                    .collect();
                self.held(&path, values, &Ty::Scalar(Scalar::U8))?
            }
            "text::Fixed" => {
                let text = self.reader.read_text()?;
                if std::str::from_utf8(&text).is_err() {
                    return fail("InvalidText");
                }
                let bound = count(0);
                if text.len() > bound {
                    return fail("OutOfRange");
                }
                let len = text.len();
                let mut bytes: Vec<Val> = text
                    .into_iter()
                    .map(|byte| Val::Int(i128::from(byte)))
                    .collect();
                bytes.resize(bound, Val::Int(0));
                Val::Struct(
                    item,
                    Rc::new(vec![Val::Array(Rc::new(bytes)), Val::Int(len as i128)]),
                )
            }
            "mem::List" => {
                let elements = self.sequence(&arg(0))?;
                self.held(&path, elements, &arg(0))?
            }
            "mem::Set" => {
                let elements = self.sequence(&arg(0))?;
                self.ascending(&elements, &arg(0))?;
                self.held(&path, elements, &arg(0))?
            }
            "mem::Bounded" => {
                let elements = self.sequence(&arg(0))?;
                if elements.len() > count(1) {
                    return fail("OutOfRange");
                }
                Val::Struct(item, Rc::new(vec![Val::Array(Rc::new(elements))]))
            }
            "mem::Map" => {
                self.nest()?;
                self.reader.begin_map()?;
                let mut keys = Vec::new();
                let mut pairs = Vec::new();
                let key_value = self.interp.ty_item("mem", "KeyValue");
                while self.reader.next_follows()? {
                    let key = self.value(&arg(0))?;
                    self.reader.map_value()?;
                    let value = self.value(&arg(1))?;
                    keys.push(key.clone());
                    pairs.push(Val::Struct(key_value, Rc::new(vec![key, value])));
                }
                self.reader.end()?;
                self.depth -= 1;
                self.ascending(&keys, &arg(0))?;
                let entry = Ty::item(key_value, vec![Arg::Ty(arg(0)), Arg::Ty(arg(1))]);
                self.held(&path, pairs, &entry)?
            }
            "mem::Box" | "mem::Shared" => {
                let contents = self.value(&arg(0))?;
                let made = match path.as_str() {
                    "mem::Box" => boxed(self.interp, self.cell, contents, &arg(0)),
                    _ => counted(self.interp, "mem::Shared", self.cell, contents, &arg(0)),
                };
                match made {
                    Some(made) => made,
                    None => return fail("OutOfMemory"),
                }
            }
            "core::Untrusted" => Val::Struct(item, Rc::new(vec![self.value(&arg(0))?])),
            "mem::Id" => {
                let index = self.scalar(Scalar::U32)?;
                Val::Struct(item, Rc::new(vec![index]))
            }
            _ => self.declared(item, args)?,
        })
    }

    /// A struct or enum that derives `Encode`.
    fn declared(&mut self, item: ItemId, args: &[Arg]) -> Decoded<Val> {
        let sig = self.interp.ck.sig(item);
        let subst = Subst::of(sig.generics.iter().map(|def| def.param), args);
        let open = self.interp.ck.db.ast(item).has_modifier(TypeModifier::Open);
        let aliases = self.interp.ck.key_aliases(item);
        match &sig.kind {
            SigKind::Struct { fields } => {
                let named: Vec<(String, Ty)> = fields
                    .iter()
                    .map(|field| (field.name.clone(), subst.ty(&field.ty)))
                    .collect();
                let values = self.record(&named, open, &aliases)?;
                let value = Val::Struct(item, Rc::new(values));
                let ty = Ty::item(item, args.to_vec());
                match self.interp.predicates_hold(item, true, &value, &ty) {
                    true => Ok(value),
                    false => fail("Invalid"),
                }
            }
            SigKind::Enum { variants } => {
                self.nest()?;
                let key = self.reader.read_variant()?;
                let key = aliases.get(&key).cloned().unwrap_or(key);
                let Some(index) = variants
                    .iter()
                    .position(|variant| variant.name.as_bytes() == key)
                else {
                    return fail("UnknownVariant");
                };
                let follows = self.reader.payload_follows()?;
                let payload = match (&variants[index].payload, follows) {
                    (Payload::None, false) => Vec::new(),
                    (Payload::Single(ty), true) => vec![self.value(&subst.ty(ty))?],
                    (Payload::Labelled(labels), true) => {
                        let named: Vec<(String, Ty)> = labels
                            .iter()
                            .map(|(name, ty)| (name.clone(), subst.ty(ty)))
                            .collect();
                        self.record(&named, false, &BTreeMap::new())?
                    }
                    _ => return fail("Malformed"),
                };
                self.reader.end()?;
                self.depth -= 1;
                Ok(Val::Variant(item, index as u32, Rc::new(payload)))
            }
            other => panic!("an Encode type of kind {other:?}"),
        }
    }

    /// The values of a record's named fields in order, as the decoding rules read them.
    fn record(
        &mut self,
        named: &[(String, Ty)],
        open: bool,
        aliases: &BTreeMap<Vec<u8>, Vec<u8>>,
    ) -> Decoded<Vec<Val>> {
        self.nest()?;
        self.reader.begin_record()?;
        let mut values: Vec<Option<Val>> = vec![None; named.len()];
        while self.reader.next_follows()? {
            let key = self.reader.read_key()?;
            let key = aliases.get(&key).cloned().unwrap_or(key);
            match named.iter().position(|(name, _)| name.as_bytes() == key) {
                Some(index) if values[index].is_some() => return fail("DuplicateKey"),
                Some(index) => values[index] = Some(self.value(&named[index].1)?),
                None if open => self.reader.skip()?,
                None => return fail("UnknownKey"),
            }
        }
        self.reader.end()?;
        self.depth -= 1;
        named
            .iter()
            .zip(values)
            .map(|((_, ty), value)| match value {
                Some(value) => Ok(value),
                None if open => self.default(ty),
                None => fail("MissingField"),
            })
            .collect()
    }

    /// The default of a missing field's type, when it implements `Default`.
    fn default(&mut self, ty: &Ty) -> Decoded<Val> {
        let defaults = self.interp.ty_item("core", "Default");
        if self.interp.ck.conformance(ty, defaults).is_none() {
            return fail("MissingField");
        }
        Ok(conformance(
            self.interp,
            "Default",
            "default",
            ty,
            Vec::new(),
            self.at,
            self.site,
        )?)
    }

    /// The variant one of `keys` names, with its payload when its key has a type.
    fn variant(&mut self, keys: &[(&str, Option<Ty>)]) -> Decoded<(usize, Option<Val>)> {
        self.nest()?;
        let key = self.reader.read_variant()?;
        let Some(index) = keys.iter().position(|(name, _)| name.as_bytes() == key) else {
            return fail("UnknownVariant");
        };
        let follows = self.reader.payload_follows()?;
        let payload = match (&keys[index].1, follows) {
            (Some(ty), true) => Some(self.value(ty)?),
            (None, false) => None,
            _ => return fail("Malformed"),
        };
        self.reader.end()?;
        self.depth -= 1;
        Ok((index, payload))
    }

    fn sequence(&mut self, element: &Ty) -> Decoded<Vec<Val>> {
        self.nest()?;
        self.reader.begin_sequence()?;
        let mut elements = Vec::new();
        while self.reader.next_follows()? {
            elements.push(self.value(element)?);
        }
        self.reader.end()?;
        self.depth -= 1;
        Ok(elements)
    }

    /// Fails unless the keys of a set or map ascend strictly by `cmp`.
    fn ascending(&mut self, keys: &[Val], ty: &Ty) -> Decoded<()> {
        for pair in keys.windows(2) {
            let order = conformance(
                self.interp,
                "Ord",
                "cmp",
                ty,
                pair.to_vec(),
                self.at,
                self.site,
            )?;
            match ordering(&order) {
                std::cmp::Ordering::Equal => return fail("DuplicateKey"),
                std::cmp::Ordering::Greater => return fail("Malformed"),
                std::cmp::Ordering::Less => {}
            }
        }
        Ok(())
    }

    /// A growable container of the std type at `path` holding values.
    fn held(&mut self, path: &str, values: Vec<Val>, element: &Ty) -> Decoded<Val> {
        match holding(self.interp, path, values, self.cell, element) {
            Some(container) => Ok(container),
            None => fail("OutOfMemory"),
        }
    }
}

/// A position in the input with the bytes before it read.
struct Input {
    bytes: Vec<u8>,
    at: usize,
}

impl Input {
    fn byte(&mut self) -> Decoded<u8> {
        let byte = *self
            .bytes
            .get(self.at)
            .ok_or(Failure::Decode("Truncated"))?;
        self.at += 1;
        Ok(byte)
    }

    fn take(&mut self, count: usize) -> Decoded<Vec<u8>> {
        if count > self.bytes.len() - self.at {
            return fail("Truncated");
        }
        let taken = self.bytes[self.at..self.at + count].to_vec();
        self.at += count;
        Ok(taken)
    }
}

/// What the wire reader is inside: a sequence or map with its elements left, a record with
/// its fields left, the end of the field value being read and the last key, or a variant
/// payload's end.
enum WireFrame {
    Counted(u64),
    Record {
        left: u64,
        value_end: Option<usize>,
        last: Option<Vec<u8>>,
    },
    Variant {
        payload_end: Option<usize>,
    },
}

struct WireReader {
    input: Input,
    frames: Vec<WireFrame>,
}

impl WireReader {
    fn new(bytes: Vec<u8>) -> WireReader {
        WireReader {
            input: Input { bytes, at: 0 },
            frames: Vec::new(),
        }
    }

    fn uvar(&mut self) -> Decoded<u64> {
        let mut value: u64 = 0;
        for index in 0..10 {
            let byte = self.input.byte()?;
            let group = u64::from(byte & 0x7F);
            if index == 9 && byte > 1 {
                return fail("Malformed");
            }
            value |= group << (7 * index);
            if byte & 0x80 == 0 {
                if byte == 0 && index > 0 {
                    return fail("Malformed");
                }
                return Ok(value);
            }
        }
        fail("Malformed")
    }

    fn framed(&mut self) -> Decoded<Vec<u8>> {
        let len = self.uvar()?;
        self.input.take(usize::try_from(len).unwrap_or(usize::MAX))
    }

    /// The end of a byte length read at the current position.
    fn length_end(&mut self) -> Decoded<usize> {
        let len = usize::try_from(self.uvar()?).unwrap_or(usize::MAX);
        if len > self.input.bytes.len() - self.input.at {
            return fail("Truncated");
        }
        Ok(self.input.at + len)
    }

    /// Fails unless the field value read last filled exactly its length.
    fn close_value(&mut self) -> Decoded<()> {
        if let Some(WireFrame::Record { value_end, .. }) = self.frames.last_mut()
            && let Some(end) = value_end.take()
            && end != self.input.at
        {
            return fail("Malformed");
        }
        Ok(())
    }
}

impl Reader for WireReader {
    fn read_bool(&mut self) -> Decoded<bool> {
        match self.input.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => fail("Malformed"),
        }
    }

    fn read_unsigned(&mut self) -> Decoded<u64> {
        self.uvar()
    }

    fn read_signed(&mut self) -> Decoded<i64> {
        let zigzag = self.uvar()?;
        Ok(((zigzag >> 1) as i64) ^ -((zigzag & 1) as i64))
    }

    fn read_float(&mut self) -> Decoded<f64> {
        let bytes = self.input.take(8)?;
        Ok(f64::from_bits(u64::from_le_bytes(
            bytes.try_into().expect("eight bytes"),
        )))
    }

    fn read_char(&mut self) -> Decoded<u32> {
        Ok(u32::try_from(self.uvar()?).unwrap_or(u32::MAX))
    }

    fn read_text(&mut self) -> Decoded<Vec<u8>> {
        self.framed()
    }

    fn read_bytes(&mut self) -> Decoded<Vec<u8>> {
        self.framed()
    }

    fn begin_sequence(&mut self) -> Decoded<()> {
        let count = self.uvar()?;
        if count > (self.input.bytes.len() - self.input.at) as u64 {
            return fail("Truncated");
        }
        self.frames.push(WireFrame::Counted(count));
        Ok(())
    }

    fn begin_map(&mut self) -> Decoded<()> {
        self.begin_sequence()
    }

    fn begin_record(&mut self) -> Decoded<()> {
        let left = self.uvar()?;
        if left > (self.input.bytes.len() - self.input.at) as u64 {
            return fail("Truncated");
        }
        self.frames.push(WireFrame::Record {
            left,
            value_end: None,
            last: None,
        });
        Ok(())
    }

    fn next_follows(&mut self) -> Decoded<bool> {
        self.close_value()?;
        match self.frames.last_mut() {
            Some(WireFrame::Counted(left) | WireFrame::Record { left, .. }) => {
                let follows = *left > 0;
                *left = left.saturating_sub(1);
                Ok(follows)
            }
            _ => fail("Malformed"),
        }
    }

    fn map_value(&mut self) -> Decoded<()> {
        Ok(())
    }

    fn read_key(&mut self) -> Decoded<Vec<u8>> {
        let key = self.framed()?;
        let end = self.length_end()?;
        let Some(WireFrame::Record {
            value_end, last, ..
        }) = self.frames.last_mut()
        else {
            return fail("Malformed");
        };
        match last.as_ref().map(|last| key.as_slice().cmp(last)) {
            Some(std::cmp::Ordering::Equal) => return fail("DuplicateKey"),
            Some(std::cmp::Ordering::Less) => return fail("Malformed"),
            _ => {}
        }
        *last = Some(key.clone());
        *value_end = Some(end);
        Ok(key)
    }

    fn read_variant(&mut self) -> Decoded<Vec<u8>> {
        let key = self.framed()?;
        self.frames.push(WireFrame::Variant { payload_end: None });
        Ok(key)
    }

    fn payload_follows(&mut self) -> Decoded<bool> {
        match self.input.byte()? {
            0 => Ok(false),
            1 => {
                let end = self.length_end()?;
                if let Some(WireFrame::Variant { payload_end }) = self.frames.last_mut() {
                    *payload_end = Some(end);
                }
                Ok(true)
            }
            _ => fail("Malformed"),
        }
    }

    fn skip(&mut self) -> Decoded<()> {
        if let Some(WireFrame::Record { value_end, .. }) = self.frames.last_mut()
            && let Some(end) = value_end.take()
        {
            self.input.at = end;
            return Ok(());
        }
        fail("Malformed")
    }

    fn end(&mut self) -> Decoded<()> {
        self.close_value()?;
        match self.frames.pop() {
            Some(WireFrame::Counted(0) | WireFrame::Record { left: 0, .. }) => Ok(()),
            Some(WireFrame::Variant { payload_end }) => match payload_end {
                Some(end) if end != self.input.at => fail("Malformed"),
                _ => Ok(()),
            },
            _ => fail("Malformed"),
        }
    }

    fn finish(&mut self) -> Decoded<()> {
        match self.input.at == self.input.bytes.len() {
            true => Ok(()),
            false => fail("Trailing"),
        }
    }
}

/// What the JSON reader is inside: an array, an object, a map entry array, or a variant
/// object; each with whether an element was read.
enum JsonFrame {
    Array { started: bool },
    Object { started: bool },
    Map { started: bool },
    Variant { payload: bool },
}

struct JsonReader {
    input: Input,
    frames: Vec<JsonFrame>,
}

impl JsonReader {
    fn new(bytes: Vec<u8>) -> JsonReader {
        JsonReader {
            input: Input { bytes, at: 0 },
            frames: Vec::new(),
        }
    }

    fn space(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.input.bytes.get(self.input.at) {
            self.input.at += 1;
        }
    }

    fn peek(&mut self) -> Decoded<u8> {
        self.space();
        self.input
            .bytes
            .get(self.input.at)
            .copied()
            .ok_or(Failure::Decode("Truncated"))
    }

    fn expect(&mut self, byte: u8) -> Decoded<()> {
        match self.peek()? == byte {
            true => {
                self.input.at += 1;
                Ok(())
            }
            false => fail("Malformed"),
        }
    }

    fn literal(&mut self, word: &[u8]) -> bool {
        self.space();
        let found = self.input.bytes[self.input.at..].starts_with(word);
        if found {
            self.input.at += word.len();
        }
        found
    }

    /// A JSON string's bytes, its escapes resolved.
    fn string(&mut self) -> Decoded<Vec<u8>> {
        self.expect(b'"')?;
        let mut text = String::new();
        loop {
            let start = self.input.at;
            let rest = &self.input.bytes[start..];
            let Some(len) = rest.iter().position(|byte| *byte == b'"' || *byte == b'\\') else {
                return fail("Truncated");
            };
            let Ok(plain) = std::str::from_utf8(&rest[..len]) else {
                return fail("InvalidText");
            };
            if plain.chars().any(|c| (c as u32) < 0x20) {
                return fail("Malformed");
            }
            text.push_str(plain);
            self.input.at += len;
            if self.input.byte()? == b'"' {
                return Ok(text.into_bytes());
            }
            let escaped = match self.input.byte()? {
                b'"' => '"',
                b'\\' => '\\',
                b'/' => '/',
                b'b' => '\u{8}',
                b'f' => '\u{C}',
                b'n' => '\n',
                b'r' => '\r',
                b't' => '\t',
                b'u' => {
                    let high = self.hex4()?;
                    let scalar = match high {
                        0xD800..=0xDBFF => {
                            if self.input.take(2)? != b"\\u" {
                                return fail("InvalidText");
                            }
                            let low = self.hex4()?;
                            if !(0xDC00..=0xDFFF).contains(&low) {
                                return fail("InvalidText");
                            }
                            0x1_0000 + ((high - 0xD800) << 10) + (low - 0xDC00)
                        }
                        0xDC00..=0xDFFF => return fail("InvalidText"),
                        _ => high,
                    };
                    char::from_u32(scalar).ok_or(Failure::Decode("InvalidText"))?
                }
                _ => return fail("Malformed"),
            };
            text.push(escaped);
        }
    }

    fn hex4(&mut self) -> Decoded<u32> {
        let digits = self.input.take(4)?;
        let text = std::str::from_utf8(&digits).map_err(|_| Failure::Decode("Malformed"))?;
        u32::from_str_radix(text, 16).map_err(|_| Failure::Decode("Malformed"))
    }

    /// The text of a JSON number, checked against the RFC 8259 grammar.
    fn number(&mut self) -> Decoded<String> {
        self.space();
        let start = self.input.at;
        let bytes = &self.input.bytes;
        let mut at = start;
        let digits = |at: &mut usize| {
            let from = *at;
            while bytes.get(*at).is_some_and(u8::is_ascii_digit) {
                *at += 1;
            }
            *at > from
        };
        if bytes.get(at) == Some(&b'-') {
            at += 1;
        }
        match bytes.get(at) {
            Some(b'0') => at += 1,
            Some(b'1'..=b'9') => {
                digits(&mut at);
            }
            _ => return fail("Malformed"),
        }
        if bytes.get(at) == Some(&b'.') {
            at += 1;
            if !digits(&mut at) {
                return fail("Malformed");
            }
        }
        if let Some(b'e' | b'E') = bytes.get(at) {
            at += 1;
            if let Some(b'+' | b'-') = bytes.get(at) {
                at += 1;
            }
            if !digits(&mut at) {
                return fail("Malformed");
            }
        }
        self.input.at = at;
        Ok(String::from_utf8(bytes[start..at].to_vec()).expect("ASCII digits"))
    }

    /// An integer without fraction or exponent, `-0` being 0.
    fn integer(&mut self) -> Decoded<i128> {
        let text = self.number()?;
        if text.contains(['.', 'e', 'E']) {
            return fail("Malformed");
        }
        text.parse::<i128>().or_else(|_| fail("OutOfRange"))
    }

    /// Passes over any JSON value.
    fn skip_value(&mut self) -> Decoded<()> {
        match self.peek()? {
            b'"' => self.string().map(|_| ()),
            b'{' | b'[' => {
                let close = if self.peek()? == b'{' { b'}' } else { b']' };
                let object = close == b'}';
                self.input.at += 1;
                if self.peek()? == close {
                    self.input.at += 1;
                    return Ok(());
                }
                loop {
                    if object {
                        self.string()?;
                        self.expect(b':')?;
                    }
                    self.skip_value()?;
                    match self.peek()? {
                        b',' => self.input.at += 1,
                        byte if byte == close => {
                            self.input.at += 1;
                            return Ok(());
                        }
                        _ => return fail("Malformed"),
                    }
                }
            }
            b't' if self.literal(b"true") => Ok(()),
            b'f' if self.literal(b"false") => Ok(()),
            b'n' if self.literal(b"null") => Ok(()),
            _ => self.number().map(|_| ()),
        }
    }

    /// Before the next element of the innermost array, object or map: whether one follows.
    fn element_follows(&mut self, close: u8) -> Decoded<bool> {
        let started = match self.frames.last_mut() {
            Some(
                JsonFrame::Array { started }
                | JsonFrame::Object { started }
                | JsonFrame::Map { started },
            ) => std::mem::replace(started, true),
            _ => return fail("Malformed"),
        };
        if self.peek()? == close {
            return Ok(false);
        }
        if started {
            self.expect(b',')?;
        }
        Ok(true)
    }
}

impl Reader for JsonReader {
    fn read_bool(&mut self) -> Decoded<bool> {
        if self.literal(b"true") {
            return Ok(true);
        }
        match self.literal(b"false") {
            true => Ok(false),
            false => fail("Malformed"),
        }
    }

    fn read_unsigned(&mut self) -> Decoded<u64> {
        u64::try_from(self.integer()?).or_else(|_| fail("OutOfRange"))
    }

    fn read_signed(&mut self) -> Decoded<i64> {
        let value = self.integer()?;
        i64::try_from(value).or_else(|_| fail("OutOfRange"))
    }

    fn read_float(&mut self) -> Decoded<f64> {
        if self.peek()? == b'"' {
            return match self.string()?.as_slice() {
                b"NaN" => Ok(f64::NAN),
                b"inf" => Ok(f64::INFINITY),
                b"-inf" => Ok(f64::NEG_INFINITY),
                _ => fail("Malformed"),
            };
        }
        let text = self.number()?;
        text.parse::<f64>().or_else(|_| fail("Malformed"))
    }

    fn read_char(&mut self) -> Decoded<u32> {
        let text = self.string()?;
        let text = String::from_utf8(text).map_err(|_| Failure::Decode("InvalidText"))?;
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Ok(c as u32),
            _ => fail("Malformed"),
        }
    }

    fn read_text(&mut self) -> Decoded<Vec<u8>> {
        self.string()
    }

    fn read_bytes(&mut self) -> Decoded<Vec<u8>> {
        let text = self.string()?;
        from_base64(&text).ok_or(Failure::Decode("Malformed"))
    }

    fn begin_sequence(&mut self) -> Decoded<()> {
        self.expect(b'[')?;
        self.frames.push(JsonFrame::Array { started: false });
        Ok(())
    }

    fn begin_map(&mut self) -> Decoded<()> {
        self.expect(b'[')?;
        self.frames.push(JsonFrame::Map { started: false });
        Ok(())
    }

    fn begin_record(&mut self) -> Decoded<()> {
        self.expect(b'{')?;
        self.frames.push(JsonFrame::Object { started: false });
        Ok(())
    }

    fn next_follows(&mut self) -> Decoded<bool> {
        match self.frames.last() {
            Some(JsonFrame::Object { .. }) => self.element_follows(b'}'),
            Some(JsonFrame::Map { started }) => {
                if *started {
                    self.expect(b']')?;
                }
                let follows = self.element_follows(b']')?;
                if follows {
                    self.expect(b'[')?;
                }
                Ok(follows)
            }
            _ => self.element_follows(b']'),
        }
    }

    fn map_value(&mut self) -> Decoded<()> {
        self.expect(b',')
    }

    fn read_key(&mut self) -> Decoded<Vec<u8>> {
        let key = self.string()?;
        self.expect(b':')?;
        Ok(key)
    }

    fn read_variant(&mut self) -> Decoded<Vec<u8>> {
        if self.peek()? == b'"' {
            self.frames.push(JsonFrame::Variant { payload: false });
            return self.string();
        }
        self.expect(b'{')?;
        let key = self.string()?;
        self.expect(b':')?;
        self.frames.push(JsonFrame::Variant { payload: true });
        Ok(key)
    }

    fn payload_follows(&mut self) -> Decoded<bool> {
        match self.frames.last() {
            Some(JsonFrame::Variant { payload }) => Ok(*payload),
            _ => fail("Malformed"),
        }
    }

    fn skip(&mut self) -> Decoded<()> {
        self.skip_value()
    }

    fn end(&mut self) -> Decoded<()> {
        match self.frames.pop() {
            Some(JsonFrame::Array { .. }) => self.expect(b']'),
            Some(JsonFrame::Object { .. }) => self.expect(b'}'),
            Some(JsonFrame::Map { .. }) => self.expect(b']'),
            Some(JsonFrame::Variant { payload: true }) => self.expect(b'}'),
            Some(JsonFrame::Variant { payload: false }) => Ok(()),
            None => fail("Malformed"),
        }
    }

    fn finish(&mut self) -> Decoded<()> {
        self.space();
        match self.input.at == self.input.bytes.len() {
            true => Ok(()),
            false => fail("Trailing"),
        }
    }
}

#[cfg(test)]
mod tests;
