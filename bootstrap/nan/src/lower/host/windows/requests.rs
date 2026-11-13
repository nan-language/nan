//! Requests: the operations a task runs on a handle and parks on until they complete, through
//! the completion port for a handle opened for overlapped I/O, and otherwise on a thread of their
//! own, which posts their completion to the port.
//!
//! A request lives in the frame of the `Request` routine running it: the `OVERLAPPED` the system
//! completes it through, then the operation record its completion reaches, whose `WAITER` field
//! holds the request's handle and whose `LINK` field the thread running it, zero for one through
//! the completion port.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, condcodes::IntCC, types};

use super::super::super::Lowering;
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::regions::origin;
use super::super::super::runtime::tasks::{self, operation, task};
use super::super::super::runtime::{Routine, state};
use super::{KEY_REQUEST, define_callback, function_address, post, spawn_thread};

/// The offsets of a request's fields after its `OVERLAPPED`: its operation record, the count a
/// call starting it writes, and for a transfer run on a thread of its own, its buffer, length
/// and kind, how far that thread has gone and the count or negated error code it gives.
pub const OP: u64 = 32;
const COUNT: u64 = OP + operation::SIZE;
const BUFFER: u64 = COUNT + 8;
const LEN: u64 = BUFFER + 8;
const KIND: u64 = LEN + 8;
const STAGE: u64 = KIND + 8;
const OUTCOME: u64 = STAGE + 8;
const SIZE: u64 = OUTCOME + 8;

/// The kinds of requests: a transfer, a read without `WRITE` and through the completion port
/// without `BLOCKING`, an accept and a connect.
pub mod kind {
    pub const WRITE: i64 = 1;
    pub const BLOCKING: i64 = 2;
    pub const ACCEPT: i64 = 4;
    pub const CONNECT: i64 = 5;
}

/// The stages of a transfer's own thread once it has started: calling the system, then
/// finished.
const CALLING: i64 = 1;
const FINISHED: i64 = 2;

/// How many times a cancellation asks the system to cancel a transfer its thread is calling.
const ATTEMPTS: i64 = 64;

/// The system error codes of an operation left pending and of one cancelled.
const IO_PENDING: i64 = 997;
const OPERATION_ABORTED: i64 = 995;

/// The bytes `AcceptEx` writes for each of a connection's addresses: an IPv6 address's and 16
/// more.
const ACCEPT_ADDRESS: i64 = 44;

/// `Request(kind, handle, a, b, c) -> (refused, result)`: runs a request of the running task on
/// `handle` and parks until it completes. A transfer calls `ReadFile` or `WriteFile` of
/// `b` bytes at `a`, through the completion port or, with `BLOCKING`, on a thread of its own; an
/// `ACCEPT` calls the `AcceptEx` at `a`, accepting into the socket `b` and writing the
/// connection's addresses at `c`; a `CONNECT` calls the `ConnectEx` at `a`, connecting to the
/// address of `c` bytes at `b`. Gives the count transferred, zero for an accept or a connect, or
/// the negated system error code, and whether the task's cancellation refused the request.
pub fn define_request(lo: &mut Lowering, fx: &mut Fx) {
    let params = fx.entry();
    let (kind, handle, a, b, c) = (params[0], params[1], params[2], params[3], params[4]);
    let record = tasks::current_task(lo, fx);
    let region = fx.load(PTR, record, task::REGION);
    let early = origin(fx, region);
    let early = fx.b.ins().icmp_imm_s(IntCC::NotEqual, early, 0);
    fx.when(early, |fx| {
        let yes = fx.int(types::I8, 1);
        let zero = fx.int(types::I64, 0);
        fx.b.ins().return_(&[yes, zero]);
    });
    let request = fx.slot(SIZE, 16);
    let zero = fx.ptr(0);
    let size = fx.ptr(SIZE);
    lo.call_routine(fx, Routine::Fill, &[request, zero, size]);
    let op = fx.b.ins().iadd_imm_s(request, OP as i64);
    fx.store(record, op, operation::TASK);
    fx.store(handle, op, operation::WAITER);
    let transfer =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedLessThan, kind, kind::ACCEPT);
    let blocking = fx.b.ins().band_imm_s(kind, kind::BLOCKING);
    let blocking = fx.b.ins().icmp_imm_s(IntCC::NotEqual, blocking, 0);
    let started = fx.choose_in(
        lo,
        transfer,
        &[types::I8, types::I64],
        |lo, fx| {
            fx.choose_in(
                lo,
                blocking,
                &[types::I8, types::I64],
                |lo, fx| start_blocking(lo, fx, request, kind, (a, b)),
                |lo, fx| {
                    let writes = fx.b.ins().band_imm_s(kind, kind::WRITE);
                    let writes = fx.b.ins().icmp_imm_s(IntCC::NotEqual, writes, 0);
                    let len = fx.b.ins().ireduce(types::I32, b);
                    let none = fx.ptr(0);
                    let args = [handle, a, len, none, request];
                    let called = fx.choose_in(
                        lo,
                        writes,
                        &[types::I32],
                        |lo, fx| lo.call_system(fx, "WriteFile", &args, &[types::I32]),
                        |lo, fx| lo.call_system(fx, "ReadFile", &args, &[types::I32]),
                    )[0];
                    pending(lo, fx, called)
                },
            )
        },
        |lo, fx| {
            let accepts = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, kind::ACCEPT);
            let count = fx.b.ins().iadd_imm_s(request, COUNT as i64);
            let none = fx.ptr(0);
            let called = fx.choose_in(
                lo,
                accepts,
                &[types::I32],
                |lo, fx| {
                    let address = fx.ptr(ACCEPT_ADDRESS as u64);
                    let args = [handle, b, c, none, address, address, count, request];
                    vec![lo.call_address(fx, a, &args, types::I32)]
                },
                |lo, fx| {
                    let args = [handle, b, c, none, none, count, request];
                    vec![lo.call_address(fx, a, &args, types::I32)]
                },
            )[0];
            pending(lo, fx, called)
        },
    );
    let refused = fx.b.ins().bxor_imm_s(started[0], 1);
    fx.when(refused, |fx| {
        let no = fx.int(types::I8, 0);
        fx.b.ins().return_(&[no, started[1]]);
    });
    let word = fx.offset(record, task::IO);
    fx.atomic_store(op, word);
    let cancelled = origin(fx, region);
    let cancelled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, cancelled, 0);
    fx.when(cancelled, |fx| lo.cancel_operation(fx, op));
    tasks::wait_delivered(lo, fx, op);
    tasks::release_io(lo, fx, word, op);
    let result = fx.choose_in(
        lo,
        blocking,
        &[types::I64],
        |lo, fx| {
            let thread = fx.load(PTR, op, operation::LINK);
            lo.call_system(fx, "CloseHandle", &[thread], &[types::I32]);
            vec![fx.load(types::I64, request, OUTCOME)]
        },
        |lo, fx| {
            let count = fx.b.ins().iadd_imm_s(request, COUNT as i64);
            let wait = fx.int(types::I32, 0);
            let done = lo.call_system(
                fx,
                "GetOverlappedResult",
                &[handle, request, count, wait],
                &[types::I32],
            )[0];
            vec![outcome(lo, fx, done, count)]
        },
    )[0];
    let aborted =
        fx.b.ins()
            .icmp_imm_s(IntCC::Equal, result, -OPERATION_ABORTED);
    let cancelled = origin(fx, region);
    let cancelled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, cancelled, 0);
    let refused = fx.b.ins().band(aborted, cancelled);
    fx.b.ins().return_(&[refused, result]);
}

/// Whether a call starting a request left it to complete through the completion port, true
/// when it succeeded or left it pending, and otherwise the negated error code it gives.
fn pending(lo: &mut Lowering, fx: &mut Fx, called: Value) -> Vec<Value> {
    let succeeded = fx.b.ins().icmp_imm_s(IntCC::NotEqual, called, 0);
    fx.choose_in(
        lo,
        succeeded,
        &[types::I8, types::I64],
        |_, fx| vec![fx.int(types::I8, 1), fx.int(types::I64, 0)],
        |lo, fx| {
            let error = last_error(lo, fx);
            let left = fx.b.ins().icmp_imm_s(IntCC::Equal, error, -IO_PENDING);
            vec![left, error]
        },
    )
}

/// The negated error code of the calling thread's last failed call.
fn last_error(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let error = lo.call_system(fx, "GetLastError", &[], &[types::I32])[0];
    let error = fx.b.ins().uextend(types::I64, error);
    fx.b.ins().ineg(error)
}

/// The outcome of a call that succeeded when `done` is nonzero with the 32-bit count at
/// `count`: that count, or the negated error code it gives otherwise.
fn outcome(lo: &mut Lowering, fx: &mut Fx, done: Value, count: Value) -> Value {
    let succeeded = fx.b.ins().icmp_imm_s(IntCC::NotEqual, done, 0);
    fx.choose_in(
        lo,
        succeeded,
        &[types::I64],
        |_, fx| {
            let count = fx.load(types::I32, count, 0);
            vec![fx.b.ins().uextend(types::I64, count)]
        },
        |lo, fx| vec![last_error(lo, fx)],
    )[0]
}

/// Starts a transfer of `len` bytes at `buffer` on a thread of its own, which records itself in
/// the request's operation: whether it started, and otherwise the negated error code.
fn start_blocking(
    lo: &mut Lowering,
    fx: &mut Fx,
    request: Value,
    kind: Value,
    (buffer, len): (Value, Value),
) -> Vec<Value> {
    fx.store(buffer, request, BUFFER);
    fx.store(len, request, LEN);
    fx.store(kind, request, KIND);
    let start = transfer_thread(lo);
    let start = function_address(lo, fx, start);
    let thread = spawn_thread(lo, fx, start, request);
    let created = fx.b.ins().icmp_imm_s(IntCC::NotEqual, thread, 0);
    fx.choose_in(
        lo,
        created,
        &[types::I8, types::I64],
        |_, fx| {
            fx.store(thread, request, OP + operation::LINK);
            vec![fx.int(types::I8, 1), fx.int(types::I64, 0)]
        },
        |lo, fx| vec![fx.int(types::I8, 0), last_error(lo, fx)],
    )
}

/// The thread running a transfer: unless its cancellation was asked before it calls the
/// system, calls `ReadFile` or `WriteFile` synchronously, records the count or the negated error
/// code, and posts the request's completion.
fn transfer_thread(lo: &mut Lowering) -> cranelift_module::FuncId {
    define_callback(lo, "_NSstack_switch_transfer", 1, |lo, fx, params| {
        let request = params[0];
        let stage = fx.b.ins().iadd_imm_s(request, STAGE as i64);
        let calling = fx.ptr(CALLING as u64);
        fx.atomic_store(calling, stage);
        let cancel =
            fx.b.ins()
                .iadd_imm_s(request, (OP + operation::CANCEL) as i64);
        let asked = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), cancel);
        let asked = fx.b.ins().icmp_imm_s(IntCC::NotEqual, asked, 0);
        let result = fx.choose_in(
            lo,
            asked,
            &[types::I64],
            |_, fx| vec![fx.int(types::I64, -OPERATION_ABORTED)],
            |lo, fx| {
                let handle = fx.load(PTR, request, OP + operation::WAITER);
                let buffer = fx.load(PTR, request, BUFFER);
                let len = fx.load(types::I32, request, LEN);
                let kind = fx.load(PTR, request, KIND);
                let count = fx.b.ins().iadd_imm_s(request, COUNT as i64);
                let none = fx.ptr(0);
                let args = [handle, buffer, len, count, none];
                let writes = fx.b.ins().band_imm_s(kind, kind::WRITE);
                let done = fx.choose_in(
                    lo,
                    writes,
                    &[types::I32],
                    |lo, fx| lo.call_system(fx, "WriteFile", &args, &[types::I32]),
                    |lo, fx| lo.call_system(fx, "ReadFile", &args, &[types::I32]),
                )[0];
                vec![outcome(lo, fx, done, count)]
            },
        )[0];
        fx.store(result, request, OUTCOME);
        let finished = fx.ptr(FINISHED as u64);
        fx.atomic_store(finished, stage);
        post(lo, fx, KEY_REQUEST, request);
        let zero = fx.int(types::I32, 0);
        fx.b.ins().return_(&[zero]);
    })
}

/// Asks once for the cancellation of a request in flight, whose record stays valid during the
/// call: one through the completion port is cancelled by `CancelIoEx`; one on a thread of its
/// own sees the cancellation before calling the system, or has its call cancelled by
/// `CancelSynchronousIo`, asked again while the thread is about to call, at most `ATTEMPTS`
/// times. A call the system cannot cancel completes as the system completes it.
pub fn cancel_request(lo: &mut Lowering, fx: &mut Fx, op: Value) {
    let thread = fx.load(PTR, op, operation::LINK);
    let request = fx.b.ins().iadd_imm_s(op, -(OP as i64));
    let alone = fx.b.ins().icmp_imm_s(IntCC::NotEqual, thread, 0);
    fx.choose_in(
        lo,
        alone,
        &[],
        |lo, fx| {
            let cancel = fx.offset(op, operation::CANCEL);
            let requested = fx.ptr(operation::REQUESTED as u64);
            fx.atomic_store(requested, cancel);
            let stage = fx.b.ins().iadd_imm_s(request, STAGE as i64);
            let zero = fx.ptr(0);
            fx.repeat(&[zero], |fx, state| {
                let now = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), stage);
                let calling = fx.b.ins().icmp_imm_s(IntCC::Equal, now, CALLING);
                let left =
                    fx.b.ins()
                        .icmp_imm_s(IntCC::UnsignedLessThan, state[0], ATTEMPTS);
                let trying = fx.b.ins().band(calling, left);
                let again = fx.choose_in(
                    lo,
                    trying,
                    &[types::I8],
                    |lo, fx| {
                        let cancelled =
                            lo.call_system(fx, "CancelSynchronousIo", &[thread], &[types::I32])[0];
                        let missed = fx.b.ins().icmp_imm_s(IntCC::Equal, cancelled, 0);
                        fx.when(missed, |fx| lo.relax(fx));
                        vec![missed]
                    },
                    |_, fx| vec![fx.int(types::I8, 0)],
                )[0];
                let next = fx.b.ins().iadd_imm_s(state[0], 1);
                (again, vec![next])
            });
            Vec::new()
        },
        |lo, fx| {
            let handle = fx.load(PTR, op, operation::WAITER);
            lo.call_system(fx, "CancelIoEx", &[handle, request], &[types::I32]);
            Vec::new()
        },
    );
}

/// Associates a handle opened for overlapped I/O with the completion port, so that its
/// requests complete there: whether the system accepted.
pub fn associate(lo: &mut Lowering, fx: &mut Fx, handle: Value) -> Value {
    let state_address = lo.state(fx);
    let port = fx.load(PTR, state_address, state::PORT);
    let key = fx.int(PTR, KEY_REQUEST);
    let concurrency = fx.int(types::I32, 0);
    let associated = lo.call_system(
        fx,
        "CreateIoCompletionPort",
        &[handle, port, key, concurrency],
        &[PTR],
    )[0];
    fx.b.ins().icmp_imm_s(IntCC::NotEqual, associated, 0)
}
