//! Domains: `sys::Domain.run`, which runs a body as the first task of a new domain, and the trap
//! that abandons one. The body's captures pass to the domain; when the body returns or fails,
//! its result or failure passes to the caller's domain with every holding the domain still
//! owns, and the domain's other chunks are freed.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC};
use cranelift_module::FuncId;

use super::super::super::body::Held;
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::tasks::{self, domain, flag, task, trap_record};
use super::super::super::runtime::{Routine, holdings, memory, report};
use super::super::super::{Glue, Lowering};
use super::super::Supply;
use super::super::mem::lists::fn_param;
use crate::check::ty::Ty;

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    match supply.path.as_str() {
        "sys::Domain.run" => run(lo, fx, supply),
        "sys::Trap.kind" => {
            let record = trap_record(fx, supply.param(0));
            let kind = fx.load(PTR, record, trap_record::KIND);
            let ty = supply.abi.result.clone();
            let layout = lo.layout(&ty);
            let slot = supply
                .sret
                .unwrap_or_else(|| fx.slot(layout.size, layout.align));
            for (index, name) in lo.variant_names(trap_kind_item(&ty)).iter().enumerate() {
                let this = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, index as i64);
                let ty = ty.clone();
                fx.when(this, |fx| {
                    let value = lo.variant_value(fx, &ty, name, &[], false);
                    lo.store_held(fx, slot, value, &ty);
                });
            }
            let held = lo.held_at(fx, &ty, slot);
            supply.ret(lo, fx, held);
        }
        "sys::Trap.render" => {
            let record = trap_record(fx, supply.param(0));
            let len = fx.load(PTR, record, trap_record::LEN);
            let bytes = fx.offset(record, trap_record::BYTES);
            let renderer = supply.param(1).address();
            super::super::render::append(lo, fx, renderer, bytes, len);
            supply.done(lo, fx);
        }
        other => panic!("no lowering of {other}"),
    }
}

/// The trap record a `sys::Trap` value names.
fn trap_record(fx: &mut Fx, trap: Held) -> Value {
    match trap {
        Held::Scalar(record) => record,
        held => fx.load(PTR, held.address(), 0),
    }
}

/// The enum item of `sys::TrapKind`.
fn trap_kind_item(ty: &Ty) -> crate::check::db::ItemId {
    let Ty::Item(item, _) = ty else {
        panic!("sys::TrapKind is an enum item");
    };
    *item
}

/// `sys::Domain.run(body, exec, alloc)`: runs `body` as the first task of a new domain nested in
/// the caller's and waits for it to end, or once a trap abandons it, for its settlement.
fn run(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let result_ty = supply.type_arg(0);
    let fails = supply.type_arg(1);
    let body_ty = fn_param(supply, 0);
    let (live, _) = super::executor(lo, fx, supply.param(1).address());
    let stale = fx.b.ins().bxor_imm_s(live, 1);
    fx.when(stale, |fx| {
        let none = fx.ptr(0);
        super::fail_cancelled(lo, fx, supply, none);
    });
    let cell = fx.load(PTR, supply.param(2).address(), 0);
    let caller_domain = tasks::current_domain(lo, fx);
    let size = fx.ptr(trap_record::BYTES + report::REPORT);
    let align = fx.ptr(8);
    let trap = lo.call_routine(fx, Routine::AllocIn, &[cell, size, align, caller_domain])[0];
    let one = fx.ptr(1);
    let record_size = fx.ptr(domain::SIZE);
    let record = lo.call_libc(fx, "calloc", &[one, record_size], &[PTR])[0];
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
    let no_trap = fx.b.ins().icmp_imm_s(IntCC::Equal, trap, 0);
    let missing = fx.b.ins().bor(missing, no_trap);
    fx.when(missing, |fx| {
        report::raise_kind(lo, fx, "trap.exhausted");
    });
    fx.store(trap, record, domain::TRAP);
    fx.store(caller_domain, record, domain::PARENT);
    let current = tasks::current_task(lo, fx);
    fx.store(current, record, domain::WAITER);
    let parent_lock = fx.offset(caller_domain, domain::LOCK);
    lo.call_routine(fx, Routine::Lock, &[parent_lock]);
    let siblings = fx.load(PTR, caller_domain, domain::CHILDREN);
    fx.store(siblings, record, domain::SIBLING);
    fx.store(record, caller_domain, domain::CHILDREN);
    lo.call_routine(fx, Routine::Unlock, &[parent_lock]);
    let region = fx.load(PTR, current, task::REGION);
    let none = fx.ptr(0);
    lo.transfer(
        fx,
        &Ty::Fn(body_ty.clone().into()),
        supply.param(0).address(),
        none,
        record,
    );
    let done = domain_done(lo);
    let first = super::scopes::start_task(
        lo,
        fx,
        &body_ty,
        supply.param(0).address(),
        cell,
        record,
        region,
        record,
        None,
        done,
    );
    lo.call_routine(fx, Routine::Wake, &[first]);
    let lock = fx.offset(record, domain::LOCK);
    fx.repeat(&[], |fx, _| {
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let flags = fx.load(PTR, first, task::FLAGS);
        let ended = fx.b.ins().band_imm_s(flags, flag::DONE);
        let ended = fx.b.ins().icmp_imm_s(IntCC::NotEqual, ended, 0);
        let abandoned = fx.load(PTR, record, domain::ABANDONED);
        let abandoned = fx.b.ins().icmp_imm_s(IntCC::NotEqual, abandoned, 0);
        let over = fx.b.ins().bor(ended, abandoned);
        let waiting = fx.b.ins().bxor_imm_s(over, 1);
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        fx.when(waiting, |fx| {
            lo.call_routine(fx, Routine::Park, &[]);
        });
        (waiting, Vec::new())
    });
    lo.call_routine(fx, Routine::Lock, &[parent_lock]);
    unlink_child(fx, caller_domain, record);
    lo.call_routine(fx, Routine::Unlock, &[parent_lock]);
    let option = supply.abi.result.clone();
    let layout = lo.layout(&option);
    let slot = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    let abandoned = fx.load(PTR, record, domain::ABANDONED);
    let abandoned = fx.b.ins().icmp_imm_s(IntCC::NotEqual, abandoned, 0);
    fx.when(abandoned, |fx| {
        lo.call_routine(fx, Routine::Settle, &[record]);
        lo.call_libc(fx, "free", &[record], &[]);
        let value = fx.slot(8, 8);
        fx.store(trap, value, 0);
        let err = lo.variant_value(fx, &option, "Err", &[Held::Address(value)], false);
        lo.store_held(fx, slot, err, &option);
        let held = lo.held_at(fx, &option, slot);
        supply.ret(lo, fx, held);
    });
    lo.call_routine(fx, Routine::Free, &[trap]);
    let outcome = fx.load(PTR, first, task::OUTCOME);
    let returned = fx.b.ins().icmp_imm_s(IntCC::Equal, outcome, 1);
    let target = supply.failure.expect("a failure address");
    let (result_ty, fails_ty) = (result_ty.clone(), fails.clone());
    fx.choose_in(
        lo,
        returned,
        &[],
        |lo, fx| {
            let result = fx.load(PTR, first, task::RESULT);
            let held = lo.held_at(fx, &result_ty, result);
            let ok = lo.variant_value(fx, &option, "Ok", &[held], false);
            lo.store_held(fx, slot, ok, &option);
            lo.transfer(fx, &option, slot, caller_domain, caller_domain);
            Vec::new()
        },
        |lo, fx| {
            let failure = fx.load(PTR, first, task::FAILURE);
            let held = lo.held_at(fx, &fails_ty, failure);
            lo.store_held(fx, target, held, &fails_ty);
            lo.transfer(fx, &fails_ty, target, caller_domain, caller_domain);
            Vec::new()
        },
    );
    holdings::pass_all(lo, fx, record, caller_domain);
    memory::free_chunks(lo, fx, record);
    lo.call_libc(fx, "free", &[first], &[]);
    lo.call_libc(fx, "free", &[record], &[]);
    let failed = fx.b.ins().bxor_imm_s(returned, 1);
    fx.when(failed, |fx| supply.fail(fx));
    let held = lo.held_at(fx, &option, slot);
    supply.ret(lo, fx, held);
}

/// Takes a domain out of the list of the domains nested in its parent; the caller holds the
/// parent's lock.
fn unlink_child(fx: &mut Fx, parent: Value, child: Value) {
    let head = fx.load(PTR, parent, domain::CHILDREN);
    let sibling = fx.load(PTR, child, domain::SIBLING);
    let first = fx.b.ins().icmp(IntCC::Equal, head, child);
    fx.choose(
        first,
        &[],
        |fx| {
            fx.store(sibling, parent, domain::CHILDREN);
            Vec::new()
        },
        |fx| {
            fx.repeat(&[head], |fx, state| {
                let current = state[0];
                let next = fx.load(PTR, current, domain::SIBLING);
                let found = fx.b.ins().icmp(IntCC::Equal, next, child);
                fx.when(found, |fx| fx.store(sibling, current, domain::SIBLING));
                let more = fx.b.ins().bxor_imm_s(found, 1);
                (more, vec![next])
            });
            Vec::new()
        },
    );
}

/// What the executor thread does once a domain's first task has ended: the caller of
/// `sys::Domain.run` wakes.
fn domain_done(lo: &mut Lowering) -> FuncId {
    lo.glue_function(Glue::DomainDone, super::record_signature(), |lo, fx| {
        let params = fx.entry();
        let first = params[0];
        let record = fx.load(PTR, first, task::OWNER);
        let lock = fx.offset(record, domain::LOCK);
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let flags = fx.load(PTR, first, task::FLAGS);
        let flags = fx.b.ins().bor_imm_s(flags, flag::DONE);
        fx.store(flags, first, task::FLAGS);
        let waiter = fx.load(PTR, record, domain::WAITER);
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        lo.call_routine(fx, Routine::Wake, &[waiter]);
        fx.b.ins().return_(&[]);
    })
}

/// Releases a trap: frees its record.
pub fn release_trap(lo: &mut Lowering, fx: &mut Fx, address: Value) {
    let record = fx.load(PTR, address, 0);
    lo.call_routine(fx, Routine::Free, &[record]);
}
