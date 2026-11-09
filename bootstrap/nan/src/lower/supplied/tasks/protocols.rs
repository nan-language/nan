//! Protocols: the calls of an outbox's operations and of its summary, the dispatch of a
//! component serving a protocol to its handlers with the replies of requests, and the
//! publication of its summary.
//!
//! A request's entry holds the address of its reply, a slot of the posting task's stack fitting
//! the operation's result and failure; the component task writes the one the handler gives there
//! before it marks the entry replied or failed and wakes the poster, or, once the handler
//! postponed the request, releases it and marks the entry postponed.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::super::abi::Abi;
use super::super::super::body::Held;
use super::super::super::call::{Called, Invoked};
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::Routine;
use super::super::super::{Instance, Lowering};
use super::super::{Supply, entry as supplied_entry};
use super::components::{self, Shape, entry, numbering, runner, status};
use crate::check::db::ItemId;
use crate::check::sig::{FnSig, SigKind};
use crate::check::ty::{Arg, Args, Ty};
use crate::nir::FnRef;

/// The protocol a message type is, when it is one.
pub fn served_protocol(lo: &Lowering, message: &Ty) -> Option<ItemId> {
    match message {
        Ty::Item(item, _) if lo.ck.is_protocol(*item) => Some(*item),
        _ => None,
    }
}

/// The summary type of the protocol a message type is, when it declares one.
pub fn summary_type(lo: &Lowering, message: &Ty) -> Option<Ty> {
    let protocol = served_protocol(lo, message)?;
    match &lo.ck.sig(protocol).kind {
        SigKind::Protocol { summary, .. } => summary.clone(),
        _ => None,
    }
}

/// The instance of a handler or the summary function of a component state, by name.
fn served(lo: &mut Lowering, state: &Ty, name: &str) -> Instance {
    let function = lo
        .ck
        .served_function(state, name)
        .expect("a function of a component serving a protocol");
    lo.callable_instance(&FnRef::item(function, Args::from(Vec::new())), false)
}

/// Passes the message in the slot `args[1]` to its operation's handler, with the state, the Env
/// and the component context of `args`, and gives a request's reply to the entry `node` that
/// posted it; a message delivered without an entry is never a request.
#[allow(clippy::too_many_arguments)]
pub fn dispatch(
    lo: &mut Lowering,
    fx: &mut Fx,
    record: Value,
    protocol: ItemId,
    state: &Ty,
    args: &[Held],
    node: Option<Value>,
) {
    let message = Ty::item(protocol, Vec::new());
    let sig = lo.ck.sig(protocol);
    let SigKind::Protocol { operations, .. } = &sig.kind else {
        return;
    };
    let slot = args[1].address();
    for (index, operation) in operations.iter().enumerate() {
        let is = lo.variant_is(fx, slot, &message, &operation.name);
        fx.when(is, |fx| {
            let instance = served(lo, state, &operation.name);
            let mut passed = vec![args[0]];
            for param in &operation.sig.params {
                let at = lo.payload_address(fx, &message, &operation.name, &param.name, slot);
                passed.push(Held::Address(at));
            }
            passed.extend([args[2], args[3]]);
            let abi = lo.abi(&instance);
            let id = lo.instance(instance);
            let called = lo.invoke(fx, Invoked::Direct(id), &abi, &passed);
            if let (true, Some(node)) = (lo.ck.is_request(protocol, index), node) {
                reply(lo, fx, record, node, &operation.sig, called);
            }
        });
    }
}

/// Gives a request's handler's result or failure to its poster, unless the poster has stopped,
/// withdrawing the entry the runner's record held as current, or the handler postponed the
/// request, in which case it is released and the poster told it was postponed.
fn reply(lo: &mut Lowering, fx: &mut Fx, record: Value, node: Value, head: &FnSig, called: Called) {
    let lock = fx.offset(record, runner::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let current = fx.load(PTR, record, runner::CURRENT);
    let live = fx.b.ins().icmp(IntCC::Equal, current, node);
    let zero = fx.ptr(0);
    fx.store(zero, record, runner::CURRENT);
    let postponed = fx.choose(
        live,
        &[types::I8],
        |fx| {
            let numbering = fx.load(PTR, node, entry::NUMBERING);
            vec![
                fx.b.ins()
                    .icmp_imm_s(IntCC::Equal, numbering, numbering::POSTPONED),
            ]
        },
        |fx| vec![fx.int(types::I8, 0)],
    )[0];
    let answered = fx.b.ins().bxor_imm_s(postponed, 1);
    let answered = fx.b.ins().band(live, answered);
    let give = |lo: &mut Lowering, fx: &mut Fx, ty: &Ty, held: Held, word: i64| {
        fx.choose_in(
            lo,
            answered,
            &[],
            |lo, fx| {
                let reply = fx.load(PTR, node, entry::REPLY);
                let poster = fx.load(PTR, node, entry::POSTER);
                lo.store_held(fx, reply, held, ty);
                components::give_posted(lo, fx, ty, reply, poster);
                let word = fx.ptr(word as u64);
                fx.store(word, node, entry::STATUS);
                lo.call_routine(fx, Routine::Wake, &[poster]);
                Vec::new()
            },
            |lo, fx| {
                if let Held::Address(address) = held {
                    lo.release(fx, ty, address);
                }
                Vec::new()
            },
        );
    };
    let fails = head.fails.clone().filter(|ty| *ty != Ty::Never);
    match (called.failed, called.failure, fails) {
        (Some(failed), Some(failure), Some(fails)) => {
            fx.choose_in(
                lo,
                failed,
                &[],
                |lo, fx| {
                    let held = lo.held_at(fx, &fails, failure);
                    give(lo, fx, &fails, held, status::FAILED);
                    Vec::new()
                },
                |lo, fx| {
                    give(lo, fx, &head.result, called.result, status::REPLIED);
                    Vec::new()
                },
            );
        }
        _ => give(lo, fx, &head.result, called.result, status::REPLIED),
    }
    fx.when(postponed, |fx| {
        let word = fx.ptr(status::POSTPONED as u64);
        fx.store(word, node, entry::STATUS);
        let poster = fx.load(PTR, node, entry::POSTER);
        lo.call_routine(fx, Routine::Wake, &[poster]);
    });
    lo.call_routine(fx, Routine::Unlock, &[lock]);
}

/// `mut cx.postpone()`: the number of the request whose handler runs, which it marks postponed,
/// none when the handler runs for no request its holder numbered or no runner is attached.
pub fn postpone(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let record = fx.load(PTR, supply.param(0).address(), 0);
    let lock = fx.offset(record, runner::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let current = fx.load(PTR, record, runner::CURRENT);
    let handling = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
    let attached = fx.load(PTR, record, runner::RUNNER);
    let attached = fx.b.ins().icmp_imm_s(IntCC::NotEqual, attached, 0);
    let postponable = fx.b.ins().band(handling, attached);
    let numbered_request = fx.choose(
        postponable,
        &[types::I8],
        |fx| {
            let numbering = fx.load(PTR, current, entry::NUMBERING);
            vec![fx.b.ins().icmp_imm_s(IntCC::NotEqual, numbering, 0)]
        },
        |fx| vec![fx.int(types::I8, 0)],
    )[0];
    let option = supply.abi.result.clone();
    fx.when(numbered_request, |fx| {
        let postponed = fx.ptr(numbering::POSTPONED as u64);
        fx.store(postponed, current, entry::NUMBERING);
        let number = fx.load(types::I64, current, entry::NUMBER);
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        let slot = fx.slot(8, 8);
        fx.store(number, slot, 0);
        let some = lo.variant_value(fx, &option, "Some", &[Held::Address(slot)], None);
        supply.ret(lo, fx, some);
    });
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    let none = lo.none_value(fx, &option);
    supply.ret(lo, fx, none);
}

/// Publishes the summary of a component whose protocol declares one: the value its summary
/// function gives for the state, kept in the runner's record.
pub fn publish(lo: &mut Lowering, fx: &mut Fx, record: Value, shape: &Shape, state: &Ty) {
    let Some((at, summary)) = &shape.summary_at else {
        return;
    };
    let instance = served(lo, state, "summary");
    let state_at = fx.offset(record, shape.state_at);
    let held = components::call_handler(lo, fx, instance, &[Held::Address(state_at)]);
    let target = fx.offset(record, *at);
    let lock = fx.offset(record, runner::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    lo.store_held(fx, target, held, summary);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
}

/// Where a runner's record holds the summary of a type.
pub fn summary_offset(lo: &mut Lowering, summary: &Ty) -> u64 {
    let layout = lo.layout(summary);
    runner::HEADER.next_multiple_of(layout.align.max(1))
}

/// Defines the call of an operation of a protocol, or of its summary, through an outbox.
pub fn call(lo: &mut Lowering, fx: &mut Fx, abi: &Abi, protocol: ItemId, method: usize) {
    let (sret, failure, params) = supplied_entry(fx, abi);
    let message = Ty::item(protocol, Vec::new());
    let name = lo.ck.method_name(protocol, method);
    let supply = Supply {
        abi,
        args: Args::from(vec![Arg::Ty(message.clone())]),
        path: format!("sys::Outbox.{name}"),
        owner: Some(lo.std_ty("sys", "Outbox", vec![message.clone()])),
        sret,
        failure,
        params,
        wrap: None,
    };
    if !lo.ck.is_operation(protocol, method) {
        read_summary(lo, fx, &supply, &message);
        return;
    }
    let held = lo.variant_value(fx, &message, &name, &supply.params[1..], None);
    if !lo.ck.is_request(protocol, method) {
        let (record, node) = components::enqueue(lo, fx, &supply, &message, held, None, None);
        components::await_taken(lo, fx, &supply, &message, record, node);
        return;
    }
    let sig = lo.ck.sig(protocol);
    let SigKind::Protocol { operations, .. } = &sig.kind else {
        return;
    };
    let head = &operations[method].sig;
    await_reply(
        lo,
        fx,
        &supply,
        &message,
        held,
        head,
        None,
        &|lo, fx, answer| match answer {
            Answer::Replied(result) => supply.ret(lo, fx, result),
            Answer::Failed(failure) => {
                let call_error = supply.abi.fails.clone().expect("a request's failure type");
                let value = lo.variant_value(fx, &call_error, "Failed", &[failure], None);
                let target = supply.failure.expect("a failure address");
                lo.store_held(fx, target, value, &call_error);
                supply.fail(fx);
            }
            Answer::Postponed => unreachable!("an unnumbered request is never postponed"),
        },
    );
}

/// How a request's handler answered its poster: with its result, with its failure, or by
/// postponing it.
pub enum Answer {
    Replied(Held),
    Failed(Held),
    Postponed,
}

/// Posts a request's message with a reply slot, numbered `number` when its holder serves it, then
/// waits until the handler has given its result or failure there or postponed it, passing
/// `answer` how it answered, until the component task has stopped or the message left the intake
/// untaken.
#[allow(clippy::too_many_arguments)]
pub fn await_reply(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    message: &Ty,
    held: Held,
    head: &FnSig,
    number: Option<Value>,
    answer: &dyn Fn(&mut Lowering, &mut Fx, Answer),
) {
    let fails = head.fails.clone().filter(|ty| *ty != Ty::Never);
    let result_layout = lo.layout(&head.result);
    let (size, align) = match &fails {
        Some(fails) => {
            let layout = lo.layout(fails);
            (
                result_layout.size.max(layout.size),
                result_layout.align.max(layout.align),
            )
        }
        None => (result_layout.size, result_layout.align),
    };
    let reply = fx.slot(size.max(1), align.max(1));
    let (record, node) = components::enqueue(lo, fx, supply, message, held, Some(reply), number);
    let lock = fx.offset(record, runner::LOCK);
    let at = fx.offset(node, entry::MESSAGE);
    fx.repeat(&[], |fx, _| {
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let current = fx.load(PTR, node, entry::STATUS);
        let replied =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, current, status::REPLIED);
        fx.when(replied, |fx| {
            components::leave_post(lo, fx, record);
            let result = lo.held_at(fx, &head.result, reply);
            answer(lo, fx, Answer::Replied(result));
        });
        if let Some(fails) = &fails {
            let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, current, status::FAILED);
            fx.when(failed, |fx| {
                components::leave_post(lo, fx, record);
                let failure = lo.held_at(fx, fails, reply);
                answer(lo, fx, Answer::Failed(failure));
            });
        }
        if number.is_some() {
            let postponed =
                fx.b.ins()
                    .icmp_imm_s(IntCC::Equal, current, status::POSTPONED);
            fx.when(postponed, |fx| {
                components::leave_post(lo, fx, record);
                answer(lo, fx, Answer::Postponed);
            });
        }
        let abandoned =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, current, status::ABANDONED);
        fx.when(abandoned, |fx| {
            components::leave_post(lo, fx, record);
            components::fail_closed(lo, fx, supply);
        });
        components::leave_untaken(lo, fx, supply, message, record, node, current, at);
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        lo.call_routine(fx, Routine::Park, &[]);
        (fx.int(types::I8, 1), Vec::new())
    });
    fx.b.ins()
        .trap(cranelift_codegen::ir::TrapCode::user(1).expect("a trap code"));
}

/// `out.summary()`: the summary the component published last, failing with `SendError.Closed`
/// when `out` is stale.
fn read_summary(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, message: &Ty) {
    let summary = summary_type(lo, message).expect("a protocol declaring a summary");
    let handle = fx.load(PTR, supply.param(0).address(), 0);
    let record = lo.call_routine(fx, Routine::OutboxClaim, &[handle])[0];
    let stale = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
    fx.when(stale, |fx| components::fail_closed(lo, fx, supply));
    let layout = lo.layout(&summary);
    let copy = fx.slot(layout.size.max(1), layout.align.max(1));
    let offset = summary_offset(lo, &summary);
    let lock = fx.offset(record, runner::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let source = fx.offset(record, offset);
    lo.copy(fx, copy, source, layout.size, layout.align);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    components::unreference(lo, fx, record);
    let held = lo.held_at(fx, &summary, copy);
    supply.ret(lo, fx, held);
}
