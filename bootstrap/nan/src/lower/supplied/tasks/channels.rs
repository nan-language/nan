//! Channels: a bounded queue of values between tasks, written through senders and read through
//! one receiver.
//!
//! A channel's record, in runtime memory, is its lock, its capacity, the number of values it
//! buffers and the position of the oldest, its unreleased senders, whether its receiver is
//! unreleased, the senders waiting for room, the receiver waiting for a value, its buffer, drawn
//! through the channel's allocator, and its reach. A sender or receiver is the holding of the
//! record. A waiting sender is a node on its stack: its task and the next waiting sender.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use cranelift_module::FuncId;

use super::super::super::body::Held;
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::regions::WAITING;
use super::super::super::runtime::{Routine, holdings, settle, tasks};
use super::super::super::transfer;
use super::super::super::{Glue, Lowering};
use super::super::Supply;
use super::super::mem::lists::fn_param;
use super::super::mem::vector::Element;
use crate::check::props::Property;
use crate::check::ty::Ty;

mod channel {
    pub const LOCK: u64 = 0;
    pub const CAPACITY: u64 = 8;
    pub const LEN: u64 = 16;
    pub const HEAD: u64 = 24;
    pub const SENDERS: u64 = 32;
    pub const RECEIVER: u64 = 40;
    pub const WAITING_SENDERS: u64 = 48;
    pub const WAITING_RECEIVER: u64 = 56;
    pub const BUFFER: u64 = 64;
    pub const REACH: u64 = 72;
    pub const SIZE: u64 = 80;
}

mod node {
    pub const TASK: u64 = 0;
    pub const NEXT: u64 = 8;
    pub const SIZE: u64 = 16;
}

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let element = supply.type_arg(0);
    match supply.path.as_str() {
        "task::Channel.bounded" | "task::Channel.try_bounded" => bounded(lo, fx, supply, &element),
        "task::Sender.split" => {
            let record = holdings::payload(fx, supply.param(0).address());
            let lock = fx.offset(record, channel::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let senders = fx.load(PTR, record, channel::SENDERS);
            let senders = fx.b.ins().iadd_imm_s(senders, 1);
            fx.store(senders, record, channel::SENDERS);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            let sret = supply.sret.expect("a sender's address");
            let holding = hold(lo, fx, record, holdings::kind::SENDER);
            fx.store(holding, sret, 0);
            supply.done(lo, fx);
        }
        "task::Sender.send" => send(lo, fx, supply, &element),
        "task::Receiver.receive" => receive(lo, fx, supply, &element),
        "task::Receiver.drain" => drain(lo, fx, supply, &element),
        other => panic!("no lowering of {other}"),
    }
}

/// Whether values of a type are linear, which channels never release.
fn linear(lo: &Lowering, ty: &Ty) -> bool {
    lo.ck.has(ty, Property::Linear)
}

/// `Channel.bounded(capacity, alloc)` and `try_bounded`.
fn bounded(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, element: &Ty) {
    let capacity = supply.param(0).scalar();
    let cell = fx.load(PTR, supply.param(1).address(), 0);
    let shape = Element::of(lo, element);
    let bytes = shape.bytes(fx, capacity);
    let one = fx.ptr(1);
    let bytes = fx.b.ins().umax(bytes, one);
    let align = fx.ptr(shape.align);
    let buffer = lo.call_routine(fx, Routine::Alloc, &[cell, bytes, align])[0];
    let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, buffer, 0);
    fx.when(failed, |fx| supply.exhausted(lo, fx, &[]));
    let size = fx.ptr(channel::SIZE);
    let record = lo.record_alloc(fx, size);
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
    fx.when(missing, |fx| {
        super::super::super::runtime::report::raise_kind(lo, fx, "trap.exhausted");
    });
    fx.store(capacity, record, channel::CAPACITY);
    fx.store(one, record, channel::SENDERS);
    fx.store(one, record, channel::RECEIVER);
    fx.store(buffer, record, channel::BUFFER);
    let reach = tasks::current_domain(lo, fx);
    fx.store(reach, record, channel::REACH);
    let channel_ty = supply.abi.result.clone();
    let sret = supply.sret.expect("a channel's address");
    for (field, kind) in [
        ("sender", holdings::kind::SENDER),
        ("receiver", holdings::kind::RECEIVER),
    ] {
        let at = lo.layouts.field(lo.ck, &channel_ty, None, field).offset;
        let holding = hold(lo, fx, record, kind);
        fx.store(holding, sret, at);
    }
    supply.done(lo, fx);
}

/// A new holding of a channel's record as a sender or its receiver.
fn hold(lo: &mut Lowering, fx: &mut Fx, record: Value, kind: i64) -> Value {
    let kind = fx.ptr(kind as u64);
    lo.call_routine(fx, Routine::Hold, &[kind, record])[0]
}

/// The address of the buffer slot at a position counted from the oldest value.
fn slot_at(fx: &mut Fx, record: Value, shape: Element, position: Value) -> Value {
    let head = fx.load(PTR, record, channel::HEAD);
    let capacity = fx.load(PTR, record, channel::CAPACITY);
    let index = fx.b.ins().iadd(head, position);
    let index = fx.b.ins().urem(index, capacity);
    let buffer = fx.load(PTR, record, channel::BUFFER);
    shape.at(fx, buffer, index)
}

/// Passes the holdings of a value just received to the receiving task's domain.
fn receive_into(lo: &mut Lowering, fx: &mut Fx, element: &Ty, address: Value) {
    if lo.transfers(element) {
        let none = fx.ptr(0);
        let domain = tasks::current_domain(lo, fx);
        lo.transfer(fx, element, address, none, domain);
    }
}

/// Removes the oldest value into an address, waking one waiting sender. The caller holds the
/// lock.
fn take_oldest(lo: &mut Lowering, fx: &mut Fx, record: Value, element: &Ty, target: Value) {
    let shape = Element::of(lo, element);
    let zero = fx.ptr(0);
    let oldest = slot_at(fx, record, shape, zero);
    let held = lo.held_at(fx, element, oldest);
    lo.store_held(fx, target, held, element);
    let head = fx.load(PTR, record, channel::HEAD);
    let capacity = fx.load(PTR, record, channel::CAPACITY);
    let head = fx.b.ins().iadd_imm_s(head, 1);
    let head = fx.b.ins().urem(head, capacity);
    fx.store(head, record, channel::HEAD);
    let len = fx.load(PTR, record, channel::LEN);
    let len = fx.b.ins().iadd_imm_s(len, -1);
    fx.store(len, record, channel::LEN);
    let sender = fx.load(PTR, record, channel::WAITING_SENDERS);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, sender, 0);
    fx.when(present, |fx| {
        let next = fx.load(PTR, sender, node::NEXT);
        fx.store(next, record, channel::WAITING_SENDERS);
        let task = fx.load(PTR, sender, node::TASK);
        lo.call_routine(fx, Routine::Wake, &[task]);
    });
}

/// Whether the running call is cancelled, with the origin a cancellation then holds.
fn cancelled(lo: &mut Lowering, fx: &mut Fx) -> (Value, Value) {
    let origin = super::origin(lo, fx);
    (fx.b.ins().icmp_imm_s(IntCC::NotEqual, origin, 0), origin)
}

/// `sender.send(v)`: appends `v`, waiting while the buffer is full; for droppable values a
/// cancellation point failing with `SendError.Closed` once the receiver is released. A value
/// in the buffer is owned by no domain, and its memory lies in chunks of the channel's reach.
fn send(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, element: &Ty) {
    let record = holdings::payload(fx, supply.param(0).address());
    let value = supply.param(1);
    let shape = Element::of(lo, element);
    let linear = linear(lo, element);
    if !linear {
        let (is, origin) = cancelled(lo, fx);
        fx.when(is, |fx| {
            if let Held::Address(address) = value {
                lo.release(fx, element, address);
            }
            super::fail_cancelled(lo, fx, supply, origin);
        });
    }
    let moves = lo.transfers(element);
    let domain = tasks::current_domain(lo, fx);
    if moves {
        let none = fx.ptr(0);
        lo.transfer(fx, element, value.address(), none, none);
    }
    let waiter = fx.slot(node::SIZE, 8);
    let current = tasks::current_task(lo, fx);
    fx.store(current, waiter, node::TASK);
    let lock = fx.offset(record, channel::LOCK);
    fx.repeat(&[domain], |fx, state| {
        let walked = state[0];
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let receiver = fx.load(PTR, record, channel::RECEIVER);
        let closed = fx.b.ins().icmp_imm_s(IntCC::Equal, receiver, 0);
        let len = fx.load(PTR, record, channel::LEN);
        let capacity = fx.load(PTR, record, channel::CAPACITY);
        let room = fx.b.ins().icmp(IntCC::UnsignedLessThan, len, capacity);
        if linear {
            fx.when(closed, |fx| {
                lo.call_routine(fx, Routine::Unlock, &[lock]);
                supply.done(lo, fx);
            });
        } else {
            fx.when(closed, |fx| {
                lo.call_routine(fx, Routine::Unlock, &[lock]);
                if let Held::Address(address) = value {
                    lo.release(fx, element, address);
                }
                fail_closed(lo, fx, supply);
            });
        }
        let reach = fx.load(PTR, record, channel::REACH);
        let placed = match moves {
            true => fx.b.ins().icmp(IntCC::Equal, reach, walked),
            false => fx.int(types::I8, 1),
        };
        let ready = fx.b.ins().band(room, placed);
        fx.when(ready, |fx| {
            let slot = slot_at(fx, record, shape, len);
            lo.store_held(fx, slot, value, element);
            let len = fx.b.ins().iadd_imm_s(len, 1);
            fx.store(len, record, channel::LEN);
            let receiving = fx.load(PTR, record, channel::WAITING_RECEIVER);
            let zero = fx.ptr(0);
            fx.store(zero, record, channel::WAITING_RECEIVER);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, receiving, 0);
            fx.when(present, |fx| {
                lo.call_routine(fx, Routine::Wake, &[receiving]);
            });
            supply.done(lo, fx);
        });
        let next = fx.choose_in(
            lo,
            room,
            &[PTR],
            |lo, fx| {
                lo.call_routine(fx, Routine::Unlock, &[lock]);
                if moves {
                    let keep = fx.int(PTR, transfer::KEEP);
                    lo.transfer(fx, element, value.address(), reach, keep);
                }
                vec![reach]
            },
            |lo, fx| {
                let head = fx.load(PTR, record, channel::WAITING_SENDERS);
                fx.store(head, waiter, node::NEXT);
                fx.store(waiter, record, channel::WAITING_SENDERS);
                if !linear {
                    tasks::waiting_on(lo, fx, WAITING);
                }
                lo.call_routine(fx, Routine::Unlock, &[lock]);
                let withdraw = withdraw_sender(lo);
                settle::enter_wait(lo, fx, withdraw, record, waiter);
                lo.call_routine(fx, Routine::Park, &[]);
                settle::leave_wait(lo, fx);
                if !linear {
                    tasks::waiting_on(lo, fx, 0);
                    let (is, origin) = cancelled(lo, fx);
                    fx.when(is, |fx| {
                        lo.call_routine(fx, Routine::Lock, &[lock]);
                        unlink(fx, record, waiter);
                        lo.call_routine(fx, Routine::Unlock, &[lock]);
                        if let Held::Address(address) = value {
                            lo.release(fx, element, address);
                        }
                        super::fail_cancelled(lo, fx, supply, origin);
                    });
                }
                lo.call_routine(fx, Routine::Lock, &[lock]);
                unlink(fx, record, waiter);
                lo.call_routine(fx, Routine::Unlock, &[lock]);
                vec![walked]
            },
        );
        let again = fx.int(types::I8, 1);
        (again, next)
    });
    fx.b.ins()
        .trap(cranelift_codegen::ir::TrapCode::user(1).expect("a trap code"));
}

/// Takes a stopped sender's node out of the list of its channel's waiting senders.
fn withdraw_sender(lo: &mut Lowering) -> FuncId {
    lo.glue_function(
        Glue::WithdrawSender,
        settle::withdraw_signature(),
        |lo, fx| {
            let params = fx.entry();
            let record = fx.load(PTR, params[0], settle::wait::OBJECT);
            let waiter = fx.load(PTR, params[0], settle::wait::NODE);
            let lock = fx.offset(record, channel::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            unlink(fx, record, waiter);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            fx.b.ins().return_(&[]);
        },
    )
}

/// Ends a stopped receiver's wait for a value in its channel.
fn withdraw_receiver(lo: &mut Lowering) -> FuncId {
    lo.glue_function(
        Glue::WithdrawReceiver,
        settle::withdraw_signature(),
        |lo, fx| {
            let params = fx.entry();
            let record = fx.load(PTR, params[0], settle::wait::OBJECT);
            let lock = fx.offset(record, channel::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let receiving = fx.load(PTR, record, channel::WAITING_RECEIVER);
            let stopped = fx.b.ins().icmp(IntCC::Equal, receiving, params[1]);
            fx.when(stopped, |fx| {
                let zero = fx.ptr(0);
                fx.store(zero, record, channel::WAITING_RECEIVER);
            });
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            fx.b.ins().return_(&[]);
        },
    )
}

/// Removes a waiting sender's node from its channel's list when it is still there. The caller
/// holds the lock.
fn unlink(fx: &mut Fx, record: Value, waiter: Value) {
    let head = fx.load(PTR, record, channel::WAITING_SENDERS);
    let first = fx.b.ins().icmp(IntCC::Equal, head, waiter);
    fx.choose(
        first,
        &[],
        |fx| {
            let next = fx.load(PTR, waiter, node::NEXT);
            fx.store(next, record, channel::WAITING_SENDERS);
            Vec::new()
        },
        |fx| {
            fx.repeat(&[head], |fx, state| {
                let current = state[0];
                let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
                let next = fx.choose(
                    present,
                    &[PTR],
                    |fx| {
                        let next = fx.load(PTR, current, node::NEXT);
                        let found = fx.b.ins().icmp(IntCC::Equal, next, waiter);
                        fx.when(found, |fx| {
                            let after = fx.load(PTR, waiter, node::NEXT);
                            fx.store(after, current, node::NEXT);
                        });
                        let zero = fx.ptr(0);
                        vec![fx.b.ins().select(found, zero, next)]
                    },
                    |fx| vec![fx.ptr(0)],
                );
                let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next[0], 0);
                (more, next)
            });
            Vec::new()
        },
    );
}

/// Fails a send with `SendError.Closed`.
fn fail_closed(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let fails = supply.abi.fails.clone().expect("a send's failure type");
    let value = lo.variant_value(fx, &fails, "Closed", &[], false);
    let failure = supply.failure.expect("a failure address");
    lo.store_held(fx, failure, value, &fails);
    supply.fail(fx);
}

/// `receiver.receive()`: the oldest value, waiting while none is buffered and a sender is
/// unreleased; `None` once every sender has been released. A cancellation point.
fn receive(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, element: &Ty) {
    let record = holdings::payload(fx, supply.param(0).address());
    let (is, origin) = cancelled(lo, fx);
    fx.when(is, |fx| super::fail_cancelled(lo, fx, supply, origin));
    let option = supply.abi.result.clone();
    let layout = lo.layout(&option);
    let result = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    let lock = fx.offset(record, channel::LOCK);
    let current = tasks::current_task(lo, fx);
    fx.repeat(&[], |fx, _| {
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let len = fx.load(PTR, record, channel::LEN);
        let buffered = fx.b.ins().icmp_imm_s(IntCC::NotEqual, len, 0);
        fx.when(buffered, |fx| {
            let element = element.clone();
            lo.write_option_with(fx, &option, result, buffered, |lo, fx, payload| {
                take_oldest(lo, fx, record, &element, payload);
                receive_into(lo, fx, &element, payload);
            });
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            let held = lo.held_at(fx, &option, result);
            supply.ret(lo, fx, held);
        });
        let senders = fx.load(PTR, record, channel::SENDERS);
        let ended = fx.b.ins().icmp_imm_s(IntCC::Equal, senders, 0);
        fx.when(ended, |fx| {
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            let none = lo.none_value(fx, &option);
            lo.store_held(fx, result, none, &option);
            let held = lo.held_at(fx, &option, result);
            supply.ret(lo, fx, held);
        });
        fx.store(current, record, channel::WAITING_RECEIVER);
        tasks::waiting_on(lo, fx, WAITING);
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        let withdraw = withdraw_receiver(lo);
        settle::enter_wait(lo, fx, withdraw, record, current);
        lo.call_routine(fx, Routine::Park, &[]);
        settle::leave_wait(lo, fx);
        tasks::waiting_on(lo, fx, 0);
        let (is, origin) = cancelled(lo, fx);
        fx.when(is, |fx| {
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let zero = fx.ptr(0);
            fx.store(zero, record, channel::WAITING_RECEIVER);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            super::fail_cancelled(lo, fx, supply, origin);
        });
        let again = fx.int(types::I8, 1);
        (again, Vec::new())
    });
    fx.b.ins()
        .trap(cranelift_codegen::ir::TrapCode::user(1).expect("a trap code"));
}

/// `take receiver.drain(visit: f)`: passes every value, oldest first, to `f` until every sender
/// has been released, then releases the receiver.
fn drain(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, element: &Ty) {
    let receiver = supply.param(0).address();
    let record = holdings::payload(fx, receiver);
    let visit = supply.param(1).address();
    let visit_ty = fn_param(supply, 1);
    let lock = fx.offset(record, channel::LOCK);
    let current = tasks::current_task(lo, fx);
    let layout = lo.layout(element);
    let value = fx.slot(layout.size, layout.align);
    fx.repeat(&[], |fx, _| {
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let len = fx.load(PTR, record, channel::LEN);
        let buffered = fx.b.ins().icmp_imm_s(IntCC::NotEqual, len, 0);
        let senders = fx.load(PTR, record, channel::SENDERS);
        let ended = fx.b.ins().icmp_imm_s(IntCC::Equal, senders, 0);
        let element = element.clone();
        let again = fx.choose_in(
            lo,
            buffered,
            &[types::I8],
            |lo, fx| {
                take_oldest(lo, fx, record, &element, value);
                lo.call_routine(fx, Routine::Unlock, &[lock]);
                receive_into(lo, fx, &element, value);
                let held = lo.held_at(fx, &element, value);
                lo.call_value(fx, &visit_ty, visit, &[held]);
                vec![fx.int(types::I8, 1)]
            },
            |lo, fx| {
                fx.choose_in(
                    lo,
                    ended,
                    &[types::I8],
                    |lo, fx| {
                        let zero = fx.ptr(0);
                        fx.store(zero, record, channel::RECEIVER);
                        lo.call_routine(fx, Routine::Unlock, &[lock]);
                        let holding = fx.load(PTR, receiver, 0);
                        lo.call_routine(fx, Routine::Unhold, &[holding]);
                        free_channel(lo, fx, record);
                        vec![fx.int(types::I8, 0)]
                    },
                    |lo, fx| {
                        fx.store(current, record, channel::WAITING_RECEIVER);
                        lo.call_routine(fx, Routine::Unlock, &[lock]);
                        let withdraw = withdraw_receiver(lo);
                        settle::enter_wait(lo, fx, withdraw, record, current);
                        lo.call_routine(fx, Routine::Park, &[]);
                        settle::leave_wait(lo, fx);
                        vec![fx.int(types::I8, 1)]
                    },
                )
            },
        )[0];
        (again, Vec::new())
    });
    supply.done(lo, fx);
}

/// Frees a channel's buffer and record once no handle of it is unreleased.
fn free_channel(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let buffer = fx.load(PTR, record, channel::BUFFER);
    lo.call_routine(fx, Routine::Free, &[buffer]);
    lo.record_free(fx, record);
}

/// Releases a sender or a receiver: a receiver of droppable values releases those it buffers
/// and lets waiting senders fail; the last handle frees the channel.
pub fn release(lo: &mut Lowering, fx: &mut Fx, path: &str, ty: &Ty, address: Value) {
    let element = ty.type_arg(0).cloned().expect("a channel's value type");
    let holding = fx.load(PTR, address, 0);
    let record = fx.load(PTR, holding, holdings::holding::PAYLOAD);
    lo.call_routine(fx, Routine::Unhold, &[holding]);
    end(lo, fx, record, path == "task::Sender", |lo, fx| {
        let zero = fx.ptr(0);
        if lo.needs_release(&element) {
            let shape = Element::of(lo, &element);
            let len = fx.load(PTR, record, channel::LEN);
            fx.each(zero, len, |fx, position| {
                let slot = slot_at(fx, record, shape, position);
                lo.release(fx, &element, slot);
            });
        }
        fx.store(zero, record, channel::LEN);
    });
}

/// Counts a sender or the receiver of a channel as released, as settlement does, releasing
/// none of the values it buffers; the last handle frees the channel.
pub fn settle_end(lo: &mut Lowering, fx: &mut Fx, record: Value, sender: bool) {
    end(lo, fx, record, sender, |_, _| {});
}

/// Ends a sender or the receiver of a channel: the last sender wakes the waiting receiver, and
/// the receiver, once `buffered` has run on its buffer, wakes every waiting sender; the last
/// handle frees the channel.
fn end(
    lo: &mut Lowering,
    fx: &mut Fx,
    record: Value,
    sender: bool,
    buffered: impl FnOnce(&mut Lowering, &mut Fx),
) {
    let lock = fx.offset(record, channel::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let woken = if sender {
        let senders = fx.load(PTR, record, channel::SENDERS);
        let senders = fx.b.ins().iadd_imm_s(senders, -1);
        fx.store(senders, record, channel::SENDERS);
        let last = fx.b.ins().icmp_imm_s(IntCC::Equal, senders, 0);
        let receiving = fx.load(PTR, record, channel::WAITING_RECEIVER);
        let zero = fx.ptr(0);
        Some(fx.b.ins().select(last, receiving, zero))
    } else {
        let zero = fx.ptr(0);
        fx.store(zero, record, channel::RECEIVER);
        buffered(lo, fx);
        let waiting = fx.load(PTR, record, channel::WAITING_SENDERS);
        fx.store(zero, record, channel::WAITING_SENDERS);
        fx.repeat(&[waiting], |fx, state| {
            let current = state[0];
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
            let next = fx.choose_in(
                lo,
                present,
                &[PTR],
                |lo, fx| {
                    let next = fx.load(PTR, current, node::NEXT);
                    let task = fx.load(PTR, current, node::TASK);
                    lo.call_routine(fx, Routine::Wake, &[task]);
                    vec![next]
                },
                |_, fx| vec![fx.ptr(0)],
            );
            (present, next)
        });
        None
    };
    let senders = fx.load(PTR, record, channel::SENDERS);
    let receiver = fx.load(PTR, record, channel::RECEIVER);
    let handles = fx.b.ins().bor(senders, receiver);
    let unused = fx.b.ins().icmp_imm_s(IntCC::Equal, handles, 0);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    if let Some(woken) = woken {
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, woken, 0);
        fx.when(present, |fx| {
            lo.call_routine(fx, Routine::Wake, &[woken]);
        });
    }
    fx.when(unused, |fx| free_channel(lo, fx, record));
}

/// Moves a sender or receiver into another domain's reach: its holding passes to `owner`, and
/// when the channel's reach widens its buffer follows, with the values buffered when the
/// receiver moves, which the walk reads from a copy taken under the lock.
pub fn transfer(
    lo: &mut Lowering,
    fx: &mut Fx,
    path: &str,
    ty: &Ty,
    address: Value,
    lift: Value,
    owner: Value,
) {
    transfer::rehome(lo, fx, address, owner);
    let element = ty.type_arg(0).cloned().expect("a channel's value type");
    let buffered = path == "task::Receiver" && lo.transfers(&element);
    let lifting = fx.b.ins().icmp_imm_s(IntCC::NotEqual, lift, 0);
    fx.when(lifting, |fx| {
        let record = holdings::payload(fx, address);
        let lock = fx.offset(record, channel::LOCK);
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let reach = fx.load(PTR, record, channel::REACH);
        let widened = tasks::nested_in(fx, reach, lift);
        fx.when(widened, |fx| fx.store(lift, record, channel::REACH));
        let shape = Element::of(lo, &element);
        let copy = match buffered {
            true => {
                let len = fx.load(PTR, record, channel::LEN);
                let zero = fx.ptr(0);
                let len = fx.b.ins().select(widened, len, zero);
                let bytes = shape.bytes(fx, len);
                let one = fx.ptr(1);
                let bytes = fx.b.ins().umax(bytes, one);
                let copy = lo.record_alloc(fx, bytes);
                let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, copy, 0);
                fx.when(missing, |fx| {
                    super::super::super::runtime::report::raise_kind(lo, fx, "trap.exhausted");
                });
                fx.each(zero, len, |fx, position| {
                    let from = slot_at(fx, record, shape, position);
                    let to = shape.at(fx, copy, position);
                    lo.copy(fx, to, from, shape.size, shape.align);
                });
                Some((copy, len))
            }
            false => None,
        };
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        fx.when(widened, |fx| {
            let buffer = fx.load(PTR, record, channel::BUFFER);
            transfer::lift(lo, fx, buffer, lift);
        });
        if let Some((copy, len)) = copy {
            let keep = fx.int(PTR, transfer::KEEP);
            let zero = fx.ptr(0);
            fx.each(zero, len, |fx, position| {
                let at = shape.at(fx, copy, position);
                lo.transfer(fx, &element, at, lift, keep);
            });
            lo.record_free(fx, copy);
        }
    });
}
