//! Domains: `sys::Domain.run` and `run_sized`, which run a body as the first task of a new domain,
//! the same runs through a `sys::Abandoner`, which abandons the domain it runs on `trigger`, and
//! the trap that abandons one. The body's captures pass to the domain; when the body returns or
//! fails, its result or failure passes to the caller's domain with every holding the domain still
//! owns, and the domain's other chunks are freed.
//!
//! An abandoner is its lock word, whether it was triggered, and the domain it runs: zero when it
//! runs none, `PENDING` while a run prepares its domain, and the domain's record otherwise.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, condcodes::IntCC};
use cranelift_module::FuncId;

use super::super::super::body::Held;
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::tasks::{self, domain, flag, task, trap_record};
use super::super::super::runtime::{Routine, holdings, memory, report};
use super::super::super::{Glue, Lowering, transfer};
use super::super::Supply;
use super::super::mem::lists::fn_param;
use crate::check::ty::Ty;

/// The largest stack the address space of an `x86_64` process holds.
const MOST_STACK: i64 = 1 << 47;

/// A stack size a call gives, trapping as exhausted when no address space holds it.
pub fn sized_stack(lo: &mut Lowering, fx: &mut Fx, stack: Value) -> Value {
    let huge =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedGreaterThan, stack, MOST_STACK);
    fx.when(huge, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
    stack
}

/// The domain word of an abandoner whose run prepares its domain.
const PENDING: i64 = 1;

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    match supply.path.as_str() {
        "sys::Domain.run" => run(lo, fx, supply, false, false),
        "sys::Domain.run_sized" => run(lo, fx, supply, true, false),
        "sys::Abandoner.run" => run(lo, fx, supply, false, true),
        "sys::Abandoner.run_sized" => run(lo, fx, supply, true, true),
        "sys::Abandoner.new" => {
            let sret = supply.sret.expect("an abandoner's slot");
            let fields = Abandoner::at(lo, fx, supply, sret);
            let zero = fx.ptr(0);
            for field in [fields.lock, fields.triggered, fields.domain] {
                fx.store(zero, field, 0);
            }
            supply.ret(lo, fx, Held::Address(sret));
        }
        "sys::Abandoner.trigger" => {
            let fields = Abandoner::at(lo, fx, supply, supply.param(0).address());
            trigger(lo, fx, &fields);
            supply.done(lo, fx);
        }
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
                    let value = lo.variant_value(fx, &ty, name, &[], None);
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

/// The addresses of an abandoner's fields.
pub struct Abandoner {
    lock: Value,
    triggered: Value,
    domain: Value,
}

impl Abandoner {
    /// The fields of the abandoner at an address, of the type that owns the operation.
    fn at(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, address: Value) -> Abandoner {
        let ty = supply.owner.clone().expect("the abandoner type");
        let mut field = |name: &str| {
            let offset = lo.layouts.field(lo.ck, &ty, None, name).offset;
            fx.offset(address, offset)
        };
        Abandoner {
            lock: field("lock"),
            triggered: field("triggered"),
            domain: field("domain"),
        }
    }
}

/// `abandoner.trigger()`: marks the abandoner triggered and abandons the domain it runs, unless
/// its first task has ended: the domain's trap report, of `trap.abandoned`, locates the call,
/// and the task waiting for the domain wakes to settle it.
fn trigger(lo: &mut Lowering, fx: &mut Fx, fields: &Abandoner) {
    lo.call_routine(fx, Routine::Lock, &[fields.lock]);
    let one = fx.ptr(1);
    fx.store(one, fields.triggered, 0);
    let record = fx.load(PTR, fields.domain, 0);
    let running =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedGreaterThan, record, PENDING);
    fx.when(running, |fx| {
        let flag = fx.offset(record, domain::ABANDONED);
        let live = fx.ptr(0);
        let seen =
            fx.b.ins()
                .atomic_cas(MemFlagsData::trusted(), flag, live, one);
        let first = fx.b.ins().icmp_imm_s(IntCC::Equal, seen, 0);
        fx.when(first, |fx| {
            let trap = fx.load(PTR, record, domain::TRAP);
            let kind = fx.ptr(super::super::super::trap::kind_number("trap.abandoned") as u64);
            lo.call_routine(fx, Routine::TrapReport, &[kind, trap]);
            let lock = fx.offset(record, domain::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let waiter = fx.load(PTR, record, domain::WAITER);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            lo.call_routine(fx, Routine::Wake, &[waiter]);
        });
    });
    lo.call_routine(fx, Routine::Unlock, &[fields.lock]);
}

/// `sys::Domain.run(body, exec, alloc)`: runs `body` as the first task of a new domain nested in
/// the caller's and waits for it to end, or once a trap abandons it, for its settlement; when
/// `sized`, as `sys::Domain.run_sized(body, stack, exec, alloc)`, on a stack of `stack` usable
/// bytes, trapping exhausted when no address space holds one. Through an abandoner, the
/// operation's receiver, as `sys::Abandoner.run` and `run_sized`: a second run at once traps
/// with `trap.duplicate-access`, and the abandoner's `trigger` abandons the domain.
fn run(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, sized: bool, through: bool) {
    let result_ty = supply.type_arg(0);
    let fails = supply.type_arg(1);
    let body = usize::from(through);
    let body_ty = fn_param(supply, body);
    let given = body + usize::from(sized);
    let (live, _) = super::executor(lo, fx, supply.param(1 + given).address());
    let stale = fx.b.ins().bxor_imm_s(live, 1);
    fx.when(stale, |fx| {
        let none = fx.ptr(0);
        super::fail_cancelled(lo, fx, supply, none);
    });
    let cell = fx.load(PTR, supply.param(2 + given).address(), 0);
    let caller_domain = tasks::current_domain(lo, fx);
    let stack = sized.then(|| sized_stack(lo, fx, supply.param(body + 1).scalar()));
    let abandoner = through.then(|| {
        let fields = Abandoner::at(lo, fx, supply, supply.param(0).address());
        claim(lo, fx, &fields);
        fields
    });
    let Nested {
        record,
        first,
        trap,
    } = run_nested(
        lo,
        fx,
        &body_ty,
        supply.param(body).address(),
        cell,
        stack,
        true,
        abandoner.as_ref(),
    );
    let option = supply.abi.result.clone();
    let layout = lo.layout(&option);
    let slot = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    let abandoned = fx.load(PTR, record, domain::ABANDONED);
    let abandoned = fx.b.ins().icmp_imm_s(IntCC::Equal, abandoned, 1);
    fx.when(abandoned, |fx| {
        lo.call_routine(fx, Routine::Settle, &[record]);
        lo.record_free(fx, record);
        let value = fx.slot(8, 8);
        fx.store(trap, value, 0);
        let err = lo.variant_value(fx, &option, "Err", &[Held::Address(value)], None);
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
            let ok = lo.variant_value(fx, &option, "Ok", &[held], None);
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
    end_nested(lo, fx, record, first, caller_domain);
    let failed = fx.b.ins().bxor_imm_s(returned, 1);
    fx.when(failed, |fx| supply.fail(fx));
    let held = lo.held_at(fx, &option, slot);
    supply.ret(lo, fx, held);
}

/// Claims an abandoner for a run, trapping with `trap.duplicate-access` when it runs another.
fn claim(lo: &mut Lowering, fx: &mut Fx, fields: &Abandoner) {
    lo.call_routine(fx, Routine::Lock, &[fields.lock]);
    let current = fx.load(PTR, fields.domain, 0);
    let busy = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
    fx.when(busy, |fx| {
        lo.call_routine(fx, Routine::Unlock, &[fields.lock]);
        report::raise_kind(lo, fx, "trap.duplicate-access");
    });
    let pending = fx.ptr(PENDING as u64);
    fx.store(pending, fields.domain, 0);
    lo.call_routine(fx, Routine::Unlock, &[fields.lock]);
}

/// A domain `run_nested` ran: its record, its first task and its trap record.
pub struct Nested {
    pub record: Value,
    pub first: Value,
    pub trap: Value,
}

/// Runs the `task fn` value of a type held at `function` as the first task of a new domain
/// nested in the running task's, its stack, of `stack` usable bytes or else of the size its type
/// gives, and its trap record drawn through a cell, the domain's cell, and waits until that task
/// ends or a trap abandons the domain; the spawn is a scheduling point when `point` holds.
/// Through a claimed abandoner, the domain is the one it runs until the wait ends, and an
/// abandoner triggered already abandons it before its first task starts, which then never does,
/// its trap report locating the run's call.
#[allow(clippy::too_many_arguments)]
pub fn run_nested(
    lo: &mut Lowering,
    fx: &mut Fx,
    body_ty: &crate::check::ty::FnTy,
    function: Value,
    cell: Value,
    stack: Option<Value>,
    point: bool,
    abandoner: Option<&Abandoner>,
) -> Nested {
    let caller_domain = tasks::current_domain(lo, fx);
    let size = fx.ptr(trap_record::BYTES + report::REPORT);
    let align = fx.ptr(8);
    let trap = lo.call_routine(fx, Routine::AllocIn, &[cell, size, align, caller_domain])[0];
    let record_size = fx.ptr(domain::SIZE);
    let record = lo.record_alloc(fx, record_size);
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
    let no_trap = fx.b.ins().icmp_imm_s(IntCC::Equal, trap, 0);
    let missing = fx.b.ins().bor(missing, no_trap);
    fx.when(missing, |fx| {
        report::raise_kind(lo, fx, "trap.exhausted");
    });
    fx.store(trap, record, domain::TRAP);
    fx.store(caller_domain, record, domain::PARENT);
    fx.store(cell, record, domain::CELL);
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
    match fx.fn_params.get(&function).copied() {
        Some(position) => transfer::move_captures(fx, function, none, record, Some(position)),
        None => lo.transfer(fx, &Ty::Fn(body_ty.clone().into()), function, none, record),
    }
    let end = domain_end(lo);
    let done = domain_done(lo);
    let start = |lo: &mut Lowering, fx: &mut Fx| {
        let first = super::scopes::start_task(
            lo,
            fx,
            body_ty,
            function,
            cell,
            record,
            region,
            record,
            stack,
            Some(end),
            done,
        );
        match point {
            true => tasks::spawned(lo, fx, first),
            false => {
                lo.call_routine(fx, Routine::Wake, &[first]);
            }
        }
        first
    };
    let first = match abandoner {
        None => start(lo, fx),
        Some(fields) => {
            lo.call_routine(fx, Routine::Lock, &[fields.lock]);
            fx.store(record, fields.domain, 0);
            let latched = fx.load(PTR, fields.triggered, 0);
            lo.call_routine(fx, Routine::Unlock, &[fields.lock]);
            let latched = fx.b.ins().icmp_imm_s(IntCC::NotEqual, latched, 0);
            fx.choose_in(
                lo,
                latched,
                &[PTR],
                |lo, fx| {
                    let one = fx.ptr(1);
                    fx.store(one, record, domain::ABANDONED);
                    let kind =
                        fx.ptr(super::super::super::trap::kind_number("trap.abandoned") as u64);
                    lo.call_routine(fx, Routine::TrapReport, &[kind, trap]);
                    vec![fx.ptr(0)]
                },
                |lo, fx| vec![start(lo, fx)],
            )[0]
        }
    };
    let lock = fx.offset(record, domain::LOCK);
    let started = fx.b.ins().icmp_imm_s(IntCC::NotEqual, first, 0);
    fx.when(started, |fx| {
        fx.repeat(&[], |fx, _| {
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let flags = fx.load(PTR, first, task::FLAGS);
            let ended = fx.b.ins().band_imm_s(flags, flag::DONE);
            let ended = fx.b.ins().icmp_imm_s(IntCC::NotEqual, ended, 0);
            let abandoned = fx.load(PTR, record, domain::ABANDONED);
            let abandoned = fx.b.ins().icmp_imm_s(IntCC::Equal, abandoned, 1);
            let over = fx.b.ins().bor(ended, abandoned);
            let waiting = fx.b.ins().bxor_imm_s(over, 1);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            fx.when(waiting, |fx| {
                lo.call_routine(fx, Routine::Park, &[]);
            });
            (waiting, Vec::new())
        });
    });
    if let Some(fields) = abandoner {
        lo.call_routine(fx, Routine::Lock, &[fields.lock]);
        let zero = fx.ptr(0);
        fx.store(zero, fields.domain, 0);
        lo.call_routine(fx, Routine::Unlock, &[fields.lock]);
    }
    lo.call_routine(fx, Routine::Lock, &[parent_lock]);
    unlink_child(fx, caller_domain, record);
    lo.call_routine(fx, Routine::Unlock, &[parent_lock]);
    Nested {
        record,
        first,
        trap,
    }
}

/// Ends a domain whose first task ended: the holdings it still owns pass to `parent`, its other
/// chunks are freed, and its records with them.
pub fn end_nested(lo: &mut Lowering, fx: &mut Fx, record: Value, first: Value, parent: Value) {
    holdings::pass_all(lo, fx, record, parent);
    memory::free_chunks(lo, fx, record);
    lo.record_free(fx, first);
    lo.record_free(fx, record);
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

/// What a domain's first task does once its entry has ended: it closes the domain, so that no
/// abandoning begins after it, or stops when one began first.
fn domain_end(lo: &mut Lowering) -> FuncId {
    lo.glue_function(Glue::DomainEnd, super::record_signature(), |lo, fx| {
        let first = fx.entry()[0];
        let record = fx.load(PTR, first, task::OWNER);
        let flag = fx.offset(record, domain::ABANDONED);
        let live = fx.ptr(0);
        let closed = fx.ptr(domain::CLOSED as u64);
        let seen =
            fx.b.ins()
                .atomic_cas(MemFlagsData::trusted(), flag, live, closed);
        let abandoned = fx.b.ins().icmp_imm_s(IntCC::NotEqual, seen, 0);
        fx.when(abandoned, |fx| {
            lo.call_routine(fx, Routine::Stop, &[first]);
        });
        fx.b.ins().return_(&[]);
    })
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
