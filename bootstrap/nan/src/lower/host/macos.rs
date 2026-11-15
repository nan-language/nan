//! The macOS targets: everything macOS-specific. The executor threads, one per processor online,
//! which all wait on the process's kqueue for the descriptors tasks wait on and for the wakes of
//! the user events each thread has there; termination requests, which a thread of their own reads
//! from a kqueue of its own; the timers of the monotonic clock, which the runtime keeps; the
//! running executor thread's record, a thread-specific value; and the values and layouts of
//! `libSystem`.
//!
//! A wait on a descriptor is a one-shot event of the executors' kqueue specific to its operation
//! record, whose `WAITER` field holds the event's filter and `LINK` field the descriptor. Deleting
//! the event cancels the wait only while no thread has taken it from the kqueue, so exactly one
//! of the cancellation and the thread taking it delivers its completion.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, condcodes::IntCC, types};

use super::super::Lowering;
use super::super::fx::{Fx, PTR};
use super::super::machine::Stub;
use super::super::runtime::regions::origin;
use super::super::runtime::system::{unavailable_if, unavailable_unless};
use super::super::runtime::tasks::{self, kind, operation, task, thread};
use super::super::runtime::{Routine, fetch_add, state, timers};
use super::Context;
use super::posix::{self, Posix};
use crate::workspace::Arch;

/// The signals of a fault in a stack's guard region: a bus error, which a protected page gives,
/// and a segmentation violation.
const SIGBUS: i32 = 10;
const SIGSEGV: i32 = 11;

/// The values and layouts of `libSystem`.
pub const POSIX: Posix = Posix {
    map_anonymous: 0x1002,
    map_stack: 0x1002,
    stop: 16,
    faults: &[SIGBUS, SIGSEGV],
    cpu_limit: 24,
    sigaction: 16,
    sigaction_flags: 12,
    siginfo: 0x40,
    on_stack: 0x1,
    restart: 0x2,
    stack_size: 8,
    stack_flags: 16,
    fault_address: 24,
};

/// The offset in a `ucontext_t` of the address of its machine context.
const MACHINE_CONTEXT: u64 = 48;

/// The offsets in a machine context of the registers a stop or fault handler reads and
/// redirects, after its exception state.
fn context(arch: Arch) -> Context {
    match arch {
        Arch::Amd64 => Context {
            arg: 16 + 4 * 8,
            fp: 16 + 6 * 8,
            sp: 16 + 7 * 8,
            ip: 16 + 16 * 8,
            link: None,
        },
        Arch::Aarch64 => Context {
            arg: 16,
            fp: 16 + 29 * 8,
            sp: 16 + 31 * 8,
            ip: 16 + 32 * 8,
            link: Some(16 + 30 * 8),
        },
    }
}

/// The address of the registers a handler's `ucontext_t` saved, and their offsets there.
pub fn registers(lo: &mut Lowering, fx: &mut Fx, context: Value) -> (Value, Context) {
    let saved = fx.load(PTR, context, MACHINE_CONTEXT);
    (saved, self::context(lo.arch()))
}

/// The filters, flags and notes of the kqueue events the runtime registers, and the layout of a
/// `struct kevent`.
mod event {
    pub const SIGNAL: i64 = -6;
    pub const USER: i64 = -10;
    pub const ADD: i64 = 0x1;
    pub const DELETE: i64 = 0x2;
    pub const ONESHOT: i64 = 0x10;
    pub const CLEAR: i64 = 0x20;
    pub const UDATA_SPECIFIC: i64 = 0x100;
    pub const ERROR: i64 = 0x4000;
    pub const TRIGGER: i64 = 0x0100_0000;
    pub const IDENT: u64 = 0;
    pub const FILTER: u64 = 8;
    pub const FLAGS: u64 = 10;
    pub const FFLAGS: u64 = 12;
    pub const DATA: u64 = 16;
    pub const UDATA: u64 = 24;
    pub const SIZE: u64 = 32;
}

/// The user event of the first termination request; executor thread i's is i + 1.
const TERMINATION: i64 = 0;

/// The events a thread takes from the kqueue at once, and the longest it waits there, a day,
/// which the system accepts whatever the first timer.
const EVENTS: u64 = 16;
const LONGEST_WAIT: u64 = 86_400_000_000_000;

/// The `sysconf` name of the number of processors online, and the clock `clock_gettime_nsec_np`
/// reads as the monotonic clock: the system's uptime, which stops while it sleeps.
const SC_NPROCESSORS_ONLN: i64 = 58;
const CLOCK_UPTIME_RAW: i64 = 8;

/// The system setting reporting `PSTATE.DIT`.
const FEAT_DIT: &[u8] = b"hw.optional.arm.FEAT_DIT\0";

impl Lowering<'_, '_> {
    /// The system setting `name`, a NUL-terminated text, as a 32-bit integer, zero when the
    /// system has none.
    pub fn system_setting(&mut self, fx: &mut Fx, name: &[u8]) -> Value {
        let name = self.view_literal(fx, name).address();
        let name = fx.load(PTR, name, 0);
        let value = fx.slot(16, 8);
        let zero = fx.ptr(0);
        fx.store(zero, value, 0);
        let size = fx.b.ins().iadd_imm_s(value, 8);
        let four = fx.ptr(4);
        fx.store(four, size, 0);
        self.call_libc(
            fx,
            "sysctlbyname",
            &[name, value, size, zero, zero],
            &[types::I32],
        );
        fx.load(types::I32, value, 0)
    }
}

/// Makes a record the running thread's: the thread-specific value of the key `Init` created.
fn make_current(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let state_address = lo.state(fx);
    let key = fx.load(PTR, state_address, state::TLS_INDEX);
    lo.call_libc(fx, "pthread_setspecific", &[key, record], &[types::I32]);
}

/// The record of the executor thread running the caller, zero outside one.
pub fn current_thread(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let state_address = lo.state(fx);
    let key = fx.load(PTR, state_address, state::TLS_INDEX);
    lo.call_libc(fx, "pthread_getspecific", &[key], &[PTR])[0]
}

/// Makes a record the running thread's, turns on its data-independent timing where the
/// processor has it, and gives it a signal stack and a stack for stack overflow reports;
/// nonzero when the system refuses one.
pub fn thread_start(lo: &mut Lowering, fx: &mut Fx, record: Value) -> Value {
    make_current(lo, fx, record);
    constant_time(lo, fx);
    posix::thread_signals(lo, fx, record)
}

/// Turns on `PSTATE.DIT` for the running thread on a processor that has it, which threads do not
/// inherit; an x86_64 process cannot turn on DOITM, which is the system's.
fn constant_time(lo: &mut Lowering, fx: &mut Fx) {
    if lo.arch() != Arch::Aarch64 {
        return;
    }
    let dit = lo.system_setting(fx, FEAT_DIT);
    let dit = fx.b.ins().icmp_imm_s(IntCC::NotEqual, dit, 0);
    fx.when(dit, |fx| {
        lo.call_stub(fx, Stub::DitOn, &[]);
    });
}

/// The executor threads of `Init`: one per processor online, the calling thread being the
/// first, which all wait on the process's kqueue, after the handlers of the stop signal and of
/// faults and the thread reading termination requests; returns 1 from `Init` when the system
/// refuses one.
pub fn start_threads(lo: &mut Lowering, fx: &mut Fx) {
    let state_address = lo.state(fx);
    let key = fx.slot(8, 8);
    let zero = fx.ptr(0);
    fx.store(zero, key, 0);
    let created = lo.call_libc(fx, "pthread_key_create", &[key, zero], &[types::I32])[0];
    let refused = fx.b.ins().icmp_imm_s(IntCC::NotEqual, created, 0);
    unavailable_if(fx, refused);
    let key = fx.load(PTR, key, 0);
    fx.store(key, state_address, state::TLS_INDEX);
    if lo.seeded() {
        posix::start_seeded(lo, fx, |lo, fx, record| {
            make_current(lo, fx, record);
            constant_time(lo, fx);
        });
        return;
    }
    let port = lo.call_libc(fx, "kqueue", &[], &[types::I32])[0];
    let refused = fx.b.ins().icmp_imm_s(IntCC::SignedLessThan, port, 0);
    unavailable_if(fx, refused);
    let port = fx.b.ins().sextend(PTR, port);
    fx.store(port, state_address, state::PORT);
    let online = fx.int(types::I32, SC_NPROCESSORS_ONLN);
    let count = lo.call_libc(fx, "sysconf", &[online], &[PTR])[0];
    let reported = fx.b.ins().icmp_imm_s(IntCC::SignedGreaterThan, count, 0);
    let one = fx.ptr(1);
    let count = fx.b.ins().select(reported, count, one);
    fx.store(count, state_address, state::THREAD_COUNT);
    let size = fx.b.ins().imul_imm_s(count, 8);
    let threads = lo.record_alloc(fx, size);
    unavailable_unless(fx, threads);
    fx.store(threads, state_address, state::THREADS);
    let terminations = fx.int(PTR, TERMINATION);
    let added = add_user_event(lo, fx, terminations);
    unavailable_if(fx, added);
    fx.each(zero, count, |fx, index| {
        let size = fx.ptr(thread::SIZE);
        let record = lo.record_alloc(fx, size);
        unavailable_unless(fx, record);
        fx.store(index, record, thread::INDEX);
        let slot = fx.b.ins().imul_imm_s(index, 8);
        let slot = fx.b.ins().iadd(threads, slot);
        fx.store(record, slot, 0);
        let ident = fx.b.ins().iadd_imm_s(index, 1);
        let refused = add_user_event(lo, fx, ident);
        unavailable_if(fx, refused);
    });
    let first = fx.load(PTR, threads, 0);
    let refused = thread_start(lo, fx, first);
    unavailable_if(fx, refused);
    posix::install_handlers(lo, fx);
    read_signals(lo, fx);
    let thread_main = lo.routine(Routine::ThreadMain);
    let thread_main = lo.func_ref(fx, thread_main);
    let thread_main = fx.b.ins().func_addr(PTR, thread_main);
    fx.each(one, count, |fx, index| {
        let slot = fx.b.ins().imul_imm_s(index, 8);
        let slot = fx.b.ins().iadd(threads, slot);
        let record = fx.load(PTR, slot, 0);
        let refused = spawn(lo, fx, thread_main, record);
        unavailable_if(fx, refused);
    });
}

/// Starts a thread running `start` with an argument; nonzero when the system refuses.
fn spawn(lo: &mut Lowering, fx: &mut Fx, start: Value, argument: Value) -> Value {
    let id = fx.slot(8, 8);
    let zero = fx.ptr(0);
    let started = lo.call_libc(
        fx,
        "pthread_create",
        &[id, zero, start, argument],
        &[types::I32],
    )[0];
    fx.b.ins().icmp_imm_s(IntCC::NotEqual, started, 0)
}

/// Writes a `struct kevent` of an identifier, a filter, flags, filter flags and user data.
fn event_of(
    fx: &mut Fx,
    at: Value,
    ident: Value,
    filter: Value,
    flags: i64,
    notes: i64,
    data: Value,
) {
    fx.store(ident, at, event::IDENT);
    fx.store(filter, at, event::FILTER);
    let flags = fx.int(types::I16, flags);
    fx.store(flags, at, event::FLAGS);
    let notes = fx.int(types::I32, notes);
    fx.store(notes, at, event::FFLAGS);
    let zero = fx.ptr(0);
    fx.store(zero, at, event::DATA);
    fx.store(data, at, event::UDATA);
}

/// Applies one change to a kqueue, giving `kevent`'s result: 0, or -1 when the change fails.
fn change(lo: &mut Lowering, fx: &mut Fx, queue: Value, change: Value) -> Value {
    let one = fx.int(types::I32, 1);
    let none = fx.ptr(0);
    let zero = fx.int(types::I32, 0);
    lo.call_libc(
        fx,
        "kevent",
        &[queue, change, one, none, zero, none],
        &[types::I32],
    )[0]
}

/// The executors' kqueue.
fn port(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let state_address = lo.state(fx);
    let port = fx.load(PTR, state_address, state::PORT);
    fx.b.ins().ireduce(types::I32, port)
}

/// Adds a user event of an identifier to the executors' kqueue, cleared once taken; nonzero
/// when the system refuses.
fn add_user_event(lo: &mut Lowering, fx: &mut Fx, ident: Value) -> Value {
    let at = fx.slot(event::SIZE, 8);
    let filter = fx.int(types::I16, event::USER);
    let zero = fx.ptr(0);
    event_of(fx, at, ident, filter, event::ADD | event::CLEAR, 0, zero);
    let port = port(lo, fx);
    let result = change(lo, fx, port, at);
    fx.b.ins().icmp_imm_s(IntCC::NotEqual, result, 0)
}

/// Triggers the user event of an identifier, which wakes a thread waiting on the executors'
/// kqueue.
fn trigger(lo: &mut Lowering, fx: &mut Fx, ident: Value) {
    let at = fx.slot(event::SIZE, 8);
    let filter = fx.int(types::I16, event::USER);
    let zero = fx.ptr(0);
    event_of(fx, at, ident, filter, 0, event::TRIGGER, zero);
    let port = port(lo, fx);
    change(lo, fx, port, at);
}

/// Ignores the termination request signals, which a kqueue still reports, and starts the thread
/// reading them from a kqueue of its own.
fn read_signals(lo: &mut Lowering, fx: &mut Fx) {
    let queue = lo.call_libc(fx, "kqueue", &[], &[types::I32])[0];
    let refused = fx.b.ins().icmp_imm_s(IntCC::SignedLessThan, queue, 0);
    unavailable_if(fx, refused);
    let at = fx.slot(event::SIZE, 8);
    for number in posix::SIGNALS {
        let ignore = fx.ptr(1);
        let signal = fx.int(types::I32, number);
        lo.call_libc(fx, "signal", &[signal, ignore], &[PTR]);
        let ident = fx.ptr(number as u64);
        let filter = fx.int(types::I16, event::SIGNAL);
        let zero = fx.ptr(0);
        event_of(fx, at, ident, filter, event::ADD, 0, zero);
        let result = change(lo, fx, queue, at);
        let refused = fx.b.ins().icmp_imm_s(IntCC::NotEqual, result, 0);
        unavailable_if(fx, refused);
    }
    let start = lo.routine(Routine::Terminations);
    let start = lo.func_ref(fx, start);
    let start = fx.b.ins().func_addr(PTR, start);
    let queue = fx.b.ins().sextend(PTR, queue);
    let refused = spawn(lo, fx, start, queue);
    unavailable_if(fx, refused);
}

/// `Terminations(queue)`: reads each termination request from its kqueue: the first records the
/// status a request with its signal number n ends the process with, 128 + n, and triggers the
/// user event the executor threads cancel the entry task's construct on; a later one ends the
/// process at once, whatever the executor threads run.
pub fn terminations(lo: &mut Lowering, fx: &mut Fx, queue: Value) {
    let queue = fx.b.ins().ireduce(types::I32, queue);
    let taken = fx.slot(event::SIZE, 8);
    fx.repeat(&[], |fx, _| {
        let none = fx.ptr(0);
        let zero = fx.int(types::I32, 0);
        let one = fx.int(types::I32, 1);
        let count = lo.call_libc(
            fx,
            "kevent",
            &[queue, none, zero, taken, one, none],
            &[types::I32],
        )[0];
        let read = fx.b.ins().icmp_imm_s(IntCC::Equal, count, 1);
        fx.when(read, |fx| {
            let signal = fx.load(PTR, taken, event::IDENT);
            let status = fx.b.ins().iadd_imm_s(signal, 128);
            let state_address = lo.state(fx);
            let word = fx.offset(state_address, state::TERMINATION);
            let zero = fx.ptr(0);
            let before =
                fx.b.ins()
                    .atomic_cas(MemFlagsData::trusted(), word, zero, status);
            let later = fx.b.ins().icmp_imm_s(IntCC::NotEqual, before, 0);
            fx.when(later, |fx| {
                lo.call_routine(fx, Routine::Exit, &[status]);
            });
            let termination = fx.int(PTR, TERMINATION);
            trigger(lo, fx, termination);
        });
        (fx.int(types::I8, 1), Vec::new())
    });
    fx.b.ins()
        .trap(cranelift_codegen::ir::TrapCode::user(1).expect("a trap code"));
}

/// `Watch(fd, filter) -> (refused, result)`: parks the running task until the descriptor `fd` is
/// ready for the kqueue filter `filter`, and gives the event's data, the bytes or room it reports,
/// or the negated error number of a wait the system refuses; a task cancelled when it starts
/// waits for nothing, and one cancelled while it waits deletes the event. Gives whether the task's
/// cancellation refused the wait.
pub fn define_watch(lo: &mut Lowering, fx: &mut Fx) {
    let params = fx.entry();
    let (fd, filter) = (params[0], params[1]);
    let record = tasks::current_task(lo, fx);
    let region = fx.load(PTR, record, task::REGION);
    let early = origin(fx, region);
    let early = fx.b.ins().icmp_imm_s(IntCC::NotEqual, early, 0);
    fx.when(early, |fx| {
        let yes = fx.int(types::I8, 1);
        let zero = fx.int(types::I64, 0);
        fx.b.ins().return_(&[yes, zero]);
    });
    let op = fx.slot(operation::SIZE, 8);
    let zero = fx.ptr(0);
    let size = fx.ptr(operation::SIZE);
    lo.call_routine(fx, Routine::Fill, &[op, zero, size]);
    fx.store(record, op, operation::TASK);
    let wait = fx.ptr(kind::WAIT as u64);
    fx.store(wait, op, operation::KIND);
    let filter = fx.b.ins().sextend(PTR, filter);
    fx.store(filter, op, operation::WAITER);
    let fd = fx.b.ins().sextend(PTR, fd);
    fx.store(fd, op, operation::LINK);
    let word = fx.offset(record, task::IO);
    fx.atomic_store(op, word);
    let at = fx.slot(event::SIZE, 8);
    let filter16 = fx.b.ins().ireduce(types::I16, filter);
    let flags = event::ADD | event::ONESHOT | event::UDATA_SPECIFIC;
    event_of(fx, at, fd, filter16, flags, 0, op);
    let port = port(lo, fx);
    let added = change(lo, fx, port, at);
    let refused = fx.b.ins().icmp_imm_s(IntCC::NotEqual, added, 0);
    fx.when(refused, |fx| {
        tasks::release_io(lo, fx, word, op);
        let error = last_error(lo, fx);
        let no = fx.int(types::I8, 0);
        fx.b.ins().return_(&[no, error]);
    });
    let cancelled = origin(fx, region);
    let cancelled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, cancelled, 0);
    fx.when(cancelled, |fx| lo.cancel_operation(fx, op));
    tasks::wait_delivered(lo, fx, op);
    tasks::release_io(lo, fx, word, op);
    let result = fx.load(types::I32, op, operation::RESULT);
    let refused = fx.b.ins().icmp_imm_s(IntCC::Equal, result, tasks::CANCELED);
    let cancelled = origin(fx, region);
    let cancelled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, cancelled, 0);
    let refused = fx.b.ins().band(refused, cancelled);
    let result = fx.b.ins().sextend(types::I64, result);
    fx.b.ins().return_(&[refused, result]);
}

/// The negated number of the calling thread's last error.
fn last_error(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let error = lo.call_libc(fx, "__error", &[], &[PTR])[0];
    let number = fx.load(types::I32, error, 0);
    let number = fx.b.ins().sextend(types::I64, number);
    fx.b.ins().ineg(number)
}

/// Asks once for the cancellation of an operation whose record stays valid during the call: a
/// timer armed and not expired leaves the timers and completes cancelled at once, and a wait on a
/// descriptor whose event no thread has taken has it deleted and completes cancelled at once.
pub fn cancel_operation(lo: &mut Lowering, fx: &mut Fx, op: Value) {
    let claimed = timers::claim_armed(lo, fx, op);
    fx.when(claimed, |fx| tasks::deliver_cancelled(lo, fx, op));
    let kind = fx.load(PTR, op, operation::KIND);
    let wait = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, kind::WAIT);
    let filter = fx.load(PTR, op, operation::WAITER);
    let watched = fx.b.ins().icmp_imm_s(IntCC::NotEqual, filter, 0);
    let watched = fx.b.ins().band(wait, watched);
    fx.when(watched, |fx| {
        let flag = fx.offset(op, operation::CANCEL);
        let zero = fx.ptr(0);
        let requested = fx.ptr(operation::REQUESTED as u64);
        let previous =
            fx.b.ins()
                .atomic_cas(MemFlagsData::trusted(), flag, zero, requested);
        let first = fx.b.ins().icmp_imm_s(IntCC::Equal, previous, 0);
        fx.when(first, |fx| {
            let at = fx.slot(event::SIZE, 8);
            let fd = fx.load(PTR, op, operation::LINK);
            let filter = fx.b.ins().ireduce(types::I16, filter);
            event_of(
                fx,
                at,
                fd,
                filter,
                event::DELETE | event::UDATA_SPECIFIC,
                0,
                op,
            );
            let port = port(lo, fx);
            let deleted = change(lo, fx, port, at);
            let deleted = fx.b.ins().icmp_imm_s(IntCC::Equal, deleted, 0);
            fx.when(deleted, |fx| tasks::deliver_cancelled(lo, fx, op));
        });
    });
}

/// Waits idle on the executors' kqueue until an event arrives, unless a task is ready, then
/// handles the events taken.
pub fn idle(lo: &mut Lowering, fx: &mut Fx) {
    let current = lo.thread(fx);
    let idle = fx.offset(current, thread::IDLE);
    let one = fx.ptr(1);
    fx.atomic_store(one, idle);
    let state_address = lo.state(fx);
    let head = fx.offset(state_address, state::READY_HEAD);
    let head = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), head);
    let empty = fx.b.ins().icmp_imm_s(IntCC::Equal, head, 0);
    take_events(lo, fx, Some(empty));
    let zero = fx.ptr(0);
    fx.atomic_store(zero, idle);
}

/// Takes the events the executors' kqueue holds, when `wait` is given and nonzero waiting for one
/// until the first timer is due, and handles them, then delivers the expiries of the timers due.
pub fn take_events(lo: &mut Lowering, fx: &mut Fx, wait: Option<Value>) {
    let events = fx.slot(EVENTS * event::SIZE, 8);
    let timeout = fx.slot(16, 8);
    let zero = fx.ptr(0);
    fx.store(zero, timeout, 0);
    fx.store(zero, timeout, 8);
    let bound = match wait {
        Some(wait) => fx.choose_in(
            lo,
            wait,
            &[PTR],
            |lo, fx| {
                let (armed, left) = timers::first_due_in(lo, fx);
                let longest = fx.ptr(LONGEST_WAIT);
                let left = fx.b.ins().umin(left, longest);
                let billion = fx.ptr(1_000_000_000);
                let seconds = fx.b.ins().udiv(left, billion);
                let nanoseconds = fx.b.ins().urem(left, billion);
                fx.store(seconds, timeout, 0);
                fx.store(nanoseconds, timeout, 8);
                let forever = fx.ptr(0);
                vec![fx.b.ins().select(armed, timeout, forever)]
            },
            |_, _| vec![timeout],
        )[0],
        None => timeout,
    };
    let port = port(lo, fx);
    let none = fx.ptr(0);
    let nothing = fx.int(types::I32, 0);
    let room = fx.int(types::I32, EVENTS as i64);
    let count = lo.call_libc(
        fx,
        "kevent",
        &[port, none, nothing, events, room, bound],
        &[types::I32],
    )[0];
    let count = fx.b.ins().sextend(PTR, count);
    let taken = fx.b.ins().icmp_imm_s(IntCC::SignedGreaterThan, count, 0);
    let count = fx.b.ins().select(taken, count, zero);
    fx.each(zero, count, |fx, index| {
        let offset = fx.b.ins().imul_imm_s(index, event::SIZE as i64);
        let taken = fx.b.ins().iadd(events, offset);
        handle_event(lo, fx, taken);
    });
    timers::expire(lo, fx);
}

/// Handles an event taken from the executors' kqueue: a descriptor's readiness is delivered to
/// the wait its user data names, with the event's data or, for an event in error, the negated
/// error number it holds, and the first termination request cancels the entry task's construct;
/// a wake needs nothing more.
fn handle_event(lo: &mut Lowering, fx: &mut Fx, taken: Value) {
    let filter = fx.load(types::I16, taken, event::FILTER);
    let filter = fx.b.ins().sextend(PTR, filter);
    let user = fx.b.ins().icmp_imm_s(IntCC::Equal, filter, event::USER);
    fx.choose_in(
        lo,
        user,
        &[],
        |lo, fx| {
            let ident = fx.load(PTR, taken, event::IDENT);
            let termination = fx.b.ins().icmp_imm_s(IntCC::Equal, ident, TERMINATION);
            fx.when(termination, |fx| {
                let state_address = lo.state(fx);
                let root = fx.load(PTR, state_address, state::ROOT);
                lo.call_routine(fx, Routine::Cancel, &[root]);
            });
            Vec::new()
        },
        |lo, fx| {
            let op = fx.load(PTR, taken, event::UDATA);
            let flags = fx.load(types::I16, taken, event::FLAGS);
            let flags = fx.b.ins().uextend(PTR, flags);
            let failed = fx.b.ins().band_imm_s(flags, event::ERROR);
            let failed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, failed, 0);
            let data = fx.load(PTR, taken, event::DATA);
            let negated = fx.b.ins().ineg(data);
            let data = fx.b.ins().select(failed, negated, data);
            let most = fx.ptr(i32::MAX as u64);
            let over = fx.b.ins().icmp(IntCC::SignedGreaterThan, data, most);
            let data = fx.b.ins().select(over, most, data);
            let result = fx.b.ins().ireduce(types::I32, data);
            tasks::deliver(lo, fx, op, result);
            Vec::new()
        },
    );
}

/// `ClockNow`: the nanoseconds of the monotonic clock, the system's uptime.
pub fn define_clock_now(lo: &mut Lowering, fx: &mut Fx) {
    fx.entry();
    let clock = fx.int(types::I32, CLOCK_UPTIME_RAW);
    let now = lo.call_libc(fx, "clock_gettime_nsec_np", &[clock], &[PTR])[0];
    fx.b.ins().return_(&[now]);
}

/// The body of `TimerArm`: arms the timer in the timers the runtime keeps, and wakes a thread
/// waiting idle, so that it waits no longer than the timer.
pub fn arm_timer(lo: &mut Lowering, fx: &mut Fx, op: Value, ns: Value) {
    timers::arm_shared(lo, fx, op, ns);
    let first = fx.ptr(1);
    trigger(lo, fx, first);
}

/// Wakes an executor thread waiting idle, other than the running one, so a task made ready runs:
/// the trigger of its user event, which whichever thread waits on the kqueue may take. Each thread
/// clears its own idle flag once its wait ends, so an event another thread took leaves a thread
/// still waiting marked idle.
pub fn wake_idle(lo: &mut Lowering, fx: &mut Fx) {
    let state_address = lo.state(fx);
    let threads = fx.load(PTR, state_address, state::THREADS);
    let count = fx.load(PTR, state_address, state::THREAD_COUNT);
    let current = lo.thread(fx);
    let zero = fx.ptr(0);
    fx.repeat(&[zero], |fx, state| {
        let index = state[0];
        let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, index, count);
        let next = fx.choose_in(
            lo,
            inside,
            &[PTR, types::I8],
            |lo, fx| {
                let slot = fx.b.ins().imul_imm_s(index, 8);
                let slot = fx.b.ins().iadd(threads, slot);
                let record = fx.load(PTR, slot, 0);
                let other = fx.b.ins().icmp(IntCC::NotEqual, record, current);
                let idle = fx.offset(record, thread::IDLE);
                let woken = fx.choose(
                    other,
                    &[types::I8],
                    |fx| {
                        let zero = fx.ptr(0);
                        let seen = fetch_add(fx, idle, zero);
                        vec![fx.b.ins().icmp_imm_s(IntCC::Equal, seen, 1)]
                    },
                    |fx| vec![fx.int(types::I8, 0)],
                )[0];
                fx.when(woken, |fx| {
                    let ident = fx.b.ins().iadd_imm_s(index, 1);
                    trigger(lo, fx, ident);
                });
                let next = fx.b.ins().iadd_imm_s(index, 1);
                vec![next, fx.b.ins().bxor_imm_s(woken, 1)]
            },
            |_, fx| vec![index, fx.int(types::I8, 0)],
        );
        (next[1], vec![next[0]])
    });
}
