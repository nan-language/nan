//! The simulated network: one loopback network per `sys::sim::net()`, whose listeners each take
//! the connections to their port on `localhost`, `127.0.0.1` or `::1`. A connection is two
//! endpoints, each buffering the bytes its peer wrote until they are read, and ending when its
//! peer closes it or abandons it.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::super::Lowering;
use super::super::super::body::Held;
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::tables::entry;
use super::super::super::runtime::{Routine, holdings};
use super::super::Supply;
use super::super::sys;
use super::{
    cancellation_point, enqueue, exhausted_unless, fail_cancelled, new_object, object, object_at,
    wait, wake,
};

/// The offsets of a network's fields: its first listener.
mod network {
    pub const LISTENERS: u64 = 8;
    pub const SIZE: u64 = 16;
}

/// The offsets of a listener's fields: its port, whether it is still held, the first and last
/// of the endpoints it has not accepted, the waiter slot of its accept, and the next listener
/// of its network.
mod listener {
    pub const PORT: u64 = 8;
    pub const HELD: u64 = 16;
    pub const FIRST: u64 = 24;
    pub const LAST: u64 = 32;
    pub const ACCEPTOR: u64 = 40;
    pub const NEXT: u64 = 48;
    pub const SIZE: u64 = 56;
}

/// The offsets of an endpoint's fields: its peer, the bytes its peer wrote and how many it has
/// read, how its peer ended, the waiter slot of its read, the next endpoint of a listener's,
/// its own index and whether its side is still held.
mod endpoint {
    pub const PEER: u64 = 8;
    pub const DATA: u64 = 16;
    pub const LEN: u64 = 24;
    pub const CAPACITY: u64 = 32;
    pub const READ: u64 = 40;
    pub const ENDED: u64 = 48;
    pub const READER: u64 = 56;
    pub const NEXT: u64 = 64;
    pub const INDEX: u64 = 72;
    pub const HELD: u64 = 80;
    pub const SIZE: u64 = 88;
}

/// How an endpoint's peer ended.
pub mod ended {
    pub const OPEN: i64 = 0;
    pub const CLOSED: i64 = 1;
    pub const RESET: i64 = 2;
}

/// The hosts a loopback network reaches.
const LOOPBACK: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

/// A new loopback network: its index.
pub fn new(lo: &mut Lowering, fx: &mut Fx) -> Value {
    new_object(lo, fx, object::NET, network::SIZE).0
}

/// The network an entry names.
fn of_entry(lo: &mut Lowering, fx: &mut Fx, record: Value) -> Value {
    let index = fx.load(PTR, record, entry::STATE);
    object_at(lo, fx, index)
}

/// The record of the listener or endpoint a resource at an address names.
fn held(lo: &mut Lowering, fx: &mut Fx, address: Value) -> Value {
    let holding = fx.offset(address, 8);
    let index = holdings::payload(fx, holding);
    object_at(lo, fx, index)
}

/// The listener of a network held on a port, zero when none.
fn listener_on(fx: &mut Fx, net: Value, port: Value) -> Value {
    let port = fx.b.ins().uextend(PTR, port);
    let first = fx.load(PTR, net, network::LISTENERS);
    fx.repeat(&[first], |fx, state| {
        let current = state[0];
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
        let found = fx.choose(
            present,
            &[types::I8],
            |fx| {
                let own = fx.load(PTR, current, listener::PORT);
                let same = fx.b.ins().icmp(IntCC::Equal, own, port);
                let held = fx.load(PTR, current, listener::HELD);
                let held = fx.b.ins().icmp_imm_s(IntCC::NotEqual, held, 0);
                vec![fx.b.ins().band(same, held)]
            },
            |fx| vec![fx.int(types::I8, 0)],
        )[0];
        let absent = fx.b.ins().bxor_imm_s(present, 1);
        let stop = fx.b.ins().bor(found, absent);
        let next = fx.choose(
            stop,
            &[PTR],
            |_| vec![current],
            |fx| vec![fx.load(PTR, current, listener::NEXT)],
        );
        (fx.b.ins().bxor_imm_s(stop, 1), next)
    })[0]
}

/// `sys::Net.listen`: a listener on a port no held listener of the network has.
pub fn listen(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, record: Value) {
    cancellation_point(lo, fx, supply);
    let port = supply.param(1).scalar();
    let zero_port = fx.b.ins().icmp_imm_s(IntCC::Equal, port, 0);
    fx.when(zero_port, |fx| sys::fail_with(lo, fx, supply, "Denied"));
    let net = of_entry(lo, fx, record);
    let taken = listener_on(fx, net, port);
    let taken = fx.b.ins().icmp_imm_s(IntCC::NotEqual, taken, 0);
    fx.when(taken, |fx| sys::fail_with(lo, fx, supply, "AddressInUse"));
    let (index, created) = new_object(lo, fx, object::LISTENER, listener::SIZE);
    let port = fx.b.ins().uextend(PTR, port);
    fx.store(port, created, listener::PORT);
    let one = fx.ptr(1);
    fx.store(one, created, listener::HELD);
    let first = fx.load(PTR, net, network::LISTENERS);
    fx.store(first, created, listener::NEXT);
    fx.store(created, net, network::LISTENERS);
    let handle = fx.load(PTR, supply.param(0).address(), 0);
    sys::ret_resource(lo, fx, supply, handle, index);
}

/// A new endpoint, held: its index and record.
fn new_endpoint(lo: &mut Lowering, fx: &mut Fx) -> (Value, Value) {
    let (index, record) = new_object(lo, fx, object::ENDPOINT, endpoint::SIZE);
    fx.store(index, record, endpoint::INDEX);
    let one = fx.ptr(1);
    fx.store(one, record, endpoint::HELD);
    (index, record)
}

/// `sys::Net.connect`: a connection to the listener on the port, whose accept takes the other
/// endpoint; a host outside the loopback network is unreachable and a port without a listener
/// refuses.
pub fn connect(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, record: Value) {
    cancellation_point(lo, fx, supply);
    let host = supply.param(1);
    let mut loopback = fx.int(types::I8, 0);
    for name in LOOPBACK {
        let known = lo.view_literal(fx, name.as_bytes());
        let same = lo.views_equal(fx, known, host);
        loopback = fx.b.ins().bor(loopback, same);
    }
    let elsewhere = fx.b.ins().bxor_imm_s(loopback, 1);
    fx.when(elsewhere, |fx| {
        sys::fail_with(lo, fx, supply, "Unreachable")
    });
    let net = of_entry(lo, fx, record);
    let target = listener_on(fx, net, supply.param(2).scalar());
    let refused = fx.b.ins().icmp_imm_s(IntCC::Equal, target, 0);
    fx.when(refused, |fx| sys::fail_with(lo, fx, supply, "Refused"));
    let (index, client) = new_endpoint(lo, fx);
    let (_, server) = new_endpoint(lo, fx);
    fx.store(server, client, endpoint::PEER);
    fx.store(client, server, endpoint::PEER);
    let last = fx.load(PTR, target, listener::LAST);
    let has_last = fx.b.ins().icmp_imm_s(IntCC::NotEqual, last, 0);
    fx.choose(
        has_last,
        &[],
        |fx| {
            fx.store(server, last, endpoint::NEXT);
            Vec::new()
        },
        |fx| {
            fx.store(server, target, listener::FIRST);
            Vec::new()
        },
    );
    fx.store(server, target, listener::LAST);
    let acceptor = fx.offset(target, listener::ACCEPTOR);
    wake(lo, fx, acceptor);
    let handle = fx.load(PTR, supply.param(0).address(), 0);
    sys::ret_resource(lo, fx, supply, handle, index);
}

/// `sys::net::Listener.accept`: the oldest connection to the listener it has not accepted,
/// waiting for one.
pub fn accept(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let address = supply.param(0).address();
    let record = held(lo, fx, address);
    let acceptor = fx.offset(record, listener::ACCEPTOR);
    fx.repeat(&[], |fx, _| {
        let first = fx.load(PTR, record, listener::FIRST);
        let none = fx.b.ins().icmp_imm_s(IntCC::Equal, first, 0);
        fx.when(none, |fx| {
            let cancelled = wait(lo, fx, Some(acceptor), |_, fx, op| {
                enqueue(fx, acceptor, op)
            });
            fx.when(cancelled, |fx| fail_cancelled(lo, fx, supply));
        });
        (none, Vec::new())
    });
    let first = fx.load(PTR, record, listener::FIRST);
    let next = fx.load(PTR, first, endpoint::NEXT);
    fx.store(next, record, listener::FIRST);
    let last = fx.b.ins().icmp_imm_s(IntCC::Equal, next, 0);
    fx.when(last, |fx| {
        let zero = fx.ptr(0);
        fx.store(zero, record, listener::LAST);
    });
    let index = fx.load(PTR, first, endpoint::INDEX);
    let handle = fx.load(PTR, address, 0);
    sys::ret_resource(lo, fx, supply, handle, index);
}

/// `sys::net::Conn.read`: the bytes its peer wrote that it has not read, as many as fit,
/// waiting for some; none once its peer has closed, and `Reset` once its peer has abandoned it.
pub fn read(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let record = held(lo, fx, supply.param(0).address());
    let reader = fx.offset(record, endpoint::READER);
    fx.repeat(&[], |fx, _| {
        let len = fx.load(PTR, record, endpoint::LEN);
        let read = fx.load(PTR, record, endpoint::READ);
        let buffered = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, len, read);
        let how = fx.load(PTR, record, endpoint::ENDED);
        let open = fx.b.ins().icmp_imm_s(IntCC::Equal, how, ended::OPEN);
        let reset = fx.b.ins().icmp_imm_s(IntCC::Equal, how, ended::RESET);
        let empty = fx.b.ins().bxor_imm_s(buffered, 1);
        let waits = fx.b.ins().band(empty, open);
        fx.when(waits, |fx| {
            let cancelled = wait(lo, fx, Some(reader), |_, fx, op| enqueue(fx, reader, op));
            fx.when(cancelled, |fx| fail_cancelled(lo, fx, supply));
        });
        let resets = fx.b.ins().band(empty, reset);
        fx.when(resets, |fx| sys::fail_with(lo, fx, supply, "Reset"));
        (waits, Vec::new())
    });
    let len = fx.load(PTR, record, endpoint::LEN);
    let read = fx.load(PTR, record, endpoint::READ);
    let left = fx.b.ins().isub(len, read);
    let (start, room) = sys::view(fx, supply.param(1));
    let count = fx.b.ins().umin(left, room);
    let data = fx.load(PTR, record, endpoint::DATA);
    let from = fx.b.ins().iadd(data, read);
    lo.call_routine(fx, Routine::Copy, &[start, from, count]);
    let read = fx.b.ins().iadd(read, count);
    let drained = fx.b.ins().icmp(IntCC::Equal, read, len);
    let zero = fx.ptr(0);
    let read = fx.b.ins().select(drained, zero, read);
    fx.store(read, record, endpoint::READ);
    let len = fx.b.ins().select(drained, zero, len);
    fx.store(len, record, endpoint::LEN);
    supply.ret(lo, fx, Held::Scalar(count));
}

/// `sys::net::Conn.write`: all of `data` enters its peer's buffer, waking its read; `Reset`
/// once its peer has ended.
pub fn write(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    cancellation_point(lo, fx, supply);
    let record = held(lo, fx, supply.param(0).address());
    let how = fx.load(PTR, record, endpoint::ENDED);
    let ended = fx.b.ins().icmp_imm_s(IntCC::NotEqual, how, ended::OPEN);
    fx.when(ended, |fx| sys::fail_with(lo, fx, supply, "Reset"));
    let peer = fx.load(PTR, record, endpoint::PEER);
    let (start, count) = sys::view(fx, supply.param(1));
    let len = fx.load(PTR, peer, endpoint::LEN);
    let needed = fx.b.ins().iadd(len, count);
    let capacity = fx.load(PTR, peer, endpoint::CAPACITY);
    let short =
        fx.b.ins()
            .icmp(IntCC::UnsignedGreaterThan, needed, capacity);
    fx.when(short, |fx| {
        let doubled = fx.b.ins().ishl_imm_s(capacity, 1);
        let grown = fx.b.ins().umax(doubled, needed);
        let data = fx.load(PTR, peer, endpoint::DATA);
        let moved = lo.record_resize(fx, data, grown);
        exhausted_unless(lo, fx, moved);
        fx.store(moved, peer, endpoint::DATA);
        fx.store(grown, peer, endpoint::CAPACITY);
    });
    let data = fx.load(PTR, peer, endpoint::DATA);
    let at = fx.b.ins().iadd(data, len);
    lo.call_routine(fx, Routine::Copy, &[at, start, count]);
    fx.store(needed, peer, endpoint::LEN);
    let reader = fx.offset(peer, endpoint::READER);
    wake(lo, fx, reader);
    supply.ret(lo, fx, Held::Scalar(count));
}

/// `sys::net::Conn.close`: its peer reads the bytes left, then nothing.
pub fn close(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    cancellation_point(lo, fx, supply);
    let record = held(lo, fx, supply.param(0).address());
    end(lo, fx, record, ended::CLOSED);
}

/// Ends an endpoint's side as `how`: its peer, unless already ended, ends that way and its
/// read wakes.
pub fn end(lo: &mut Lowering, fx: &mut Fx, record: Value, how: i64) {
    let held = fx.load(PTR, record, endpoint::HELD);
    let held = fx.b.ins().icmp_imm_s(IntCC::NotEqual, held, 0);
    fx.when(held, |fx| {
        let zero = fx.ptr(0);
        fx.store(zero, record, endpoint::HELD);
        let peer = fx.load(PTR, record, endpoint::PEER);
        let current = fx.load(PTR, peer, endpoint::ENDED);
        let open = fx.b.ins().icmp_imm_s(IntCC::Equal, current, ended::OPEN);
        fx.when(open, |fx| {
            let how = fx.ptr(how as u64);
            fx.store(how, peer, endpoint::ENDED);
            let reader = fx.offset(peer, endpoint::READER);
            wake(lo, fx, reader);
        });
    });
}

/// Releases a listener: its port is free again, and each connection it has not accepted is
/// reset.
pub fn close_listener(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let zero = fx.ptr(0);
    fx.store(zero, record, listener::HELD);
    let first = fx.load(PTR, record, listener::FIRST);
    fx.store(zero, record, listener::FIRST);
    fx.store(zero, record, listener::LAST);
    fx.repeat(&[first], |fx, state| {
        let current = state[0];
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
        let next = fx.choose_in(
            lo,
            present,
            &[PTR],
            |lo, fx| {
                let next = fx.load(PTR, current, endpoint::NEXT);
                end(lo, fx, current, ended::RESET);
                vec![next]
            },
            |_, fx| vec![fx.ptr(0)],
        );
        (present, next)
    });
}
