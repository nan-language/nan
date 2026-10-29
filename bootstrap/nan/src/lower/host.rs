//! The host targets: everything Linux-specific. The program entry seam `main`, which the C
//! library's start-up code calls; the executor threads, one per processor of the process's
//! affinity set, each with its io_uring; termination requests, which a thread of their own reads
//! from a `signalfd`; the support routines' calls into the system; and linking the image with the
//! host target's linker.
//!
//! An operation record is what a completion reaches: the task waiting on it, its result, how far its completion has been delivered, the
//! thread whose ring holds it, how far its cancellation has gone, its kind and its links in its
//! thread's list. Only the thread whose ring holds an operation lists it, delivers its
//! completion and submits its cancellation, so an operation another thread asks to cancel is
//! cancelled only while it is in flight.

use std::path::Path;
use std::process::Command;

use cranelift_codegen::ir::{
    AbiParam, AtomicRmwOp, InstBuilder, MemFlagsData, Signature, TrapCode, Value, condcodes::IntCC,
    types,
};
use cranelift_module::{Linkage, Module};

use super::abi::Returned;
use super::fx::{Fx, PTR};
use super::runtime::system::{self, unavailable_unless};
use super::runtime::tasks::{self, kind, operation, task, thread};
use super::runtime::{Routine, memory, state};
use super::{CALL_CONV, Instance, Lowering, Program, lines};
use crate::nir::{Role, Span};
use crate::workspace::{Arch, Target};

mod mmap {
    pub const READ_WRITE: i64 = 3;
    pub const NONE: i64 = 0;
    pub const PRIVATE_ANONYMOUS: i64 = 0x22;
    pub const STACK: i64 = 0x2_4022;
    pub const SHARED_POPULATE: i64 = 0x8001;
    pub const FAILED: i64 = -1;
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

/// The signal asking a thread to stop the task it runs, the signal of a fault, and that of an
/// exceeded processor time limit.
pub const SIGURG: i32 = 23;
const SIGSEGV: i32 = 11;
const SIGXCPU: i32 = 24;

/// The resource limit of processor time, and the seconds a test run may use.
const RLIMIT_CPU: i64 = 0;
const CPU_SECONDS: u64 = 10;

/// The `sigaction` flags of the runtime's handlers: given a `siginfo_t` and a context, run on
/// the thread's signal stack, and restarting interrupted system calls.
const SA_SIGINFO: i64 = 4;
const SA_ONSTACK: i64 = 0x0800_0000;
const SA_RESTART: i64 = 0x1000_0000;

/// The size of a `struct sigaction` and the offset of its flags.
const SIGACTION: u64 = 152;
const SIGACTION_FLAGS: u64 = 136;

/// The offset of the fault address in a `siginfo_t`.
const FAULT_ADDRESS: u64 = 16;

/// The offsets in a `ucontext_t` of the registers a stop or fault handler reads and redirects:
/// the register a redirected call passes its argument in, the frame and stack pointers, the
/// instruction pointer, and the link register a leaf's return address lives in where the
/// architecture has one.
#[derive(Clone, Copy)]
struct Context {
    arg: u64,
    fp: u64,
    sp: u64,
    ip: u64,
    link: Option<u64>,
}

impl Context {
    fn of(target: Target) -> Context {
        match target.arch().expect("a host target's architecture") {
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
}

/// The size of each thread's signal stack and of its stack for stack overflow reports.
const SIGNAL_STACK: u64 = 65_536;

/// The termination request signals and the length of a `signalfd_siginfo`.
const SIGNALS: [i64; 3] = [1, 2, 15];
const SIGINFO: u64 = 128;
const SIGPIPE: i64 = 13;

/// Defines the program entry seam `main` and the glue running the entry function as the first
/// task.
pub fn entry_seam(lo: &mut Lowering, program: &Program) {
    let glue = entry_glue(lo, program);
    let stack = lo
        .ck
        .fn_sig(program.entry)
        .and_then(|sig| sig.limits.stack)
        .unwrap_or(tasks::ENTRY_STACK);
    let head = Span::at(lo.ck.db.item(program.entry).ast.token);
    lo.task(glue, program.entry, head, Some(stack));
    let mut signature = Signature::new(CALL_CONV);
    signature.params.push(AbiParam::new(types::I32));
    signature.params.push(AbiParam::new(PTR));
    signature.params.push(AbiParam::new(PTR));
    signature.returns.push(AbiParam::new(types::I32));
    let id = lo
        .module
        .declare_function("main", Linkage::Export, &signature)
        .expect("the program entry seam");
    lo.define(id, signature, lines::HIDDEN | lines::CRITICAL, |lo, fx| {
        let params = fx.entry();
        let failed = lo.call_routine(fx, Routine::Init, &[])[0];
        let unavailable = fx.b.ins().icmp_imm_s(IntCC::NotEqual, failed, 0);
        fx.when(unavailable, |fx| system::unavailable(lo, fx));
        let world_ty = lo.std_ty("host", "World", Vec::new());
        let layout = lo.layout(&world_ty);
        let world = fx.slot(layout.size, layout.align);
        let landing = lo.instance(Instance::Nir {
            item: program.landing,
            role: Role::Body,
            index: 0,
            args: crate::check::ty::no_args(),
        });
        let reference = lo.func_ref(fx, landing);
        fx.b.ins()
            .call(reference, &[world, params[0], params[1], params[2]]);
        let env_size = fx.ptr(layout.size.max(8));
        let env = lo.call_routine(fx, Routine::TaskEnv, &[env_size])[0];
        lo.copy(fx, env, world, layout.size, layout.align);
        let glue_ref = lo.func_ref(fx, glue);
        let glue_address = fx.b.ins().func_addr(PTR, glue_ref);
        let state_address = lo.state(fx);
        let process = fx.load(PTR, state_address, state::PROCESS);
        let root = fx.load(PTR, state_address, state::ROOT);
        let zero = fx.ptr(0);
        let stack = fx.ptr(stack);
        let record = lo.call_routine(
            fx,
            Routine::Spawn,
            &[glue_address, zero, env, zero, stack, zero, process, root],
        )[0];
        let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
        fx.when(missing, |fx| system::unavailable(lo, fx));
        fx.store(record, state_address, state::ENTRY_TASK);
        lo.call_routine(fx, Routine::Wake, &[record]);
        lo.call_routine(fx, Routine::Schedule, &[]);
        fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
    });
}

/// Defines the entry seam `main` of a test image, whose arguments after the program's name are
/// the texts naming its run.
pub fn test_seam(lo: &mut Lowering, entry: cranelift_module::FuncId) {
    let mut signature = Signature::new(CALL_CONV);
    signature.params.push(AbiParam::new(types::I32));
    signature.params.push(AbiParam::new(PTR));
    signature.params.push(AbiParam::new(PTR));
    signature.returns.push(AbiParam::new(types::I32));
    let id = lo
        .module
        .declare_function("main", Linkage::Export, &signature)
        .expect("the test entry seam");
    lo.define(id, signature, lines::HIDDEN | lines::CRITICAL, |lo, fx| {
        let params = fx.entry();
        let count = fx.b.ins().uextend(PTR, params[0]);
        let count = fx.b.ins().iadd_imm_s(count, -1);
        let texts = fx.b.ins().iadd_imm_s(params[1], 8);
        super::test_image::start_run(lo, fx, entry, count, texts);
    });
}

/// Writes a byte range to a descriptor, whole unless the descriptor refuses.
pub fn write_all(lo: &mut Lowering, fx: &mut Fx, fd: i64, text: Value, len: Value) {
    let fd = fx.int(types::I32, fd);
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

/// The glue the entry task runs: calls the entry function with the root grant, then ends the
/// process with the status its outcome gives.
fn entry_glue(lo: &mut Lowering, program: &Program) -> cranelift_module::FuncId {
    let mut signature = Signature::new(CALL_CONV);
    signature.params.push(AbiParam::new(PTR));
    let id = lo
        .module
        .declare_function("_NSstack_switch_entry", Linkage::Local, &signature)
        .expect("the entry glue");
    let main = Instance::Nir {
        item: program.entry,
        role: lo.entry_role(program.entry),
        index: 0,
        args: crate::check::ty::no_args(),
    };
    let abi = lo.abi(&main);
    let main = lo.instance(main);
    lo.define(id, signature, lines::HIDDEN, |lo, fx| {
        let params = fx.entry();
        let record = params[0];
        let world = fx.load(PTR, record, task::ENV);
        let failure_ty = abi.fails.clone().expect("an entry function that may fail");
        let failure_layout = lo.layout(&failure_ty);
        let failure = fx.slot(failure_layout.size, failure_layout.align);
        let reference = lo.func_ref(fx, main);
        let call = fx.b.ins().call(reference, &[failure, world]);
        let results = fx.b.inst_results(call).to_vec();
        let state_address = lo.state(fx);
        let termination = fx.load(PTR, state_address, state::TERMINATION);
        let requested = fx.b.ins().icmp_imm_s(IntCC::NotEqual, termination, 0);
        fx.when(requested, |fx| {
            let status = fx.b.ins().iadd_imm_s(termination, 128);
            lo.call_routine(fx, Routine::Exit, &[status]);
        });
        let failed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, results[0], 0);
        fx.when(failed, |fx| {
            write_err_literal(lo, fx, "error: ");
            let failure = lo.held_at(fx, &failure_ty, failure);
            let (text, len) =
                super::supplied::render::render_value(lo, fx, &failure_ty, failure, 4_096);
            lo.call_routine(fx, Routine::WriteErr, &[text, len]);
            write_err_literal(lo, fx, "\n");
            let status = fx.ptr(1);
            lo.call_routine(fx, Routine::Exit, &[status]);
        });
        let status = match abi.returned {
            Returned::Scalar(_) => fx.b.ins().uextend(PTR, results[1]),
            _ => fx.ptr(0),
        };
        lo.call_routine(fx, Routine::Exit, &[status]);
        fx.b.ins().return_(&[]);
    });
    id
}

/// Writes a literal text to standard error.
fn write_err_literal(lo: &mut Lowering, fx: &mut Fx, text: &str) {
    let view = lo.view_literal(fx, text.as_bytes()).address();
    let start = fx.load(PTR, view, 0);
    let len = fx.load(PTR, view, 8);
    lo.call_routine(fx, Routine::WriteErr, &[start, len]);
}

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
            let status = fx.b.ins().ireduce(types::I32, params[0]);
            lo.call_libc(fx, "_exit", &[status], &[]);
            fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
        }
        Routine::Preempt => preempt(lo, fx, params[2]),
        Routine::Terminations => terminations(lo, fx, params[0]),
        Routine::Overflow => overflow(lo, fx, params[1], params[2]),
        Routine::CpuLimit => {
            let status = fx.int(types::I32, i64::from(super::test_image::TIMED_OUT));
            lo.call_libc(fx, "_exit", &[status], &[]);
            fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
        }
        _ => unreachable!("a host routine"),
    }
}

/// Records the running thread in its record and gives it a signal stack and a stack for stack
/// overflow reports; nonzero when the system refuses one.
pub fn thread_signals(lo: &mut Lowering, fx: &mut Fx, record: Value) -> Value {
    let pthread = lo.call_libc(fx, "pthread_self", &[], &[PTR])[0];
    fx.store(pthread, record, thread::PTHREAD);
    let size = fx.ptr(SIGNAL_STACK);
    let signal_stack = map_stack(lo, fx, size);
    let trap_stack = map_stack(lo, fx, size);
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
            fx.store(zero, stack, 8);
            fx.store(size, stack, 16);
            lo.call_libc(fx, "sigaltstack", &[stack, zero], &[types::I32]);
            vec![fx.int(types::I8, 0)]
        },
    )[0]
}

/// Installs a routine as the handler of a signal, run on the thread's signal stack.
fn install_handler(lo: &mut Lowering, fx: &mut Fx, signal: i32, routine: Routine, restart: bool) {
    let action = fx.slot(SIGACTION, 8);
    let zero = fx.ptr(0);
    let size = fx.ptr(SIGACTION);
    lo.call_routine(fx, Routine::Fill, &[action, zero, size]);
    let handler = lo.routine(routine);
    let handler = lo.func_ref(fx, handler);
    let handler = fx.b.ins().func_addr(PTR, handler);
    fx.store(handler, action, 0);
    let restart = if restart { SA_RESTART } else { 0 };
    let flags = fx.int(types::I32, SA_SIGINFO | SA_ONSTACK | restart);
    fx.store(flags, action, SIGACTION_FLAGS);
    let signal = fx.int(types::I32, i64::from(signal));
    lo.call_libc(fx, "sigaction", &[signal, action, zero], &[types::I32]);
}

/// Whether a code address lies in a function a stopping task does not stop inside; an address
/// outside lowered code counts as one when `unknown` holds.
fn critical_at(lo: &mut Lowering, fx: &mut Fx, address: Value, unknown: bool) -> Value {
    let (start, flags) = super::runtime::report::function_at(lo, fx, address);
    let found = fx.b.ins().icmp_imm_s(IntCC::NotEqual, start, 0);
    let critical = fx.b.ins().band_imm_s(flags, i64::from(lines::CRITICAL));
    let critical = fx.b.ins().icmp_imm_s(IntCC::NotEqual, critical, 0);
    let outside = fx.int(types::I8, i64::from(unknown));
    fx.b.ins().select(found, critical, outside)
}

/// `Preempt(signal, info, context)`: the handler of the stop signal. When the task the thread
/// runs is asked to stop, holds no lock and has no frame of a function it does not stop inside,
/// the handler makes it resume in `Stop` on its own stack, below the interrupted frame; otherwise
/// the task runs on and the stop signal comes again.
fn preempt(lo: &mut Lowering, fx: &mut Fx, context: Value) {
    let ctx = Context::of(lo.ck.db.target);
    let thread = lo.thread(fx);
    let outside = fx.b.ins().icmp_imm_s(IntCC::Equal, thread, 0);
    fx.when(outside, |fx| {
        fx.b.ins().return_(&[]);
    });
    let record = fx.load(PTR, thread, thread::CURRENT);
    let idle = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
    fx.when(idle, |fx| {
        fx.b.ins().return_(&[]);
    });
    let asked = tasks::stop_asked(fx, record);
    let locks = fx.load(PTR, thread, thread::LOCKS);
    let unlocked = fx.b.ins().icmp_imm_s(IntCC::Equal, locks, 0);
    let stopping = fx.b.ins().band(asked, unlocked);
    let ip = fx.load(PTR, context, ctx.ip);
    let sp = fx.load(PTR, context, ctx.sp);
    let fp = fx.load(PTR, context, ctx.fp);
    let low = fx.load(PTR, record, task::GUARD);
    let base = fx.load(PTR, record, task::STACK);
    let size = fx.load(PTR, record, task::STACK_SIZE);
    let high = fx.b.ins().iadd(base, size);
    let above = fx.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, sp, low);
    let room = fx.b.ins().iadd_imm_s(sp, 16);
    let below = fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, room, high);
    let on_stack = fx.b.ins().band(above, below);
    let stopping = fx.b.ins().band(stopping, on_stack);
    fx.when(stopping, |fx| {
        let in_ip = critical_at(lo, fx, ip, true);
        // The return address of a function interrupted in its prologue or epilogue, before its
        // frame pointer is set or after it is restored, lives in the link register where the
        // architecture has one, and otherwise in the two words at the stack pointer.
        let busy = match ctx.link {
            Some(link) => {
                let address = fx.load(PTR, context, link);
                let in_link = critical_at(lo, fx, address, false);
                fx.b.ins().bor(in_ip, in_link)
            }
            None => {
                let top = fx.load(PTR, sp, 0);
                let in_top = critical_at(lo, fx, top, false);
                let next = fx.load(PTR, sp, 8);
                let in_next = critical_at(lo, fx, next, false);
                let busy = fx.b.ins().bor(in_ip, in_top);
                fx.b.ins().bor(busy, in_next)
            }
        };
        let free = fx.b.ins().bxor_imm_s(busy, 1);
        fx.when(free, |fx| {
            let walked = fx.repeat(&[fp, free], |fx, state| {
                let (frame, free) = (state[0], state[1]);
                let above =
                    fx.b.ins()
                        .icmp(IntCC::UnsignedGreaterThanOrEqual, frame, low);
                let end = fx.b.ins().iadd_imm_s(frame, 16);
                let below = fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, end, high);
                let inside = fx.b.ins().band(above, below);
                let result = fx.choose_in(
                    lo,
                    inside,
                    &[PTR, types::I8],
                    |lo, fx| {
                        let address = fx.load(PTR, frame, 8);
                        let base = fx.b.ins().icmp_imm_s(IntCC::Equal, address, 0);
                        let critical = critical_at(lo, fx, address, true);
                        let free = fx.b.ins().bxor_imm_s(critical, 1);
                        let free = fx.b.ins().bor(base, free);
                        vec![fx.load(PTR, frame, 0), free]
                    },
                    |_, fx| vec![fx.ptr(0), free],
                );
                let more = fx.b.ins().band(inside, result[1]);
                (more, result)
            });
            let free = walked[1];
            fx.when(free, |fx| {
                // Resume in `Stop` below the interrupted frame: where the return address lives on
                // the stack, past the red zone and behind a null return; where it lives in the
                // link register, at the aligned stack pointer with that register cleared.
                let resumed = match ctx.link {
                    Some(link) => {
                        let aligned = fx.b.ins().band_imm_s(sp, -16);
                        let zero = fx.ptr(0);
                        fx.store(zero, context, link);
                        aligned
                    }
                    None => {
                        let lowered = fx.b.ins().iadd_imm_s(sp, -128);
                        let aligned = fx.b.ins().band_imm_s(lowered, -16);
                        let entry = fx.b.ins().iadd_imm_s(aligned, -8);
                        let zero = fx.ptr(0);
                        fx.store(zero, entry, 0);
                        entry
                    }
                };
                fx.store(resumed, context, ctx.sp);
                let stop = lo.routine(Routine::Stop);
                let stop = lo.func_ref(fx, stop);
                let stop = fx.b.ins().func_addr(PTR, stop);
                fx.store(stop, context, ctx.ip);
                fx.store(record, context, ctx.arg);
            });
        });
    });
    fx.b.ins().return_(&[]);
}

/// `Overflow(signal, info, context)`: the handler of a fault. A fault in the guard region of
/// the running task's stack resumes in `OverflowRaise` on the thread's trap stack, as if
/// called at the call whose frame does not fit; any other fault restores the default action
/// and faults again.
fn overflow(lo: &mut Lowering, fx: &mut Fx, info: Value, context: Value) {
    let ctx = Context::of(lo.ck.db.target);
    let fault = fx.load(PTR, info, FAULT_ADDRESS);
    let thread = lo.thread(fx);
    let inside = fx.b.ins().icmp_imm_s(IntCC::NotEqual, thread, 0);
    let record = fx.choose(
        inside,
        &[PTR],
        |fx| vec![fx.load(PTR, thread, thread::CURRENT)],
        |fx| vec![fx.ptr(0)],
    )[0];
    let running = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
    let guarded = fx.choose(
        running,
        &[types::I8],
        |fx| {
            let base = fx.load(PTR, record, task::STACK);
            let low = fx.load(PTR, record, task::GUARD);
            let above =
                fx.b.ins()
                    .icmp(IntCC::UnsignedGreaterThanOrEqual, fault, base);
            let below = fx.b.ins().icmp(IntCC::UnsignedLessThan, fault, low);
            vec![fx.b.ins().band(above, below)]
        },
        |fx| vec![fx.int(types::I8, 0)],
    )[0];
    let other = fx.b.ins().bxor_imm_s(guarded, 1);
    fx.when(other, |fx| {
        let signal = fx.int(types::I32, i64::from(SIGSEGV));
        let default = fx.ptr(0);
        lo.call_libc(fx, "signal", &[signal, default], &[PTR]);
        fx.b.ins().return_(&[]);
    });
    let ip = fx.load(PTR, context, ctx.ip);
    let sp = fx.load(PTR, context, ctx.sp);
    let fp = fx.load(PTR, context, ctx.fp);
    let (start, _) = super::runtime::report::function_at(lo, fx, ip);
    let entering = fx.b.ins().icmp(IntCC::Equal, start, ip);
    // The caller the faulting call returns to, and that caller's frame pointer: a function
    // faulting as it is entered has not built its frame, so its return address is the one the call
    // left — in the link register where the architecture has one, and otherwise at the stack
    // pointer, unless the push growing the frame is what faulted; a function faulting deeper has
    // its caller in its frame record.
    let caller = match ctx.link {
        Some(link) => {
            let address = fx.load(PTR, context, link);
            fx.choose(
                entering,
                &[PTR, PTR],
                |_fx| vec![address, fp],
                |fx| vec![fx.load(PTR, fp, 8), fx.load(PTR, fp, 0)],
            )
        }
        None => {
            let pushed = fx.b.ins().iadd_imm_s(sp, -8);
            let calling = fx.b.ins().icmp(IntCC::Equal, fault, pushed);
            fx.choose(
                entering,
                &[PTR, PTR],
                |fx| vec![fx.load(PTR, sp, 0), fp],
                |fx| {
                    fx.choose(
                        calling,
                        &[PTR, PTR],
                        |fx| vec![fx.b.ins().iadd_imm_s(ip, 1), fp],
                        |fx| vec![fx.load(PTR, fp, 8), fx.load(PTR, fp, 0)],
                    )
                },
            )
        }
    };
    let top = fx.load(PTR, thread, thread::TRAP_STACK);
    // Resume in `OverflowRaise` on the trap stack as if the faulting site had called it: with the
    // return address where a call leaves it — behind the stack pointer, or in the link register.
    match ctx.link {
        Some(link) => {
            let aligned = fx.b.ins().band_imm_s(top, -16);
            fx.store(aligned, context, ctx.sp);
            fx.store(caller[0], context, link);
        }
        None => {
            let entry = fx.b.ins().iadd_imm_s(top, -8);
            fx.store(caller[0], entry, 0);
            fx.store(entry, context, ctx.sp);
        }
    }
    fx.store(caller[1], context, ctx.fp);
    let raise = lo.routine(Routine::OverflowRaise);
    let raise = lo.func_ref(fx, raise);
    let raise = fx.b.ins().func_addr(PTR, raise);
    fx.store(raise, context, ctx.ip);
    fx.b.ins().return_(&[]);
}

/// The executor threads of `Init`: one per processor of the affinity set with its ring, the
/// calling thread being the first, and the reading of termination requests; returns 1 from
/// `Init` when the system refuses one.
pub fn start_threads(lo: &mut Lowering, fx: &mut Fx) {
    let state_address = lo.state(fx);
    if lo.seeded() {
        start_seeded(lo, fx);
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
    let refused = thread_signals(lo, fx, first);
    fx.when(refused, |fx| {
        let one = fx.ptr(1);
        fx.b.ins().return_(&[one]);
    });
    install_handler(lo, fx, SIGURG, Routine::Preempt, true);
    install_handler(lo, fx, SIGSEGV, Routine::Overflow, false);
    let ignore = fx.ptr(1);
    let pipe = fx.int(types::I32, SIGPIPE);
    lo.call_libc(fx, "signal", &[pipe, ignore], &[PTR]);
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

/// The executor of `Init` for the seeded scheduler: the calling thread is the one executor
/// thread, without a ring; a fault reports a stack overflow, and the run ends with status `TIMED_OUT` once it has
/// used 10 seconds of processor time.
fn start_seeded(lo: &mut Lowering, fx: &mut Fx) {
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
    let slot = lo.thread_slot(fx);
    fx.store(record, slot, 0);
    let refused = thread_signals(lo, fx, record);
    fx.when(refused, |fx| {
        let one = fx.ptr(1);
        fx.b.ins().return_(&[one]);
    });
    install_handler(lo, fx, SIGSEGV, Routine::Overflow, false);
    install_handler(lo, fx, SIGXCPU, Routine::CpuLimit, false);
    let limit = fx.slot(16, 8);
    let soft = fx.ptr(CPU_SECONDS);
    let hard = fx.ptr(CPU_SECONDS + 2);
    fx.store(soft, limit, 0);
    fx.store(hard, limit, 8);
    let cpu = fx.int(types::I32, RLIMIT_CPU);
    lo.call_libc(fx, "setrlimit", &[cpu, limit], &[types::I32]);
    let ignore = fx.ptr(1);
    let pipe = fx.int(types::I32, SIGPIPE);
    lo.call_libc(fx, "signal", &[pipe, ignore], &[PTR]);
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
    let bits = SIGNALS
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
fn terminations(lo: &mut Lowering, fx: &mut Fx, op: Value) {
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

/// Handles the first termination request, which the thread reading them passes on: it cancels
/// the entry task's construct.
fn termination(lo: &mut Lowering, fx: &mut Fx, op: Value) {
    let signal = fx.load(types::I32, op, operation::SIZE);
    let signal = fx.b.ins().uextend(PTR, signal);
    let state_address = lo.state(fx);
    fx.store(signal, state_address, state::TERMINATION);
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
    fx.b.ins()
        .atomic_store(MemFlagsData::trusted(), next, tail_address);
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
    fx.b.ins().atomic_store(MemFlagsData::trusted(), one, idle);
    let state_address = lo.state(fx);
    let head = fx.offset(state_address, state::READY_HEAD);
    let head = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), head);
    let empty = fx.b.ins().icmp_imm_s(IntCC::Equal, head, 0);
    reap(lo, fx, Some(empty));
    let zero = fx.ptr(0);
    fx.b.ins().atomic_store(MemFlagsData::trusted(), zero, idle);
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
            fx.b.ins()
                .atomic_store(MemFlagsData::trusted(), next, head_address);
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
    fx.b.ins()
        .atomic_store(MemFlagsData::trusted(), submitted, word);
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
                fx.b.ins().atomic_store(MemFlagsData::trusted(), one, flag);
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

/// Maps `count` chunks aligned to their size, zero when the system refuses.
pub fn map_chunks(lo: &mut Lowering, fx: &mut Fx, count: Value) -> Value {
    let len = fx.b.ins().imul_imm_s(count, memory::CHUNK as i64);
    let padded = fx.b.ins().iadd_imm_s(len, memory::CHUNK as i64);
    let zero = fx.ptr(0);
    let protection = fx.int(types::I32, mmap::READ_WRITE);
    let flags = fx.int(types::I32, mmap::PRIVATE_ANONYMOUS);
    let no_fd = fx.int(types::I32, -1);
    let base = lo.call_libc(
        fx,
        "mmap",
        &[zero, padded, protection, flags, no_fd, zero],
        &[PTR],
    )[0];
    let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, base, mmap::FAILED);
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
    lo.call_libc(fx, "munmap", &[address, len], &[types::I32]);
}

/// Maps a task stack of `len` bytes whose lowest page is an inaccessible guard, zero when the
/// system refuses.
pub fn map_stack(lo: &mut Lowering, fx: &mut Fx, len: Value) -> Value {
    let zero = fx.ptr(0);
    let protection = fx.int(types::I32, mmap::READ_WRITE);
    let flags = fx.int(types::I32, mmap::STACK);
    let no_fd = fx.int(types::I32, -1);
    let base = lo.call_libc(
        fx,
        "mmap",
        &[zero, len, protection, flags, no_fd, zero],
        &[PTR],
    )[0];
    let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, base, mmap::FAILED);
    fx.choose_in(
        lo,
        failed,
        &[PTR],
        |_, fx| vec![fx.ptr(0)],
        |lo, fx| {
            protect_guard(lo, fx, base, false);
            vec![base]
        },
    )[0]
}

/// Makes the guard region at the base of a stack inaccessible, or accessible again.
pub fn protect_guard(lo: &mut Lowering, fx: &mut Fx, base: Value, accessible: bool) {
    let guard = fx.ptr(tasks::GUARD);
    let protection = match accessible {
        true => mmap::READ_WRITE,
        false => mmap::NONE,
    };
    let protection = fx.int(types::I32, protection);
    lo.call_libc(fx, "mprotect", &[base, guard, protection], &[types::I32]);
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

#[cfg(test)]
mod tests;
