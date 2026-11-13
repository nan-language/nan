//! Tasks: scopes, joins, deadlines and races, cowns and `await when`, channels, executors and
//! their accounts, and domains.

pub mod accounts;
pub mod channels;
pub mod components;
pub mod cowns;
pub mod deadlines;
pub mod domains;
pub mod protocols;
pub mod scopes;

use cranelift_codegen::ir::{AbiParam, InstBuilder, Signature, Value, condcodes::IntCC};

use super::super::body::Held;
use super::super::fx::{Fx, PTR};
use super::super::runtime::tables;
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
        "task::scope" | "task::Scope" | "task::Join" | "task::race" => {
            scopes::lower(lo, fx, supply)
        }
        "task::within" | "task::within_ns" | "task::paced" | "task::deadline_ns" => {
            deadlines::lower(lo, fx, supply)
        }
        "task::Cown" => cowns::lower(lo, fx, supply),
        "task::Executor" | "task::Account" => accounts::lower(lo, fx, supply),
        "task::Channel" | "task::Sender" | "task::Receiver" => channels::lower(lo, fx, supply),
        "sys::Domain" | "sys::Abandoner" | "sys::Trap" => domains::lower(lo, fx, supply),
        "host::Runner"
        | "sys::Cx"
        | "sys::Outbox"
        | "sys::Stepper"
        | "sys::Waker"
        | "sys::current_header"
        | "sys::run_headed" => components::lower(lo, fx, supply),
        _ => return false,
    }
    true
}

/// Whether the handle of the executor at an address is live, and the state words of its entry
/// when it is: its cell, its clock's handle and its resolution.
pub fn executor(lo: &mut Lowering, fx: &mut Fx, handle: Value) -> (Value, [Value; 4]) {
    let handle = fx.load(PTR, handle, 0);
    tables::snapshot(lo, fx, handle)
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

/// Parks the running task where it waits at a cancellation point, as `tasks::waiting_on` recorded,
/// unless its call is cancelled: a cancellation that comes after this reads the origin finds the
/// task waiting and wakes it.
pub fn park_waiting(lo: &mut Lowering, fx: &mut Fx) {
    let origin = origin(lo, fx);
    let open = fx.b.ins().icmp_imm_s(IntCC::Equal, origin, 0);
    fx.when(open, |fx| {
        lo.call_routine(fx, Routine::Park, &[]);
    });
}

/// Whether releasing a task type does anything, none for a type std declares in full.
pub fn needs_release(path: &str) -> Option<bool> {
    match path {
        "task::Join" | "task::Cown" | "task::Sender" | "task::Receiver" | "sys::Trap" => Some(true),
        "task::Scope" | "task::Executor" | "task::Account" | "task::Cancelled"
        | "task::Poisoned" | "sys::Cx" | "sys::Outbox" | "sys::Abandoner" | "sys::Stepper"
        | "sys::Waker" => Some(false),
        _ => None,
    }
}

/// Whether moving a task type into another domain's reach does anything, none for a type std
/// declares in full.
pub fn transfers(path: &str) -> Option<bool> {
    match path {
        "task::Cown" | "task::Sender" | "task::Receiver" | "sys::Trap" => Some(true),
        "task::Scope" | "task::Account" | "task::Cancelled" | "task::Poisoned"
        | "sys::Abandoner" => Some(false),
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
