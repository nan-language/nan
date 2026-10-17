//! Task scopes, their children and join handles, deadlines and races.
//!
//! A scope's record is its region, its lock, the state of its kept failure (none, being
//! recorded, a value of its failure type, or a cancellation's origin), that origin, its
//! executor's cell, the count of what references it, whether its call has returned, and its
//! kept failure. A child's record keeps its scope as owner; a join handle is the child's
//! record. A race's record is its region, its lock and the task that decided it.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, condcodes::IntCC, types};
use cranelift_module::FuncId;

use super::super::super::body::{Body, Held};
use super::super::super::call::adapt_value;
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::regions::{self, region};
use super::super::super::runtime::tasks::{self, flag, kind, operation, task};
use super::super::super::runtime::{Routine, report, tables};
use super::super::super::{Glue, Lowering};
use super::super::Supply;
use super::super::mem::lists::fn_param;
use crate::check::ty::{FnTy, Ty};
use crate::nir::{Callable, FnRef, Op, Operand, Role, Value as NirValue};

mod scope {
    pub const LOCK: u64 = 40;
    pub const FAILED: u64 = 48;
    pub const ORIGIN: u64 = 56;
    pub const EXECUTOR: u64 = 64;
    pub const REFS: u64 = 72;
    pub const RETURNED: u64 = 80;
    pub const FAILURE: u64 = 88;
}

/// The states of a scope's kept failure.
mod kept {
    pub const RECORDING: i64 = 1;
    pub const VALUE: i64 = 2;
    pub const CANCELLATION: i64 = 3;
}

mod race {
    pub const LOCK: u64 = 40;
    pub const DECIDED: u64 = 48;
    pub const SIZE: u64 = 56;
}

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    match supply.path.as_str() {
        "task::scope" => scope_call(lo, fx, supply),
        "task::within" => within(lo, fx, supply),
        "task::race" => race_call(lo, fx, supply),
        "task::Scope.cancel" => {
            let record = fx.load(PTR, supply.param(0).address(), 0);
            lo.call_routine(fx, Routine::Cancel, &[record]);
            supply.done(lo, fx);
        }
        "task::Scope.is_cancelled" => {
            let record = fx.load(PTR, supply.param(0).address(), 0);
            let origin = regions::origin(fx, record);
            let cancelled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, origin, 0);
            supply.ret(lo, fx, Held::Scalar(cancelled));
        }
        "task::Scope.spawn" => {
            let child_ty = fn_param(supply, 1);
            let scope_record = fx.load(PTR, supply.param(0).address(), 0);
            let child = supply.param(1).address();
            let join = spawn_child(lo, fx, scope_record, child, &child_ty, None);
            let sret = supply.sret.expect("a join handle's address");
            fx.store(join, sret, 0);
            supply.done(lo, fx);
        }
        "task::Join.join" => join(lo, fx, supply),
        other => panic!("no lowering of {other}"),
    }
}

/// A call of `task::Scope.spawn` lowered where it is written: at a lexical spawn, the child's
/// failure converts to the failure type of its scope's body closure.
pub fn spawn_at(body: &mut Body, operands: &[Operand]) -> Held {
    let scope_fails = lexical_scope(body, &operands[0]);
    let (receiver, receiver_nty) = body.operand(&operands[0]);
    let receiver = super::address_of(body.lo, body.fx, receiver_nty.ty(), receiver);
    let scope_record = body.fx.load(PTR, receiver, 0);
    let (child, child_nty) = body.operand(&operands[1]);
    let Ty::Fn(child_ty) = child_nty.ty().clone() else {
        panic!("a spawned task fn value");
    };
    let child = super::address_of(body.lo, body.fx, child_nty.ty(), child);
    let join = spawn_child(
        body.lo,
        body.fx,
        scope_record,
        child,
        &child_ty,
        scope_fails,
    );
    let slot = body.fx.slot(8, 8);
    body.fx.store(join, slot, 0);
    Held::Address(slot)
}

/// Whether a call names `task::Scope.spawn`.
pub fn is_spawn(lo: &Lowering, fn_ref: &FnRef) -> bool {
    matches!(fn_ref.callable, Callable::Item { item, role: Role::Body, .. }
        if lo.ck.db.is_std(item) && lo.ck.db.canonical(item) == "task::Scope.spawn")
}

/// The failure type of the body closure whose parameter a spawn's receiver is, reached through
/// the captures of the closures nested in it; none when the spawn is not lexical.
fn lexical_scope(body: &Body, receiver: &Operand) -> Option<Ty> {
    let value = match receiver {
        Operand::Value(value) => *value,
        Operand::Place(_, place) if place.steps.is_empty() => place.root,
        _ => return None,
    };
    let unit = body.unit.clone();
    let mut function = body.function;
    let mut value = value;
    loop {
        let nir = &unit.functions[function];
        let position = nir.params.iter().position(|param| param.value == value)?;
        nir.closure.as_ref()?;
        let captures = body.lo.closure_captures(&unit, nir.index);
        if position >= captures {
            let first = nir.params.get(captures)?;
            let is_scope = body.lo.ck.is_std(nir.ty(first.value).ty(), "task::Scope");
            if position != captures || !is_scope {
                return None;
            }
            return scope_failure(body, &unit, nir.index);
        }
        let (parent, operand) =
            unit.functions
                .iter()
                .enumerate()
                .find_map(|(index, candidate)| {
                    candidate
                        .blocks
                        .iter()
                        .flat_map(|block| &block.insts)
                        .find_map(|inst| match &inst.op {
                            Op::Closure {
                                index: closure,
                                captures,
                            } if *closure == nir.index => {
                                Some((index, captures.get(position)?.clone()))
                            }
                            _ => None,
                        })
                })?;
        value = match operand {
            Operand::Value(value) => value,
            Operand::Place(_, place) if place.steps.is_empty() => place.root,
            _ => return None,
        };
        function = parent;
    }
}

/// The failure type of the `task::scope` call whose body argument is the closure of index
/// `closure`, written there.
fn scope_failure(body: &Body, unit: &crate::nir::Unit, closure: u32) -> Option<Ty> {
    for function in &unit.functions {
        let insts = function.blocks.iter().flat_map(|block| &block.insts);
        let Some(created) = insts
            .filter_map(|inst| match &inst.op {
                Op::Closure { index, .. } if *index == closure => inst.result,
                _ => None,
            })
            .next()
        else {
            continue;
        };
        let holders: Vec<NirValue> = std::iter::once(created)
            .chain(
                function
                    .blocks
                    .iter()
                    .flat_map(|block| &block.insts)
                    .filter_map(|inst| match &inst.op {
                        Op::Init(place, Operand::Value(value))
                            if *value == created && place.steps.is_empty() =>
                        {
                            Some(place.root)
                        }
                        _ => None,
                    }),
            )
            .collect();
        let names = |operand: &Operand| match operand {
            Operand::Value(value) => holders.contains(value),
            Operand::Place(_, place) => place.steps.is_empty() && holders.contains(&place.root),
            _ => false,
        };
        let calls = function.blocks.iter().flat_map(|block| {
            let terminator = match &block.term {
                crate::nir::Terminator::Call { call, .. } => Some(call),
                _ => None,
            };
            block
                .insts
                .iter()
                .filter_map(|inst| match &inst.op {
                    Op::Call(call) => Some(call),
                    _ => None,
                })
                .chain(terminator)
        });
        for call in calls {
            let crate::nir::Callee::Fn(fn_ref) = &call.callee else {
                continue;
            };
            let Callable::Item { item, .. } = fn_ref.callable else {
                continue;
            };
            if body.lo.ck.db.canonical(item) == "task::scope" && call.args.get(1).is_some_and(names)
            {
                return fn_ref
                    .args
                    .first()
                    .and_then(crate::check::ty::Arg::ty)
                    .map(|ty| body.subst.ty(ty));
            }
        }
    }
    None
}

/// `task::scope(exec, body: b)`.
fn scope_call(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let fails = supply.type_arg(0);
    let body_ty = fn_param(supply, 1);
    let (live, cell) = super::executor(lo, fx, supply.param(0).address());
    let stale = fx.b.ins().bxor_imm_s(live, 1);
    fx.when(stale, |fx| {
        let none = fx.ptr(0);
        super::fail_cancelled(lo, fx, supply, none);
    });
    let layout = lo.layout(&fails);
    let size = fx.ptr(scope::FAILURE + layout.size.max(8));
    let record = lo.call_routine(fx, Routine::RegionOpen, &[size])[0];
    fx.store(cell, record, scope::EXECUTOR);
    let one = fx.ptr(1);
    fx.store(one, record, scope::REFS);
    let current = tasks::current_task(lo, fx);
    let enclosing = fx.load(PTR, current, task::REGION);
    fx.store(record, current, task::REGION);
    let value = fx.slot(8, 8);
    fx.store(record, value, 0);
    let called = lo.call_value(
        fx,
        &body_ty,
        supply.param(1).address(),
        &[Held::Address(value)],
    );
    fx.store(enclosing, current, task::REGION);
    if let (Some(failed), Some(failure)) = (called.failed, called.failure) {
        fx.when(failed, |fx| {
            let held = lo.held_at(fx, &fails, failure);
            keep_failure(lo, fx, record, &fails, held);
        });
    }
    wait_children(lo, fx, record, region::LIVE);
    let state = fx.load(PTR, record, scope::FAILED);
    let target = supply.failure.expect("a scope's failure address");
    let value_kept = fx.b.ins().icmp_imm_s(IntCC::Equal, state, kept::VALUE);
    fx.when(value_kept, |fx| {
        let kept = fx.offset(record, scope::FAILURE);
        let held = lo.held_at(fx, &fails, kept);
        lo.store_held(fx, target, held, &fails);
    });
    let cancellation_kept =
        fx.b.ins()
            .icmp_imm_s(IntCC::Equal, state, kept::CANCELLATION);
    fx.when(cancellation_kept, |fx| {
        let origin = fx.load(PTR, record, scope::ORIGIN);
        let held = lo.cancellation(fx, &fails, origin);
        lo.store_held(fx, target, held, &fails);
    });
    let one = fx.ptr(1);
    fx.store(one, record, scope::RETURNED);
    unreference(lo, fx, record);
    let failed =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, state, kept::VALUE);
    fx.when(failed, |fx| supply.fail(fx));
    supply.done(lo, fx);
}

/// Keeps a failure of a scope's failure type as the scope's failure unless it is a cancellation
/// of the scope itself, which the scope absorbs, or another failure was kept first, which it
/// releases.
fn keep_failure(lo: &mut Lowering, fx: &mut Fx, record: Value, fails: &Ty, held: Held) {
    let id = fx.load(PTR, record, region::ID);
    let absorbed = match lo.cancellation_origin(fx, fails, held) {
        Some((is_cancellation, origin)) => {
            let own = fx.b.ins().icmp(IntCC::Equal, origin, id);
            fx.b.ins().band(is_cancellation, own)
        }
        None => fx.int(types::I8, 0),
    };
    let recorded = fx.b.ins().bxor_imm_s(absorbed, 1);
    fx.when(recorded, |fx| {
        let first = claim(lo, fx, record);
        let fails = fails.clone();
        fx.choose_in(
            lo,
            first,
            &[],
            |lo, fx| {
                let kept = fx.offset(record, scope::FAILURE);
                lo.store_held(fx, kept, held, &fails);
                settle_kept(lo, fx, record, kept::VALUE);
                Vec::new()
            },
            |lo, fx| {
                let address = super::address_of(lo, fx, &fails, held);
                lo.release(fx, &fails, address);
                Vec::new()
            },
        );
    });
}

/// Claims a scope's kept failure: nonzero for the first claimant.
fn claim(_lo: &mut Lowering, fx: &mut Fx, record: Value) -> Value {
    let state = fx.offset(record, scope::FAILED);
    let zero = fx.ptr(0);
    let recording = fx.ptr(kept::RECORDING as u64);
    let previous =
        fx.b.ins()
            .atomic_cas(MemFlagsData::trusted(), state, zero, recording);
    fx.b.ins().icmp_imm_s(IntCC::Equal, previous, 0)
}

/// Ends the recording of a scope's kept failure and cancels the scope.
fn settle_kept(lo: &mut Lowering, fx: &mut Fx, record: Value, state: i64) {
    let word = fx.offset(record, scope::FAILED);
    let state = fx.ptr(state as u64);
    fx.b.ins()
        .atomic_store(MemFlagsData::trusted(), state, word);
    lo.call_routine(fx, Routine::Cancel, &[record]);
}

/// Waits until a construct has no live task, whatever cancellation reaches the waiting task.
fn wait_children(lo: &mut Lowering, fx: &mut Fx, record: Value, live_at: u64) {
    let lock = fx.offset(record, scope::LOCK);
    let current = tasks::current_task(lo, fx);
    fx.repeat(&[], |fx, _| {
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let live = fx.load(PTR, record, live_at);
        let waiting = fx.b.ins().icmp_imm_s(IntCC::NotEqual, live, 0);
        fx.when(waiting, |fx| fx.store(current, record, region::WAITER));
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        fx.when(waiting, |fx| {
            lo.call_routine(fx, Routine::Park, &[]);
        });
        (waiting, Vec::new())
    });
}

/// Drops one reference to a scope's record, freeing it with the last.
fn unreference(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let refs = fx.offset(record, scope::REFS);
    let minus = fx.ptr(u64::MAX);
    let previous = super::super::super::runtime::fetch_add(fx, refs, minus);
    let last = fx.b.ins().icmp_imm_s(IntCC::Equal, previous, 1);
    fx.when(last, |fx| {
        lo.record_free(fx, record);
    });
}

/// The offsets of a task's result and failure after its record, and the bytes they take.
fn storage(lo: &mut Lowering, fn_ty: &FnTy) -> (u64, u64) {
    let result = lo.layout(&fn_ty.result);
    let failure = fn_ty.fails.as_ref().map_or((8, 8), |fails| {
        let layout = lo.layout(fails);
        (layout.size, layout.align.max(1))
    });
    let failure_at = result.size.next_multiple_of(failure.1);
    (failure_at, failure_at + failure.0)
}

/// Starts a task running a `task fn` value of a type held at an address, in a region, owned by
/// a construct or domain record, its stack drawn through a cell for a domain: its record,
/// ready. It traps when the stack cannot be drawn.
#[allow(clippy::too_many_arguments)]
pub fn start_task(
    lo: &mut Lowering,
    fx: &mut Fx,
    fn_ty: &FnTy,
    function: Value,
    cell: Value,
    domain: Value,
    region_record: Value,
    owner: Value,
    end: Option<FuncId>,
    done: FuncId,
) -> Value {
    let (failure_at, extra) = storage(lo, fn_ty);
    let glue = entry_glue(lo, fn_ty);
    let glue = lo.func_ref(fx, glue);
    let glue = fx.b.ins().func_addr(PTR, glue);
    let code = fx.load(PTR, function, 0);
    let env = fx.load(PTR, function, 8);
    let extra = fx.ptr(extra);
    let stack = fx.ptr(super::stack_size(fn_ty));
    let record = lo.call_routine(
        fx,
        Routine::Spawn,
        &[glue, code, env, extra, stack, cell, domain, region_record],
    )[0];
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
    fx.when(missing, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
    let failure =
        fx.b.ins()
            .iadd_imm_s(record, (task::SIZE + failure_at) as i64);
    fx.store(failure, record, task::FAILURE);
    fx.store(owner, record, task::OWNER);
    if let Some(end) = end {
        let end = lo.func_ref(fx, end);
        let end = fx.b.ins().func_addr(PTR, end);
        fx.store(end, record, task::END);
    }
    let done = lo.func_ref(fx, done);
    let done = fx.b.ins().func_addr(PTR, done);
    fx.store(done, record, task::DONE);
    if lo.needs_release(&fn_ty.result) {
        let release = lo.instance(super::super::super::Instance::Release(fn_ty.result.clone()));
        let release = lo.func_ref(fx, release);
        let release = fx.b.ins().func_addr(PTR, release);
        fx.store(release, record, task::RESULT_RELEASE);
    }
    record
}

/// The glue running a task's entry, a function value of a type taking at most the argument the
/// record holds: it writes the result or failure after the record, records the outcome and
/// frees the closure's environment.
fn entry_glue(lo: &mut Lowering, fn_ty: &FnTy) -> FuncId {
    let fn_ty = fn_ty.clone();
    lo.glue_function(
        Glue::Entry(fn_ty.clone()),
        super::record_signature(),
        move |lo, fx| {
            let params = fx.entry();
            let record = params[0];
            let abi = lo.fn_abi(&fn_ty);
            let code = fx.load(PTR, record, task::CODE);
            let env = fx.load(PTR, record, task::ENV);
            let result = fx.load(PTR, record, task::RESULT);
            let failure = fx.load(PTR, record, task::FAILURE);
            let mut args = vec![env];
            if abi.returned == super::super::super::abi::Returned::Indirect {
                args.push(result);
            }
            if abi.flagged {
                args.push(failure);
            }
            for (nty, repr) in &abi.params {
                let argument = fx.offset(record, task::ARGUMENT);
                let held = adapt_value(lo, fx, Held::Address(argument), nty, *repr);
                args.extend(held.values());
            }
            let signature = fx.b.import_signature(abi.signature());
            let call = fx.b.ins().call_indirect(signature, code, &args);
            let returns = fx.b.inst_results(call).to_vec();
            let mut returns = returns.into_iter();
            let failed = abi.flagged.then(|| returns.next().expect("a failure flag"));
            if let super::super::super::abi::Returned::Scalar(_) = abi.returned {
                let value = returns.next().expect("a result");
                lo.store_held(fx, result, Held::Scalar(value), &fn_ty.result);
            }
            let returned = fx.ptr(1);
            let outcome = match failed {
                Some(failed) => {
                    let failed = fx.b.ins().uextend(PTR, failed);
                    fx.b.ins().iadd(returned, failed)
                }
                None => returned,
            };
            fx.store(outcome, record, task::OUTCOME);
            lo.call_routine(fx, Routine::TaskEnvFree, &[env]);
            fx.b.ins().return_(&[]);
        },
    )
}

/// Spawns a scope's child running a `task fn` value: its join handle, which is its record.
fn spawn_child(
    lo: &mut Lowering,
    fx: &mut Fx,
    scope_record: Value,
    child: Value,
    child_ty: &FnTy,
    scope_fails: Option<Ty>,
) -> Value {
    let cell = fx.load(PTR, scope_record, scope::EXECUTOR);
    let domain = tasks::current_domain(lo, fx);
    let end = child_end(lo, child_ty, scope_fails);
    let done = child_done(lo);
    let lock = fx.offset(scope_record, scope::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let live = fx.load(PTR, scope_record, region::LIVE);
    let live = fx.b.ins().iadd_imm_s(live, 1);
    fx.store(live, scope_record, region::LIVE);
    let refs = fx.load(PTR, scope_record, scope::REFS);
    let refs = fx.b.ins().iadd_imm_s(refs, 1);
    fx.store(refs, scope_record, scope::REFS);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    let record = start_task(
        lo,
        fx,
        child_ty,
        child,
        cell,
        domain,
        scope_record,
        scope_record,
        Some(end),
        done,
    );
    tasks::spawned(lo, fx, record);
    record
}

/// The end of a scope's child: a failure that is no cancellation of the scope itself becomes
/// the scope's kept failure, converted to the scope's failure type at a lexical spawn, or its
/// cancellation's origin.
fn child_end(lo: &mut Lowering, child_ty: &FnTy, scope_fails: Option<Ty>) -> FuncId {
    let child_ty = child_ty.clone();
    let glue = Glue::ChildEnd(child_ty.clone(), scope_fails.clone());
    lo.glue_function(glue, super::record_signature(), move |lo, fx| {
        let params = fx.entry();
        let record = params[0];
        let Some(fails) = child_ty.fails.clone() else {
            fx.b.ins().return_(&[]);
            return;
        };
        let outcome = fx.load(PTR, record, task::OUTCOME);
        let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, outcome, 2);
        fx.when(failed, |fx| {
            let scope_record = fx.load(PTR, record, task::OWNER);
            let id = fx.load(PTR, scope_record, region::ID);
            let failure = fx.load(PTR, record, task::FAILURE);
            let held = lo.held_at(fx, &fails, failure);
            let (is_cancellation, origin) =
                lo.cancellation_origin(fx, &fails, held).unwrap_or_else(|| {
                    let no = fx.int(types::I8, 0);
                    let zero = fx.ptr(0);
                    (no, zero)
                });
            fx.choose_in(
                lo,
                is_cancellation,
                &[],
                |lo, fx| {
                    let other = fx.b.ins().icmp(IntCC::NotEqual, origin, id);
                    fx.when(other, |fx| {
                        let first = claim(lo, fx, scope_record);
                        fx.when(first, |fx| {
                            fx.store(origin, scope_record, scope::ORIGIN);
                            settle_kept(lo, fx, scope_record, kept::CANCELLATION);
                        });
                    });
                    Vec::new()
                },
                |lo, fx| {
                    match &scope_fails {
                        Some(scope_fails) => {
                            let via = lo.ck.conversion_variant(&fails, scope_fails);
                            let converted =
                                lo.convert_failure(fx, held, &fails, scope_fails, via.as_deref());
                            keep_failure(lo, fx, scope_record, scope_fails, converted);
                        }
                        None => {
                            let address = super::address_of(lo, fx, &fails, held);
                            lo.release(fx, &fails, address);
                        }
                    }
                    Vec::new()
                },
            );
        });
        fx.b.ins().return_(&[]);
    })
}

/// What the executor thread does once a scope's child has ended: the joiner and, after the
/// last child, the scope's caller wake; a reaped child's result is released and its record
/// freed.
fn child_done(lo: &mut Lowering) -> FuncId {
    lo.glue_function(Glue::ChildDone, super::record_signature(), |lo, fx| {
        let params = fx.entry();
        let record = params[0];
        let scope_record = fx.load(PTR, record, task::OWNER);
        let lock = fx.offset(scope_record, scope::LOCK);
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let flags = fx.load(PTR, record, task::FLAGS);
        let flags = fx.b.ins().bor_imm_s(flags, flag::DONE);
        fx.store(flags, record, task::FLAGS);
        let live = fx.load(PTR, scope_record, region::LIVE);
        let live = fx.b.ins().iadd_imm_s(live, -1);
        fx.store(live, scope_record, region::LIVE);
        let joiner = fx.load(PTR, record, task::JOINER);
        let last = fx.b.ins().icmp_imm_s(IntCC::Equal, live, 0);
        let waiter = fx.load(PTR, scope_record, region::WAITER);
        let zero = fx.ptr(0);
        let waiter = fx.b.ins().select(last, waiter, zero);
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        for woken in [joiner, waiter] {
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, woken, 0);
            fx.when(present, |fx| {
                lo.call_routine(fx, Routine::Wake, &[woken]);
            });
        }
        let reaped = fx.b.ins().band_imm_s(flags, flag::REAPED);
        let reaped = fx.b.ins().icmp_imm_s(IntCC::NotEqual, reaped, 0);
        fx.when(reaped, |fx| {
            finish_child(lo, fx, record, scope_record, None)
        });
        fx.b.ins().return_(&[]);
    })
}

/// Frees an ended child's record, releasing its result when it returned one, and drops its
/// reference to its scope. The result's type, when known, releases it by a direct call; the
/// executor thread, which ends children of every type, releases it through the release the
/// child's record holds.
fn finish_child(
    lo: &mut Lowering,
    fx: &mut Fx,
    record: Value,
    scope_record: Value,
    result_ty: Option<&Ty>,
) {
    let outcome = fx.load(PTR, record, task::OUTCOME);
    let returned = fx.b.ins().icmp_imm_s(IntCC::Equal, outcome, 1);
    match result_ty {
        Some(ty) => {
            if lo.needs_release(ty) {
                fx.when(returned, |fx| {
                    let result = fx.load(PTR, record, task::RESULT);
                    lo.release(fx, ty, result);
                });
            }
        }
        None => {
            let release = fx.load(PTR, record, task::RESULT_RELEASE);
            let releases = fx.b.ins().icmp_imm_s(IntCC::NotEqual, release, 0);
            let releases = fx.b.ins().band(returned, releases);
            fx.when(releases, |fx| {
                let result = fx.load(PTR, record, task::RESULT);
                let signature = fx.b.import_signature(super::record_signature());
                fx.b.ins().call_indirect(signature, release, &[result]);
            });
        }
    }
    lo.record_free(fx, record);
    unreference(lo, fx, scope_record);
}

/// `take h.join()`: waits until the child has ended, then yields its result, or fails with the
/// cancellation of its failure type when it did not return.
fn join(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let result_ty = supply.type_arg(0);
    let record = fx.load(PTR, supply.param(0).address(), 0);
    let scope_record = fx.load(PTR, record, task::OWNER);
    let returned = fx.load(PTR, scope_record, scope::RETURNED);
    let stale = fx.b.ins().icmp_imm_s(IntCC::NotEqual, returned, 0);
    fx.when(stale, |fx| report::raise_kind(lo, fx, "trap.stale-handle"));
    let lock = fx.offset(scope_record, scope::LOCK);
    let current = tasks::current_task(lo, fx);
    fx.repeat(&[], |fx, _| {
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let flags = fx.load(PTR, record, task::FLAGS);
        let done = fx.b.ins().band_imm_s(flags, flag::DONE);
        let waiting = fx.b.ins().icmp_imm_s(IntCC::Equal, done, 0);
        fx.when(waiting, |fx| fx.store(current, record, task::JOINER));
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        fx.when(waiting, |fx| {
            lo.call_routine(fx, Routine::Park, &[]);
        });
        (waiting, Vec::new())
    });
    let outcome = fx.load(PTR, record, task::OUTCOME);
    let child_returned = fx.b.ins().icmp_imm_s(IntCC::Equal, outcome, 1);
    let layout = lo.layout(&result_ty);
    let slot = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    fx.when(child_returned, |fx| {
        let result = fx.load(PTR, record, task::RESULT);
        let held = lo.held_at(fx, &result_ty, result);
        lo.store_held(fx, slot, held, &result_ty);
    });
    lo.record_free(fx, record);
    unreference(lo, fx, scope_record);
    let failed = fx.b.ins().bxor_imm_s(child_returned, 1);
    fx.when(failed, |fx| {
        let origin = super::origin(lo, fx);
        super::fail_cancelled(lo, fx, supply, origin);
    });
    let held = lo.held_at(fx, &result_ty, slot);
    supply.ret(lo, fx, held);
}

/// Releases a join handle: reaps its child, releasing its result once it has ended.
pub fn release_join(lo: &mut Lowering, fx: &mut Fx, address: Value, result: &Ty) {
    let record = fx.load(PTR, address, 0);
    let scope_record = fx.load(PTR, record, task::OWNER);
    let lock = fx.offset(scope_record, scope::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let flags = fx.load(PTR, record, task::FLAGS);
    let done = fx.b.ins().band_imm_s(flags, flag::DONE);
    let done = fx.b.ins().icmp_imm_s(IntCC::NotEqual, done, 0);
    let reaped = fx.b.ins().bor_imm_s(flags, flag::REAPED);
    let flags = fx.b.ins().select(done, flags, reaped);
    fx.store(flags, record, task::FLAGS);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    fx.when(done, |fx| {
        finish_child(lo, fx, record, scope_record, Some(result))
    });
}

/// `task::within(clock, ms, body: b)`: runs `b` in a region that a deadline cancels; `None`
/// when `b` fails with a cancellation of that region.
fn within(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let body_ty = fn_param(supply, 2);
    let clock = fx.load(PTR, supply.param(0).address(), 0);
    let ms = supply.param(1).scalar();
    let size = fx.ptr(region::SIZE);
    let record = lo.call_routine(fx, Routine::RegionOpen, &[size])[0];
    let timer = fx.slot(tables::deadline::SIZE, 8);
    let zero = fx.ptr(0);
    let timer_size = fx.ptr(tables::deadline::SIZE);
    lo.call_routine(fx, Routine::Fill, &[timer, zero, timer_size]);
    let deadline = fx.int(PTR, kind::DEADLINE);
    fx.store(deadline, timer, operation::KIND);
    fx.store(record, timer, operation::TASK);
    let live = tables::link_deadline(lo, fx, clock, timer);
    fx.choose_in(
        lo,
        live,
        &[],
        |lo, fx| {
            arm_deadline(lo, fx, timer, ms);
            Vec::new()
        },
        |lo, fx| {
            lo.call_routine(fx, Routine::Cancel, &[record]);
            Vec::new()
        },
    );
    let current = tasks::current_task(lo, fx);
    let enclosing = fx.load(PTR, current, task::REGION);
    fx.store(record, current, task::REGION);
    let called = lo.call_value(fx, &body_ty, supply.param(2).address(), &[]);
    fx.store(enclosing, current, task::REGION);
    fx.when(live, |fx| {
        tables::unlink_deadline(lo, fx, timer);
        disarm_deadline(lo, fx, timer);
    });
    let id = fx.load(PTR, record, region::ID);
    lo.record_free(fx, record);
    let option = supply.abi.result.clone();
    let layout = lo.layout(&option);
    let slot = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    let fails = body_ty.fails.clone();
    if let (Some(failed), Some(failure), Some(fails)) = (called.failed, called.failure, fails) {
        fx.when(failed, |fx| {
            let held = lo.held_at(fx, &fails, failure);
            let own = lo
                .cancellation_origin(fx, &fails, held)
                .map(|(is_cancellation, origin)| {
                    let same = fx.b.ins().icmp(IntCC::Equal, origin, id);
                    fx.b.ins().band(is_cancellation, same)
                })
                .unwrap_or_else(|| fx.int(types::I8, 0));
            fx.choose_in(
                lo,
                own,
                &[],
                |lo, fx| {
                    let none = fx.int(types::I8, 0);
                    lo.write_option(fx, &option, slot, none, Held::Empty);
                    let held = lo.held_at(fx, &option, slot);
                    supply.ret(lo, fx, held);
                    Vec::new()
                },
                |lo, fx| {
                    let target = supply.failure.expect("a failure address");
                    lo.store_held(fx, target, held, &fails);
                    supply.fail(fx);
                    Vec::new()
                },
            );
        });
    }
    let some = fx.int(types::I8, 1);
    lo.write_option(fx, &option, slot, some, called.result);
    let held = lo.held_at(fx, &option, slot);
    supply.ret(lo, fx, held);
}

/// Arms a deadline's timer of `ms` milliseconds, whose expiry cancels its region, first among
/// the deadlines the running task armed.
fn arm_deadline(lo: &mut Lowering, fx: &mut Fx, timer: Value, ms: Value) {
    let current = tasks::current_task(lo, fx);
    let armed = fx.load(PTR, current, task::DEADLINES);
    fx.store(armed, timer, operation::LINK);
    fx.store(timer, current, task::DEADLINES);
    let spec = fx.offset(timer, operation::SIZE);
    lo.arm_timer(fx, timer, ms, spec);
}

/// Cancels a deadline's timeout unless it has expired, waits for its completion, and takes it
/// out of the deadlines the running task armed, of which it is the first.
fn disarm_deadline(lo: &mut Lowering, fx: &mut Fx, timer: Value) {
    let current = tasks::current_task(lo, fx);
    let waiter = fx.offset(timer, operation::WAITER);
    fx.b.ins()
        .atomic_store(MemFlagsData::trusted(), current, waiter);
    lo.cancel_operation(fx, timer);
    tasks::wait_delivered(lo, fx, timer);
    let current = tasks::current_task(lo, fx);
    let armed = fx.load(PTR, timer, operation::LINK);
    fx.store(armed, current, task::DEADLINES);
}

/// `task::race(exec, left: l, right: r)`: the result or failure of the first of two tasks to
/// end, once both have ended.
fn race_call(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let result_ty = supply.type_arg(0);
    let fails = supply.type_arg(1);
    let task_ty = fn_param(supply, 1);
    let (live, cell) = super::executor(lo, fx, supply.param(0).address());
    let stale = fx.b.ins().bxor_imm_s(live, 1);
    fx.when(stale, |fx| {
        let none = fx.ptr(0);
        super::fail_cancelled(lo, fx, supply, none);
    });
    let size = fx.ptr(race::SIZE);
    let record = lo.call_routine(fx, Routine::RegionOpen, &[size])[0];
    let two = fx.ptr(2);
    fx.store(two, record, region::LIVE);
    let domain = tasks::current_domain(lo, fx);
    let end = race_end(lo);
    let done = race_done(lo);
    let mut tasks = Vec::new();
    for index in [1, 2] {
        let function = supply.param(index).address();
        let started = start_task(
            lo,
            fx,
            &task_ty,
            function,
            cell,
            domain,
            record,
            record,
            Some(end),
            done,
        );
        tasks.push(started);
    }
    for started in &tasks {
        tasks::spawned(lo, fx, *started);
    }
    wait_children(lo, fx, record, region::LIVE);
    let decided = fx.load(PTR, record, race::DECIDED);
    let other = fx.b.ins().icmp(IntCC::Equal, decided, tasks[0]);
    let other = fx.b.ins().select(other, tasks[1], tasks[0]);
    release_outcome(lo, fx, other, &result_ty, &fails);
    let outcome = fx.load(PTR, decided, task::OUTCOME);
    let returned = fx.b.ins().icmp_imm_s(IntCC::Equal, outcome, 1);
    let layout = lo.layout(&result_ty);
    let slot = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    let target = supply.failure.expect("a race's failure address");
    fx.choose_in(
        lo,
        returned,
        &[],
        |lo, fx| {
            let result = fx.load(PTR, decided, task::RESULT);
            let held = lo.held_at(fx, &result_ty, result);
            lo.store_held(fx, slot, held, &result_ty);
            Vec::new()
        },
        |lo, fx| {
            let failure = fx.load(PTR, decided, task::FAILURE);
            let held = lo.held_at(fx, &fails, failure);
            lo.store_held(fx, target, held, &fails);
            Vec::new()
        },
    );
    for started in &tasks {
        lo.record_free(fx, *started);
    }
    lo.record_free(fx, record);
    let failed = fx.b.ins().bxor_imm_s(returned, 1);
    fx.when(failed, |fx| supply.fail(fx));
    let held = lo.held_at(fx, &result_ty, slot);
    supply.ret(lo, fx, held);
}

/// Releases what an ended task's entry gave: its result or its failure.
fn release_outcome(lo: &mut Lowering, fx: &mut Fx, record: Value, result_ty: &Ty, fails: &Ty) {
    let outcome = fx.load(PTR, record, task::OUTCOME);
    let returned = fx.b.ins().icmp_imm_s(IntCC::Equal, outcome, 1);
    let (result_ty, fails) = (result_ty.clone(), fails.clone());
    fx.choose_in(
        lo,
        returned,
        &[],
        |lo, fx| {
            let result = fx.load(PTR, record, task::RESULT);
            lo.release(fx, &result_ty, result);
            Vec::new()
        },
        |lo, fx| {
            let failure = fx.load(PTR, record, task::FAILURE);
            lo.release(fx, &fails, failure);
            Vec::new()
        },
    );
}

/// The end of a race's task: the first to end decides the race, which then cancels.
fn race_end(lo: &mut Lowering) -> FuncId {
    lo.glue_function(Glue::RaceEnd, super::record_signature(), |lo, fx| {
        let params = fx.entry();
        let record = params[0];
        let owner = fx.load(PTR, record, task::OWNER);
        let decided = fx.offset(owner, race::DECIDED);
        let zero = fx.ptr(0);
        let previous =
            fx.b.ins()
                .atomic_cas(MemFlagsData::trusted(), decided, zero, record);
        let first = fx.b.ins().icmp_imm_s(IntCC::Equal, previous, 0);
        fx.when(first, |fx| {
            lo.call_routine(fx, Routine::Cancel, &[owner]);
        });
        fx.b.ins().return_(&[]);
    })
}

/// What the executor thread does once a race's task has ended: after the second, the race's
/// caller wakes.
fn race_done(lo: &mut Lowering) -> FuncId {
    lo.glue_function(Glue::RaceDone, super::record_signature(), |lo, fx| {
        let params = fx.entry();
        let record = params[0];
        let owner = fx.load(PTR, record, task::OWNER);
        let lock = fx.offset(owner, race::LOCK);
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let live = fx.load(PTR, owner, region::LIVE);
        let live = fx.b.ins().iadd_imm_s(live, -1);
        fx.store(live, owner, region::LIVE);
        let waiter = fx.load(PTR, owner, region::WAITER);
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        let last = fx.b.ins().icmp_imm_s(IntCC::Equal, live, 0);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, waiter, 0);
        let wakes = fx.b.ins().band(last, present);
        fx.when(wakes, |fx| {
            lo.call_routine(fx, Routine::Wake, &[waiter]);
        });
        fx.b.ins().return_(&[]);
    })
}
