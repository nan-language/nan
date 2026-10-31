//! The codecs the interpreter supplies, `mem::wire::Codec` with its schemas, erased decoders and
//! migrations, `text::json::Codec` and `text::construction::Codec`, and `encode` and `decode` through an encoder or decoder of any
//! type. A value encodes into the tree of kinds `core::Encoder` writes, which each format then
//! writes out and an encoder's calls replay; decoding reads a format or a decoder's calls through
//! the calls of `core::Decoder`, in the order the std encodings and derived decoders make them.

mod construction;

use std::collections::BTreeMap;
use std::rc::Rc;

use super::super::value::{FnVal, Loc, Val, View};
use super::super::{End, Exec, Flow, Interpreter, Site};
use super::collections::{conformance, ordering};
use super::render::float_text;
use super::stores::{boxed, cell_of, counted, entries, exhausted, holding, place};
use crate::check::db::{ItemId, TypeKey};
use crate::check::sig::{Payload, SigKind};
use crate::check::ty::{Arg, Const, Scalar, Subst, Ty};
use crate::hash::Hash;
use crate::nir::{Callable, FnRef, Role, Span};

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
    let format = match owner {
        "mem::wire::Codec" => Format::Wire,
        "text::json::Codec" => Format::Json,
        "text::construction::Codec" => Format::Construction,
        _ => return None,
    };
    let ty = type_args
        .first()
        .and_then(Arg::ty)
        .cloned()
        .unwrap_or(Ty::Unit);
    Some(match member {
        "encode" | "try_encode" => {
            let write = match format {
                Format::Wire => write_wire,
                Format::Json => write_json,
                Format::Construction => return None,
            };
            match appended(interp, write, &args[0], &ty, &args[1]) {
                Some(()) => Ok(Flow::Return(Val::Unit)),
                None => exhausted(interp, member == "try_encode", at),
            }
        }
        "decode_from" => {
            let wanted = hash_value(interp, &args[0]);
            let decoded = match migrated(interp, &ty, wanted, true) {
                Some(None) => read(interp, format, &ty, &args[1..], at, site),
                Some(Some((migration, shape))) => {
                    read(interp, format, &shape, &args[1..], at, site).and_then(
                        |old| match migrate(interp, migration, old, &args[2], at, site)? {
                            Flow::Return(value) => Ok(value),
                            _ => fail("OutOfMemory"),
                        },
                    )
                }
                None => fail("UnknownSchema"),
            };
            outcome(interp, decoded)
        }
        "encode_as" => {
            let wanted = hash_value(interp, &args[1]);
            let written = match migrated(interp, &ty, wanted, false) {
                Some(None) => appended(interp, write_wire, &args[0], &ty, &args[2]),
                Some(Some((migration, shape))) => {
                    let old = match migrate(interp, migration, args[0].clone(), &args[3], at, site)
                    {
                        Ok(Flow::Return(old)) => old,
                        Ok(_) => return Some(exhausted(interp, false, at)),
                        Err(end) => return Some(Err(end)),
                    };
                    let written = appended(interp, write_wire, &old, &shape, &args[2]);
                    super::stores::release(interp, &old);
                    written
                }
                None => return Some(Ok(Flow::Return(Val::Bool(false)))),
            };
            match written {
                Some(()) => Ok(Flow::Return(Val::Bool(true))),
                None => exhausted(interp, false, at),
            }
        }
        "decode" => {
            let decoded = read(interp, format, &ty, args, at, site);
            outcome(interp, decoded)
        }
        "canonical" => {
            let canonical = read(interp, format, &ty, args, at, site).and_then(|value| {
                let mut out = Vec::new();
                write_wire(&encoded(interp, &value, &ty), &mut out);
                super::stores::release(interp, &value);
                let bytes = out
                    .into_iter()
                    .map(|byte| Val::Int(i128::from(byte)))
                    .collect();
                let cell = cell_of(interp, &args[1]);
                let Some(bytes) =
                    holding(interp, "mem::Bytes", bytes, cell, &Ty::Scalar(Scalar::U8))
                else {
                    return fail("OutOfMemory");
                };
                let untrusted = interp.ty_item("core", "Untrusted");
                Ok(Val::Struct(untrusted, Rc::new(vec![bytes])))
            });
            outcome(interp, canonical)
        }
        "schema_hash" => Ok(Flow::Return(schema_hash(interp, &ty))),
        "schema" => {
            let schema = Ty::item(interp.ty_item("mem::wire", "Schema"), Vec::new());
            let mut reader = WireReader::new(interp.ck.schema(&ty).wire());
            let cell = cell_of(interp, &args[0]);
            let mut decoding = Decoding {
                interp,
                reader: &mut reader,
                cell,
                depth: 0,
                at,
                site,
            };
            match decoding.value(&schema) {
                Ok(tree) => Ok(Flow::Return(tree)),
                Err(Failure::Run(end)) => Err(end),
                Err(_) => exhausted(interp, false, at),
            }
        }
        "erased" => {
            let codec = interp.ty_item("mem::wire", "Codec");
            let canonical = interp.ck.db.members(TypeKey::Item(codec), "canonical")[0];
            let function = FnVal::Function(FnRef {
                callable: Callable::Item {
                    item: canonical,
                    role: Role::Body,
                    index: 0,
                },
                args: Rc::from(type_args.to_vec()),
            });
            let erased = interp.ty_item("mem::wire", "Erased");
            let fields = vec![schema_hash(interp, &ty), Val::Fn(Rc::new(function))];
            Ok(Flow::Return(Val::Struct(erased, Rc::new(fields))))
        }
        _ => return None,
    })
}

/// One value of a type read from a format's bytes, the Untrusted slice `args[0]`, drawing on
/// the allocator `args[1]`, and nothing left after it.
fn read(
    interp: &mut Interpreter,
    format: Format,
    ty: &Ty,
    args: &[Val],
    at: (ItemId, Span),
    site: Site,
) -> Decoded<Val> {
    let raw = interp.deref(&args[0]).fields()[0].clone();
    let input = interp.bytes(&raw);
    let cell = cell_of(interp, &args[1]);
    let mut reader: Box<dyn Reader> = match format {
        Format::Wire => Box::new(WireReader::new(input)),
        Format::Json => Box::new(JsonReader::new(input)),
        Format::Construction => Box::new(construction::ConstructionReader::new(input)),
    };
    let mut decoding = Decoding {
        interp,
        reader: reader.as_mut(),
        cell,
        depth: 0,
        at,
        site,
    };
    decoding.value(ty).and_then(|value| {
        decoding.reader.finish(decoding.interp)?;
        Ok(value)
    })
}

/// Appends a value's encoding in a format to the bytes at the place `out` holds; none on
/// exhaustion, which leaves them unchanged.
fn appended(
    interp: &mut Interpreter,
    write: fn(&Tree, &mut Vec<u8>),
    value: &Val,
    ty: &Ty,
    out: &Val,
) -> Option<()> {
    let mut written = Vec::new();
    write(&encoded(interp, value, ty), &mut written);
    let bytes: Vec<Val> = written
        .into_iter()
        .map(|byte| Val::Int(i128::from(byte)))
        .collect();
    super::stores::append(interp, &place(out), &bytes, &Ty::Scalar(Scalar::U8))
}

/// The hash a `mem::wire::SchemaHash` value holds.
fn hash_value(interp: &Interpreter, value: &Val) -> Hash {
    let bytes = interp.deref(value).fields()[0].clone();
    let mut hash = [0; 33];
    for (byte, held) in hash.iter_mut().zip(bytes.fields()) {
        *byte = held.int() as u8;
    }
    Hash(hash)
}

/// How values of a type convert from or to the schema hash `wanted`: as themselves, through a
/// forward migration or an inverse with its frozen shape, or not at all.
fn migrated(
    interp: &Interpreter,
    ty: &Ty,
    wanted: Hash,
    forward: bool,
) -> Option<Option<(ItemId, Ty)>> {
    if interp.ck.schema(ty).hash() == wanted {
        return Some(None);
    }
    let Ty::Item(item, _) = ty else {
        return None;
    };
    interp
        .ck
        .migrations(*item)
        .into_iter()
        .find(|migration| {
            migration.forward == forward && interp.ck.shape_hash(migration.shape) == wanted
        })
        .map(|migration| Some((migration.function, Ty::item(migration.shape, Vec::new()))))
}

/// Runs a migration on a value, with the allocator `alloc` when it takes one.
fn migrate(
    interp: &mut Interpreter,
    function: ItemId,
    value: Val,
    alloc: &Val,
    at: (ItemId, Span),
    site: Site,
) -> Exec<Flow> {
    let mut args = vec![value];
    if interp
        .ck
        .fn_sig(function)
        .is_some_and(|sig| sig.params.len() == 2)
    {
        args.push(alloc.clone());
    }
    let fn_ref = FnRef {
        callable: Callable::Item {
            item: function,
            role: Role::Body,
            index: 0,
        },
        args: Rc::from(Vec::new()),
    };
    interp.call_function(&fn_ref, args, at, site)
}

/// The `mem::wire::SchemaHash` of a type implementing `Encode`.
fn schema_hash(interp: &Interpreter, ty: &Ty) -> Val {
    let bytes = interp
        .ck
        .schema(ty)
        .hash()
        .0
        .iter()
        .map(|byte| Val::Int(i128::from(*byte)))
        .collect();
    let item = interp.ty_item("mem::wire", "SchemaHash");
    Val::Struct(item, Rc::new(vec![Val::Array(Rc::new(bytes))]))
}

/// `T.encode(self, mut e)` or `T.decode(mut d, alloc)` of a type whose `Encode` a derive writes
/// or the toolchain supplies, through the calls of an encoder or decoder of type `through`: the
/// value's encoded tree written out, or a value read in the order the std encodings and derived
/// decoders read it.
pub fn through(
    interp: &mut Interpreter,
    ty: &Ty,
    through: &Ty,
    method: &str,
    args: &[Val],
    at: (ItemId, Span),
    site: Site,
) -> Exec<Flow> {
    if method == "encode" {
        let calls = Calls {
            target: args[1].clone(),
            ty: through.clone(),
            interface: interp.ty_item("core", "Encoder"),
            cell: 0,
            at,
            site,
        };
        let tree = encoded(interp, &args[0], ty);
        calls.write(interp, &tree)?;
        return Ok(Flow::Return(Val::Unit));
    }
    let cell = cell_of(interp, &args[1]);
    let mut calls = Calls {
        target: args[0].clone(),
        ty: through.clone(),
        interface: interp.ty_item("core", "Decoder"),
        cell,
        at,
        site,
    };
    let mut decoding = Decoding {
        interp,
        reader: &mut calls,
        cell,
        depth: 0,
        at,
        site,
    };
    let decoded = decoding.value(ty);
    outcome(interp, decoded)
}

/// What a decoding gives: its value, or its failure as a `core::DecodeError`.
fn outcome(interp: &Interpreter, decoded: Decoded<Val>) -> Exec<Flow> {
    match decoded {
        Ok(value) => Ok(Flow::Return(value)),
        Err(Failure::Decode(kind)) => {
            let error = interp.ty_item("core", "DecodeError");
            let index = interp.variant_index(error, kind);
            Ok(Flow::Fail(Val::Variant(error, index, Rc::new(Vec::new()))))
        }
        Err(Failure::Raised(error)) => Ok(Flow::Fail(error)),
        Err(Failure::Run(end)) => Err(end),
    }
}

/// The calls of an encoder or decoder of a type, at the place `target` holds it, made by an
/// encoding or decoding drawing on `cell`.
struct Calls {
    target: Val,
    ty: Ty,
    interface: ItemId,
    cell: u32,
    at: (ItemId, Span),
    site: Site,
}

impl Calls {
    /// Calls the interface's method `name` on the target with `args`, giving its result, or
    /// the failure it raises.
    fn call(&self, interp: &mut Interpreter, name: &str, args: Vec<Val>) -> Decoded<Val> {
        let method = interp
            .ck
            .interface_method(self.interface, name)
            .expect("a method of the interface");
        let fn_ref = FnRef {
            callable: Callable::Method {
                interface: self.interface,
                method: method as u32,
            },
            args: Rc::from(vec![Arg::Ty(self.ty.clone())]),
        };
        let mut all = vec![self.target.clone()];
        all.extend(args);
        match interp.call_function(&fn_ref, all, self.at, self.site)? {
            Flow::Fail(error) => Err(Failure::Raised(error)),
            Flow::Return(value) | Flow::Yield(value) => Ok(value),
        }
    }

    /// Writes an encoded tree through the encoder's calls.
    fn write(&self, interp: &mut Interpreter, tree: &Tree) -> Exec<()> {
        let call = |interp: &mut Interpreter, name: &str, args: Vec<Val>| -> Exec<()> {
            match self.call(interp, name, args) {
                Err(Failure::Run(end)) => Err(end),
                _ => Ok(()),
            }
        };
        let count = |len: usize| Val::Int(len as i128);
        match tree {
            Tree::Bool(value) => call(interp, "write_bool", vec![Val::Bool(*value)]),
            Tree::Unsigned(value) => {
                call(interp, "write_unsigned", vec![Val::Int(i128::from(*value))])
            }
            Tree::Signed(value) => call(interp, "write_signed", vec![Val::Int(i128::from(*value))]),
            Tree::Float(value) => call(interp, "write_float", vec![Val::F64(*value)]),
            Tree::Char(c) => call(interp, "write_char", vec![Val::Char(*c)]),
            Tree::Text(bytes) => {
                let text = byte_view(interp, bytes, false);
                call(interp, "write_text", vec![text])
            }
            Tree::Bytes(bytes) => {
                let bytes = byte_view(interp, bytes, false);
                call(interp, "write_bytes", vec![bytes])
            }
            Tree::Sequence(elements) => {
                call(interp, "begin_sequence", vec![count(elements.len())])?;
                for element in elements {
                    self.write(interp, element)?;
                }
                call(interp, "end", Vec::new())
            }
            Tree::Map(entries) => {
                call(interp, "begin_map", vec![count(entries.len())])?;
                for (key, value) in entries {
                    self.write(interp, key)?;
                    self.write(interp, value)?;
                }
                call(interp, "end", Vec::new())
            }
            Tree::Record(fields) => {
                let fields = declared(fields);
                call(interp, "begin_record", vec![count(fields.len())])?;
                for (key, value) in fields {
                    let key = byte_view(interp, key, false);
                    call(interp, "write_key", vec![key])?;
                    self.write(interp, value)?;
                }
                call(interp, "end", Vec::new())
            }
            Tree::Variant(key, payload) => {
                let key = byte_view(interp, key, false);
                let follows = Val::Bool(payload.is_some());
                call(interp, "begin_variant", vec![key, follows])?;
                if let Some(payload) = payload {
                    self.write(interp, payload)?;
                }
                call(interp, "end", Vec::new())
            }
            Tree::Raw(_) => unreachable!("kept fields written through calls"),
        }
    }

    /// The bytes of text or bytes of length `len`, read in one chunk once the room for them
    /// is known: at most `most` bytes for a fixed text, or what the cell has left otherwise.
    fn chunk(&self, interp: &mut Interpreter, len: Val, most: Option<usize>) -> Decoded<Vec<u8>> {
        let len = len.int() as u64;
        match most {
            Some(most) if len > most as u64 => return fail("OutOfRange"),
            None if interp.budgets.charged(self.cell) + u128::from(len)
                > interp.budgets.limit(self.cell) =>
            {
                return fail("OutOfMemory");
            }
            _ => {}
        }
        let out = byte_view(interp, &vec![0; len as usize], true);
        self.call(interp, "read_chunk", vec![out.clone()])?;
        let Val::View(view) = out else {
            unreachable!("a view");
        };
        Ok(interp
            .view_values(&view)
            .iter()
            .map(|byte| byte.int() as u8)
            .collect())
    }

    /// The position among `keys` of the key a decoder reads through its method `name`.
    fn keyed(
        &self,
        interp: &mut Interpreter,
        name: &str,
        keys: &[&[u8]],
    ) -> Decoded<Option<usize>> {
        let views: Vec<Val> = keys
            .iter()
            .map(|key| byte_view(interp, key, false))
            .collect();
        let count = views.len();
        let keys = Val::View(View {
            array: Loc {
                extent: interp.memory.alloc(Val::Array(Rc::new(views)), false, 0),
                path: Vec::new(),
            },
            start: 0,
            len: count,
        });
        Ok(match self.call(interp, name, vec![keys])? {
            Val::Variant(_, _, payload) if !payload.is_empty() => {
                Some(payload[0].int() as usize).filter(|index| *index < count)
            }
            _ => None,
        })
    }
}

impl Reader for Calls {
    fn read_bool(&mut self, interp: &mut Interpreter) -> Decoded<bool> {
        Ok(self.call(interp, "read_bool", Vec::new())?.bool())
    }

    fn read_unsigned(&mut self, interp: &mut Interpreter) -> Decoded<u64> {
        Ok(self.call(interp, "read_unsigned", Vec::new())?.int() as u64)
    }

    fn read_signed(&mut self, interp: &mut Interpreter) -> Decoded<i64> {
        Ok(self.call(interp, "read_signed", Vec::new())?.int() as i64)
    }

    fn read_float(&mut self, interp: &mut Interpreter) -> Decoded<f64> {
        match self.call(interp, "read_float", Vec::new())? {
            Val::F64(value) => Ok(value),
            other => panic!("a float, not {other:?}"),
        }
    }

    fn read_char(&mut self, interp: &mut Interpreter) -> Decoded<u32> {
        match self.call(interp, "read_char", Vec::new())? {
            Val::Char(c) => Ok(u32::from(c)),
            other => panic!("a char, not {other:?}"),
        }
    }

    fn read_text(&mut self, interp: &mut Interpreter, most: Option<usize>) -> Decoded<Vec<u8>> {
        let len = self.call(interp, "begin_text", Vec::new())?;
        self.chunk(interp, len, most)
    }

    fn read_bytes(&mut self, interp: &mut Interpreter) -> Decoded<Vec<u8>> {
        let len = self.call(interp, "begin_bytes", Vec::new())?;
        self.chunk(interp, len, None)
    }

    fn begin_sequence(&mut self, interp: &mut Interpreter) -> Decoded<()> {
        self.call(interp, "begin_sequence", Vec::new()).map(drop)
    }

    fn begin_map(&mut self, interp: &mut Interpreter) -> Decoded<()> {
        self.call(interp, "begin_map", Vec::new()).map(drop)
    }

    fn begin_record(&mut self, interp: &mut Interpreter) -> Decoded<()> {
        self.call(interp, "begin_record", Vec::new()).map(drop)
    }

    fn next_follows(&mut self, interp: &mut Interpreter) -> Decoded<bool> {
        Ok(self.call(interp, "next_follows", Vec::new())?.bool())
    }

    fn map_value(&mut self, _: &mut Interpreter) -> Decoded<()> {
        Ok(())
    }

    fn read_key(&mut self, interp: &mut Interpreter, keys: &[&[u8]]) -> Decoded<Option<usize>> {
        self.keyed(interp, "read_key", keys)
    }

    fn read_variant(&mut self, interp: &mut Interpreter, keys: &[&[u8]]) -> Decoded<Option<usize>> {
        self.keyed(interp, "read_variant", keys)
    }

    fn payload_follows(&mut self, interp: &mut Interpreter) -> Decoded<bool> {
        Ok(self.call(interp, "payload_follows", Vec::new())?.bool())
    }

    fn skip(&mut self, interp: &mut Interpreter, _: usize) -> Decoded<()> {
        self.call(interp, "skip", Vec::new()).map(drop)
    }

    fn end(&mut self, interp: &mut Interpreter) -> Decoded<()> {
        self.call(interp, "end", Vec::new()).map(drop)
    }

    fn finish(&mut self, _: &mut Interpreter) -> Decoded<()> {
        Ok(())
    }
}

/// A view of bytes in an extent of their own, writable or not.
fn byte_view(interp: &mut Interpreter, bytes: &[u8], writable: bool) -> Val {
    let values = bytes
        .iter()
        .map(|byte| Val::Int(i128::from(*byte)))
        .collect();
    let extent = interp
        .memory
        .alloc(Val::Array(Rc::new(values)), writable, bytes.len() as u64);
    Val::View(View {
        array: Loc {
            extent,
            path: Vec::new(),
        },
        start: 0,
        len: bytes.len(),
    })
}

enum Format {
    Wire,
    Json,
    Construction,
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
    /// The `mem::wire` bytes of a kept field's value.
    Raw(Vec<u8>),
}

/// The fields of a record but its kept ones, which only `mem::wire` writes.
fn declared(fields: &[(Vec<u8>, Tree)]) -> Vec<&(Vec<u8>, Tree)> {
    fields
        .iter()
        .filter(|(_, value)| !matches!(value, Tree::Raw(_)))
        .collect()
}

/// The fields `Kept.Fields` holds: each key with the bytes of its value.
fn kept_entries(bytes: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut reader = WireReader::new(bytes.to_vec());
    let mut entries = Vec::new();
    while reader.input.at < bytes.len() {
        let (Ok(key), Ok(value)) = (reader.framed(), reader.framed()) else {
            break;
        };
        entries.push((key, value));
    }
    entries
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
                _ => match &interp.ck.sig(*item).kind {
                    SigKind::Struct { fields } => {
                        let subst = Subst::of(
                            interp.ck.sig(*item).generics.iter().map(|def| def.param),
                            args,
                        );
                        let named = fields
                            .iter()
                            .zip(value.fields())
                            .filter(|(field, _)| !field.is_kept())
                            .map(|(field, part)| (field.name.as_str(), part, subst.ty(&field.ty)));
                        let mut tree = record(interp, named);
                        if let (Some(Val::Variant(_, 1, held)), Tree::Record(fields)) =
                            (value.fields().last(), &mut tree)
                            && interp.ck.kept_type(*item).is_some()
                        {
                            let bytes = interp.deref(&held[0]).fields()[0].clone();
                            let kept = bytes_of(&entries(interp, &bytes));
                            fields.extend(
                                kept_entries(&kept)
                                    .into_iter()
                                    .map(|(key, value)| (key, Tree::Raw(value))),
                            );
                            fields.sort_by(|left, right| left.0.cmp(&right.0));
                        }
                        tree
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
                                let named = labels
                                    .iter()
                                    .zip(payload.iter())
                                    .map(|((name, ty), part)| (name.as_str(), part, subst.ty(ty)));
                                Some(Box::new(record(interp, named)))
                            }
                        };
                        Tree::Variant(variant.name.clone().into_bytes(), encoded_payload)
                    }
                    other => panic!("an Encode type of kind {other:?}"),
                },
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
        Tree::Raw(bytes) => out.extend_from_slice(bytes),
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
            for (index, (key, value)) in declared(fields).into_iter().enumerate() {
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
        Tree::Raw(_) => unreachable!("kept fields written to JSON"),
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

/// Why decoding stopped: a decode error, by its variant's name, a decoder's own failure, or an
/// end of the run.
enum Failure {
    Decode(&'static str),
    Raised(Val),
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

/// A source of encoded values read through the calls of `core::Decoder`: a format, or a
/// decoder whose calls the interpreter runs.
trait Reader {
    fn read_bool(&mut self, interp: &mut Interpreter) -> Decoded<bool>;
    fn read_unsigned(&mut self, interp: &mut Interpreter) -> Decoded<u64>;
    fn read_signed(&mut self, interp: &mut Interpreter) -> Decoded<i64>;
    fn read_float(&mut self, interp: &mut Interpreter) -> Decoded<f64>;
    /// A char's scalar value, as written.
    fn read_char(&mut self, interp: &mut Interpreter) -> Decoded<u32>;
    /// The bytes of text, for a fixed text of at most `most` bytes.
    fn read_text(&mut self, interp: &mut Interpreter, most: Option<usize>) -> Decoded<Vec<u8>>;
    fn read_bytes(&mut self, interp: &mut Interpreter) -> Decoded<Vec<u8>>;
    fn begin_sequence(&mut self, interp: &mut Interpreter) -> Decoded<()>;
    fn begin_map(&mut self, interp: &mut Interpreter) -> Decoded<()>;
    fn begin_record(&mut self, interp: &mut Interpreter) -> Decoded<()>;
    /// Begins the record of a variant's labelled payload.
    fn begin_labelled(&mut self, interp: &mut Interpreter) -> Decoded<()> {
        self.begin_record(interp)
    }
    fn next_follows(&mut self, interp: &mut Interpreter) -> Decoded<bool>;
    /// Between a map entry's key and its value.
    fn map_value(&mut self, interp: &mut Interpreter) -> Decoded<()>;
    /// The position among `keys` of the key read, if any.
    fn read_key(&mut self, interp: &mut Interpreter, keys: &[&[u8]]) -> Decoded<Option<usize>>;
    /// The position among `keys` of the variant key read, if any.
    fn read_variant(&mut self, interp: &mut Interpreter, keys: &[&[u8]]) -> Decoded<Option<usize>>;
    fn payload_follows(&mut self, interp: &mut Interpreter) -> Decoded<bool>;
    /// Passes over the value of the field whose key was just read, at a nesting depth.
    fn skip(&mut self, interp: &mut Interpreter, depth: usize) -> Decoded<()>;
    /// Passes over the field whose key was read last, giving its key and value as `mem::wire`
    /// frames them when the reader keeps skipped fields.
    fn keep(&mut self, interp: &mut Interpreter, depth: usize) -> Decoded<Option<Vec<u8>>> {
        self.skip(interp, depth).map(|()| None)
    }
    fn end(&mut self, interp: &mut Interpreter) -> Decoded<()>;
    /// Fails on what is left after the value.
    fn finish(&mut self, interp: &mut Interpreter) -> Decoded<()>;
}

/// The position of a key among `keys`.
fn position(keys: &[&[u8]], key: &[u8]) -> Option<usize> {
    keys.iter().position(|named| *named == key)
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
                self.reader.begin_record(self.interp)?;
                if self.reader.next_follows(self.interp)? {
                    self.reader.read_key(self.interp, &[])?;
                    return fail("UnknownKey");
                }
                self.reader.end(self.interp)?;
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
            Scalar::Bool => Val::Bool(self.reader.read_bool(self.interp)?),
            Scalar::Char => match char::from_u32(self.reader.read_char(self.interp)?) {
                Some(c) => Val::Char(c),
                None => return fail("Invalid"),
            },
            Scalar::F64 => Val::F64(self.reader.read_float(self.interp)?),
            Scalar::F32 => Val::F32(self.reader.read_float(self.interp)? as f32),
            _ if scalar.is_signed() => {
                let value = i128::from(self.reader.read_signed(self.interp)?);
                match in_range(value) {
                    true => Val::Int(value),
                    false => return fail("OutOfRange"),
                }
            }
            _ => {
                let value = i128::from(self.reader.read_unsigned(self.interp)?);
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
                        let text = self.reader.read_text(self.interp, None)?;
                        if std::str::from_utf8(&text).is_err() {
                            return fail("InvalidText");
                        }
                        text
                    }
                    _ => self.reader.read_bytes(self.interp)?,
                };
                let values = bytes
                    .into_iter()
                    .map(|byte| Val::Int(i128::from(byte)))
                    .collect();
                self.held(&path, values, &Ty::Scalar(Scalar::U8))?
            }
            "text::Fixed" => {
                let bound = count(0);
                let text = self.reader.read_text(self.interp, Some(bound))?;
                if std::str::from_utf8(&text).is_err() {
                    return fail("InvalidText");
                }
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
                self.reader.begin_map(self.interp)?;
                let mut keys = Vec::new();
                let mut pairs = Vec::new();
                let key_value = self.interp.ty_item("mem", "KeyValue");
                while self.reader.next_follows(self.interp)? {
                    let key = self.value(&arg(0))?;
                    self.reader.map_value(self.interp)?;
                    let value = self.value(&arg(1))?;
                    keys.push(key.clone());
                    pairs.push(Val::Struct(key_value, Rc::new(vec![key, value])));
                }
                self.reader.end(self.interp)?;
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
        let open = self.interp.ck.db.ast(item).is_open();
        let aliases = self.interp.ck.key_aliases(item);
        match &sig.kind {
            SigKind::Struct { fields } => {
                let named: Vec<(String, Ty)> = fields
                    .iter()
                    .filter(|field| !field.is_kept())
                    .map(|field| (field.name.clone(), subst.ty(&field.ty)))
                    .collect();
                let kept = self.interp.ck.kept_type(item).map(|_| Vec::new());
                let (mut values, kept) = self.record(&named, open, kept, &aliases)?;
                if let Some(kept) = kept {
                    values.push(self.kept(kept)?);
                }
                let value = Val::Struct(item, Rc::new(values));
                let ty = Ty::item(item, args.to_vec());
                match self.interp.predicates_hold(item, true, &value, &ty) {
                    true => Ok(value),
                    false => fail("Invalid"),
                }
            }
            SigKind::Enum { variants } => {
                self.nest()?;
                let names: Vec<&[u8]> = variants
                    .iter()
                    .map(|variant| variant.name.as_bytes())
                    .collect();
                let Some(index) = self.named(&names, &aliases, true)? else {
                    return fail("UnknownVariant");
                };
                let follows = self.reader.payload_follows(self.interp)?;
                let payload = match (&variants[index].payload, follows) {
                    (Payload::None, false) => Vec::new(),
                    (Payload::Single(ty), true) => vec![self.value(&subst.ty(ty))?],
                    (Payload::Labelled(labels), true) => {
                        let named: Vec<(String, Ty)> = labels
                            .iter()
                            .map(|(name, ty)| (name.clone(), subst.ty(ty)))
                            .collect();
                        self.labelled(&named)?
                    }
                    _ => return fail("Malformed"),
                };
                self.reader.end(self.interp)?;
                self.depth -= 1;
                Ok(Val::Variant(item, index as u32, Rc::new(payload)))
            }
            other => panic!("an Encode type of kind {other:?}"),
        }
    }

    /// The values of a labelled payload's fields in order.
    fn labelled(&mut self, named: &[(String, Ty)]) -> Decoded<Vec<Val>> {
        self.nest()?;
        self.reader.begin_labelled(self.interp)?;
        self.fields(named, false, None, &BTreeMap::new())
            .map(|(values, _)| values)
    }

    /// The values of a struct's named fields in order, as the decoding rules read them, given
    /// whether it decodes as open, and the kept fields of a flexible struct, empty at first.
    fn record(
        &mut self,
        named: &[(String, Ty)],
        open: bool,
        kept: Option<Vec<u8>>,
        aliases: &BTreeMap<Vec<u8>, Vec<u8>>,
    ) -> Decoded<(Vec<Val>, Option<Vec<u8>>)> {
        self.nest()?;
        self.reader.begin_record(self.interp)?;
        self.fields(named, open, kept, aliases)
    }

    /// The fields of a record or labelled payload begun, and its end.
    fn fields(
        &mut self,
        named: &[(String, Ty)],
        open: bool,
        mut kept: Option<Vec<u8>>,
        aliases: &BTreeMap<Vec<u8>, Vec<u8>>,
    ) -> Decoded<(Vec<Val>, Option<Vec<u8>>)> {
        let mut values: Vec<Option<Val>> = vec![None; named.len()];
        let names: Vec<&[u8]> = named.iter().map(|(name, _)| name.as_bytes()).collect();
        while self.reader.next_follows(self.interp)? {
            match self.named(&names, aliases, false)? {
                Some(index) if values[index].is_some() => return fail("DuplicateKey"),
                Some(index) => values[index] = Some(self.value(&named[index].1)?),
                None if open => match kept.as_mut() {
                    Some(kept) => kept.extend(
                        self.reader
                            .keep(self.interp, self.depth)?
                            .unwrap_or_default(),
                    ),
                    None => self.reader.skip(self.interp, self.depth)?,
                },
                None => return fail("UnknownKey"),
            }
        }
        self.reader.end(self.interp)?;
        self.depth -= 1;
        let values = named
            .iter()
            .zip(values)
            .map(|((_, ty), value)| match value {
                Some(value) => Ok(value),
                None if open => self.default(ty),
                None => fail("MissingField"),
            })
            .collect::<Decoded<Vec<Val>>>()?;
        Ok((values, kept))
    }

    /// The kept fields of a flexible struct, its skipped fields' bytes as `mem::wire` frames
    /// them.
    fn kept(&mut self, bytes: Vec<u8>) -> Decoded<Val> {
        let kept = self.interp.ty_item("mem::wire", "Kept");
        if bytes.is_empty() {
            return Ok(Val::Variant(kept, 0, Rc::new(Vec::new())));
        }
        let values = bytes
            .into_iter()
            .map(|byte| Val::Int(i128::from(byte)))
            .collect();
        let held = self.held("mem::Bytes", values, &Ty::Scalar(Scalar::U8))?;
        let untrusted = self.interp.ty_item("core", "Untrusted");
        let fields = Val::Struct(untrusted, Rc::new(vec![held]));
        Ok(Val::Variant(kept, 1, Rc::new(vec![fields])))
    }

    /// The position among `names` of the field or variant the key read next names, directly or
    /// through an alias: the keys read are the names, then the old keys of the aliases of names.
    fn named(
        &mut self,
        names: &[&[u8]],
        aliases: &BTreeMap<Vec<u8>, Vec<u8>>,
        variant: bool,
    ) -> Decoded<Option<usize>> {
        let aliased: Vec<(&[u8], usize)> = aliases
            .iter()
            .filter_map(|(old, current)| Some((old.as_slice(), position(names, current)?)))
            .collect();
        let keys: Vec<&[u8]> = names
            .iter()
            .copied()
            .chain(aliased.iter().map(|(old, _)| *old))
            .collect();
        let read = match variant {
            true => self.reader.read_variant(self.interp, &keys)?,
            false => self.reader.read_key(self.interp, &keys)?,
        };
        Ok(read.map(|index| match index.checked_sub(names.len()) {
            Some(alias) => aliased[alias].1,
            None => index,
        }))
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
        let names: Vec<&[u8]> = keys.iter().map(|(name, _)| name.as_bytes()).collect();
        let Some(index) = self.named(&names, &BTreeMap::new(), true)? else {
            return fail("UnknownVariant");
        };
        let follows = self.reader.payload_follows(self.interp)?;
        let payload = match (&keys[index].1, follows) {
            (Some(ty), true) => Some(self.value(ty)?),
            (None, false) => None,
            _ => return fail("Malformed"),
        };
        self.reader.end(self.interp)?;
        self.depth -= 1;
        Ok((index, payload))
    }

    fn sequence(&mut self, element: &Ty) -> Decoded<Vec<Val>> {
        self.nest()?;
        self.reader.begin_sequence(self.interp)?;
        let mut elements = Vec::new();
        while self.reader.next_follows(self.interp)? {
            elements.push(self.value(element)?);
        }
        self.reader.end(self.interp)?;
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

    /// Passes over the whitespace of the text formats.
    fn space(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.bytes.get(self.at) {
            self.at += 1;
        }
    }

    /// The next byte after whitespace, not passed over.
    fn peek(&mut self) -> Decoded<u8> {
        self.space();
        self.bytes
            .get(self.at)
            .copied()
            .ok_or(Failure::Decode("Truncated"))
    }

    /// Passes over a byte after whitespace.
    fn expect(&mut self, byte: u8) -> Decoded<()> {
        match self.peek()? == byte {
            true => {
                self.at += 1;
                Ok(())
            }
            false => fail("Malformed"),
        }
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
        key_start: usize,
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
    fn read_bool(&mut self, _: &mut Interpreter) -> Decoded<bool> {
        match self.input.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => fail("Malformed"),
        }
    }

    fn read_unsigned(&mut self, _: &mut Interpreter) -> Decoded<u64> {
        self.uvar()
    }

    fn read_signed(&mut self, _: &mut Interpreter) -> Decoded<i64> {
        let zigzag = self.uvar()?;
        Ok(((zigzag >> 1) as i64) ^ -((zigzag & 1) as i64))
    }

    fn read_float(&mut self, _: &mut Interpreter) -> Decoded<f64> {
        let bytes = self.input.take(8)?;
        Ok(f64::from_bits(u64::from_le_bytes(
            bytes.try_into().expect("eight bytes"),
        )))
    }

    fn read_char(&mut self, _: &mut Interpreter) -> Decoded<u32> {
        Ok(u32::try_from(self.uvar()?).unwrap_or(u32::MAX))
    }

    fn read_text(&mut self, _: &mut Interpreter, _: Option<usize>) -> Decoded<Vec<u8>> {
        self.framed()
    }

    fn read_bytes(&mut self, _: &mut Interpreter) -> Decoded<Vec<u8>> {
        self.framed()
    }

    fn begin_sequence(&mut self, _: &mut Interpreter) -> Decoded<()> {
        let count = self.uvar()?;
        if count > (self.input.bytes.len() - self.input.at) as u64 {
            return fail("Truncated");
        }
        self.frames.push(WireFrame::Counted(count));
        Ok(())
    }

    fn begin_map(&mut self, interp: &mut Interpreter) -> Decoded<()> {
        self.begin_sequence(interp)
    }

    fn begin_record(&mut self, _: &mut Interpreter) -> Decoded<()> {
        let left = self.uvar()?;
        if left > (self.input.bytes.len() - self.input.at) as u64 {
            return fail("Truncated");
        }
        self.frames.push(WireFrame::Record {
            left,
            value_end: None,
            last: None,
            key_start: 0,
        });
        Ok(())
    }

    fn next_follows(&mut self, _: &mut Interpreter) -> Decoded<bool> {
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

    fn map_value(&mut self, _: &mut Interpreter) -> Decoded<()> {
        Ok(())
    }

    fn read_key(&mut self, _: &mut Interpreter, keys: &[&[u8]]) -> Decoded<Option<usize>> {
        let start = self.input.at;
        let key = self.framed()?;
        let end = self.length_end()?;
        let Some(WireFrame::Record {
            value_end,
            last,
            key_start,
            ..
        }) = self.frames.last_mut()
        else {
            return fail("Malformed");
        };
        *key_start = start;
        match last.as_ref().map(|last| key.as_slice().cmp(last)) {
            Some(std::cmp::Ordering::Equal) => return fail("DuplicateKey"),
            Some(std::cmp::Ordering::Less) => return fail("Malformed"),
            _ => {}
        }
        *value_end = Some(end);
        let named = position(keys, &key);
        *last = Some(key);
        Ok(named)
    }

    fn read_variant(&mut self, _: &mut Interpreter, keys: &[&[u8]]) -> Decoded<Option<usize>> {
        let key = self.framed()?;
        self.frames.push(WireFrame::Variant { payload_end: None });
        Ok(position(keys, &key))
    }

    fn payload_follows(&mut self, _: &mut Interpreter) -> Decoded<bool> {
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

    fn skip(&mut self, _: &mut Interpreter, _: usize) -> Decoded<()> {
        if let Some(WireFrame::Record { value_end, .. }) = self.frames.last_mut()
            && let Some(end) = value_end.take()
        {
            self.input.at = end;
            return Ok(());
        }
        fail("Malformed")
    }

    fn keep(&mut self, _: &mut Interpreter, _: usize) -> Decoded<Option<Vec<u8>>> {
        if let Some(WireFrame::Record {
            value_end,
            key_start,
            ..
        }) = self.frames.last_mut()
            && let Some(end) = value_end.take()
        {
            let entry = self.input.bytes[*key_start..end].to_vec();
            self.input.at = end;
            return Ok(Some(entry));
        }
        fail("Malformed")
    }

    fn end(&mut self, _: &mut Interpreter) -> Decoded<()> {
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

    fn finish(&mut self, _: &mut Interpreter) -> Decoded<()> {
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

    fn literal(&mut self, word: &[u8]) -> bool {
        self.input.space();
        let found = self.input.bytes[self.input.at..].starts_with(word);
        if found {
            self.input.at += word.len();
        }
        found
    }

    /// A JSON string's bytes, its escapes resolved.
    fn string(&mut self) -> Decoded<Vec<u8>> {
        self.input.expect(b'"')?;
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
        self.input.space();
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
        match self.input.peek()? {
            b'"' => self.string().map(|_| ()),
            b'{' | b'[' => {
                let close = if self.input.peek()? == b'{' {
                    b'}'
                } else {
                    b']'
                };
                let object = close == b'}';
                self.input.at += 1;
                if self.input.peek()? == close {
                    self.input.at += 1;
                    return Ok(());
                }
                loop {
                    if object {
                        self.string()?;
                        self.input.expect(b':')?;
                    }
                    self.skip_value()?;
                    match self.input.peek()? {
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
        if self.input.peek()? == close {
            return Ok(false);
        }
        if started {
            self.input.expect(b',')?;
        }
        Ok(true)
    }
}

impl Reader for JsonReader {
    fn read_bool(&mut self, _: &mut Interpreter) -> Decoded<bool> {
        if self.literal(b"true") {
            return Ok(true);
        }
        match self.literal(b"false") {
            true => Ok(false),
            false => fail("Malformed"),
        }
    }

    fn read_unsigned(&mut self, _: &mut Interpreter) -> Decoded<u64> {
        u64::try_from(self.integer()?).or_else(|_| fail("OutOfRange"))
    }

    fn read_signed(&mut self, _: &mut Interpreter) -> Decoded<i64> {
        let value = self.integer()?;
        i64::try_from(value).or_else(|_| fail("OutOfRange"))
    }

    fn read_float(&mut self, _: &mut Interpreter) -> Decoded<f64> {
        if self.input.peek()? == b'"' {
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

    fn read_char(&mut self, _: &mut Interpreter) -> Decoded<u32> {
        let text = self.string()?;
        let text = String::from_utf8(text).map_err(|_| Failure::Decode("InvalidText"))?;
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Ok(c as u32),
            _ => fail("Malformed"),
        }
    }

    fn read_text(&mut self, _: &mut Interpreter, _: Option<usize>) -> Decoded<Vec<u8>> {
        self.string()
    }

    fn read_bytes(&mut self, _: &mut Interpreter) -> Decoded<Vec<u8>> {
        let text = self.string()?;
        from_base64(&text).ok_or(Failure::Decode("Malformed"))
    }

    fn begin_sequence(&mut self, _: &mut Interpreter) -> Decoded<()> {
        self.input.expect(b'[')?;
        self.frames.push(JsonFrame::Array { started: false });
        Ok(())
    }

    fn begin_map(&mut self, _: &mut Interpreter) -> Decoded<()> {
        self.input.expect(b'[')?;
        self.frames.push(JsonFrame::Map { started: false });
        Ok(())
    }

    fn begin_record(&mut self, _: &mut Interpreter) -> Decoded<()> {
        self.input.expect(b'{')?;
        self.frames.push(JsonFrame::Object { started: false });
        Ok(())
    }

    fn next_follows(&mut self, _: &mut Interpreter) -> Decoded<bool> {
        match self.frames.last() {
            Some(JsonFrame::Object { .. }) => self.element_follows(b'}'),
            Some(JsonFrame::Map { started }) => {
                if *started {
                    self.input.expect(b']')?;
                }
                let follows = self.element_follows(b']')?;
                if follows {
                    self.input.expect(b'[')?;
                }
                Ok(follows)
            }
            _ => self.element_follows(b']'),
        }
    }

    fn map_value(&mut self, _: &mut Interpreter) -> Decoded<()> {
        self.input.expect(b',')
    }

    fn read_key(&mut self, _: &mut Interpreter, keys: &[&[u8]]) -> Decoded<Option<usize>> {
        let key = self.string()?;
        self.input.expect(b':')?;
        Ok(position(keys, &key))
    }

    fn read_variant(&mut self, _: &mut Interpreter, keys: &[&[u8]]) -> Decoded<Option<usize>> {
        if self.input.peek()? == b'"' {
            self.frames.push(JsonFrame::Variant { payload: false });
            return Ok(position(keys, &self.string()?));
        }
        self.input.expect(b'{')?;
        let key = self.string()?;
        self.input.expect(b':')?;
        self.frames.push(JsonFrame::Variant { payload: true });
        Ok(position(keys, &key))
    }

    fn payload_follows(&mut self, _: &mut Interpreter) -> Decoded<bool> {
        match self.frames.last() {
            Some(JsonFrame::Variant { payload }) => Ok(*payload),
            _ => fail("Malformed"),
        }
    }

    fn skip(&mut self, _: &mut Interpreter, _: usize) -> Decoded<()> {
        self.skip_value()
    }

    fn end(&mut self, _: &mut Interpreter) -> Decoded<()> {
        match self.frames.pop() {
            Some(JsonFrame::Array { .. }) => self.input.expect(b']'),
            Some(JsonFrame::Object { .. }) => self.input.expect(b'}'),
            Some(JsonFrame::Map { .. }) => self.input.expect(b']'),
            Some(JsonFrame::Variant { payload: true }) => self.input.expect(b'}'),
            Some(JsonFrame::Variant { payload: false }) => Ok(()),
            None => fail("Malformed"),
        }
    }

    fn finish(&mut self, _: &mut Interpreter) -> Decoded<()> {
        self.input.space();
        match self.input.at == self.input.bytes.len() {
            true => Ok(()),
            false => fail("Trailing"),
        }
    }
}

#[cfg(test)]
mod tests;
