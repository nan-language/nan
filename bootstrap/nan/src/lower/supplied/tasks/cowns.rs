//! Cowns and `await when`.
//!
//! A cown's record is its count of handles, its rank, whether it is poisoned, its lock, whether
//! an `await when` holds it, the first and last of the requests waiting for it, its reach, then
//! its value. A request is a node on the requesting task's stack: the task, the next request,
//! whether the cown was granted to it and whether it waits in the cown's queue. The cowns an
//! `await when` holds are its count, then for each cown its record, the address of its value and
//! the walk moving its value; its requests follow on its stack, one per cown in the same order,
//! and its task's wait record names both.

use cranelift_codegen::ir::{self, InstBuilder, MemFlagsData, Value, condcodes::IntCC};

use super::super::super::abi::Repr;
use super::super::super::body::{Body, Held};
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::{Routine, settle, state, tasks};
use super::super::super::{Glue, Lowering, transfer, trap};
use super::super::Supply;
use crate::check::ty::Ty;
use crate::nir::{Operand, Region, Value as NirValue};

mod cown {
    pub const COUNT: u64 = 0;
    pub const RANK: u64 = 8;
    pub const POISONED: u64 = 16;
    pub const LOCK: u64 = 24;
    pub const HOLDER: u64 = 32;
    pub const HEAD: u64 = 40;
    pub const TAIL: u64 = 48;
    pub const REACH: u64 = 56;
    pub const VALUE: u64 = 64;
}

/// The offsets of the words of a cown an `await when` holds, after the count.
mod held {
    pub const RECORD: u64 = 0;
    pub const VALUE: u64 = 8;
    pub const WALK: u64 = 16;
    pub const SIZE: u64 = 24;
}

mod request {
    pub const TASK: u64 = 0;
    pub const NEXT: u64 = 8;
    pub const GRANTED: u64 = 16;
    pub const QUEUED: u64 = 24;
    pub const SIZE: u64 = 32;
}

/// The offset of a cown's value of a type.
fn value_at(lo: &mut Lowering, value: &Ty) -> u64 {
    cown::VALUE.next_multiple_of(lo.layout(value).align.max(1))
}

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let value_ty = supply.type_arg(0);
    match supply.member() {
        "of" | "try_of" => {
            let value = supply.param(0);
            let cell = fx.load(PTR, supply.param(1).address(), 0);
            let at = value_at(lo, &value_ty);
            let layout = lo.layout(&value_ty);
            let size = fx.ptr(at + layout.size);
            let align = fx.ptr(layout.align.max(8));
            let record = lo.call_routine(fx, Routine::Alloc, &[cell, size, align])[0];
            let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
            fx.when(failed, |fx| {
                supply.exhausted(lo, fx, &[(value, value_ty.clone())])
            });
            let zero = fx.ptr(0);
            let header = fx.ptr(cown::VALUE);
            lo.call_routine(fx, Routine::Fill, &[record, zero, header]);
            let one = fx.ptr(1);
            fx.store(one, record, cown::COUNT);
            let reach = tasks::current_domain(lo, fx);
            fx.store(reach, record, cown::REACH);
            let state_address = lo.state(fx);
            let ranks = fx.offset(state_address, state::RANKS);
            let rank = super::super::super::runtime::fetch_add(fx, ranks, one);
            let rank = fx.b.ins().iadd_imm_s(rank, 1);
            fx.store(rank, record, cown::RANK);
            let stored = fx.offset(record, at);
            lo.store_held(fx, stored, value, &value_ty);
            let sret = supply.sret.expect("a cown handle's address");
            fx.store(record, sret, 0);
            supply.done(lo, fx);
        }
        "share" => {
            let record = fx.load(PTR, supply.param(0).address(), 0);
            let count = fx.offset(record, cown::COUNT);
            let one = fx.ptr(1);
            super::super::super::runtime::fetch_add(fx, count, one);
            let sret = supply.sret.expect("a cown handle's address");
            fx.store(record, sret, 0);
            supply.done(lo, fx);
        }
        member => panic!("no lowering of task::Cown.{member}"),
    }
}

/// Releases a cown handle: the last one releases the value of a cown not poisoned, and frees
/// the cown.
pub fn release(lo: &mut Lowering, fx: &mut Fx, ty: &Ty, address: Value) {
    let value_ty = ty.type_arg(0).cloned().expect("a cown's value type");
    let record = fx.load(PTR, address, 0);
    let count = fx.offset(record, cown::COUNT);
    let minus = fx.ptr(u64::MAX);
    let previous = super::super::super::runtime::fetch_add(fx, count, minus);
    let last = fx.b.ins().icmp_imm_s(IntCC::Equal, previous, 1);
    fx.when(last, |fx| {
        let poisoned = fx.load(PTR, record, cown::POISONED);
        let healthy = fx.b.ins().icmp_imm_s(IntCC::Equal, poisoned, 0);
        fx.when(healthy, |fx| {
            let at = value_at(lo, &value_ty);
            let value = fx.offset(record, at);
            lo.release(fx, &value_ty, value);
            lo.call_routine(fx, Routine::Free, &[record]);
        });
    });
}

/// The address of the words of the cown at an index of those an `await when` holds.
fn held_at(fx: &mut Fx, held: Value, index: Value) -> Value {
    let at = fx.b.ins().imul_imm_s(index, held::SIZE as i64);
    let at = fx.b.ins().iadd(held, at);
    fx.offset(at, 8)
}

/// `CownRelease(held)`: releases every cown an `await when` holds, each granted to the next
/// request waiting for it. A cown whose reach passes the running task's domain first takes the
/// value into its reach, again until the reach stays as the value was moved to.
pub fn release_routine(lo: &mut Lowering, fx: &mut Fx) {
    let params = fx.entry();
    let held = params[0];
    let count = fx.load(PTR, held, 0);
    let domain = tasks::current_domain(lo, fx);
    let zero = fx.ptr(0);
    fx.each(zero, count, |fx, index| {
        let entry = held_at(fx, held, index);
        let record = fx.load(PTR, entry, held::RECORD);
        let value = fx.load(PTR, entry, held::VALUE);
        let walk = fx.load(PTR, entry, held::WALK);
        let lock = fx.offset(record, cown::LOCK);
        fx.repeat(&[domain], |fx, state| {
            let moved = state[0];
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let reach = fx.load(PTR, record, cown::REACH);
            let walks = fx.b.ins().icmp_imm_s(IntCC::NotEqual, walk, 0);
            let wider = fx.b.ins().icmp(IntCC::NotEqual, reach, moved);
            let pending = fx.b.ins().band(walks, wider);
            let settled = fx.b.ins().bxor_imm_s(pending, 1);
            fx.when(settled, |fx| grant_next_locked(lo, fx, record));
            fx.when(pending, |fx| {
                lo.call_routine(fx, Routine::Unlock, &[lock]);
                let owner = transfer::raised(fx, reach);
                transfer::call_walker(fx, walk, &[value, reach, owner]);
            });
            (pending, vec![reach])
        });
    });
    let current = tasks::current_task(lo, fx);
    let zero = fx.ptr(0);
    fx.store(zero, current, tasks::task::HELD);
    settle::leave_wait(lo, fx);
    fx.b.ins().return_(&[]);
}

/// Poisons the cowns an `await when` of a stopped task of a domain holds, each passed on to the
/// next request waiting for it. The holdings in the value of a cown whose reach passes the
/// domain stay with that reach.
pub fn poison(lo: &mut Lowering, fx: &mut Fx, held: Value, domain: Value) {
    let count = fx.load(PTR, held, 0);
    let zero = fx.ptr(0);
    fx.each(zero, count, |fx, index| {
        let entry = held_at(fx, held, index);
        let record = fx.load(PTR, entry, held::RECORD);
        let one = fx.ptr(1);
        let flag = fx.offset(record, cown::POISONED);
        fx.b.ins().atomic_store(MemFlagsData::trusted(), one, flag);
        let reach = fx.load(PTR, record, cown::REACH);
        let walk = fx.load(PTR, entry, held::WALK);
        let walks = fx.b.ins().icmp_imm_s(IntCC::NotEqual, walk, 0);
        let wider = fx.b.ins().icmp(IntCC::NotEqual, reach, domain);
        let walks = fx.b.ins().band(walks, wider);
        fx.when(walks, |fx| {
            let value = fx.load(PTR, entry, held::VALUE);
            let none = fx.ptr(0);
            let owner = transfer::raised(fx, reach);
            transfer::call_walker(fx, walk, &[value, none, owner]);
        });
        grant_next(lo, fx, record);
    });
}

/// Moves a cown handle into another domain's reach: when the cown's reach widens, its record
/// follows, and its value, holdings raised to the new reach, when no `await when` holds it and it
/// is not poisoned; an `await when` holding it moves the value as it releases the cown.
pub fn transfer(lo: &mut Lowering, fx: &mut Fx, ty: &Ty, address: Value, lift: Value) {
    let value_ty = ty.type_arg(0).cloned().expect("a cown's value type");
    let lifting = fx.b.ins().icmp_imm_s(IntCC::NotEqual, lift, 0);
    fx.when(lifting, |fx| {
        let record = fx.load(PTR, address, 0);
        let lock = fx.offset(record, cown::LOCK);
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let reach = fx.load(PTR, record, cown::REACH);
        let widened = tasks::nested_in(fx, reach, lift);
        fx.when(widened, |fx| fx.store(lift, record, cown::REACH));
        let holder = fx.load(PTR, record, cown::HOLDER);
        let head = fx.load(PTR, record, cown::HEAD);
        let poisoned = fx.load(PTR, record, cown::POISONED);
        let busy = fx.b.ins().bor(holder, head);
        let busy = fx.b.ins().bor(busy, poisoned);
        let idle = fx.b.ins().icmp_imm_s(IntCC::Equal, busy, 0);
        let free = fx.b.ins().band(widened, idle);
        fx.when(free, |fx| {
            let one = fx.ptr(1);
            fx.store(one, record, cown::HOLDER);
        });
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        fx.when(widened, |fx| {
            transfer::lift(lo, fx, record, lift);
        });
        fx.when(free, |fx| {
            let at = value_at(lo, &value_ty);
            let value = fx.offset(record, at);
            let owner = transfer::raised(fx, lift);
            lo.transfer(fx, &value_ty, value, lift, owner);
            grant_next(lo, fx, record);
        });
    });
}

/// Withdraws the requests of an `await when` whose body has not started: a cown already
/// granted passes on unpoisoned, and a request waiting in a cown's queue leaves it.
fn withdraw_when(lo: &mut Lowering) -> cranelift_module::FuncId {
    lo.glue_function(
        Glue::WithdrawWhen,
        settle::withdraw_signature(),
        |lo, fx| {
            let params = fx.entry();
            let (wait, record) = (params[0], params[1]);
            let held = fx.load(PTR, wait, settle::wait::OBJECT);
            let requests = fx.load(PTR, wait, settle::wait::NODE);
            let running = fx.load(PTR, record, tasks::task::HELD);
            let started = fx.b.ins().icmp(IntCC::Equal, running, held);
            let pending = fx.b.ins().bxor_imm_s(started, 1);
            fx.when(pending, |fx| {
                let count = fx.load(PTR, held, 0);
                let zero = fx.ptr(0);
                fx.each(zero, count, |fx, index| {
                    let entry = held_at(fx, held, index);
                    let cown_record = fx.load(PTR, entry, held::RECORD);
                    let at = fx.b.ins().imul_imm_s(index, request::SIZE as i64);
                    let node = fx.b.ins().iadd(requests, at);
                    let lock = fx.offset(cown_record, cown::LOCK);
                    lo.call_routine(fx, Routine::Lock, &[lock]);
                    let granted = fx.load(PTR, node, request::GRANTED);
                    let granted = fx.b.ins().icmp_imm_s(IntCC::NotEqual, granted, 0);
                    let queued = fx.load(PTR, node, request::QUEUED);
                    let queued = fx.b.ins().icmp_imm_s(IntCC::NotEqual, queued, 0);
                    let ungranted = fx.b.ins().bxor_imm_s(granted, 1);
                    let waits = fx.b.ins().band(queued, ungranted);
                    fx.when(waits, |fx| dequeue(fx, cown_record, node));
                    lo.call_routine(fx, Routine::Unlock, &[lock]);
                    fx.when(granted, |fx| grant_next(lo, fx, cown_record));
                });
            });
            fx.b.ins().return_(&[]);
        },
    )
}

/// Takes a waiting request out of a cown's queue. The caller holds the cown's lock.
fn dequeue(fx: &mut Fx, record: Value, node: Value) {
    let head = fx.load(PTR, record, cown::HEAD);
    let next = fx.load(PTR, node, request::NEXT);
    let first = fx.b.ins().icmp(IntCC::Equal, head, node);
    let previous = fx.choose(
        first,
        &[PTR],
        |fx| {
            fx.store(next, record, cown::HEAD);
            vec![fx.ptr(0)]
        },
        |fx| {
            let before = fx.repeat(&[head], |fx, state| {
                let current = state[0];
                let after = fx.load(PTR, current, request::NEXT);
                let found = fx.b.ins().icmp(IntCC::Equal, after, node);
                let more = fx.b.ins().bxor_imm_s(found, 1);
                let step = fx.b.ins().select(found, current, after);
                (more, vec![step])
            })[0];
            fx.store(next, before, request::NEXT);
            vec![before]
        },
    )[0];
    let tail = fx.load(PTR, record, cown::TAIL);
    let last = fx.b.ins().icmp(IntCC::Equal, tail, node);
    fx.when(last, |fx| fx.store(previous, record, cown::TAIL));
}

/// Passes a cown on to its oldest waiting request, or leaves it free.
fn grant_next(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let lock = fx.offset(record, cown::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    grant_next_locked(lo, fx, record);
}

/// Passes a cown on to its oldest waiting request, or leaves it free, then releases the cown's
/// lock, which the caller holds.
fn grant_next_locked(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let lock = fx.offset(record, cown::LOCK);
    let head = fx.load(PTR, record, cown::HEAD);
    let waiting = fx.b.ins().icmp_imm_s(IntCC::NotEqual, head, 0);
    let next = fx.choose(
        waiting,
        &[PTR],
        |fx| vec![fx.load(PTR, head, request::NEXT)],
        |fx| vec![fx.ptr(0)],
    )[0];
    fx.store(next, record, cown::HEAD);
    let emptied = fx.b.ins().icmp_imm_s(IntCC::Equal, next, 0);
    fx.when(emptied, |fx| {
        let zero = fx.ptr(0);
        fx.store(zero, record, cown::TAIL);
    });
    let holder = fx.b.ins().uextend(PTR, waiting);
    fx.store(holder, record, cown::HOLDER);
    let task = fx.choose(
        waiting,
        &[PTR],
        |fx| {
            let task = fx.load(PTR, head, request::TASK);
            let granted = fx.offset(head, request::GRANTED);
            let one = fx.ptr(1);
            fx.b.ins()
                .atomic_store(MemFlagsData::trusted(), one, granted);
            vec![task]
        },
        |fx| vec![fx.ptr(0)],
    )[0];
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    fx.when(waiting, |fx| {
        lo.call_routine(fx, Routine::Wake, &[task]);
    });
}

/// Requests a cown for the running task through a request node, suspending until it is
/// granted.
fn acquire(lo: &mut Lowering, fx: &mut Fx, record: Value, node: Value) {
    let current = tasks::current_task(lo, fx);
    let zero = fx.ptr(0);
    fx.store(current, node, request::TASK);
    fx.store(zero, node, request::NEXT);
    fx.store(zero, node, request::GRANTED);
    fx.store(zero, node, request::QUEUED);
    let lock = fx.offset(record, cown::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let holder = fx.load(PTR, record, cown::HOLDER);
    let head = fx.load(PTR, record, cown::HEAD);
    let busy = fx.b.ins().bor(holder, head);
    let free = fx.b.ins().icmp_imm_s(IntCC::Equal, busy, 0);
    fx.choose(
        free,
        &[],
        |fx| {
            let one = fx.ptr(1);
            fx.store(one, record, cown::HOLDER);
            fx.store(one, node, request::GRANTED);
            Vec::new()
        },
        |fx| {
            let tail = fx.load(PTR, record, cown::TAIL);
            let has_tail = fx.b.ins().icmp_imm_s(IntCC::NotEqual, tail, 0);
            fx.choose(
                has_tail,
                &[],
                |fx| {
                    fx.store(node, tail, request::NEXT);
                    Vec::new()
                },
                |fx| {
                    fx.store(node, record, cown::HEAD);
                    Vec::new()
                },
            );
            fx.store(node, record, cown::TAIL);
            let one = fx.ptr(1);
            fx.store(one, node, request::QUEUED);
            Vec::new()
        },
    );
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    fx.repeat(&[], |fx, _| {
        let granted = fx.offset(node, request::GRANTED);
        let granted =
            fx.b.ins()
                .atomic_load(PTR, MemFlagsData::trusted(), granted);
        let waiting = fx.b.ins().icmp_imm_s(IntCC::Equal, granted, 0);
        fx.when(waiting, |fx| {
            lo.call_routine(fx, Routine::Park, &[]);
        });
        (waiting, Vec::new())
    });
}

/// `await when`: acquires the cowns its accesses name in increasing rank, checks for poisoning,
/// runs the region with a reference to each cown's value, and releases them as the region
/// yields or any exit leaves it.
pub fn when(
    body: &mut Body,
    attempt: bool,
    accesses: &[Operand],
    region: &Region,
    result: Option<NirValue>,
) -> Held {
    let mut cowns = Vec::new();
    let mut values = Vec::new();
    for access in accesses {
        let (held, nty) = body.operand(access);
        let handle = super::address_of(body.lo, body.fx, nty.ty(), held);
        cowns.push(body.fx.load(PTR, handle, 0));
        values.push(nty.ty().type_arg(0).cloned().expect("a cown's value type"));
    }
    for (index, first) in cowns.iter().enumerate() {
        for second in &cowns[index + 1..] {
            let distinct = body.fx.b.ins().icmp(IntCC::NotEqual, *first, *second);
            body.trap_unless(distinct, "trap.duplicate-access", trap::Operands::None);
        }
    }
    let held = body.fx.slot(8 + held::SIZE * cowns.len() as u64, 8);
    let count = body.fx.ptr(cowns.len() as u64);
    body.fx.store(count, held, 0);
    let mut ordered: Vec<[Value; 3]> = cowns
        .iter()
        .zip(&values)
        .map(|(record, value_ty)| {
            let at = value_at(body.lo, value_ty);
            let value = body.fx.offset(*record, at);
            let walk = body.lo.walker_address(body.fx, value_ty);
            [*record, value, walk]
        })
        .collect();
    for pass in 0..ordered.len() {
        for index in 0..ordered.len().saturating_sub(pass + 1) {
            let (left, right) = (ordered[index], ordered[index + 1]);
            let left_rank = body.fx.load(PTR, left[0], cown::RANK);
            let right_rank = body.fx.load(PTR, right[0], cown::RANK);
            let swap = body
                .fx
                .b
                .ins()
                .icmp(IntCC::UnsignedGreaterThan, left_rank, right_rank);
            for word in 0..3 {
                ordered[index][word] = body.fx.b.ins().select(swap, right[word], left[word]);
                ordered[index + 1][word] = body.fx.b.ins().select(swap, left[word], right[word]);
            }
        }
    }
    let requests = body.fx.slot(request::SIZE * cowns.len() as u64, 8);
    for (index, words) in ordered.iter().enumerate() {
        let entry = 8 + held::SIZE * index as u64;
        body.fx.store(words[0], held, entry + held::RECORD);
        body.fx.store(words[1], held, entry + held::VALUE);
        body.fx.store(words[2], held, entry + held::WALK);
    }
    let withdraw = withdraw_when(body.lo);
    settle::enter_wait(body.lo, body.fx, withdraw, held, requests);
    for (index, words) in ordered.iter().enumerate() {
        let node = body.fx.offset(requests, request::SIZE * index as u64);
        acquire(body.lo, body.fx, words[0], node);
    }
    let current = tasks::current_task(body.lo, body.fx);
    body.fx.store(held, current, tasks::task::HELD);
    let mut poisoned = body.fx.ptr(0);
    for words in &ordered {
        let flag = body.fx.load(PTR, words[0], cown::POISONED);
        poisoned = body.fx.b.ins().bor(poisoned, flag);
    }
    let poisoned = body.fx.b.ins().icmp_imm_s(IntCC::NotEqual, poisoned, 0);
    let result_ty = result
        .map(|value| body.nty(value).ty().clone())
        .unwrap_or(Ty::Unit);
    let yielded_ty = match attempt {
        true => result_ty
            .type_arg(0)
            .cloned()
            .expect("an attempted when's value type"),
        false => result_ty.clone(),
    };
    let done = body.fx.b.create_block();
    let done_params = match body.lo.owned_repr(&result_ty) {
        Repr::Scalar(ty) => vec![ty],
        Repr::Address => vec![PTR],
        Repr::Empty => Vec::new(),
    };
    for ty in &done_params {
        body.fx.b.append_block_param(done, *ty);
    }
    let healthy = body.fx.b.create_block();
    let sick = body.fx.b.create_block();
    body.fx.b.ins().brif(poisoned, sick, &[], healthy, &[]);
    body.fx.b.switch_to_block(sick);
    body.fx.b.seal_block(sick);
    body.lo.call_routine(body.fx, Routine::CownRelease, &[held]);
    if attempt {
        let err = body
            .lo
            .variant_value(body.fx, &result_ty, "Err", &[Held::Empty], None);
        let args: Vec<ir::BlockArg> = err.values().into_iter().map(Into::into).collect();
        body.fx.b.ins().jump(done, &args);
    } else {
        body.trap_now("trap.poisoned", trap::Operands::None);
    }
    body.fx.b.switch_to_block(healthy);
    body.fx.b.seal_block(healthy);
    let release = body.fx.b.create_block();
    match body.lo.owned_repr(&yielded_ty) {
        Repr::Scalar(ty) => {
            body.fx.b.append_block_param(release, ty);
        }
        Repr::Address => {
            body.fx.b.append_block_param(release, PTR);
        }
        Repr::Empty => {}
    }
    body.open_region(region, release, Some(held));
    let mut args: Vec<ir::BlockArg> = Vec::new();
    for (record, value_ty) in cowns.iter().zip(&values) {
        let at = value_at(body.lo, value_ty);
        args.push(body.fx.offset(*record, at).into());
    }
    let entry = body.region_entry(region);
    body.fx.b.ins().jump(entry, &args);
    body.fx.b.switch_to_block(release);
    let params = body.fx.b.block_params(release).to_vec();
    let value = match body.lo.owned_repr(&yielded_ty) {
        Repr::Scalar(_) => Held::Scalar(params[0]),
        Repr::Address => Held::Address(params[0]),
        Repr::Empty => Held::Empty,
    };
    body.lo.call_routine(body.fx, Routine::CownRelease, &[held]);
    let outcome = match attempt {
        true => body
            .lo
            .variant_value(body.fx, &result_ty, "Ok", &[value], None),
        false => value,
    };
    let args: Vec<ir::BlockArg> = outcome.values().into_iter().map(Into::into).collect();
    body.fx.b.ins().jump(done, &args);
    body.fx.b.switch_to_block(done);
    body.fx.b.seal_block(done);
    let params = body.fx.b.block_params(done).to_vec();
    match body.lo.owned_repr(&result_ty) {
        Repr::Scalar(_) => Held::Scalar(params[0]),
        Repr::Address => Held::Address(params[0]),
        Repr::Empty => Held::Empty,
    }
}
