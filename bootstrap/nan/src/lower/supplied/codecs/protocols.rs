//! The wire encoding of protocols: the encodings of calls of operations as members of
//! `sys::Wire`, `sys::Wire.hash`, `sys::Wire.schema`, `sys::Wire.serve`,
//! `sys::Wire.encode_message`, `sys::Reply.decode`, and the handles `sys::Handle.of` makes and
//! capability ids read.

use cranelift_codegen::ir::{InstBuilder, TrapCode, Value, condcodes::IntCC, types};

use super::super::super::abi::Abi;
use super::super::super::body::Held;
use super::super::super::call::Invoked;
use super::super::super::fx::{Fx, PTR};
use super::super::super::{Instance, Lowering};
use super::super::mem::vector;
use super::super::tasks::protocols::{Answer, await_reply};
use super::super::{Supply, entry as supplied_entry, tasks::address_of};
use super::{
    BYTE, DecodeFormat, Format, HANDLE, Kind, constant, decode_constant, decode_entry,
    decode_every, encode_case, encoder, every_case, failure, kind_of, read_from, returned,
    same_hash, writer,
};
use crate::check::db::{ItemId, TypeKey};
use crate::check::sig::SigKind;
use crate::check::ty::{Arg, Args, Ty};
use crate::nir::Role;

/// A `sys::Handle`: the generational handle, then its type's handle hash.
pub mod handle {
    pub const HANDLE: u64 = 0;
    pub const HASH: u64 = 8;
}

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    match supply.path.as_str() {
        "sys::Wire.hash" => hash(lo, fx, supply),
        "sys::Wire.schema" => {
            let schema = lo.ck.protocol_schema(protocol_of(supply));
            constant(lo, fx, supply, &schema);
        }
        "sys::Wire.serve" => serve(lo, fx, supply),
        "sys::Wire.encode_message" => encode_message(lo, fx, supply),
        "sys::Wire.operations" => {
            let schemas = lo.ck.operation_schemas(protocol_of(supply));
            constant(lo, fx, supply, &schemas);
        }
        "sys::Wire.encode_any" => encode_any(lo, fx, supply),
        "sys::Wire.decode_any" => decode_any(lo, fx, supply),
        "sys::Reply.decode" => decode_reply(lo, fx, supply),
        "sys::Handle.of" => handle_of(lo, fx, supply),
        "sys::CapabilityId.of_handle" => capability_of(lo, fx, supply),
        _ => return false,
    }
    true
}

/// `Handle.of(capability)`: the handle holding `capability` with the handle hash of its type.
fn handle_of(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let hash = lo.ck.handle_hash(&supply.type_arg(0));
    let sret = supply.sret.expect("a handle's address");
    let word = fx.load(types::I64, supply.param(0).address(), 0);
    fx.store(word, sret, handle::HANDLE);
    let data = lo.bytes_data(&hash.0);
    let data = lo.data_address(fx, data);
    let at = fx.offset(sret, handle::HASH);
    lo.copy(fx, at, data, 33, 1);
    supply.done(lo, fx);
}

/// `id.of_handle(handle)`: `Some` of the capability `handle` holds when its handle hash is that
/// of the id's capability type, `None` otherwise.
fn capability_of(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let hash = lo.ck.handle_hash(&supply.type_arg(0));
    let handle = supply.param(1).address();
    let wanted = lo.bytes_data(&hash.0);
    let wanted = lo.data_address(fx, wanted);
    let carried = fx.offset(handle, handle::HASH);
    let same = same_hash(lo, fx, wanted, carried);
    let option = supply.abi.result.clone();
    fx.when(same, |fx| {
        let word = fx.load(types::I64, handle, handle::HANDLE);
        let slot = fx.slot(8, 8);
        fx.store(word, slot, 0);
        let some = lo.variant_value(fx, &option, "Some", &[Held::Address(slot)], None);
        supply.ret(lo, fx, some);
    });
    let none = lo.none_value(fx, &option);
    supply.ret(lo, fx, none);
}

/// The protocol a `sys::Wire` operation's first type argument names.
fn protocol_of(supply: &Supply) -> ItemId {
    match supply.type_arg(0) {
        Ty::Item(protocol, _) => protocol,
        other => panic!("a protocol, not {other:?}"),
    }
}

/// The address of a protocol's hash, as read-only data.
fn protocol_hash(lo: &mut Lowering, fx: &mut Fx, protocol: ItemId) -> Value {
    let (hash, _) = lo.ck.protocol_hashes(protocol);
    let data = lo.bytes_data(&hash.0);
    lo.data_address(fx, data)
}

/// `Wire.hash()`: the protocol hash of P.
fn hash(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let result = supply.abi.result.clone();
    let layout = lo.layout(&result);
    let target = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    let hash = protocol_hash(lo, fx, protocol_of(supply));
    lo.copy(fx, target, hash, 33, 1);
    let held = lo.held_at(fx, &result, target);
    supply.ret(lo, fx, held);
}

/// The addresses of the bytes and the handles of the encoded call or reply at `encoded`.
fn encoded_parts(lo: &mut Lowering, fx: &mut Fx, encoded: Value) -> (Value, Value) {
    let ty = lo.std_ty("sys", "Encoded", Vec::new());
    let bytes = lo.layouts.field(lo.ck, &ty, None, "bytes").offset;
    let handles = lo.layouts.field(lo.ck, &ty, None, "handles").offset;
    (fx.offset(encoded, bytes), fx.offset(encoded, handles))
}

/// Appends what `write` writes through a writer to the bytes at `bytes` and its handles to the
/// list at `handles`: measured, room made for exactly it, then written there, trapping when
/// memory is exhausted.
fn append(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    (bytes, handles): (Value, Value),
    write: impl Fn(&mut Lowering, &mut Fx, Value),
) {
    let first = fx.load(PTR, handles, vector::LEN);
    let writing = |fx: &mut Fx, buffer: Value| {
        let state = fx.slot(writer::SIZE, 8);
        fx.store(buffer, state, writer::BUFFER);
        let zero = fx.ptr(0);
        fx.store(zero, state, writer::LEN);
        fx.store(handles, state, writer::HANDLES);
        fx.store(first, state, writer::NEXT);
        state
    };
    let zero = fx.ptr(0);
    let measuring = writing(fx, zero);
    write(lo, fx, measuring);
    let size = fx.load(PTR, measuring, writer::LEN);
    let next = fx.load(PTR, measuring, writer::NEXT);
    let len = fx.load(PTR, bytes, vector::LEN);
    let (needed, overflow) = fx.b.ins().uadd_overflow(len, size);
    let drawn = fx.choose_in(
        lo,
        overflow,
        &[types::I8],
        |_, fx| vec![fx.int(types::I8, 0)],
        |lo, fx| {
            let bytes_drawn = vector::reserve(lo, fx, bytes, needed, BYTE);
            let handles_drawn = vector::reserve(lo, fx, handles, next, HANDLE);
            vec![fx.b.ins().band(bytes_drawn, handles_drawn)]
        },
    )[0];
    let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, drawn, 0);
    fx.when(failed, |fx| supply.exhausted(lo, fx, &[]));
    let data = fx.load(PTR, bytes, vector::DATA);
    let target = fx.b.ins().iadd(data, len);
    let state = writing(fx, target);
    write(lo, fx, state);
    fx.store(needed, bytes, vector::LEN);
}

/// `Wire[P].op(mut out, ...)`: the call's message, encoded as its variant, appended to `out`.
pub fn encode_call(lo: &mut Lowering, fx: &mut Fx, abi: &Abi, protocol: ItemId, method: usize) {
    let (sret, failure, params) = supplied_entry(fx, abi);
    let message = Ty::item(protocol, Vec::new());
    let name = lo.ck.method_name(protocol, method);
    let supply = Supply {
        abi,
        args: Args::from(Vec::new()),
        path: format!("sys::Wire.{name}"),
        owner: None,
        sret,
        failure,
        params,
        wrap: None,
    };
    let held = lo.variant_value(fx, &message, &name, &supply.params[1..], None);
    let value = address_of(lo, fx, &message, held);
    let Kind::Variant(cases) = kind_of(lo, &message) else {
        unreachable!("a protocol encodes as a variant")
    };
    let case = (cases.into_iter())
        .find(|case| case.index == method)
        .expect("a pub operation");
    let parts = encoded_parts(lo, fx, supply.param(0).address());
    append(lo, fx, &supply, parts, |lo, fx, state| {
        encode_case(lo, fx, Format::Wire, &message, &case, value, state)
    });
    lo.release(fx, &message, value);
    supply.done(lo, fx);
}

/// `Wire[P].encode_message(message, mut out)`: the call the message makes, encoded as its
/// variant, appended to `out`, and `Some` of its operation's position among the operations of
/// P's protocol schema; `None` for an operation that is not `pub`.
fn encode_message(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let message = Ty::item(protocol_of(supply), Vec::new());
    let value = address_of(lo, fx, &message, supply.param(0));
    let Kind::Variant(cases) = kind_of(lo, &message) else {
        unreachable!("a protocol encodes as a variant")
    };
    let mut keys: Vec<&str> = cases.iter().map(|case| case.key.as_str()).collect();
    keys.sort_unstable();
    let positions: Vec<usize> = (cases.iter())
        .map(|case| {
            keys.binary_search(&case.key.as_str())
                .expect("a pub operation")
        })
        .collect();
    let option = supply.abi.result.clone();
    let index = lo.variant_index(fx, &message, value);
    let parts = encoded_parts(lo, fx, supply.param(1).address());
    for (case, position) in cases.iter().zip(positions) {
        let this =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, index, case.index as i64);
        fx.when(this, |fx| {
            append(lo, fx, supply, parts, |lo, fx, state| {
                encode_case(lo, fx, Format::Wire, &message, case, value, state)
            });
            let position = fx.int(types::I32, position as i64);
            let some = lo.variant_value(fx, &option, "Some", &[Held::Scalar(position)], None);
            supply.ret(lo, fx, some);
        });
    }
    let none = lo.none_value(fx, &option);
    supply.ret(lo, fx, none);
}

/// `Wire[P].encode_any(message, mut out)`: the call the message makes, of any operation,
/// encoded as its variant, appended to `out`, and its operation's number.
fn encode_any(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let protocol = protocol_of(supply);
    let message = Ty::item(protocol, Vec::new());
    let value = address_of(lo, fx, &message, supply.param(0));
    let cases = every_case(lo, protocol);
    let index = lo.variant_index(fx, &message, value);
    let parts = encoded_parts(lo, fx, supply.param(1).address());
    for case in &cases {
        let this =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, index, case.index as i64);
        fx.when(this, |fx| {
            append(lo, fx, supply, parts, |lo, fx, state| {
                encode_case(lo, fx, Format::Wire, &message, case, value, state)
            });
            let number = fx.int(types::I16, case.index as i64);
            supply.ret(lo, fx, Held::Scalar(number));
        });
    }
    fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
}

/// `Wire[P].decode_any(raw, handles, alloc)`: the message of any operation whose call `raw`
/// encodes against `handles`, failing with the code of its decoding.
fn decode_any(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let protocol = protocol_of(supply);
    let message = Ty::item(protocol, Vec::new());
    let layout = lo.layout(&message);
    let target = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    let raw = supply.param(0).address();
    let handles = supply.param(1).address();
    let alloc = supply.param(2).address();
    let mut decode =
        |lo: &mut Lowering, fx: &mut Fx, args: &[Value]| decode_every(lo, fx, protocol, args);
    let format = DecodeFormat::Wire;
    let code = read_from(
        lo,
        fx,
        raw,
        alloc,
        Some(handles),
        &format,
        &message,
        target,
        &mut decode,
    );
    returned(lo, fx, supply, &message, target, code);
}

/// Fails with `CallError.Malformed` holding the `core::DecodeError` a decoding's code names.
fn fail_malformed(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, code: Value) {
    let error = super::decode_error(lo, fx, code);
    fail_with(lo, fx, supply, "Malformed", Some(error));
}

/// Fails with the variant of the operation's `sys::CallError` of that name holding the value at
/// `payload`.
fn fail_with(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    variant: &str,
    payload: Option<Value>,
) {
    let fails = supply.abi.fails.clone().expect("a sys::CallError");
    let payload_ty = call_error_payload(lo, &fails, variant);
    let held: Vec<Held> = (payload.into_iter())
        .map(|address| lo.held_at(fx, &payload_ty, address))
        .collect();
    let value = lo.variant_value(fx, &fails, variant, &held, None);
    let target = supply.failure.expect("a failure address");
    lo.store_held(fx, target, value, &fails);
    supply.fail(fx);
}

/// The payload type of a variant of an instantiation of `sys::CallError`.
fn call_error_payload(lo: &Lowering, call_error: &Ty, variant: &str) -> Ty {
    match variant {
        "Failed" => call_error.type_arg(0).cloned().expect("a failure type"),
        "Malformed" => Ty::item(
            lo.ck
                .db
                .core_item("DecodeError")
                .expect("core::DecodeError"),
            Vec::new(),
        ),
        "Closed" => lo.std_ty("task", "SendError", Vec::new()),
        _ => lo.std_ty("task", "Cancelled", Vec::new()),
    }
}

/// Reads one value of `ty` at `target` from the Untrusted bytes of parameter `raw` against the
/// handles of the parameter after it, drawing through the allocator of the one after that: the
/// code of its decoding, `Trailing` when bytes or handles are left.
fn read_handled(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    raw: usize,
    ty: &Ty,
    target: Value,
) -> Value {
    let handles = supply.param(raw + 1).address();
    let alloc = supply.param(raw + 2).address();
    let raw = supply.param(raw).address();
    let mut decode = |lo: &mut Lowering, fx: &mut Fx, args: &[Value]| {
        decode_entry(lo, fx, &DecodeFormat::Wire, ty, args)
    };
    let format = DecodeFormat::Wire;
    read_from(
        lo,
        fx,
        raw,
        alloc,
        Some(handles),
        &format,
        ty,
        target,
        &mut decode,
    )
}

/// `Wire[P].serve(out, request, hash, peers, raw, handles, alloc)`: the call decoded, refused
/// under another protocol hash unless P serves its operation for the peer of `peers` holding
/// `hash`, its operation's `requires` evaluated, then made through `out`, a request numbered
/// `request`, giving the encoding of its reply, none when its handler postponed it.
fn serve(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let protocol = protocol_of(supply);
    let message = Ty::item(protocol, Vec::new());
    let own = protocol_hash(lo, fx, protocol);
    let same = same_hash(lo, fx, supply.param(2).address(), own);
    let other = fx.b.ins().bxor_imm_s(same, 1);
    let layout = lo.layout(&message);
    let slot = fx.slot(layout.size, layout.align);
    let code = read_handled(lo, fx, supply, 4, &message, slot);
    let failed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, code, 0);
    fx.when(failed, |fx| fail_malformed(lo, fx, supply, code));
    let sig = lo.ck.sig(protocol);
    let SigKind::Protocol { operations, .. } = &sig.kind else {
        unreachable!("a protocol's signature")
    };
    for method in lo.ck.hashed_operations(protocol) {
        let operation = &operations[method];
        let is = lo.variant_is(fx, slot, &message, &operation.name);
        fx.when(is, |fx| {
            let args: Vec<Held> = (operation.sig.params.iter())
                .map(|param| {
                    let at = lo.payload_address(fx, &message, &operation.name, &param.name, slot);
                    Held::Address(at)
                })
                .collect();
            fx.when(other, |fx| {
                let served = serves(lo, fx, supply, protocol, &operation.name);
                let refused = fx.b.ins().icmp_imm_s(IntCC::Equal, served, 0);
                fx.when(refused, |fx| {
                    lo.release(fx, &message, slot);
                    let code = fx.int(types::I32, failure::UNKNOWN_SCHEMA);
                    fail_malformed(lo, fx, supply, code);
                });
            });
            for index in lo.ck.operation_requires(protocol, method) {
                let instance = Instance::Nir {
                    item: protocol,
                    role: Role::Requires,
                    index,
                    args: Args::from(Vec::new()),
                };
                let abi = lo.abi(&instance);
                let id = lo.instance(instance);
                let holds = lo
                    .invoke(fx, Invoked::Direct(id), &abi, &args)
                    .result
                    .scalar();
                let broken = fx.b.ins().icmp_imm_s(IntCC::Equal, holds, 0);
                fx.when(broken, |fx| {
                    lo.release(fx, &message, slot);
                    let code = fx.int(types::I32, failure::INVALID);
                    fail_malformed(lo, fx, supply, code);
                });
            }
            if lo.ck.is_request(protocol, method) {
                let fails = operation.sig.fails.clone().unwrap_or(Ty::Never);
                let result = lo.ck.db.core_item("Result").expect("core::Result");
                let reply = Ty::item(
                    result,
                    vec![Arg::Ty(operation.sig.result.clone()), Arg::Ty(fails)],
                );
                let number = supply.param(1).scalar();
                let held = Held::Address(slot);
                await_reply(
                    lo,
                    fx,
                    supply,
                    &message,
                    held,
                    &operation.sig,
                    Some(number),
                    &|lo, fx, answer| {
                        let value = match answer {
                            Answer::Replied(result) => {
                                lo.variant_value(fx, &reply, "Ok", &[result], None)
                            }
                            Answer::Failed(failure) => {
                                lo.variant_value(fx, &reply, "Err", &[failure], None)
                            }
                            Answer::Postponed => {
                                let none = lo.none_value(fx, &supply.abi.result);
                                supply.ret(lo, fx, none);
                                return;
                            }
                        };
                        give(lo, fx, supply, Some((&reply, value)));
                    },
                );
                return;
            }
            let instance = Instance::Operation {
                protocol,
                method: method as u32,
            };
            let abi = lo.abi(&instance);
            let id = lo.instance(instance);
            let mut passed = vec![supply.param(0)];
            passed.extend(args);
            let called = lo.invoke(fx, Invoked::Direct(id), &abi, &passed);
            if let (Some(failed), Some(failure)) = (called.failed, called.failure) {
                let failed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, failed, 0);
                fx.when(failed, |fx| {
                    fail_with(lo, fx, supply, "Closed", Some(failure))
                });
            }
            give(lo, fx, supply, None);
        });
    }
    fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
}

/// Whether P's protocol schema, drawn through `serve`'s allocator and then released, serves a call
/// of the operation `name` under the hash and the peer schemas `serve` was given, as
/// `sys::ProtocolSchema.serves_call` decides through that allocator.
fn serves(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, protocol: ItemId, name: &str) -> Value {
    let ty = lo.std_ty("sys", "ProtocolSchema", Vec::new());
    let layout = lo.layout(&ty);
    let own = fx.slot(layout.size, layout.align);
    let alloc = fx.load(PTR, supply.param(6).address(), 0);
    let encoding = lo.ck.protocol_schema(protocol);
    let code = decode_constant(lo, fx, &encoding, &ty, alloc, own);
    let failed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, code, 0);
    fx.when(failed, |fx| supply.exhausted(lo, fx, &[]));
    let Ty::Item(item, _) = &ty else {
        unreachable!("sys::ProtocolSchema is an item")
    };
    let function = lo.ck.db.members(TypeKey::Item(*item), "serves_call")[0];
    let instance = Instance::Nir {
        item: function,
        role: Role::Body,
        index: 0,
        args: Args::from(Vec::new()),
    };
    let abi = lo.abi(&instance);
    let id = lo.instance(instance);
    let operation = lo.view_literal(fx, name.as_bytes());
    let args = [
        Held::Address(own),
        supply.param(2),
        supply.param(3),
        operation,
        supply.param(6),
    ];
    let served = lo
        .invoke(fx, Invoked::Direct(id), &abi, &args)
        .result
        .scalar();
    lo.release(fx, &ty, own);
    served
}

/// Returns `Some` of an encoded reply drawn through the operation's allocator: holding the
/// encoding of a reply held as given, which is then released, or nothing.
fn give(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, reply: Option<(&Ty, Held)>) {
    let ty = lo.std_ty("sys", "Encoded", Vec::new());
    let layout = lo.layout(&ty);
    let encoded = fx.slot(layout.size, layout.align);
    let alloc = fx.load(PTR, supply.param(6).address(), 0);
    let (bytes, handles) = encoded_parts(lo, fx, encoded);
    vector::empty(fx, bytes, alloc, BYTE);
    vector::empty(fx, handles, alloc, HANDLE);
    if let Some((reply, held)) = reply {
        let value = address_of(lo, fx, reply, held);
        let function = encoder(lo, Format::Wire, reply);
        append(lo, fx, supply, (bytes, handles), |lo, fx, state| {
            super::call(lo, fx, function, &[value, state]);
        });
        lo.release(fx, reply, value);
    }
    let held = lo.held_at(fx, &ty, encoded);
    let some = lo.variant_value(fx, &supply.abi.result, "Some", &[held], None);
    supply.ret(lo, fx, some);
}

/// `reply.decode(raw, handles, alloc)`: the result of an `Ok` reply, or `CallError.Failed`
/// holding the failure of an `Err` one.
fn decode_reply(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let (result, fails) = (supply.type_arg(0), supply.type_arg(1));
    let core_result = lo.ck.db.core_item("Result").expect("core::Result");
    let reply = Ty::item(
        core_result,
        vec![Arg::Ty(result.clone()), Arg::Ty(fails.clone())],
    );
    let layout = lo.layout(&reply);
    let slot = fx.slot(layout.size, layout.align);
    let code = read_handled(lo, fx, supply, 1, &reply, slot);
    let failed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, code, 0);
    fx.when(failed, |fx| fail_malformed(lo, fx, supply, code));
    let refused = lo.variant_is(fx, slot, &reply, "Err");
    fx.when(refused, |fx| {
        let payload = lo.payload_address(fx, &reply, "Err", "", slot);
        fail_with(lo, fx, supply, "Failed", Some(payload));
    });
    let payload = lo.payload_address(fx, &reply, "Ok", "", slot);
    let held = lo.held_at(fx, &result, payload);
    supply.ret(lo, fx, held);
}
