//! Tasks: scopes, joins, deadlines and races, cowns and `await when`, channels, and domains.

pub mod channels;
pub mod components;
pub mod cowns;
pub mod domains;
pub mod scopes;

use cranelift_codegen::ir::{AbiParam, InstBuilder, Signature, Value, condcodes::IntCC};

use super::super::body::Held;
use super::super::fx::{Fx, PTR};
use super::super::runtime::tables::{entry, live_entry};
use super::super::runtime::{Routine, tasks};
use super::super::{CALL_CONV, Lowering};
use super::Supply;
use crate::check::ty::{FnTy, Ty};

pub use cowns::when;

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    let owner = supply
        .path
        .rsplit_once('.')
        .map_or(supply.path.as_str(), |(owner, _)| owner);
    match owner {
        "task::scope" | "task::Scope" | "task::Join" | "task::within" | "task::race" => {
            scopes::lower(lo, fx, supply)
        }
        "task::Cown" => cowns::lower(lo, fx, supply),
        "task::Channel" | "task::Sender" | "task::Receiver" => channels::lower(lo, fx, supply),
        "sys::Domain" | "sys::Trap" => domains::lower(lo, fx, supply),
        "host::Runner" | "sys::Cx" | "sys::Outbox" => components::lower(lo, fx, supply),
        _ => return false,
    }
    true
}

/// The cell of the executor whose handle is at an address, and whether the handle is live.
pub fn executor(lo: &mut Lowering, fx: &mut Fx, handle: Value) -> (Value, Value) {
    let handle = fx.load(PTR, handle, 0);
    let record = live_entry(lo, fx, handle);
    let live = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
    let zero = fx.ptr(0);
    let cell = fx.choose(
        live,
        &[PTR],
        |fx| vec![fx.load(PTR, record, entry::STATE)],
        |_| vec![zero],
    )[0];
    (live, cell)
}

/// The signature of glue given a task's record.
pub fn record_signature() -> Signature {
    let mut signature = Signature::new(CALL_CONV);
    signature.params.push(AbiParam::new(PTR));
    signature
}

/// The usable bytes of the stack of a task whose entry has a function type.
pub fn stack_size(fn_ty: &FnTy) -> u64 {
    fn_ty.limits.stack.unwrap_or(tasks::STACK)
}

/// Fails the operation with the cancellation of its failure type holding an origin.
pub fn fail_cancelled(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, origin: Value) {
    let fails = supply.abi.fails.clone().expect("a cancellable operation");
    let value = lo.cancellation(fx, &fails, origin);
    let failure = supply.failure.expect("a failure address");
    lo.store_held(fx, failure, value, &fails);
    supply.fail(fx);
}

/// The origin of a cancellation of the running call.
pub fn origin(lo: &mut Lowering, fx: &mut Fx) -> Value {
    lo.call_routine(fx, Routine::Origin, &[])[0]
}

/// Whether releasing a task type does anything, none for a type std declares in full.
pub fn needs_release(path: &str) -> Option<bool> {
    match path {
        "task::Join" | "task::Cown" | "task::Sender" | "task::Receiver" | "sys::Trap" => Some(true),
        "task::Scope" | "task::Executor" | "task::Cancelled" | "task::Poisoned" | "sys::Cx"
        | "sys::Outbox" => Some(false),
        _ => None,
    }
}

/// Whether moving a task type into another domain's reach does anything, none for a type std
/// declares in full.
pub fn transfers(path: &str) -> Option<bool> {
    match path {
        "task::Cown" | "task::Sender" | "task::Receiver" | "sys::Trap" => Some(true),
        "task::Cancelled" | "task::Poisoned" => Some(false),
        _ => None,
    }
}

/// Moves a value of a task type at an address into another domain's reach; false for a type
/// std declares in full.
pub fn transfer(
    lo: &mut Lowering,
    fx: &mut Fx,
    path: &str,
    ty: &Ty,
    address: Value,
    lift: Value,
    owner: Value,
) -> bool {
    match path {
        "task::Cown" => cowns::transfer(lo, fx, ty, address, lift),
        "task::Sender" | "task::Receiver" => {
            channels::transfer(lo, fx, path, ty, address, lift, owner)
        }
        "sys::Trap" => {
            let record = fx.load(PTR, address, 0);
            super::super::transfer::lift(lo, fx, record, lift);
        }
        _ => return false,
    }
    true
}

/// Releases a value of a task type at an address; false for a type std declares in full.
pub fn release(lo: &mut Lowering, fx: &mut Fx, path: &str, ty: &Ty, address: Value) -> bool {
    match path {
        "task::Join" => {
            let result = ty
                .type_arg(0)
                .cloned()
                .expect("a join handle's result type");
            scopes::release_join(lo, fx, address, &result);
        }
        "task::Cown" => cowns::release(lo, fx, ty, address),
        "task::Sender" | "task::Receiver" => channels::release(lo, fx, path, ty, address),
        "sys::Trap" => domains::release_trap(lo, fx, address),
        _ => return false,
    }
    true
}

/// A held value's address, the value stored in a new slot when held in a register.
pub fn address_of(lo: &mut Lowering, fx: &mut Fx, ty: &Ty, held: Held) -> Value {
    match held {
        Held::Address(address) => address,
        held => {
            let layout = lo.layout(ty);
            let slot = fx.slot(layout.size, layout.align);
            lo.store_held(fx, slot, held, ty);
            slot
        }
    }
}
