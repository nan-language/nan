//! An isolated space's runtime: its one executor thread, which runs in user mode over a boot
//! area its kernel maps as it is used, the system calls through which it reads its kernel's
//! clock, waits for its first timer or its doorbell and ends, and its start, which runs
//! `nanos::isolated` as its first task.

use cranelift_codegen::ir::{
    AbiParam, InstBuilder, Signature, TrapCode, Value, condcodes::IntCC, types,
};
use cranelift_module::{FuncId, Linkage, Module};

use super::super::body::Held;
use super::super::call::Invoked;
use super::super::data::Reserved;
use super::super::fx::{Fx, PTR};
use super::super::machine::{BOOT_STACK, Stub};
use super::super::stack::{At, Demand, Judged, Report};
use super::super::{CALL_CONV, Instance, Lowering, lines};
use super::system::unavailable_unless;
use super::tasks::{self, EXPIRED, task, thread};
use super::{Routine, state};
use crate::check::ty::no_args;
use crate::nir::{Role, Span};

/// The numbers of the system calls the runtime makes: waiting until a deadline of the kernel's
/// clock, drawing and releasing pages of the boot area, reading the clock, invoking a capability
/// its kernel serves, and ending the space with how its job ended.
pub const WAIT: u64 = 1;
pub const DRAW: u64 = 2;
pub const RELEASE: u64 = 3;
pub const NOW: u64 = 5;
pub const INVOKE: u64 = 6;
pub const END: u64 = 11;

/// How an `END` call tells a trap.
const TRAPPED: u64 = 2;

/// The symbols of the start routine, which the kernel enters with the addresses of the start record
/// and of the boot area, and of the glue of the first task.
pub const START: &str = "_NSuser_start";
const MAIN: &str = "_NSuser_main";

/// The operation of the task waiting on the doorbell, zero when none waits.
pub const BELL: Reserved = Reserved {
    name: "_NSspace_bell",
    size: 8,
    align: 8,
};

/// The stack the start routine and the scheduler run on, and the boot area.
pub const START_STACK: u64 = BOOT_STACK;
pub const BOOT_AREA: u64 = super::super::machine::memory::BOOT_AREA;

/// Makes a system call with up to six arguments, giving its result.
pub fn call(lo: &mut Lowering, fx: &mut Fx, number: u64, args: &[Value]) -> Value {
    let mut words: Vec<Value> = args.to_vec();
    while words.len() < 6 {
        words.push(fx.ptr(0));
    }
    words.push(fx.ptr(number));
    lo.call_stub(fx, Stub::Syscall, &words)[0]
}

/// The record of the runtime's one executor thread.
pub fn thread(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let state_address = lo.state(fx);
    let threads = fx.load(PTR, state_address, state::THREADS);
    fx.load(PTR, threads, 0)
}

/// The executor of `Init`: one thread, whose record it draws.
pub fn init(lo: &mut Lowering, fx: &mut Fx) {
    let state_address = lo.state(fx);
    let one = fx.ptr(1);
    fx.store(one, state_address, state::THREAD_COUNT);
    let word = fx.ptr(8);
    let threads = lo.record_alloc(fx, word);
    unavailable_unless(fx, threads);
    fx.store(threads, state_address, state::THREADS);
    let size = fx.ptr(thread::SIZE);
    let record = lo.record_alloc(fx, size);
    unavailable_unless(fx, record);
    fx.store(record, threads, 0);
}

/// `ClockNow` reads the kernel's clock; `Extended` and `ExtendedOwn` give zero, the space's tasks
/// sharing the extended state its kernel saves for its thread.
pub fn define(lo: &mut Lowering, fx: &mut Fx, routine: Routine) {
    fx.entry();
    let result = match routine {
        Routine::ClockNow => call(lo, fx, NOW, &[]),
        _ => fx.ptr(0),
    };
    fx.b.ins().return_(&[result]);
}

/// Waits until the first timer is due, or for good without one, or until the kernel rings the
/// doorbell, which wakes the task waiting on it; then expires the timers due.
pub fn idle(lo: &mut Lowering, fx: &mut Fx) {
    let state_address = lo.state(fx);
    let lock = fx.offset(state_address, state::TIMER_LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let first = fx.load(PTR, state_address, state::TIMERS);
    let armed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, first, 0);
    let deadline = fx.choose(
        armed,
        &[PTR],
        |fx| {
            let due = fx.load(PTR, first, super::tasks::operation::DUE);
            let one = fx.ptr(1);
            vec![fx.b.ins().umax(due, one)]
        },
        |fx| vec![fx.ptr(0)],
    )[0];
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    let rung = call(lo, fx, WAIT, &[deadline]);
    let rung = fx.b.ins().icmp_imm_s(IntCC::NotEqual, rung, 0);
    fx.when(rung, |fx| {
        let slot = lo.reserved(fx, BELL);
        let op = fx.load(PTR, slot, 0);
        let waiting = fx.b.ins().icmp_imm_s(IntCC::NotEqual, op, 0);
        fx.when(waiting, |fx| {
            let zero = fx.ptr(0);
            fx.store(zero, slot, 0);
            let claimed = lo.kernel_claim(fx, op);
            fx.when(claimed, |fx| {
                let expired = fx.int(types::I32, EXPIRED);
                tasks::deliver(lo, fx, op, expired);
            });
        });
    });
    lo.expire_timers(fx);
}

/// Expires the timers due, when any is armed.
pub fn reap(lo: &mut Lowering, fx: &mut Fx) {
    let state_address = lo.state(fx);
    let first = fx.load(PTR, state_address, state::TIMERS);
    let armed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, first, 0);
    fx.when(armed, |fx| lo.expire_timers(fx));
}

/// `Exit`: ends the space as a trap abandoning it.
pub fn exit(lo: &mut Lowering, fx: &mut Fx) {
    let how = fx.ptr(TRAPPED);
    let kind = fx.ptr(super::super::trap::kind_number("trap.abandoned") as u64);
    call(lo, fx, END, &[how, kind]);
    fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
}

impl Lowering<'_, '_> {
    /// Defines the start routine: it sets up the runtime over the boot area, starts `nanos::isolated` on the record as the first task, with the stack its own
    /// `limits stack N` demands, and runs the scheduler.
    pub fn space_start(&mut self) {
        let isolated = self
            .ck
            .db
            .std_item("nanos", "isolated")
            .expect("the isolated space's entry");
        let stack = self
            .ck
            .fn_sig(isolated)
            .and_then(|sig| sig.limits.stack)
            .expect("the isolated space's entry stack");
        let main = self.space_main(isolated, stack);
        let mut signature = Signature::new(CALL_CONV);
        signature.params.extend([AbiParam::new(PTR); 2]);
        let id = self
            .module
            .declare_function(START, Linkage::Local, &signature)
            .expect("the start routine");
        self.define(id, signature, lines::HIDDEN, |lo, fx| {
            let params = fx.entry();
            let (start, area) = (params[0], params[1]);
            let state_address = lo.state(fx);
            fx.store(area, state_address, state::BOOT_AREA);
            super::system::start(lo, fx);
            let process = fx.load(PTR, state_address, state::PROCESS);
            let root = fx.load(PTR, state_address, state::ROOT);
            let glue = lo.func_ref(fx, main);
            let glue = fx.b.ins().func_addr(PTR, glue);
            let zero = fx.ptr(0);
            let usable = fx.ptr(stack);
            let record = lo.call_routine(
                fx,
                Routine::Spawn,
                &[glue, zero, zero, zero, usable, zero, process, root],
            )[0];
            let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
            fx.when(missing, |fx| exit(lo, fx));
            fx.store(start, record, task::ARGUMENT);
            fx.store(record, state_address, state::ENTRY_TASK);
            lo.call_routine(fx, Routine::Wake, &[record]);
            lo.call_routine(fx, Routine::Schedule, &[]);
            fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
        });
    }

    /// The glue of the first task, which calls `nanos::isolated` on the start record its task
    /// record holds, its stack demanded at the function's head.
    fn space_main(&mut self, isolated: crate::check::db::ItemId, stack: u64) -> FuncId {
        let instance = Instance::Nir {
            item: isolated,
            role: Role::Body,
            index: 0,
            args: no_args(),
        };
        let mut signature = Signature::new(CALL_CONV);
        signature.params.push(AbiParam::new(PTR));
        let id = self
            .module
            .declare_function(MAIN, Linkage::Local, &signature)
            .expect("the first task's glue");
        self.define(id, signature, lines::HIDDEN, |lo, fx| {
            let record = fx.entry()[0];
            let start = fx.load(PTR, record, task::ARGUMENT);
            let abi = lo.abi(&instance);
            let callee = lo.instance(instance.clone());
            lo.invoke(fx, Invoked::Direct(callee), &abi, &[Held::Scalar(start)]);
            fx.b.ins().return_(&[]);
        });
        let head = At {
            item: isolated,
            span: Span::at(self.ck.db.item(isolated).ast.token),
        };
        self.demands.push(Demand {
            judged: Judged::Function(id),
            stack,
            report: Report::Own { head },
        });
        id
    }
}
