//! The wire encoding of protocols in interpreter runs: the encodings of calls of operations as
//! members of `sys::Wire`, `sys::Wire.hash`, `sys::Wire.schema` and `sys::Reply.decode`. A
//! `sys::Handle` holds the value of a handle type, then that type's handle hash.

use std::rc::Rc;

use super::super::super::value::Val;
use super::super::super::{Exec, Flow, Interpreter, Site};
use super::super::stores::{append, cell_of, entries, exhausted, place, release};
use super::{Decoding, Failure, Tree, WireReader, constant, encoded, write_wire};
use crate::check::db::ItemId;
use crate::check::sig::SigKind;
use crate::check::ty::{Arg, Scalar, Ty};
use crate::nir::Span;

/// A member of `sys::Wire` or `sys::Reply`; none for another.
pub fn member(
    interp: &mut Interpreter,
    owner: &str,
    member: &str,
    type_args: &[Arg],
    args: &[Val],
    at: (ItemId, Span),
    site: Site,
) -> Option<Exec<Flow>> {
    let protocol = || match type_args.first().and_then(Arg::ty) {
        Some(Ty::Item(protocol, _)) => *protocol,
        other => panic!("a protocol, not {other:?}"),
    };
    Some(match (owner, member) {
        ("sys::Wire", "hash") => {
            let (hash, _) = interp.ck.protocol_hashes(protocol());
            let item = interp.ty_item("sys", "ProtocolHash");
            Ok(Flow::Return(Val::Struct(
                item,
                Rc::new(vec![bytes(&hash.0)]),
            )))
        }
        ("sys::Wire", "schema") => {
            let schema = Ty::item(interp.ty_item("sys", "ProtocolSchema"), Vec::new());
            let encoding = interp.ck.protocol_schema(protocol());
            constant(interp, encoding, &schema, &args[0], at, site)
        }
        ("sys::Reply", "decode") => decode_reply(interp, type_args, args, at, site),
        _ => return None,
    })
}

/// An array value of bytes.
fn bytes(bytes: &[u8]) -> Val {
    Val::Array(Rc::new(
        bytes
            .iter()
            .map(|byte| Val::Int(i128::from(*byte)))
            .collect(),
    ))
}

/// The position of a field of a std struct.
fn field(interp: &Interpreter, item: ItemId, name: &str) -> u32 {
    match &interp.ck.sig(item).kind {
        SigKind::Struct { fields } => fields
            .iter()
            .position(|field| field.name == name)
            .expect("a field of the struct") as u32,
        _ => panic!("a struct"),
    }
}

/// `Wire[P].op(mut out, ...)`: the encoding of the call appended to `out.bytes`, each value of a
/// handle type taken as the index of its handle, which goes to `out.handles`, trapping when
/// memory is exhausted; a request's `sys::Reply`.
pub fn encode_call(
    interp: &mut Interpreter,
    protocol: ItemId,
    method: usize,
    args: &[Val],
    at: (ItemId, Span),
) -> Exec<Flow> {
    let SigKind::Protocol { operations, .. } = &interp.ck.sig(protocol).kind else {
        unreachable!("a protocol's signature")
    };
    let operation = operations[method].clone();
    let encoded_item = interp.ty_item("sys", "Encoded");
    let handle_item = interp.ty_item("sys", "Handle");
    let out = place(&args[0]);
    let bytes_at = out.with(field(interp, encoded_item, "bytes"));
    let handles_at = out.with(field(interp, encoded_item, "handles"));
    let held = interp.memory.get(&handles_at).clone();
    let first = entries(interp, &held).len();
    let mut named: Vec<_> = operation.sig.params.iter().zip(&args[1..]).collect();
    named.sort_by(|left, right| left.0.name.cmp(&right.0.name));
    let mut carried = Vec::new();
    let mut fields = Vec::new();
    for (param, value) in named {
        let tree = argument_tree(interp, value, &param.ty, first, &mut carried);
        fields.push((param.name.clone().into_bytes(), tree));
    }
    let payload = (!fields.is_empty()).then(|| Box::new(Tree::Record(fields)));
    let mut written = Vec::new();
    write_wire(
        &Tree::Variant(operation.name.clone().into_bytes(), payload),
        &mut written,
    );
    let written: Vec<Val> = written
        .into_iter()
        .map(|byte| Val::Int(i128::from(byte)))
        .collect();
    let handle = Ty::item(handle_item, Vec::new());
    let appended = append(interp, &bytes_at, &written, &Ty::Scalar(Scalar::U8))
        .and_then(|()| append(interp, &handles_at, &carried, &handle));
    if appended.is_none() {
        return exhausted(interp, false, at);
    }
    for value in &args[1..] {
        release(interp, value);
    }
    Ok(Flow::Return(match interp.ck.is_request(protocol, method) {
        true => Val::Struct(interp.ty_item("sys", "Reply"), Rc::new(Vec::new())),
        false => Val::Unit,
    }))
}

/// The tree of an argument of a call: a value of a handle type as the index among the handles
/// beside the encoding, the first at `first`, of the handle it adds to `carried`; a handle record
/// as the record of its fields; another value as it encodes.
fn argument_tree(
    interp: &Interpreter,
    value: &Val,
    ty: &Ty,
    first: usize,
    carried: &mut Vec<Val>,
) -> Tree {
    if interp.ck.is_handle(ty) {
        let hash = interp.ck.handle_hash(ty);
        let fields = vec![interp.deref(value), bytes(&hash.0)];
        carried.push(Val::Struct(
            interp.ty_item("sys", "Handle"),
            Rc::new(fields),
        ));
        return Tree::Unsigned((first + carried.len() - 1) as u64);
    }
    let Some(fields) = interp.ck.handle_record(ty) else {
        return encoded(interp, value, ty);
    };
    let value = interp.deref(value);
    let mut named: Vec<_> = fields.into_iter().zip(value.fields().iter()).collect();
    named.sort_by(|left, right| left.0.0.cmp(&right.0.0));
    let parts = (named.into_iter())
        .map(|((name, ty), part)| {
            let tree = argument_tree(interp, part, &ty, first, carried);
            (name.into_bytes(), tree)
        })
        .collect();
    Tree::Record(parts)
}

/// `reply.decode(raw, handles, alloc)`: the result of an `Ok` reply decoded from `raw` against
/// `handles`, failing with `CallError.Failed` holding the failure of an `Err` one and with
/// `CallError.Malformed` when the reply does not decode or leaves bytes or handles unread.
fn decode_reply(
    interp: &mut Interpreter,
    type_args: &[Arg],
    args: &[Val],
    at: (ItemId, Span),
    site: Site,
) -> Exec<Flow> {
    let result_item = interp.ty_item("core", "Result");
    let reply = Ty::item(result_item, type_args.to_vec());
    let raw = interp.deref(&args[1]).fields()[0].clone();
    let input = interp.bytes(&raw);
    let view = interp.view(&args[2]);
    let handles = interp.view_values(&view);
    let cell = cell_of(interp, &args[3]);
    let mut reader = WireReader::new(input);
    let mut decoding = Decoding {
        interp,
        reader: &mut reader,
        cell,
        depth: 0,
        at,
        site,
        carried: Some((handles, 0)),
    };
    let decoded = decoding.value(&reply).and_then(|value| {
        decoding.reader.finish(decoding.interp)?;
        match decoding.carried {
            Some((ref handles, taken)) if taken != handles.len() => super::fail("Trailing"),
            _ => Ok(value),
        }
    });
    let call_error = interp.ty_item("sys", "CallError");
    let failed = |interp: &Interpreter, variant: &str, payload: Val| {
        let index = interp.variant_index(call_error, variant);
        Ok(Flow::Fail(Val::Variant(
            call_error,
            index,
            Rc::new(vec![payload]),
        )))
    };
    match decoded {
        Ok(Val::Variant(_, index, payload)) => {
            match index == interp.variant_index(result_item, "Ok") {
                true => Ok(Flow::Return(payload[0].clone())),
                false => failed(interp, "Failed", payload[0].clone()),
            }
        }
        Ok(other) => panic!("a reply, not {other:?}"),
        Err(Failure::Decode(kind)) => {
            let error = interp.ty_item("core", "DecodeError");
            let index = interp.variant_index(error, kind);
            failed(
                interp,
                "Malformed",
                Val::Variant(error, index, Rc::new(Vec::new())),
            )
        }
        Err(Failure::Raised(error)) => panic!("no decoder raises {error:?} in a reply"),
        Err(Failure::Run(end)) => Err(end),
    }
}
