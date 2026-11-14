//! The host targets, whose programs run as processes of an operating system: the glue running a
//! program's entry function as its entry task, and what a stop or a stack overflow does to an
//! interrupted thread's saved registers. `linux` holds everything Linux-specific and `windows`
//! everything Windows-specific.

pub mod linux;
pub mod windows;

use cranelift_codegen::ir::{AbiParam, InstBuilder, Signature, TrapCode, Value, condcodes::IntCC};
use cranelift_module::{FuncId, Linkage, Module};

use super::abi::Returned;
use super::fx::{Fx, PTR};
use super::runtime::tasks::{self, task, thread};
use super::runtime::{Routine, state, stops};
use super::{CALL_CONV, Instance, Lowering, Program, lines};
use crate::nir::{Role, Span};

/// The offsets in an interrupted thread's saved context of the registers a stop or fault handler
/// reads and redirects: the register a redirected call passes its argument in, the frame and
/// stack pointers, the instruction pointer, and the link register a leaf's return address lives
/// in where the architecture has one.
#[derive(Clone, Copy)]
pub struct Context {
    pub arg: u64,
    pub fp: u64,
    pub sp: u64,
    pub ip: u64,
    pub link: Option<u64>,
}

/// Declares the glue running the entry function as the entry task, giving it with the stack the
/// entry task gets.
pub fn entry_task(lo: &mut Lowering, program: &Program) -> (FuncId, u64) {
    let glue = entry_glue(lo, program);
    let stack = lo
        .ck
        .fn_sig(program.entry)
        .and_then(|sig| sig.limits.stack)
        .unwrap_or(tasks::ENTRY_STACK);
    let head = Span::at(lo.ck.db.item(program.entry).ast.token);
    lo.task(glue, program.entry, head, Some(stack));
    (glue, stack)
}

/// Mints the root grant through the entry landing fn, given `landing` after the grant's address,
/// then starts the entry task, which `entry_task` gave, and runs the executor's scheduler on the
/// calling thread.
pub fn run_entry(
    lo: &mut Lowering,
    fx: &mut Fx,
    program: &Program,
    (glue, stack): (FuncId, u64),
    landing: &[Value],
) {
    let world_ty = lo.std_ty("host", "World", Vec::new());
    let layout = lo.layout(&world_ty);
    let world = fx.slot(layout.size, layout.align);
    let landing_fn = lo.instance(Instance::Nir {
        item: program.landing,
        role: Role::Body,
        index: 0,
        args: crate::check::ty::no_args(),
    });
    let reference = lo.call_ref(fx, landing_fn);
    let mut args = vec![world];
    args.extend_from_slice(landing);
    fx.b.ins().call(reference, &args);
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
    fx.when(missing, |fx| super::runtime::system::unavailable(lo, fx));
    fx.store(record, state_address, state::ENTRY_TASK);
    lo.call_routine(fx, Routine::Wake, &[record]);
    lo.call_routine(fx, Routine::Schedule, &[]);
    fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
}

/// The glue the entry task runs: calls the entry function with the root grant, then ends the
/// process with the status its outcome gives.
fn entry_glue(lo: &mut Lowering, program: &Program) -> FuncId {
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
        let reference = lo.call_ref(fx, main);
        let call = fx.b.ins().call(reference, &[failure, world]);
        let results = fx.b.inst_results(call).to_vec();
        let state_address = lo.state(fx);
        let termination = fx.load(PTR, state_address, state::TERMINATION);
        let requested = fx.b.ins().icmp_imm_s(IntCC::NotEqual, termination, 0);
        fx.when(requested, |fx| {
            lo.call_routine(fx, Routine::Exit, &[termination]);
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

/// Stops the task an executor thread runs, interrupted with its registers saved at `context`,
/// when that task is asked to stop, holds no lock, runs on its own stack and has no frame of a
/// function it does not stop inside: the context then resumes in `Stop` on that stack, below the
/// interrupted frame. Otherwise the task runs on, and the stop is asked again later.
pub fn stop_interrupted(
    lo: &mut Lowering,
    fx: &mut Fx,
    thread: Value,
    context: Value,
    ctx: Context,
) {
    let record = fx.load(PTR, thread, thread::CURRENT);
    let running = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
    fx.when(running, |fx| {
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
            let link = ctx.link.map(|link| fx.load(PTR, context, link));
            let at = stops::Interrupted {
                ip,
                sp,
                fp,
                link,
                low,
                high,
                segment: None,
            };
            let free = stops::stoppable(lo, fx, &at);
            fx.when(free, |fx| {
                // Resume in `Stop` below the interrupted frame: where the return address lives
                // on the stack, past the red zone and behind a null return; where it lives in
                // the link register, at the aligned stack pointer with that register cleared.
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
}

/// Whether a fault at address `fault` of a thread interrupted with its registers saved at
/// `context` lies in the guard region of the stack of the task the thread runs, `thread` being
/// zero outside an executor thread; when it does, the context resumes in `OverflowRaise` on the
/// thread's trap stack, as if called at the call whose frame does not fit.
pub fn overflow_interrupted(
    lo: &mut Lowering,
    fx: &mut Fx,
    thread: Value,
    fault: Value,
    context: Value,
    ctx: Context,
) -> Value {
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
        &[cranelift_codegen::ir::types::I8],
        |fx| {
            let base = fx.load(PTR, record, task::STACK);
            let low = fx.load(PTR, record, task::GUARD);
            let above =
                fx.b.ins()
                    .icmp(IntCC::UnsignedGreaterThanOrEqual, fault, base);
            let below = fx.b.ins().icmp(IntCC::UnsignedLessThan, fault, low);
            vec![fx.b.ins().band(above, below)]
        },
        |fx| vec![fx.int(cranelift_codegen::ir::types::I8, 0)],
    )[0];
    fx.when(guarded, |fx| {
        let ip = fx.load(PTR, context, ctx.ip);
        let sp = fx.load(PTR, context, ctx.sp);
        let fp = fx.load(PTR, context, ctx.fp);
        let (start, _) = super::runtime::report::function_at(lo, fx, ip);
        let entering = fx.b.ins().icmp(IntCC::Equal, start, ip);
        // The caller the faulting call returns to, and that caller's frame pointer: a function
        // faulting as it is entered has not built its frame, so its return address is the one
        // the call left — in the link register where the architecture has one, and otherwise at
        // the stack pointer, unless the push growing the frame is what faulted; a function
        // faulting deeper has its caller in its frame record.
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
        // Resume in `OverflowRaise` on the trap stack as if the faulting site had called it:
        // with the return address where a call leaves it — behind the stack pointer, or in the
        // link register.
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
    });
    guarded
}
