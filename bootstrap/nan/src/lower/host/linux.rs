//! The Linux targets: everything Linux-specific. The executor threads, one per processor of the
//! process's affinity set, each with its io_uring; termination requests, which a thread of their
//! own reads from a `signalfd`; the values and layouts of glibc; and linking the image with the
//! target's linker.
//!
//! An operation record is what a completion reaches: the task waiting on it, its result, how far its completion has been delivered, the
//! thread whose ring holds it, how far its cancellation has gone, its kind and its links in its
//! thread's list. Only the thread whose ring holds an operation lists it, delivers its
//! completion and submits its cancellation, so an operation another thread asks to cancel is
//! cancelled only while it is in flight.

use std::path::Path;
use std::process::Command;

use cranelift_codegen::ir::{
    AtomicRmwOp, InstBuilder, MemFlagsData, TrapCode, Value, condcodes::IntCC, types,
};

use super::super::Lowering;
use super::super::fx::{Fx, PTR};
use super::super::machine::Stub;
use super::super::runtime::system::unavailable_unless;
use super::super::runtime::tasks::{self, kind, operation, thread};
use super::super::runtime::{Routine, state};
use super::Context;
use super::posix::{self, Posix};
use crate::workspace::{Arch, Target};

/// The `mmap` protection and flags of an io_uring's queues.
mod mmap {
    pub const READ_WRITE: i64 = 3;
    pub const SHARED_POPULATE: i64 = 0x8001;
}

/// The io_uring system calls, their constants and the opcodes the runtime submits itself.
mod uring {
    pub const SETUP: i64 = 425;
    pub const ENTER: i64 = 426;
    pub const ENTRIES: i64 = 256;
    pub const GET_EVENTS: i64 = 1;
    pub const SQES: i64 = 0x1000_0000;
    pub const TIMEOUT: i64 = 11;
    pub const ASYNC_CANCEL: i64 = 14;
    pub const MSG_RING: i64 = 40;
}

/// The offsets of a ring record's fields.
mod ring {
    pub const FD: u64 = 0;
    pub const SQ_HEAD: u64 = 8;
    pub const SQ_TAIL: u64 = 16;
    pub const SQ_MASK: u64 = 24;
    pub const SQ_ARRAY: u64 = 32;
    pub const SQES: u64 = 40;
    pub const CQ_HEAD: u64 = 48;
    pub const CQ_TAIL: u64 = 56;
    pub const CQ_MASK: u64 = 64;
    pub const CQES: u64 = 72;
    pub const SIZE: u64 = 80;
}

/// The signal of a fault.
pub const SIGSEGV: i32 = 11;

/// The values and layouts of glibc.
pub const POSIX: Posix = Posix {
    map_anonymous: 0x22,
    map_stack: 0x2_4022,
    stop: 23,
    faults: &[SIGSEGV],
    cpu_limit: 24,
    sigaction: 152,
    sigaction_flags: 136,
    siginfo: 4,
    on_stack: 0x0800_0000,
    restart: 0x1000_0000,
    stack_size: 16,
    stack_flags: 8,
    fault_address: 16,
};

/// The offsets of the registers a stop or fault handler reads and redirects in a `ucontext_t`.
pub fn context(target: Target) -> Context {
    match target.arch().expect("a Linux target's architecture") {
        Arch::Amd64 => Context {
            arg: 40 + 8 * 8,
            fp: 40 + 10 * 8,
            sp: 40 + 15 * 8,
            ip: 40 + 16 * 8,
            link: None,
        },
        Arch::Aarch64 => Context {
            arg: 184,
            fp: 416,
            sp: 432,
            ip: 440,
            link: Some(424),
        },
    }
}

/// The length of a `signalfd_siginfo`.
const SIGINFO: u64 = 128;

/// The `getauxval` type of the hardware capabilities, and their AArch64 bit for `PSTATE.DIT`.
const AT_HWCAP: u64 = 16;
const HWCAP_DIT: i64 = 1 << 24;

/// The executor threads of `Init`: one per processor of the affinity set with its ring, the
/// calling thread being the first, and the reading of termination requests, after the
/// data-independent timing mode, which they inherit; returns 1 from `Init` when the system
/// refuses one.
pub fn start_threads(lo: &mut Lowering, fx: &mut Fx) {
    constant_time(lo, fx);
    let state_address = lo.state(fx);
    if lo.seeded() {
        posix::start_seeded(lo, fx, |lo, fx, record| {
            let slot = lo.thread_slot(fx);
            fx.store(record, slot, 0);
        });
        return;
    }
    let count = processors(lo, fx);
    fx.store(count, state_address, state::THREAD_COUNT);
    let eight = fx.ptr(8);
    let threads = lo.call_libc(fx, "calloc", &[count, eight], &[PTR])[0];
    unavailable_unless(fx, threads);
    fx.store(threads, state_address, state::THREADS);
    let zero = fx.ptr(0);
    fx.each(zero, count, |fx, index| {
        let one = fx.ptr(1);
        let size = fx.ptr(thread::SIZE);
        let record = lo.call_libc(fx, "calloc", &[one, size], &[PTR])[0];
        unavailable_unless(fx, record);
        fx.store(index, record, thread::INDEX);
        let ring = setup_ring(lo, fx);
        unavailable_unless(fx, ring);
        fx.store(ring, record, thread::RING);
        let slot = fx.b.ins().imul_imm_s(index, 8);
        let slot = fx.b.ins().iadd(threads, slot);
        fx.store(record, slot, 0);
    });
    let first = fx.load(PTR, threads, 0);
    let slot = lo.thread_slot(fx);
    fx.store(first, slot, 0);
    let refused = posix::thread_signals(lo, fx, first);
    fx.when(refused, |fx| {
        let one = fx.ptr(1);
        fx.b.ins().return_(&[one]);
    });
    posix::install_handlers(lo, fx);
    read_signals(lo, fx);
    let thread_main = lo.routine(Routine::ThreadMain);
    let thread_main = lo.func_ref(fx, thread_main);
    let thread_main = fx.b.ins().func_addr(PTR, thread_main);
    let one = fx.ptr(1);
    fx.each(one, count, |fx, index| {
        let slot = fx.b.ins().imul_imm_s(index, 8);
        let slot = fx.b.ins().iadd(threads, slot);
        let record = fx.load(PTR, slot, 0);
        let id = fx.slot(8, 8);
        let zero = fx.ptr(0);
        let started = lo.call_libc(
            fx,
            "pthread_create",
            &[id, zero, thread_main, record],
            &[types::I32],
        )[0];
        let failed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, started, 0);
        fx.when(failed, |fx| {
            let one = fx.ptr(1);
            fx.b.ins().return_(&[one]);
        });
    });
}

/// Turns on `PSTATE.DIT` on an AArch64 host whose kernel reports `HWCAP_DIT`, and so emulates the
/// stub's read of `ID_AA64PFR0_EL1`. An x86_64 process cannot turn on DOITM, which is the
/// kernel's.
fn constant_time(lo: &mut Lowering, fx: &mut Fx) {
    if lo.ck.db.target.arch() != Some(Arch::Aarch64) {
        return;
    }
    let kind = fx.ptr(AT_HWCAP);
    let hwcap = lo.call_libc(fx, "getauxval", &[kind], &[PTR])[0];
    let dit = fx.b.ins().band_imm_s(hwcap, HWCAP_DIT);
    fx.when(dit, |fx| {
        lo.call_stub(fx, Stub::ConstantTime, &[]);
    });
}

/// The number of processors in the process's affinity set, at least 1.
fn processors(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let mask = fx.slot(128, 8);
    let zero = fx.ptr(0);
    let size = fx.ptr(128);
    lo.call_routine(fx, Routine::Fill, &[mask, zero, size]);
    let pid = fx.int(types::I32, 0);
    lo.call_libc(fx, "sched_getaffinity", &[pid, size, mask], &[types::I32]);
    let mut count = fx.ptr(0);
    for word in 0..16 {
        let bits = fx.load(PTR, mask, 8 * word);
        let set = fx.b.ins().popcnt(bits);
        count = fx.b.ins().iadd(count, set);
    }
    let one = fx.ptr(1);
    fx.b.ins().umax(count, one)
}

/// Blocks the termination request signals in every thread and starts the thread reading them
/// from a `signalfd`, with a ring of its own.
fn read_signals(lo: &mut Lowering, fx: &mut Fx) {
    let mask = fx.slot(128, 8);
    let zero = fx.ptr(0);
    let size = fx.ptr(128);
    lo.call_routine(fx, Routine::Fill, &[mask, zero, size]);
    let bits = posix::SIGNALS
        .iter()
        .fold(0i64, |bits, signal| bits | 1 << (signal - 1));
    let bits = fx.int(PTR, bits);
    fx.store(bits, mask, 0);
    let block = fx.int(types::I32, 0);
    lo.call_libc(fx, "pthread_sigmask", &[block, mask, zero], &[types::I32]);
    let new = fx.int(types::I32, -1);
    let close_on_exec = fx.int(types::I32, 0x8_0000);
    let fd = lo.call_libc(fx, "signalfd", &[new, mask, close_on_exec], &[types::I32])[0];
    let one = fx.ptr(1);
    let size = fx.ptr(operation::SIZE + SIGINFO);
    let op = lo.call_libc(fx, "calloc", &[one, size], &[PTR])[0];
    unavailable_unless(fx, op);
    let signal = fx.int(PTR, kind::SIGNAL);
    fx.store(signal, op, operation::KIND);
    let fd = fx.b.ins().sextend(PTR, fd);
    fx.store(fd, op, operation::TASK);
    let state_address = lo.state(fx);
    fx.store(op, state_address, state::SIGNALS);
    let size = fx.ptr(thread::SIZE);
    let record = lo.call_libc(fx, "calloc", &[one, size], &[PTR])[0];
    unavailable_unless(fx, record);
    let ring = setup_ring(lo, fx);
    unavailable_unless(fx, ring);
    fx.store(ring, record, thread::RING);
    fx.store(record, op, operation::THREAD);
    let start = lo.routine(Routine::Terminations);
    let start = lo.func_ref(fx, start);
    let start = fx.b.ins().func_addr(PTR, start);
    let id = fx.slot(8, 8);
    let zero = fx.ptr(0);
    let started = lo.call_libc(fx, "pthread_create", &[id, zero, start, op], &[types::I32])[0];
    let failed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, started, 0);
    fx.when(failed, |fx| {
        let one = fx.ptr(1);
        fx.b.ins().return_(&[one]);
    });
}

/// `Terminations(op)`: reads each termination request from the `signalfd` of `op`, then
/// passes the first with `op` to the first executor thread's ring and ends the process at once
/// at a later one, whatever the executor threads run.
pub fn terminations(lo: &mut Lowering, fx: &mut Fx, op: Value) {
    let record = fx.load(PTR, op, operation::THREAD);
    let slot = lo.thread_slot(fx);
    fx.store(record, slot, 0);
    let fd = fx.load(PTR, op, operation::TASK);
    let fd = fx.b.ins().ireduce(types::I32, fd);
    let buffer = fx.b.ins().iadd_imm_s(op, operation::SIZE as i64);
    let len = fx.ptr(SIGINFO);
    let none = fx.int(types::I8, 0);
    fx.repeat(&[none], |fx, state| {
        let passed = state[0];
        let read = lo.call_libc(fx, "read", &[fd, buffer, len], &[PTR])[0];
        let whole = fx.b.ins().icmp_imm_s(IntCC::Equal, read, SIGINFO as i64);
        let later = fx.b.ins().band(whole, passed);
        fx.when(later, |fx| {
            let signal = fx.load(types::I32, op, operation::SIZE);
            let signal = fx.b.ins().uextend(PTR, signal);
            let status = fx.b.ins().iadd_imm_s(signal, 128);
            lo.call_routine(fx, Routine::Exit, &[status]);
        });
        let first = fx.b.ins().bxor_imm_s(passed, 1);
        let first = fx.b.ins().band(whole, first);
        fx.when(first, |fx| {
            let state_address = lo.state(fx);
            let threads = fx.load(PTR, state_address, state::THREADS);
            let target = fx.load(PTR, threads, 0);
            let ring = fx.load(PTR, target, thread::RING);
            let target = fx.load(PTR, ring, ring::FD);
            let target = fx.b.ins().ireduce(types::I32, target);
            let read = fx.b.ins().ireduce(types::I32, read);
            let zero = fx.ptr(0);
            let flags = fx.int(types::I32, 0);
            submit_built(lo, fx, uring::MSG_RING, target, zero, read, op, flags, None);
        });
        let passed = fx.b.ins().bor(passed, whole);
        (fx.int(types::I8, 1), vec![passed])
    });
    fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
}

/// Handles the first termination request, which the thread reading them passes on: it records
/// the status a request with its signal number n ends the process with, 128 + n, and cancels the
/// entry task's construct.
fn termination(lo: &mut Lowering, fx: &mut Fx, op: Value) {
    let signal = fx.load(types::I32, op, operation::SIZE);
    let signal = fx.b.ins().uextend(PTR, signal);
    let status = fx.b.ins().iadd_imm_s(signal, 128);
    let state_address = lo.state(fx);
    fx.store(status, state_address, state::TERMINATION);
    let root = fx.load(PTR, state_address, state::ROOT);
    lo.call_routine(fx, Routine::Cancel, &[root]);
}

/// Sets up an io_uring and gives its ring record, zero when the system refuses one.
fn setup_ring(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let params = fx.slot(120, 8);
    let zero = fx.ptr(0);
    let len = fx.ptr(120);
    lo.call_routine(fx, Routine::Fill, &[params, zero, len]);
    let number = fx.int(PTR, uring::SETUP);
    let entries = fx.int(PTR, uring::ENTRIES);
    let fd = syscall(lo, fx, number, &[entries, params]);
    let usable =
        fx.b.ins()
            .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, fd, 0);
    fx.choose_in(
        lo,
        usable,
        &[PTR],
        |lo, fx| {
            let one = fx.ptr(1);
            let size = fx.ptr(ring::SIZE);
            let record = lo.call_libc(fx, "calloc", &[one, size], &[PTR])[0];
            fx.store(fd, record, ring::FD);
            let sq_entries = fx.load(types::I32, params, 0);
            let sq_entries = fx.b.ins().uextend(PTR, sq_entries);
            let cq_entries = fx.load(types::I32, params, 4);
            let cq_entries = fx.b.ins().uextend(PTR, cq_entries);
            let sq_array = fx.load(types::I32, params, 40 + 24);
            let sq_array = fx.b.ins().uextend(PTR, sq_array);
            let cq_cqes = fx.load(types::I32, params, 80 + 20);
            let cq_cqes = fx.b.ins().uextend(PTR, cq_cqes);
            let sq_size = fx.b.ins().imul_imm_s(sq_entries, 4);
            let sq_size = fx.b.ins().iadd(sq_array, sq_size);
            let cq_size = fx.b.ins().imul_imm_s(cq_entries, 16);
            let cq_size = fx.b.ins().iadd(cq_cqes, cq_size);
            let size = fx.b.ins().umax(sq_size, cq_size);
            let zero = fx.ptr(0);
            let protection = fx.int(types::I32, mmap::READ_WRITE);
            let flags = fx.int(types::I32, mmap::SHARED_POPULATE);
            let fd32 = fx.b.ins().ireduce(types::I32, fd);
            let rings = lo.call_libc(
                fx,
                "mmap",
                &[zero, size, protection, flags, fd32, zero],
                &[PTR],
            )[0];
            let sqes_size = fx.b.ins().imul_imm_s(sq_entries, 64);
            let offset = fx.int(PTR, uring::SQES);
            let sqes = lo.call_libc(
                fx,
                "mmap",
                &[zero, sqes_size, protection, flags, fd32, offset],
                &[PTR],
            )[0];
            for (field, source) in [
                (ring::SQ_HEAD, 40),
                (ring::SQ_TAIL, 44),
                (ring::SQ_MASK, 48),
                (ring::SQ_ARRAY, 64),
                (ring::CQ_HEAD, 80),
                (ring::CQ_TAIL, 84),
                (ring::CQ_MASK, 88),
                (ring::CQES, 100),
            ] {
                let offset = fx.load(types::I32, params, source);
                let offset = fx.b.ins().uextend(PTR, offset);
                let address = fx.b.ins().iadd(rings, offset);
                fx.store(address, record, field);
            }
            fx.store(sqes, record, ring::SQES);
            vec![record]
        },
        |_, fx| vec![fx.ptr(0)],
    )[0]
}

/// Submits a 64-byte entry on the running thread's ring, its completion to reach `op`, which
/// joins the thread's operations in flight; an entry without an operation completes unseen.
pub fn submit(lo: &mut Lowering, fx: &mut Fx, entry: Value, op: Option<Value>) {
    let current = lo.thread(fx);
    if let Some(op) = op {
        fx.store(current, op, operation::THREAD);
        let head = fx.load(PTR, current, thread::OPERATIONS);
        fx.store(head, op, operation::NEXT);
        let zero = fx.ptr(0);
        fx.store(zero, op, operation::PREVIOUS);
        let has_head = fx.b.ins().icmp_imm_s(IntCC::NotEqual, head, 0);
        fx.when(has_head, |fx| fx.store(op, head, operation::PREVIOUS));
        fx.store(op, current, thread::OPERATIONS);
    }
    let ring = fx.load(PTR, current, thread::RING);
    let tail_address = fx.load(PTR, ring, ring::SQ_TAIL);
    let tail = fx.load(types::I32, tail_address, 0);
    let mask_address = fx.load(PTR, ring, ring::SQ_MASK);
    let mask = fx.load(types::I32, mask_address, 0);
    let index = fx.b.ins().band(tail, mask);
    let index = fx.b.ins().uextend(PTR, index);
    let sqes = fx.load(PTR, ring, ring::SQES);
    let offset = fx.b.ins().imul_imm_s(index, 64);
    let sqe = fx.b.ins().iadd(sqes, offset);
    fx.copy_small(sqe, entry, 64, 8);
    let user = op.unwrap_or_else(|| fx.ptr(0));
    fx.store(user, sqe, 32);
    let array = fx.load(PTR, ring, ring::SQ_ARRAY);
    let slot_offset = fx.b.ins().imul_imm_s(index, 4);
    let slot = fx.b.ins().iadd(array, slot_offset);
    let index32 = fx.b.ins().ireduce(types::I32, index);
    fx.store(index32, slot, 0);
    let next = fx.b.ins().iadd_imm_s(tail, 1);
    fx.atomic_store(next, tail_address);
    enter(lo, fx, ring, 1, 0, 0);
}

/// Submits an entry the runtime builds itself, of an opcode on a descriptor with an address,
/// a length, an offset and flags, its completion reaching `op` when there is one.
#[allow(clippy::too_many_arguments)]
fn submit_built(
    lo: &mut Lowering,
    fx: &mut Fx,
    opcode: i64,
    fd: Value,
    address: Value,
    len: Value,
    offset: Value,
    flags: Value,
    op: Option<Value>,
) {
    let entry = fx.slot(64, 8);
    let zero = fx.ptr(0);
    let size = fx.ptr(64);
    lo.call_routine(fx, Routine::Fill, &[entry, zero, size]);
    let opcode = fx.int(types::I8, opcode);
    fx.store(opcode, entry, 0);
    fx.store(fd, entry, 4);
    fx.store(offset, entry, 8);
    fx.store(address, entry, 16);
    fx.store(len, entry, 24);
    fx.store(flags, entry, 28);
    submit(lo, fx, entry, op);
}

/// `io_uring_enter` on a ring.
fn enter(lo: &mut Lowering, fx: &mut Fx, ring: Value, submitted: i64, complete: i64, flags: i64) {
    let number = fx.int(PTR, uring::ENTER);
    let fd = fx.load(PTR, ring, ring::FD);
    let submitted = fx.int(PTR, submitted);
    let complete = fx.int(PTR, complete);
    let flags = fx.int(PTR, flags);
    let zero = fx.ptr(0);
    syscall(
        lo,
        fx,
        number,
        &[fd, submitted, complete, flags, zero, zero],
    );
}

/// Waits idle in the running thread's ring until a completion or a wake arrives, unless a task
/// is ready, then handles the completions.
pub fn idle(lo: &mut Lowering, fx: &mut Fx) {
    let current = lo.thread(fx);
    let idle = fx.offset(current, thread::IDLE);
    let one = fx.ptr(1);
    fx.atomic_store(one, idle);
    let state_address = lo.state(fx);
    let head = fx.offset(state_address, state::READY_HEAD);
    let head = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), head);
    let empty = fx.b.ins().icmp_imm_s(IntCC::Equal, head, 0);
    reap(lo, fx, Some(empty));
    let zero = fx.ptr(0);
    fx.atomic_store(zero, idle);
}

/// Handles the completions on the running thread's ring, first submitting the cancellations
/// other threads have asked of it and, when `wait` is given and nonzero, waiting for a
/// completion.
pub fn reap(lo: &mut Lowering, fx: &mut Fx, wait: Option<Value>) {
    let current = lo.thread(fx);
    let ring = fx.load(PTR, current, thread::RING);
    if let Some(wait) = wait {
        fx.when(wait, |fx| enter(lo, fx, ring, 0, 1, uring::GET_EVENTS));
    }
    submit_requested(lo, fx, current);
    let head_address = fx.load(PTR, ring, ring::CQ_HEAD);
    let tail_address = fx.load(PTR, ring, ring::CQ_TAIL);
    let mask_address = fx.load(PTR, ring, ring::CQ_MASK);
    let mask = fx.load(types::I32, mask_address, 0);
    let cqes = fx.load(PTR, ring, ring::CQES);
    let head = fx.load(types::I32, head_address, 0);
    let tail =
        fx.b.ins()
            .atomic_load(types::I32, MemFlagsData::trusted(), tail_address);
    let pending = fx.b.ins().icmp(IntCC::NotEqual, head, tail);
    fx.when(pending, |fx| {
        fx.repeat(&[head], |fx, state| {
            let head = state[0];
            let index = fx.b.ins().band(head, mask);
            let index = fx.b.ins().uextend(PTR, index);
            let offset = fx.b.ins().imul_imm_s(index, 16);
            let cqe = fx.b.ins().iadd(cqes, offset);
            let user = fx.load(PTR, cqe, 0);
            let result = fx.load(types::I32, cqe, 8);
            let next = fx.b.ins().iadd_imm_s(head, 1);
            fx.atomic_store(next, head_address);
            let is_operation = fx.b.ins().icmp_imm_s(IntCC::NotEqual, user, 0);
            fx.when(is_operation, |fx| complete(lo, fx, user, result));
            let more = fx.b.ins().icmp(IntCC::NotEqual, next, tail);
            (more, vec![next])
        });
    });
}

/// Delivers a completion to the operation it reaches, which leaves its thread's operations in
/// flight; the first termination request, passed on by the thread reading them, is in none.
fn complete(lo: &mut Lowering, fx: &mut Fx, op: Value, result: Value) {
    let kind = fx.load(PTR, op, operation::KIND);
    let signal = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, kind::SIGNAL);
    fx.choose_in(
        lo,
        signal,
        &[],
        |lo, fx| {
            termination(lo, fx, op);
            Vec::new()
        },
        |lo, fx| {
            let current = lo.thread(fx);
            let next = fx.load(PTR, op, operation::NEXT);
            let previous = fx.load(PTR, op, operation::PREVIOUS);
            let has_previous = fx.b.ins().icmp_imm_s(IntCC::NotEqual, previous, 0);
            fx.choose(
                has_previous,
                &[],
                |fx| {
                    fx.store(next, previous, operation::NEXT);
                    Vec::new()
                },
                |fx| {
                    fx.store(next, current, thread::OPERATIONS);
                    Vec::new()
                },
            );
            let has_next = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next, 0);
            fx.when(has_next, |fx| fx.store(previous, next, operation::PREVIOUS));
            tasks::deliver(lo, fx, op, result);
            Vec::new()
        },
    );
}

/// Submits a deadline's timeout of the relative time at `spec` on the running thread's ring.
pub fn submit_timeout(lo: &mut Lowering, fx: &mut Fx, op: Value, spec: Value) {
    let fd = fx.int(types::I32, -1);
    let len = fx.int(types::I32, 1);
    let zero = fx.ptr(0);
    let flags = fx.int(types::I32, 0);
    submit_built(lo, fx, uring::TIMEOUT, fd, spec, len, zero, flags, Some(op));
}

/// Submits the cancellations other threads have asked of the operations in flight on the
/// running thread's ring.
fn submit_requested(lo: &mut Lowering, fx: &mut Fx, current: Value) {
    let flag = fx.offset(current, thread::REQUESTS);
    let zero = fx.ptr(0);
    let asked =
        fx.b.ins()
            .atomic_rmw(PTR, MemFlagsData::trusted(), AtomicRmwOp::Xchg, flag, zero);
    let asked = fx.b.ins().icmp_imm_s(IntCC::NotEqual, asked, 0);
    fx.when(asked, |fx| {
        let head = fx.load(PTR, current, thread::OPERATIONS);
        fx.repeat(&[head], |fx, state| {
            let op = state[0];
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, op, 0);
            let next = fx.choose_in(
                lo,
                present,
                &[PTR],
                |lo, fx| {
                    let cancel = fx.offset(op, operation::CANCEL);
                    let cancel = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), cancel);
                    let requested =
                        fx.b.ins()
                            .icmp_imm_s(IntCC::Equal, cancel, operation::REQUESTED);
                    fx.when(requested, |fx| submit_cancel(lo, fx, op));
                    vec![fx.load(PTR, op, operation::NEXT)]
                },
                |_, fx| vec![fx.ptr(0)],
            );
            (present, next)
        });
    });
}

/// Submits the cancellation of an operation in flight on the running thread's ring.
fn submit_cancel(lo: &mut Lowering, fx: &mut Fx, op: Value) {
    let fd = fx.int(types::I32, -1);
    let len = fx.int(types::I32, 0);
    let zero = fx.ptr(0);
    let flags = fx.int(types::I32, 0);
    submit_built(lo, fx, uring::ASYNC_CANCEL, fd, op, len, zero, flags, None);
    let submitted = fx.ptr(operation::SUBMITTED as u64);
    let word = fx.offset(op, operation::CANCEL);
    fx.atomic_store(submitted, word);
}

/// Asks once for the cancellation of an operation whose record stays valid during the call:
/// submitted at once when the running thread's ring holds it and it has not completed,
/// otherwise asked of the thread whose ring holds it.
pub fn cancel_operation(lo: &mut Lowering, fx: &mut Fx, op: Value) {
    let flag = fx.offset(op, operation::CANCEL);
    let zero = fx.ptr(0);
    let requested = fx.ptr(operation::REQUESTED as u64);
    let previous =
        fx.b.ins()
            .atomic_cas(MemFlagsData::trusted(), flag, zero, requested);
    let first = fx.b.ins().icmp_imm_s(IntCC::Equal, previous, 0);
    fx.when(first, |fx| {
        let owner = fx.load(PTR, op, operation::THREAD);
        let current = lo.thread(fx);
        let here = fx.b.ins().icmp(IntCC::Equal, owner, current);
        fx.choose_in(
            lo,
            here,
            &[],
            |lo, fx| {
                let done = fx.offset(op, operation::DONE);
                let done = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), done);
                let in_flight = fx.b.ins().icmp_imm_s(IntCC::Equal, done, 0);
                fx.when(in_flight, |fx| submit_cancel(lo, fx, op));
                Vec::new()
            },
            |lo, fx| {
                let flag = fx.offset(owner, thread::REQUESTS);
                let one = fx.ptr(1);
                fx.atomic_store(one, flag);
                send_wake(lo, fx, owner);
                Vec::new()
            },
        );
    });
}

/// Wakes another executor thread waiting in its ring with a message on it.
fn send_wake(lo: &mut Lowering, fx: &mut Fx, target: Value) {
    let ring = fx.load(PTR, target, thread::RING);
    let fd = fx.load(PTR, ring, ring::FD);
    let fd = fx.b.ins().ireduce(types::I32, fd);
    let zero = fx.ptr(0);
    let len = fx.int(types::I32, 0);
    let flags = fx.int(types::I32, 0);
    submit_built(lo, fx, uring::MSG_RING, fd, zero, len, zero, flags, None);
}

/// Wakes one executor thread waiting idle in its ring, other than the running one, so a task
/// made ready runs.
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
                let one = fx.ptr(1);
                let zero = fx.ptr(0);
                let woken = fx.choose(
                    other,
                    &[types::I8],
                    |fx| {
                        let seen =
                            fx.b.ins()
                                .atomic_cas(MemFlagsData::trusted(), idle, one, zero);
                        vec![fx.b.ins().icmp_imm_s(IntCC::Equal, seen, 1)]
                    },
                    |fx| vec![fx.int(types::I8, 0)],
                )[0];
                fx.when(woken, |fx| send_wake(lo, fx, record));
                let next = fx.b.ins().iadd_imm_s(index, 1);
                vec![next, fx.b.ins().bxor_imm_s(woken, 1)]
            },
            |_, fx| vec![index, fx.int(types::I8, 0)],
        );
        (next[1], vec![next[0]])
    });
}

/// A system call through the C library's `syscall`, its arguments padded to six words.
fn syscall(lo: &mut Lowering, fx: &mut Fx, number: Value, args: &[Value]) -> Value {
    let mut words = vec![number];
    words.extend_from_slice(args);
    while words.len() < 7 {
        words.push(fx.ptr(0));
    }
    lo.call_libc(fx, "syscall", &words, &[PTR])[0]
}

/// The linker of a host target: `cc` from the `PATH` when the machine running the toolchain has
/// the target's architecture, and the GNU cross compiler for that architecture otherwise.
pub fn linker(target: Target) -> &'static str {
    if target.arch() == Some(Arch::running()) {
        return "cc";
    }
    match target.arch() {
        Some(Arch::Aarch64) => "aarch64-linux-gnu-gcc",
        _ => "x86_64-linux-gnu-gcc",
    }
}

/// Links an object into a position-independent executable at `image` with the host target's
/// linker; the linker's output when it fails.
pub fn link(target: Target, object: &Path, image: &Path) -> Result<(), String> {
    let output = Command::new(linker(target))
        .arg("-pie")
        .arg("-o")
        .arg(image)
        .arg(object)
        .output();
    match output {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            Err(text)
        }
        Err(error) => Err(error.to_string()),
    }
}
