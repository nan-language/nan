//! What the Linux and macOS targets share as systems whose C library calls into them: the
//! program entry seam `main`, which the system's start-up code calls; the runtime's records, exit
//! and standard stream writes; the memory of chunks and task stacks with their guard regions;
//! each thread's signal stack; and the handlers of the stop signal, of faults and of an exceeded
//! processor time limit. Each system's module gives the values and layouts its C library has.

use cranelift_codegen::ir::{AbiParam, InstBuilder, Signature, TrapCode, Value, condcodes::IntCC};
use cranelift_codegen::ir::{types, types::I32};
use cranelift_module::{FuncId, Linkage, Module};

use super::super::fx::{Fx, PTR};
use super::super::runtime::system::{self, unavailable_unless};
use super::super::runtime::tasks::{self, thread};
use super::super::runtime::{Routine, memory, state};
use super::super::{CALL_CONV, Lowering, Program, lines};
use super::Context;
use crate::workspace::Kind;

/// The symbol of the program entry seam, which the system's start-up code calls with the
/// program's arguments and environment.
pub const ENTRY: &str = "main";

/// What the C libraries of the systems lay out or number apart where the routines they share
/// call them.
pub struct Posix {
    /// The `mmap` flags of private anonymous memory, and of a task stack.
    pub map_anonymous: i64,
    pub map_stack: i64,
    /// The signal asking a thread to stop the task it runs, the signals of a fault in a stack's
    /// guard region, and that of an exceeded processor time limit.
    pub stop: i32,
    pub faults: &'static [i32],
    pub cpu_limit: i32,
    /// The size of a `struct sigaction`, the offset of its flags, and the flags of the runtime's
    /// handlers: given a `siginfo_t` and a context, run on the thread's signal stack, and
    /// restarting interrupted system calls.
    pub sigaction: u64,
    pub sigaction_flags: u64,
    pub siginfo: i64,
    pub on_stack: i64,
    pub restart: i64,
    /// The offsets of the size and flags of a `stack_t`.
    pub stack_size: u64,
    pub stack_flags: u64,
    /// The offset of the fault address in a `siginfo_t`.
    pub fault_address: u64,
}

/// The values and layouts of the target's system.
fn posix(lo: &Lowering) -> &'static Posix {
    match lo.ck.db.target.kind() {
        Some(Kind::Macos) => &super::macos::POSIX,
        _ => &super::linux::POSIX,
    }
}

/// The signal asking a thread of the target's system to stop the task it runs.
pub fn stop_signal(lo: &Lowering) -> i32 {
    posix(lo).stop
}

const MAP_FAILED: i64 = -1;
const PROTECT_READ_WRITE: i64 = 3;
const PROTECT_NONE: i64 = 0;

/// The resource limit of processor time, and the seconds a test run may use.
const RLIMIT_CPU: i64 = 0;
const CPU_SECONDS: u64 = 10;

/// The size of each thread's signal stack and of its stack for stack overflow reports.
const SIGNAL_STACK: u64 = 65_536;

const SIGPIPE: i64 = 13;

/// The termination request signals: a hang-up, an interrupt and a termination.
pub const SIGNALS: [i64; 3] = [1, 2, 15];

/// The signature of the program entry seam `main` and of a test image's.
fn main_signature() -> Signature {
    let mut signature = Signature::new(CALL_CONV);
    signature.params.push(AbiParam::new(I32));
    signature.params.push(AbiParam::new(PTR));
    signature.params.push(AbiParam::new(PTR));
    signature.returns.push(AbiParam::new(I32));
    signature
}

/// Defines the program entry seam `main` and the glue running the entry function as the first
/// task.
pub fn entry_seam(lo: &mut Lowering, program: &Program) {
    let entry = super::entry_task(lo, program);
    let signature = main_signature();
    let id = lo
        .module
        .declare_function(ENTRY, Linkage::Export, &signature)
        .expect("the program entry seam");
    lo.define(id, signature, lines::HIDDEN | lines::CRITICAL, |lo, fx| {
        let params = fx.entry();
        lo.check_level(fx);
        system::start(lo, fx);
        super::run_entry(lo, fx, program, entry, &params);
    });
}

/// Defines the entry seam `main` of a test image, whose arguments after the program's name are
/// the texts naming its run.
pub fn test_seam(lo: &mut Lowering, entry: FuncId) {
    let signature = main_signature();
    let id = lo
        .module
        .declare_function(ENTRY, Linkage::Export, &signature)
        .expect("the test entry seam");
    lo.define(id, signature, lines::HIDDEN | lines::CRITICAL, |lo, fx| {
        let params = fx.entry();
        system::start(lo, fx);
        let count = fx.b.ins().uextend(PTR, params[0]);
        let count = fx.b.ins().iadd_imm_s(count, -1);
        let texts = fx.b.ins().iadd_imm_s(params[1], 8);
        super::super::test_image::start_run(lo, fx, entry, count, texts);
    });
}

/// Writes a byte range to a descriptor, whole unless the descriptor refuses.
pub fn write_all(lo: &mut Lowering, fx: &mut Fx, fd: i64, text: Value, len: Value) {
    let fd = fx.int(I32, fd);
    fx.repeat(&[text, len], |fx, state| {
        let (at, left) = (state[0], state[1]);
        let written = lo.call_libc(fx, "write", &[fd, at, left], &[PTR])[0];
        let progressed = fx.b.ins().icmp_imm_s(IntCC::SignedGreaterThan, written, 0);
        let next_at = fx.b.ins().iadd(at, written);
        let next_left = fx.b.ins().isub(left, written);
        let zero = fx.ptr(0);
        let next_left = fx.b.ins().select(progressed, next_left, zero);
        let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next_left, 0);
        (more, vec![next_at, next_left])
    });
}

/// Defines the record routines over the C library's heap, `Exit`, and the handlers of the stop
/// signal, of faults and of an exceeded processor time limit; the system's module defines how
/// termination requests are read.
pub fn define(lo: &mut Lowering, fx: &mut Fx, routine: Routine) {
    let params = fx.entry();
    match routine {
        Routine::RecordAlloc => {
            let one = fx.ptr(1);
            let address = lo.call_libc(fx, "calloc", &[one, params[0]], &[PTR])[0];
            fx.b.ins().return_(&[address]);
        }
        Routine::RecordFree => {
            lo.call_libc(fx, "free", &[params[0]], &[]);
            fx.b.ins().return_(&[]);
        }
        Routine::RecordResize => {
            let address = lo.call_libc(fx, "realloc", &[params[0], params[1]], &[PTR])[0];
            fx.b.ins().return_(&[address]);
        }
        Routine::Exit => {
            let status = fx.b.ins().ireduce(I32, params[0]);
            lo.call_libc(fx, "_exit", &[status], &[]);
            fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
        }
        Routine::Preempt => preempt(lo, fx, params[2]),
        Routine::Terminations if lo.macos() => super::macos::terminations(lo, fx, params[0]),
        Routine::Terminations => super::linux::terminations(lo, fx, params[0]),
        Routine::Overflow => overflow(lo, fx, &params),
        Routine::CpuLimit => {
            let status = fx.int(I32, i64::from(super::super::test_image::TIMED_OUT));
            lo.call_libc(fx, "_exit", &[status], &[]);
            fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
        }
        _ => unreachable!("a host routine"),
    }
}

/// The address of the registers a handler's context saved, and their offsets there.
fn registers(lo: &mut Lowering, fx: &mut Fx, context: Value) -> (Value, Context) {
    match lo.macos() {
        true => super::macos::registers(lo, fx, context),
        false => (context, super::linux::context(lo.ck.db.target)),
    }
}

/// Gives the running thread a signal stack and a stack for stack overflow reports, recording it
/// in its record; nonzero when the system refuses one.
pub fn thread_signals(lo: &mut Lowering, fx: &mut Fx, record: Value) -> Value {
    let posix = posix(lo);
    let pthread = lo.call_libc(fx, "pthread_self", &[], &[PTR])[0];
    fx.store(pthread, record, thread::SYSTEM);
    let size = fx.ptr(SIGNAL_STACK);
    let signal_stack = map_stack(lo, fx, size, true);
    let trap_stack = map_stack(lo, fx, size, true);
    let no_signal = fx.b.ins().icmp_imm_s(IntCC::Equal, signal_stack, 0);
    let no_trap = fx.b.ins().icmp_imm_s(IntCC::Equal, trap_stack, 0);
    let refused = fx.b.ins().bor(no_signal, no_trap);
    fx.choose_in(
        lo,
        refused,
        &[types::I8],
        |_, fx| vec![fx.int(types::I8, 1)],
        |lo, fx| {
            let top = fx.b.ins().iadd_imm_s(trap_stack, SIGNAL_STACK as i64);
            fx.store(top, record, thread::TRAP_STACK);
            let stack = fx.slot(24, 8);
            fx.store(signal_stack, stack, 0);
            let zero = fx.ptr(0);
            fx.store(zero, stack, posix.stack_flags);
            fx.store(size, stack, posix.stack_size);
            lo.call_libc(fx, "sigaltstack", &[stack, zero], &[I32]);
            vec![fx.int(types::I8, 0)]
        },
    )[0]
}

/// Installs a routine as the handler of a signal, run on the thread's signal stack.
pub fn install_handler(
    lo: &mut Lowering,
    fx: &mut Fx,
    signal: i32,
    routine: Routine,
    restart: bool,
) {
    let posix = posix(lo);
    let action = fx.slot(posix.sigaction, 8);
    let zero = fx.ptr(0);
    let size = fx.ptr(posix.sigaction);
    lo.call_routine(fx, Routine::Fill, &[action, zero, size]);
    let handler = lo.routine(routine);
    let handler = lo.func_ref(fx, handler);
    let handler = fx.b.ins().func_addr(PTR, handler);
    fx.store(handler, action, 0);
    let restart = if restart { posix.restart } else { 0 };
    let flags = fx.int(I32, posix.siginfo | posix.on_stack | restart);
    fx.store(flags, action, posix.sigaction_flags);
    let signal = fx.int(I32, i64::from(signal));
    lo.call_libc(fx, "sigaction", &[signal, action, zero], &[I32]);
}

/// Installs the handlers of the stop signal and of faults, and ignores `SIGPIPE`.
pub fn install_handlers(lo: &mut Lowering, fx: &mut Fx) {
    let posix = posix(lo);
    install_handler(lo, fx, posix.stop, Routine::Preempt, true);
    for fault in posix.faults {
        install_handler(lo, fx, *fault, Routine::Overflow, false);
    }
    ignore_broken_pipes(lo, fx);
}

/// Ignores `SIGPIPE`, so that a write to a closed pipe or connection fails instead.
fn ignore_broken_pipes(lo: &mut Lowering, fx: &mut Fx) {
    let ignore = fx.ptr(1);
    let pipe = fx.int(I32, SIGPIPE);
    lo.call_libc(fx, "signal", &[pipe, ignore], &[PTR]);
}

/// `Preempt(signal, info, context)`: the handler of the stop signal, which stops the task the
/// thread runs when it may stop there.
fn preempt(lo: &mut Lowering, fx: &mut Fx, context: Value) {
    let thread = lo.thread(fx);
    let inside = fx.b.ins().icmp_imm_s(IntCC::NotEqual, thread, 0);
    fx.when(inside, |fx| {
        let (saved, ctx) = registers(lo, fx, context);
        super::stop_interrupted(lo, fx, thread, saved, ctx);
    });
    fx.b.ins().return_(&[]);
}

/// `Overflow(signal, info, context)`: the handler of a fault. A fault in the guard region of
/// the running task's stack resumes in `OverflowRaise` on the thread's trap stack; any other
/// fault restores the signal's default action and faults again.
fn overflow(lo: &mut Lowering, fx: &mut Fx, params: &[Value]) {
    let fault = fx.load(PTR, params[1], posix(lo).fault_address);
    let thread = lo.thread(fx);
    let (saved, ctx) = registers(lo, fx, params[2]);
    let guarded = super::overflow_interrupted(lo, fx, thread, fault, saved, ctx);
    let other = fx.b.ins().bxor_imm_s(guarded, 1);
    fx.when(other, |fx| {
        let signal = match lo.macos() {
            true => params[0],
            false => fx.int(I32, i64::from(super::linux::SIGSEGV)),
        };
        let default = fx.ptr(0);
        lo.call_libc(fx, "signal", &[signal, default], &[PTR]);
    });
    fx.b.ins().return_(&[]);
}

/// The executor of `Init` for the seeded scheduler: the calling thread is the one executor
/// thread, which `make_current` makes the record's; a fault reports a stack overflow, and the
/// run ends with status `TIMED_OUT` once it has used 10 seconds of processor time.
pub fn start_seeded(
    lo: &mut Lowering,
    fx: &mut Fx,
    make_current: impl FnOnce(&mut Lowering, &mut Fx, Value),
) {
    let posix = posix(lo);
    let state_address = lo.state(fx);
    let one = fx.ptr(1);
    fx.store(one, state_address, state::THREAD_COUNT);
    let eight = fx.ptr(8);
    let threads = lo.call_libc(fx, "calloc", &[one, eight], &[PTR])[0];
    unavailable_unless(fx, threads);
    fx.store(threads, state_address, state::THREADS);
    let size = fx.ptr(thread::SIZE);
    let record = lo.call_libc(fx, "calloc", &[one, size], &[PTR])[0];
    unavailable_unless(fx, record);
    fx.store(record, threads, 0);
    make_current(lo, fx, record);
    let refused = thread_signals(lo, fx, record);
    fx.when(refused, |fx| {
        let one = fx.ptr(1);
        fx.b.ins().return_(&[one]);
    });
    for fault in posix.faults {
        install_handler(lo, fx, *fault, Routine::Overflow, false);
    }
    install_handler(lo, fx, posix.cpu_limit, Routine::CpuLimit, false);
    let limit = fx.slot(16, 8);
    let soft = fx.ptr(CPU_SECONDS);
    let hard = fx.ptr(CPU_SECONDS + 2);
    fx.store(soft, limit, 0);
    fx.store(hard, limit, 8);
    let cpu = fx.int(I32, RLIMIT_CPU);
    lo.call_libc(fx, "setrlimit", &[cpu, limit], &[I32]);
    ignore_broken_pipes(lo, fx);
}

/// Maps `count` chunks aligned to their size, zero when the system refuses.
pub fn map_chunks(lo: &mut Lowering, fx: &mut Fx, count: Value) -> Value {
    let len = fx.b.ins().imul_imm_s(count, memory::CHUNK as i64);
    let padded = fx.b.ins().iadd_imm_s(len, memory::CHUNK as i64);
    let zero = fx.ptr(0);
    let protection = fx.int(I32, PROTECT_READ_WRITE);
    let flags = fx.int(I32, posix(lo).map_anonymous);
    let no_fd = fx.int(I32, -1);
    let base = lo.call_libc(
        fx,
        "mmap",
        &[zero, padded, protection, flags, no_fd, zero],
        &[PTR],
    )[0];
    let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, base, MAP_FAILED);
    fx.choose_in(
        lo,
        failed,
        &[PTR],
        |_, fx| vec![fx.ptr(0)],
        |lo, fx| {
            let bumped = fx.b.ins().iadd_imm_s(base, memory::CHUNK as i64 - 1);
            let aligned = fx.b.ins().band_imm_s(bumped, !(memory::CHUNK as i64 - 1));
            let head = fx.b.ins().isub(aligned, base);
            let has_head = fx.b.ins().icmp_imm_s(IntCC::NotEqual, head, 0);
            fx.when(has_head, |fx| unmap(lo, fx, base, head));
            let end = fx.b.ins().iadd(aligned, len);
            let mapped_end = fx.b.ins().iadd(base, padded);
            let tail = fx.b.ins().isub(mapped_end, end);
            let has_tail = fx.b.ins().icmp_imm_s(IntCC::NotEqual, tail, 0);
            fx.when(has_tail, |fx| unmap(lo, fx, end, tail));
            vec![aligned]
        },
    )[0]
}

/// Unmaps memory.
pub fn unmap(lo: &mut Lowering, fx: &mut Fx, address: Value, len: Value) {
    lo.call_libc(fx, "munmap", &[address, len], &[I32]);
}

/// Maps a stack of `len` bytes, its lowest page an inaccessible guard when `guarded`, zero when
/// the system refuses.
pub fn map_stack(lo: &mut Lowering, fx: &mut Fx, len: Value, guarded: bool) -> Value {
    let zero = fx.ptr(0);
    let protection = fx.int(I32, PROTECT_READ_WRITE);
    let flags = fx.int(I32, posix(lo).map_stack);
    let no_fd = fx.int(I32, -1);
    let base = lo.call_libc(
        fx,
        "mmap",
        &[zero, len, protection, flags, no_fd, zero],
        &[PTR],
    )[0];
    let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, base, MAP_FAILED);
    fx.choose_in(
        lo,
        failed,
        &[PTR],
        |_, fx| vec![fx.ptr(0)],
        |lo, fx| {
            if guarded {
                protect_guard(lo, fx, base, false);
            }
            vec![base]
        },
    )[0]
}

/// Makes the guard region at the base of a stack inaccessible, or accessible again.
pub fn protect_guard(lo: &mut Lowering, fx: &mut Fx, base: Value, accessible: bool) {
    let guard = fx.ptr(tasks::GUARD);
    let protection = match accessible {
        true => PROTECT_READ_WRITE,
        false => PROTECT_NONE,
    };
    let protection = fx.int(I32, protection);
    lo.call_libc(fx, "mprotect", &[base, guard, protection], &[I32]);
}

#[cfg(all(test, unix))]
mod tests;
