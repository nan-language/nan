//! Capabilities kinds serve: `C.served(service)` of `Fs`, `Net`, `Random`, `Log`, `Stdio` and
//! `Runner`, the calls of a service interface's methods on a kind, and the operations of a served
//! `Fs` or `Net` and of the files, listeners and connections opened through one.
//!
//! A served resource's holding holds a record of the handle of the kind serving it, the kind's
//! method table, the number the kind gave the resource and the code of the method releasing it:
//! `FsService.release`, `NetService.release` or `NetService.reset`, which share a signature.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC};

use super::super::Lowering;
use super::super::body::Held;
use super::super::call::{Called, Invoked};
use super::super::fx::{Fx, PTR};
use super::super::runtime::{Routine, holdings, tables};
use super::Supply;
use super::sys::{checked_text, fail_with, new_view, view};
use crate::check::db::ItemId;
use crate::check::sig::SigKind;
use crate::check::ty::{Ty, no_args};

pub mod record {
    pub const SERVICE: u64 = 0;
    pub const TABLE: u64 = 8;
    pub const NUMBER: u64 = 16;
    pub const RELEASE: u64 = 24;
    pub const SIZE: u64 = 32;
}

/// `C.served(service)`: an entry of a kind deriving from the entry of the capability kind
/// `service`, whose operations call its methods for a service interface; `Revoked.Stale` when
/// `service` is stale.
pub fn served(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, kind: i64, interface: &str) -> bool {
    let service_ty = supply.type_arg(0);
    let service = fx.load(PTR, supply.param(0).address(), 0);
    let interface = service_interface(lo, interface);
    let (table, _) = lo.dyn_table(&service_ty, interface, &no_args());
    let table = lo.data_ref(fx, table);
    let table = fx.b.ins().symbol_value(PTR, table);
    let derived = tables::served(lo, fx, kind, service, table);
    let stale = fx.b.ins().icmp_imm_s(IntCC::Equal, derived, -1);
    fx.when(stale, |fx| fail_with(lo, fx, supply, "Revoked"));
    let sret = supply.sret.expect("a capability's address");
    fx.store(derived, sret, 0);
    supply.done(lo, fx);
    true
}

fn service_interface(lo: &Lowering, name: &str) -> ItemId {
    lo.ck.db.std_item("sys", name).expect("a service interface")
}

/// The index of a service interface's method in its method table.
fn method_index(lo: &Lowering, interface: ItemId, name: &str) -> u32 {
    let SigKind::Interface { methods } = &lo.ck.sig(interface).kind else {
        panic!("a service interface");
    };
    let index = methods.iter().position(|method| method.name == name);
    index.expect("a service method") as u32
}

/// Calls the method of a service interface named `method` on the kind whose handle is `service`
/// through the kind's method table, with `args` after the kind's handle.
pub fn call(
    lo: &mut Lowering,
    fx: &mut Fx,
    interface: &str,
    method: &str,
    (service, table): (Value, Value),
    args: &[Held],
) -> Called {
    let interface = service_interface(lo, interface);
    let method = method_index(lo, interface, method);
    let slot = kind_slot(fx, service);
    let code = fx.load(PTR, table, 8 * (u64::from(method) + 3));
    invoke(lo, fx, interface, method, code, slot, args)
}

/// A slot holding the handle of a kind, which its methods take as `self`.
fn kind_slot(fx: &mut Fx, service: Value) -> Value {
    let slot = fx.slot(8, 8);
    fx.store(service, slot, 0);
    slot
}

fn invoke(
    lo: &mut Lowering,
    fx: &mut Fx,
    interface: ItemId,
    method: u32,
    code: Value,
    slot: Value,
    args: &[Held],
) -> Called {
    let abi = lo.dyn_abi(interface, &no_args(), method);
    let (head, _) = lo.method_head(&Ty::Unit, interface, &no_args(), method);
    let mut held = vec![Held::Address(slot)];
    held.extend_from_slice(args);
    lo.invoke(fx, Invoked::Code(code, head.limits.stack), &abi, &held)
}

/// An operation of a served `Fs` opening, reading, writing or removing a file: the kind's method
/// of its name with its path joined to the entry's, a file it opens holding the number the kind
/// gave it.
pub fn fs(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    capability: Value,
    kind: (Value, Value),
) {
    let member = supply.member();
    let (prefix, prefix_len) = checked_text(lo, fx, supply, capability);
    let separated = fx.b.ins().icmp_imm_s(IntCC::NotEqual, prefix_len, 0);
    let path = view(fx, supply.param(1));
    let (joined, len) = tables::join(lo, fx, (prefix, prefix_len), path, separated);
    lo.record_free(fx, prefix);
    let mut args = vec![new_view(fx, joined, len)];
    if matches!(member, "read_all" | "write_all") {
        args.push(supply.param(2));
    }
    let called = call(lo, fx, "FsService", member, kind, &args);
    lo.record_free(fx, joined);
    supply.forward_failure(lo, fx, &called);
    match member {
        "open" | "create" => {
            let handle = fx.load(PTR, capability, 0);
            let number = called.result.scalar();
            ret_resource(
                lo,
                fx,
                supply,
                handle,
                kind,
                number,
                ("FsService", "release"),
            );
        }
        "read_all" => supply.ret(lo, fx, called.result),
        _ => supply.done(lo, fx),
    }
}

/// `listen` or `connect` of a served `Net`: the kind's method of its name, the listener or
/// connection it opens holding the number the kind gave it.
pub fn net(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    capability: Value,
    kind: (Value, Value),
) {
    let (args, release) = match supply.member() {
        "listen" => (vec![supply.param(1)], "release"),
        _ => (vec![supply.param(1), supply.param(2)], "reset"),
    };
    let called = call(lo, fx, "NetService", supply.member(), kind, &args);
    supply.forward_failure(lo, fx, &called);
    let handle = fx.load(PTR, capability, 0);
    let number = called.result.scalar();
    ret_resource(
        lo,
        fx,
        supply,
        handle,
        kind,
        number,
        ("NetService", release),
    );
}

/// Returns a file, listener or connection a kind serves: the entry it was opened through and the
/// holding of its record, whose releasing method is the method `release` of an interface.
fn ret_resource(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    handle: Value,
    (service, table): (Value, Value),
    number: Value,
    (interface, release): (&str, &str),
) {
    let size = fx.ptr(record::SIZE);
    let served = lo.record_alloc(fx, size);
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, served, 0);
    fx.when(missing, |fx| {
        super::super::runtime::report::raise_kind(lo, fx, "trap.exhausted")
    });
    let interface = service_interface(lo, interface);
    let method = method_index(lo, interface, release);
    let code = fx.load(PTR, table, 8 * (u64::from(method) + 3));
    fx.store(service, served, record::SERVICE);
    fx.store(table, served, record::TABLE);
    fx.store(number, served, record::NUMBER);
    fx.store(code, served, record::RELEASE);
    let kind = fx.ptr(holdings::kind::SERVED as u64);
    let holding = lo.call_routine(fx, Routine::Hold, &[kind, served])[0];
    let sret = supply.sret.expect("a resource's address");
    fx.store(handle, sret, 0);
    fx.store(holding, sret, 8);
    supply.done(lo, fx);
}

/// An operation of a file, listener or connection a kind serves, through the record its holding
/// holds; `close` and `abandon` release the record, whether or not the kind's call fails.
pub fn resource(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, address: Value) {
    let holding = fx.load(PTR, address, 8);
    let served = fx.load(PTR, holding, holdings::holding::PAYLOAD);
    let kind = (
        fx.load(PTR, served, record::SERVICE),
        fx.load(PTR, served, record::TABLE),
    );
    let number = Held::Scalar(fx.load(PTR, served, record::NUMBER));
    let owner = supply.path.rsplit_once('.').map_or("", |(owner, _)| owner);
    let interface = match owner {
        "sys::fs::File" => "FsService",
        _ => "NetService",
    };
    let member = supply.member();
    match member {
        "abandon" => {
            release(lo, fx, served);
            lo.call_routine(fx, Routine::Unhold, &[holding]);
            supply.done(lo, fx);
        }
        "close" => {
            let called = call(lo, fx, interface, member, kind, &[number]);
            lo.record_free(fx, served);
            lo.call_routine(fx, Routine::Unhold, &[holding]);
            supply.forward_failure(lo, fx, &called);
            supply.done(lo, fx);
        }
        "accept" => {
            let called = call(lo, fx, interface, member, kind, &[number]);
            supply.forward_failure(lo, fx, &called);
            let handle = fx.load(PTR, address, 0);
            let conn = called.result.scalar();
            ret_resource(lo, fx, supply, handle, kind, conn, (interface, "reset"));
        }
        "byte_len" => {
            let called = call(lo, fx, interface, member, kind, &[number]);
            supply.forward_failure(lo, fx, &called);
            supply.ret(lo, fx, called.result);
        }
        _ => {
            let data = supply.param(1);
            let called = call(lo, fx, interface, member, kind, &[number, data]);
            supply.forward_failure(lo, fx, &called);
            let (_, len) = view(fx, data);
            let count = fx.b.ins().umin(called.result.scalar(), len);
            let count = match member {
                "write" => {
                    let one = fx.ptr(1);
                    let least = fx.b.ins().umin(len, one);
                    fx.b.ins().umax(count, least)
                }
                _ => count,
            };
            supply.ret(lo, fx, Held::Scalar(count));
        }
    }
}

/// Releases a served resource's record: calls its releasing method, then frees it.
pub fn release(lo: &mut Lowering, fx: &mut Fx, served: Value) {
    let service = fx.load(PTR, served, record::SERVICE);
    let number = fx.load(PTR, served, record::NUMBER);
    let code = fx.load(PTR, served, record::RELEASE);
    let interface = service_interface(lo, "FsService");
    let method = method_index(lo, interface, "release");
    let slot = kind_slot(fx, service);
    invoke(
        lo,
        fx,
        interface,
        method,
        code,
        slot,
        &[Held::Scalar(number)],
    );
    lo.record_free(fx, served);
}
