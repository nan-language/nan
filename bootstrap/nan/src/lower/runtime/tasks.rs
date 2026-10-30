//! Tasks: their records and stacks, the run queue, the protocol by which a task parks and a
//! waker makes it ready, the scheduler each executor thread runs, the start and end of a task,
//! and the waits of tasks on kernel operations.
//!
//! A task's state word moves from running to parking when it suspends, then to parked once its
//! executor thread has switched away from its stack; a waker turns new or parked into ready,
//! and running or parking into notified, which the task or its thread turns back into ready.
//! A task asked to stop, or trapping, becomes stopped once off its thread instead, and is never
//! resumed.

use cranelift_codegen::ir::{
    AbiParam, InstBuilder, MemFlagsData, Signature, TrapCode, Value, condcodes::IntCC, types,
};

use cranelift_module::{DataDescription, FuncId, Linkage, Module};

use super::super::fx::{Fx, PTR};
use super::super::host;
use super::super::machine::Stub;
use super::super::{CALL_CONV, Lowering, stack};
use super::regions::origin;
use super::{Routine, state, system};
use crate::check::db::ItemId;
use crate::nir::Span;

/// The offsets of a task record's fields.
pub mod task {
    /// The saved stack pointer, frame pointer and resume address.
    pub const CONTEXT: u64 = 0;
    pub const STATE: u64 = 24;
    /// The stack's lowest address and its length, its guard region included.
    pub const STACK: u64 = 32;
    pub const STACK_SIZE: u64 = 40;
    pub const DOMAIN: u64 = 48;
    /// The innermost region holding the call the task runs.
    pub const REGION: u64 = 56;
    /// The glue running the task's entry, given the record, and the entry's code and
    /// environment.
    pub const GLUE: u64 = 64;
    pub const CODE: u64 = 72;
    pub const ENV: u64 = 80;
    /// Where the entry's result and failure go.
    pub const RESULT: u64 = 88;
    pub const FAILURE: u64 = 96;
    /// How the entry ended: zero while it runs, 1 returned, 2 failed.
    pub const OUTCOME: u64 = 104;
    pub const NEXT: u64 = 112;
    /// The links of the list of live tasks.
    pub const ALL_NEXT: u64 = 120;
    pub const ALL_PREVIOUS: u64 = 128;
    /// What the task waits on at a cancellation point: an operation in flight, with `CLAIMED`
    /// set while a cancellation reads its record, the waiting marker, or zero.
    pub const IO: u64 = 136;
    /// The glue run on the task's stack once its entry has ended, given the record.
    pub const END: u64 = 144;
    /// The construct or domain whose task it is.
    pub const OWNER: u64 = 152;
    /// The lowest address of the stack's usable part, above its guard region.
    pub const GUARD: u64 = 160;
    /// The thread running it.
    pub const THREAD: u64 = 168;
    /// The task waiting in `join` for this one to end.
    pub const JOINER: u64 = 176;
    /// Whether its join handle has reaped it, and whether its scope has returned.
    pub const FLAGS: u64 = 184;
    /// The cowns of the `await when` body it runs.
    pub const HELD: u64 = 192;
    /// The cell its stack was drawn through, zero for a mapped stack.
    pub const STACK_CELL: u64 = 200;
    /// The routine its executor thread runs, given the record, once the task has ended and
    /// switched away for the last time.
    pub const DONE: u64 = 208;
    /// The release of its entry's result, zero when releasing one does nothing.
    pub const RESULT_RELEASE: u64 = 216;
    /// The argument its entry takes, when the entry has a parameter.
    pub const ARGUMENT: u64 = 224;
    /// Nonzero once the abandoning of its domain asks it to stop.
    pub const STOP: u64 = 240;
    /// The first of the deadlines in flight that the constructs it runs armed, linked through
    /// their records.
    pub const DEADLINES: u64 = 248;
    /// The record of its wait in a structure other domains reach, which settlement withdraws:
    /// the glue withdrawing it, given the record, then what that glue reads.
    pub const WAIT: u64 = 256;
    /// The next task of the abandoned domain being settled.
    pub const SETTLE: u64 = 264;
    /// On the seeded scheduler, the order in which it last became ready.
    pub const ORDER: u64 = 272;
    /// On a kernel, nonzero while an interrupt that preempted the task waits to return into it.
    pub const PREEMPTED: u64 = 280;
    pub const SIZE: u64 = 288;
}

/// The offsets of an executor thread record's fields: the scheduler's saved context, the task
/// it runs, its index and the locks it holds; on a host target also its ring, whether it waits
/// idle in its ring, the operations in flight on it, whether another thread asked to cancel one,
/// its POSIX thread and the top of the stack its stack overflow reports run on; on a kernel the
/// flags from before its outermost held lock disabled interrupts.
pub mod thread {
    pub const CONTEXT: u64 = 0;
    pub const CURRENT: u64 = 24;
    pub const RING: u64 = 32;
    pub const INDEX: u64 = 40;
    pub const IDLE: u64 = 48;
    pub const OPERATIONS: u64 = 56;
    pub const REQUESTS: u64 = 64;
    /// The number of locks the thread holds, then up to `HELD_LOCKS` of them in the order it
    /// took them.
    pub const LOCKS: u64 = 72;
    pub const PTHREAD: u64 = 80;
    pub const TRAP_STACK: u64 = 88;
    pub const HELD: u64 = 96;
    pub const SAVED_IF: u64 = HELD + 8 * super::super::locks::HELD_LOCKS;
    pub const SIZE: u64 = SAVED_IF + 8;
}

/// The bits of a task record's flags.
pub mod flag {
    /// Its join handle has reaped it.
    pub const REAPED: i64 = 1;
    /// It has ended and its executor thread has switched away from it.
    pub const DONE: i64 = 2;
}

/// The states of a task.
pub mod run {
    pub const NEW: i64 = 0;
    pub const READY: i64 = 1;
    pub const RUNNING: i64 = 2;
    pub const PARKING: i64 = 3;
    pub const PARKED: i64 = 4;
    pub const NOTIFIED: i64 = 5;
    pub const ENDED: i64 = 6;
    /// A trapping task switching away for the last time.
    pub const STOPPING: i64 = 7;
    pub const STOPPED: i64 = 8;
}

/// The offsets of a domain record's fields.
pub mod domain {
    pub const PARENT: u64 = 0;
    pub const POOLS: u64 = 8;
    pub const CHUNKS: u64 = 16;
    /// Nonzero once a trap abandons it.
    pub const ABANDONED: u64 = 24;
    /// The task waiting for the domain's first task to end, and the lock of that wait.
    pub const WAITER: u64 = 32;
    pub const LOCK: u64 = 40;
    /// The record its trap report is written into: the trap kind's number, the report's
    /// length and its bytes.
    pub const TRAP: u64 = 48;
    /// The first of the domains nested in it, and the next domain nested in its parent.
    pub const CHILDREN: u64 = 56;
    pub const SIBLING: u64 = 64;
    /// The first of the holdings it owns.
    pub const HOLDINGS: u64 = 72;
    /// The handle of its cell, which the chunks passing to it are charged to.
    pub const CELL: u64 = 80;
    pub const SIZE: u64 = 88;
}

/// The offsets of an operation record's fields: what a task waits on at a cancellation point,
/// or a timer, which a completion, the virtual clock, a kernel's clock or a sim reaches.
pub mod operation {
    pub const TASK: u64 = 0;
    pub const RESULT: u64 = 8;
    /// Zero in flight, `COMPLETED` once its completion arrived, then `DELIVERED` once its waiter
    /// has been woken, after which the thread delivering it no longer reads the record.
    pub const DONE: u64 = 16;
    pub const THREAD: u64 = 24;
    /// Zero, `REQUESTED` once its cancellation is asked, `SUBMITTED` once submitted.
    pub const CANCEL: u64 = 32;
    pub const KIND: u64 = 40;
    /// Its links among its thread's operations in flight; on the seeded scheduler and on a
    /// kernel, among the timers of its clock or in the queue of the sim it waits in.
    pub const NEXT: u64 = 48;
    pub const PREVIOUS: u64 = 56;
    /// For a deadline, the task to wake once it completes, and the next deadline its task
    /// armed; for a wait in a sim, the sim's record.
    pub const WAITER: u64 = 64;
    pub const LINK: u64 = 72;
    /// On the seeded scheduler and on a kernel, when a timer is due and the order in which it
    /// was armed, and for a wait in a sim, the glue withdrawing it from the sim, given the
    /// record.
    pub const DUE: u64 = 80;
    pub const ORDER: u64 = 88;
    pub const WITHDRAW: u64 = 96;
    pub const SIZE: u64 = 104;
    pub const COMPLETED: i64 = 1;
    pub const DELIVERED: i64 = 2;
    pub const REQUESTED: i64 = 1;
    pub const SUBMITTED: i64 = 2;
}

/// The kinds of operations.
pub mod kind {
    /// A task waits on it.
    pub const WAIT: i64 = 0;
    /// It reads the next termination request.
    pub const SIGNAL: i64 = 1;
    /// A deadline, whose expiry cancels the region its `TASK` field names.
    pub const DEADLINE: i64 = 2;
    /// A component's delayed message, whose expiry lets the messages due into the intake of
    /// the runner its `TASK` field names.
    pub const AFTER: i64 = 3;
}

/// The error numbers of an expired timeout and of a cancelled operation, negated.
pub const EXPIRED: i64 = -62;
pub const CANCELED: i64 = -125;

/// The offsets of a trap record's fields.
pub mod trap_record {
    pub const KIND: u64 = 0;
    pub const LEN: u64 = 8;
    pub const BYTES: u64 = 16;
}

/// The default size of a task's stack, of the entry task's stack on a host target, and of a
/// guard region.
pub const STACK: u64 = 65_536;
pub const ENTRY_STACK: u64 = 67_108_864;
pub const GUARD: u64 = 4_096;

/// The bytes a task's stack holds beyond its usable size for the support routines and C library
/// functions that its calls of support routines run, whose use past the stub call bound the
/// usable size does not count.
const ROUTINE_RESERVE: u64 = 32_768;

/// The bit of a task's `IO` word set while a cancellation reads the record of the operation
/// the task waits on, which the task keeps until the bit clears.
pub const CLAIMED: i64 = 4;

pub fn define(lo: &mut Lowering, fx: &mut Fx, routine: Routine) {
    match routine {
        Routine::Spawn => spawn(lo, fx),
        Routine::Wake => {
            let params = fx.entry();
            wake(lo, fx, params[0]);
            fx.b.ins().return_(&[]);
        }
        Routine::Park => {
            fx.entry();
            park(lo, fx);
            fx.b.ins().return_(&[]);
        }
        Routine::TaskStart => task_start(lo, fx),
        Routine::Schedule => schedule(lo, fx),
        Routine::ThreadMain => {
            let params = fx.entry();
            let slot = lo.thread_slot(fx);
            fx.store(params[0], slot, 0);
            let refused = host::thread_signals(lo, fx, params[0]);
            fx.when(refused, |fx| {
                let status = fx.ptr(70);
                lo.call_routine(fx, Routine::Exit, &[status]);
            });
            lo.call_routine(fx, Routine::Schedule, &[]);
            fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
        }
        Routine::Stop => {
            let params = fx.entry();
            stop_running(lo, fx, params[0]);
        }
        Routine::IoSubmit => io_submit(lo, fx),
        Routine::Thread => {
            fx.entry();
            let slot = lo.thread_slot(fx);
            let record = fx.load(PTR, slot, 0);
            fx.b.ins().return_(&[record]);
        }
        _ => unreachable!("a task routine"),
    }
}

/// Sets what the running task waits on at a cancellation point: `regions::WAITING` for a plain
/// wait, zero once it no longer waits.
pub fn waiting_on(lo: &mut Lowering, fx: &mut Fx, what: i64) {
    let current = current_task(lo, fx);
    let word = fx.offset(current, task::IO);
    let what = fx.ptr(what as u64);
    fx.b.ins().atomic_store(MemFlagsData::trusted(), what, word);
}

/// The task the running executor thread runs, zero outside a task.
pub fn current_task(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let thread = lo.thread(fx);
    let inside = fx.b.ins().icmp_imm_s(IntCC::NotEqual, thread, 0);
    fx.choose(
        inside,
        &[PTR],
        |fx| vec![fx.load(PTR, thread, thread::CURRENT)],
        |fx| vec![fx.ptr(0)],
    )[0]
}

/// Whether a domain is `outer` or nested in it.
pub fn within(fx: &mut Fx, inner: Value, outer: Value) -> Value {
    let found = fx.repeat(&[inner], |fx, state| {
        let current = state[0];
        let same = fx.b.ins().icmp(IntCC::Equal, current, outer);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
        let parent = fx.choose(
            present,
            &[PTR],
            |fx| vec![fx.load(PTR, current, domain::PARENT)],
            |fx| vec![fx.ptr(0)],
        )[0];
        let has_parent = fx.b.ins().icmp_imm_s(IntCC::NotEqual, parent, 0);
        let differ = fx.b.ins().bxor_imm_s(same, 1);
        let more = fx.b.ins().band(differ, has_parent);
        let next = fx.b.ins().select(same, current, parent);
        (more, vec![next])
    })[0];
    fx.b.ins().icmp(IntCC::Equal, found, outer)
}

/// Whether a domain is nested in `outer`, `outer` itself excluded.
pub fn nested_in(fx: &mut Fx, inner: Value, outer: Value) -> Value {
    let differ = fx.b.ins().icmp(IntCC::NotEqual, inner, outer);
    let inside = within(fx, inner, outer);
    fx.b.ins().band(differ, inside)
}

/// The domain of the running task, the process domain outside a task.
pub fn current_domain(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let task = current_task(lo, fx);
    let state_address = lo.state(fx);
    let process = fx.load(PTR, state_address, state::PROCESS);
    let inside = fx.b.ins().icmp_imm_s(IntCC::NotEqual, task, 0);
    fx.choose(
        inside,
        &[PTR],
        |fx| vec![fx.load(PTR, task, task::DOMAIN)],
        |_| vec![process],
    )[0]
}

/// The symbol of the table of task entries whose own `limits stack N` passes the default task
/// stack: their count, then each entry's code address and N.
const ENTRY_STACKS: &str = "_NSentry_stacks";

impl Lowering<'_, '_> {
    /// Records the code of a task entry with its own `limits stack N`, when N passes the
    /// default task stack.
    pub fn entry_stack(&mut self, code: FuncId, own: Option<u64>) {
        if let Some(stack) = own.filter(|stack| *stack > STACK)
            && !self.entry_stacks.contains(&(code, stack))
        {
            self.entry_stacks.push((code, stack));
        }
    }

    /// Records a task entry written at `span` of `item`, once, with the stack its task gets: its
    /// own `limits stack N`, or the default task stack.
    pub fn task(&mut self, function: FuncId, item: ItemId, span: Span, own: Option<u64>) {
        if !self.tasks.iter().any(|task| task.function == function) {
            self.tasks.push(stack::Task {
                function,
                at: stack::At { item, span },
                stack: own.unwrap_or(STACK),
            });
        }
    }

    /// The address of the table of task entries' own stacks, declared once.
    fn entry_table(&mut self, fx: &mut Fx) -> Value {
        let module = &mut self.module;
        let id = *self.entry_table.get_or_insert_with(|| {
            module
                .declare_data(ENTRY_STACKS, Linkage::Local, false, false)
                .expect("the entry stacks table")
        });
        let global = self.data_ref(fx, id);
        fx.b.ins().symbol_value(PTR, global)
    }
}

/// Defines the table of task entries' own stacks, when a spawn reads it.
pub fn emit_entry_stacks(lo: &mut Lowering) {
    let Some(id) = lo.entry_table else {
        return;
    };
    let mut bytes = (lo.entry_stacks.len() as u64).to_le_bytes().to_vec();
    let mut description = DataDescription::new();
    let mut relocations = Vec::new();
    for (index, (code, stack)) in lo.entry_stacks.iter().enumerate() {
        relocations.push((8 + 16 * index as u32, *code));
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&stack.to_le_bytes());
    }
    description.define(bytes.into_boxed_slice());
    description.set_align(8);
    for (offset, code) in relocations {
        let reference = lo.module.declare_func_in_data(code, &mut description);
        description.write_function_addr(offset, reference);
    }
    lo.module
        .define_data(id, &description)
        .expect("the entry stacks table");
}

/// The usable stack a task entry's own `limits stack N` needs, when its code is in the table of
/// such entries, or `usable`.
fn own_stack(lo: &mut Lowering, fx: &mut Fx, code: Value, usable: Value) -> Value {
    let table = lo.entry_table(fx);
    let count = fx.load(PTR, table, 0);
    let any = fx.b.ins().icmp_imm_s(IntCC::NotEqual, count, 0);
    fx.choose(
        any,
        &[PTR],
        |fx| {
            let zero = fx.ptr(0);
            let found = fx.repeat(&[zero, usable], |fx, state| {
                let (index, size) = (state[0], state[1]);
                let offset = fx.b.ins().imul_imm_s(index, 16);
                let record = fx.b.ins().iadd(table, offset);
                let entry = fx.load(PTR, record, 8);
                let stack = fx.load(PTR, record, 16);
                let same = fx.b.ins().icmp(IntCC::Equal, entry, code);
                let larger = fx.b.ins().umax(size, stack);
                let size = fx.b.ins().select(same, larger, size);
                let next = fx.b.ins().iadd_imm_s(index, 1);
                let more = fx.b.ins().icmp(IntCC::UnsignedLessThan, next, count);
                (more, vec![next, size])
            });
            vec![found[1]]
        },
        |_| vec![usable],
    )[0]
}

/// `Spawn(glue, code, env, extra, stack, cell, domain, region)`: a new task record with
/// `extra` bytes after it for the entry's result and failure, and a stack of `stack` usable
/// bytes, or of the larger stack the entry's own `limits stack N` needs, with the routine
/// reserve and the guard region, drawn through a cell for a domain, or mapped for the zero
/// cell; zero when the stack cannot be drawn. The task joins the list of live tasks.
fn spawn(lo: &mut Lowering, fx: &mut Fx) {
    let params = fx.entry();
    let (glue, code, env, extra) = (params[0], params[1], params[2], params[3]);
    let (cell, domain, region) = (params[5], params[6], params[7]);
    let usable = own_stack(lo, fx, code, params[4]);
    let size = fx.b.ins().iadd_imm_s(extra, task::SIZE as i64);
    let record = lo.record_alloc(fx, size);
    let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
    fx.when(failed, |fx| {
        super::report::raise_kind(lo, fx, "trap.exhausted")
    });
    let length =
        fx.b.ins()
            .iadd_imm_s(usable, (GUARD + ROUTINE_RESERVE + 15) as i64);
    let length = fx.b.ins().band_imm_s(length, -16);
    let mapped = fx.b.ins().icmp_imm_s(IntCC::Equal, cell, 0);
    let stack = fx.choose_in(
        lo,
        mapped,
        &[PTR],
        |lo, fx| vec![system::map_stack(lo, fx, length)],
        |lo, fx| {
            let align = fx.ptr(super::memory::CHUNK);
            let drawn = lo.call_routine(fx, Routine::AllocIn, &[cell, length, align, domain])[0];
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, drawn, 0);
            fx.when(present, |fx| system::protect_guard(lo, fx, drawn, false));
            vec![drawn]
        },
    )[0];
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, stack, 0);
    fx.when(missing, |fx| {
        lo.record_free(fx, record);
        let zero = fx.ptr(0);
        fx.b.ins().return_(&[zero]);
    });
    fx.store(stack, record, task::STACK);
    fx.store(length, record, task::STACK_SIZE);
    fx.store(cell, record, task::STACK_CELL);
    let guard = fx.b.ins().iadd_imm_s(stack, GUARD as i64);
    fx.store(guard, record, task::GUARD);
    // The initial stack pointer of the task's first run, which its switch restores: aligned to
    // 16 bytes, which the start routine is entered with on AArch64, and on x86_64 eight below
    // that, over a zeroed return slot, so the start routine is entered as a call leaves the
    // stack.
    let zero = fx.ptr(0);
    let top = fx.b.ins().iadd(stack, length);
    let top = if lo.ck.db.target.arch() == Some(crate::workspace::Arch::Aarch64) {
        top
    } else {
        let top = fx.b.ins().iadd_imm_s(top, -8);
        fx.store(zero, top, 0);
        top
    };
    fx.store(top, record, task::CONTEXT);
    fx.store(zero, record, task::CONTEXT + 8);
    let start = lo.routine(Routine::TaskStart);
    let start = lo.func_ref(fx, start);
    let start = fx.b.ins().func_addr(PTR, start);
    fx.store(start, record, task::CONTEXT + 16);
    fx.store(glue, record, task::GLUE);
    fx.store(code, record, task::CODE);
    fx.store(env, record, task::ENV);
    let storage = fx.b.ins().iadd_imm_s(record, task::SIZE as i64);
    fx.store(storage, record, task::RESULT);
    fx.store(storage, record, task::FAILURE);
    fx.store(domain, record, task::DOMAIN);
    fx.store(region, record, task::REGION);
    let new = fx.int(types::I32, run::NEW);
    fx.store(new, record, task::STATE);
    let state_address = lo.state(fx);
    let lock = fx.offset(state_address, state::TASKS_LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let head = fx.load(PTR, state_address, state::TASKS);
    fx.store(head, record, task::ALL_NEXT);
    let has_head = fx.b.ins().icmp_imm_s(IntCC::NotEqual, head, 0);
    fx.when(has_head, |fx| fx.store(record, head, task::ALL_PREVIOUS));
    fx.store(record, state_address, state::TASKS);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    fx.b.ins().return_(&[record]);
}

/// The first code a task runs, on its own stack: its glue, then its end, then it leaves the
/// list of live tasks and switches away for the last time.
fn task_start(lo: &mut Lowering, fx: &mut Fx) {
    let params = fx.entry();
    let record = params[0];
    let mut signature = Signature::new(CALL_CONV);
    signature.params.push(AbiParam::new(PTR));
    let signature = fx.b.import_signature(signature);
    let glue = fx.load(PTR, record, task::GLUE);
    fx.b.ins().call_indirect(signature, glue, &[record]);
    let end = fx.load(PTR, record, task::END);
    let has_end = fx.b.ins().icmp_imm_s(IntCC::NotEqual, end, 0);
    fx.when(has_end, |fx| {
        fx.b.ins().call_indirect(signature, end, &[record]);
    });
    let state_address = lo.state(fx);
    let lock = fx.offset(state_address, state::TASKS_LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let next = fx.load(PTR, record, task::ALL_NEXT);
    let previous = fx.load(PTR, record, task::ALL_PREVIOUS);
    let has_previous = fx.b.ins().icmp_imm_s(IntCC::NotEqual, previous, 0);
    fx.choose(
        has_previous,
        &[],
        |fx| {
            fx.store(next, previous, task::ALL_NEXT);
            Vec::new()
        },
        |fx| {
            fx.store(next, state_address, state::TASKS);
            Vec::new()
        },
    );
    let has_next = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next, 0);
    fx.when(has_next, |fx| fx.store(previous, next, task::ALL_PREVIOUS));
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    let ended = fx.int(types::I32, run::ENDED);
    let word = fx.offset(record, task::STATE);
    fx.b.ins()
        .atomic_store(MemFlagsData::trusted(), ended, word);
    switch_out(lo, fx, record);
    fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
}

/// Switches from the running task to its thread's scheduler.
fn switch_out(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let thread = lo.thread(fx);
    let scheduler = fx.offset(thread, thread::CONTEXT);
    let context = fx.offset(record, task::CONTEXT);
    lo.switch_stacks(fx, context, scheduler, record);
}

/// Switches the running kernel task, which an interrupt preempted, away from its CPU: it becomes
/// ready again once its registers are saved, and resumes through the seam that preempted it. A
/// task interrupted while it switches away for another reason, parking or ending, stays as it is.
pub fn yield_preempted(lo: &mut Lowering, fx: &mut Fx) {
    let record = current_task(lo, fx);
    let word = fx.offset(record, task::STATE);
    let running = fx.int(types::I32, run::RUNNING);
    let notified = fx.int(types::I32, run::NOTIFIED);
    let seen =
        fx.b.ins()
            .atomic_cas(MemFlagsData::trusted(), word, running, notified);
    let was_running = fx.b.ins().icmp(IntCC::Equal, seen, running);
    let was_notified = fx.b.ins().icmp(IntCC::Equal, seen, notified);
    let yields = fx.b.ins().bor(was_running, was_notified);
    fx.when(yields, |fx| {
        let one = fx.ptr(1);
        fx.store(one, record, task::PREEMPTED);
        switch_out(lo, fx, record);
    });
}

/// Suspends the running task until a wake, unless one came since it last ran.
fn park(lo: &mut Lowering, fx: &mut Fx) {
    let record = current_task(lo, fx);
    let word = fx.offset(record, task::STATE);
    let running = fx.int(types::I32, run::RUNNING);
    let parking = fx.int(types::I32, run::PARKING);
    let previous =
        fx.b.ins()
            .atomic_cas(MemFlagsData::trusted(), word, running, parking);
    let parked = fx.b.ins().icmp(IntCC::Equal, previous, running);
    fx.choose_in(
        lo,
        parked,
        &[],
        |lo, fx| {
            switch_out(lo, fx, record);
            Vec::new()
        },
        |_, fx| {
            fx.b.ins()
                .atomic_store(MemFlagsData::trusted(), running, word);
            Vec::new()
        },
    );
}

/// Makes a task ready: a new or parked one joins the run queue, a running or parking one is
/// notified.
pub fn wake(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let word = fx.offset(record, task::STATE);
    let notified = fx.int(types::I32, run::NOTIFIED);
    let ready = fx.int(types::I32, run::READY);
    fx.repeat(&[], |fx, _| {
        let current =
            fx.b.ins()
                .atomic_load(types::I32, MemFlagsData::trusted(), word);
        let is_new = fx.b.ins().icmp_imm_s(IntCC::Equal, current, run::NEW);
        let is_parked = fx.b.ins().icmp_imm_s(IntCC::Equal, current, run::PARKED);
        let is_parked = fx.b.ins().bor(is_new, is_parked);
        let is_awake = fx.b.ins().icmp_imm_s(IntCC::Equal, current, run::RUNNING);
        let is_parking = fx.b.ins().icmp_imm_s(IntCC::Equal, current, run::PARKING);
        let notify = fx.b.ins().bor(is_awake, is_parking);
        let target = fx.b.ins().select(is_parked, ready, notified);
        let acting = fx.b.ins().bor(is_parked, notify);
        let again = fx.choose_in(
            lo,
            acting,
            &[types::I8],
            |lo, fx| {
                let seen =
                    fx.b.ins()
                        .atomic_cas(MemFlagsData::trusted(), word, current, target);
                let done = fx.b.ins().icmp(IntCC::Equal, seen, current);
                let enqueue = fx.b.ins().band(done, is_parked);
                fx.when(enqueue, |fx| push_ready(lo, fx, record));
                vec![fx.b.ins().bxor_imm_s(done, 1)]
            },
            |_, fx| vec![fx.int(types::I8, 0)],
        );
        (again[0], Vec::new())
    });
}

/// Makes a task just spawned ready; the spawn is a scheduling point of the running task.
pub fn spawned(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    lo.call_routine(fx, Routine::Wake, &[record]);
    super::seeded::scheduling_point(lo, fx);
}

/// Appends a ready task to the run queue, waking an idle executor thread.
pub fn push_ready(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    if lo.seeded() {
        super::seeded::push_ready(lo, fx, record);
        return;
    }
    let state_address = lo.state(fx);
    let lock = fx.offset(state_address, state::QUEUE_LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let zero = fx.ptr(0);
    fx.store(zero, record, task::NEXT);
    let tail = fx.load(PTR, state_address, state::READY_TAIL);
    let has_tail = fx.b.ins().icmp_imm_s(IntCC::NotEqual, tail, 0);
    fx.choose(
        has_tail,
        &[],
        |fx| {
            fx.store(record, tail, task::NEXT);
            Vec::new()
        },
        |fx| {
            fx.store(record, state_address, state::READY_HEAD);
            Vec::new()
        },
    );
    fx.store(record, state_address, state::READY_TAIL);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    system::wake_idle(lo, fx);
}

/// Takes the first ready task, zero when none.
pub fn pop_ready(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let state_address = lo.state(fx);
    let lock = fx.offset(state_address, state::QUEUE_LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let head = fx.load(PTR, state_address, state::READY_HEAD);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, head, 0);
    fx.when(present, |fx| {
        let next = fx.load(PTR, head, task::NEXT);
        fx.store(next, state_address, state::READY_HEAD);
        let last = fx.b.ins().icmp_imm_s(IntCC::Equal, next, 0);
        fx.when(last, |fx| {
            let zero = fx.ptr(0);
            fx.store(zero, state_address, state::READY_TAIL);
        });
    });
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    head
}

/// The scheduler of an executor thread: runs ready tasks, handling the completions on its ring
/// between them, and waits for a completion when no task is ready; the seeded scheduler
/// chooses each task it runs and advances its virtual clock instead of waiting. Never returns.
fn schedule(lo: &mut Lowering, fx: &mut Fx) {
    fx.entry();
    let header = fx.block();
    fx.b.ins().jump(header, &[]);
    fx.b.switch_to_block(header);
    if lo.kernel() {
        lo.call_stub(fx, Stub::Cli, &[]);
    }
    let record = match lo.seeded() {
        true => super::seeded::pick(lo, fx),
        false => pop_ready(lo, fx),
    };
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
    fx.choose_in(
        lo,
        present,
        &[],
        |lo, fx| {
            run_task(lo, fx, record);
            lo.reap(fx);
            Vec::new()
        },
        |lo, fx| {
            match lo.seeded() {
                true => super::seeded::idle(lo, fx),
                false => system::idle(lo, fx),
            }
            Vec::new()
        },
    );
    fx.b.ins().jump(header, &[]);
    let after = fx.block();
    fx.b.switch_to_block(after);
    fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
}

/// Switches to a ready task until it parks or ends, then settles its state; an ended task's
/// stack returns. A task asked to stop is not switched to, and one that parks or is notified
/// while asked to stop, or that traps, becomes stopped.
fn run_task(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let word = fx.offset(record, task::STATE);
    let asked = stop_asked(fx, record);
    fx.choose_in(
        lo,
        asked,
        &[],
        |_, fx| {
            let stopped = fx.int(types::I32, run::STOPPED);
            fx.b.ins()
                .atomic_store(MemFlagsData::trusted(), stopped, word);
            Vec::new()
        },
        |lo, fx| {
            switch_in(lo, fx, record);
            Vec::new()
        },
    );
}

/// Runs a task on the executor thread until it switches away, then settles its state.
fn switch_in(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let word = fx.offset(record, task::STATE);
    let thread = lo.thread(fx);
    let running = fx.int(types::I32, run::RUNNING);
    fx.b.ins()
        .atomic_store(MemFlagsData::trusted(), running, word);
    fx.store(record, thread, thread::CURRENT);
    fx.store(thread, record, task::THREAD);
    let scheduler = fx.offset(thread, thread::CONTEXT);
    let context = fx.offset(record, task::CONTEXT);
    if lo.kernel() {
        // A preempted task resumes through the seam that preempted it, whose return restores its
        // interrupt flag; every other task runs with maskable interrupts enabled.
        let preempted = fx.load(PTR, record, task::PREEMPTED);
        let resumed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, preempted, 0);
        fx.choose_in(
            lo,
            resumed,
            &[],
            |_, fx| {
                let zero = fx.ptr(0);
                fx.store(zero, record, task::PREEMPTED);
                Vec::new()
            },
            |lo, fx| {
                lo.call_stub(fx, Stub::Sti, &[]);
                Vec::new()
            },
        );
    }
    lo.switch_stacks(fx, scheduler, context, record);
    let thread = lo.thread(fx);
    let zero = fx.ptr(0);
    fx.store(zero, thread, thread::CURRENT);
    let state_now =
        fx.b.ins()
            .atomic_load(types::I32, MemFlagsData::trusted(), word);
    let ended = fx.b.ins().icmp_imm_s(IntCC::Equal, state_now, run::ENDED);
    let trapped =
        fx.b.ins()
            .icmp_imm_s(IntCC::Equal, state_now, run::STOPPING);
    let asked = stop_asked(fx, record);
    let not_ended = fx.b.ins().bxor_imm_s(ended, 1);
    let asked = fx.b.ins().band(asked, not_ended);
    let stops = fx.b.ins().bor(trapped, asked);
    fx.choose_in(
        lo,
        stops,
        &[],
        |_, fx| {
            let stopped = fx.int(types::I32, run::STOPPED);
            fx.b.ins()
                .atomic_store(MemFlagsData::trusted(), stopped, word);
            Vec::new()
        },
        |lo, fx| {
            // A task switching away notified was woken after it parked, or was preempted.
            let parking = fx.b.ins().icmp_imm_s(IntCC::Equal, state_now, run::PARKING);
            let notified =
                fx.b.ins()
                    .icmp_imm_s(IntCC::Equal, state_now, run::NOTIFIED);
            let parking = fx.b.ins().bor(parking, notified);
            fx.when(parking, |fx| {
                let parking = fx.int(types::I32, run::PARKING);
                let parked = fx.int(types::I32, run::PARKED);
                let seen =
                    fx.b.ins()
                        .atomic_cas(MemFlagsData::trusted(), word, parking, parked);
                let notified = fx.b.ins().icmp(IntCC::NotEqual, seen, parking);
                fx.when(notified, |fx| {
                    let ready = fx.int(types::I32, run::READY);
                    fx.b.ins()
                        .atomic_store(MemFlagsData::trusted(), ready, word);
                    push_ready(lo, fx, record);
                });
            });
            fx.when(ended, |fx| {
                release_stack(lo, fx, record);
                let done = fx.load(PTR, record, task::DONE);
                let has_done = fx.b.ins().icmp_imm_s(IntCC::NotEqual, done, 0);
                fx.when(has_done, |fx| {
                    let mut signature = Signature::new(CALL_CONV);
                    signature.params.push(AbiParam::new(PTR));
                    let signature = fx.b.import_signature(signature);
                    fx.b.ins().call_indirect(signature, done, &[record]);
                });
            });
            Vec::new()
        },
    );
}

/// Whether the abandoning of a task's domain asks it to stop.
pub fn stop_asked(fx: &mut Fx, record: Value) -> Value {
    let word = fx.offset(record, task::STOP);
    let asked =
        fx.b.ins()
            .atomic_load(types::I64, MemFlagsData::trusted(), word);
    fx.b.ins().icmp_imm_s(IntCC::NotEqual, asked, 0)
}

/// Switches the running task away for the last time, as stopped: its thread never resumes it.
pub fn stop_running(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let stopping = fx.int(types::I32, run::STOPPING);
    let word = fx.offset(record, task::STATE);
    fx.b.ins()
        .atomic_store(MemFlagsData::trusted(), stopping, word);
    switch_out(lo, fx, record);
    fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
}

/// Returns an ended task's stack: unmapped, or freed through its cell.
pub fn release_stack(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let stack = fx.load(PTR, record, task::STACK);
    let cell = fx.load(PTR, record, task::STACK_CELL);
    let mapped = fx.b.ins().icmp_imm_s(IntCC::Equal, cell, 0);
    fx.choose_in(
        lo,
        mapped,
        &[],
        |lo, fx| {
            let length = fx.load(PTR, record, task::STACK_SIZE);
            system::unmap_stack(lo, fx, stack, length);
            Vec::new()
        },
        |lo, fx| {
            system::protect_guard(lo, fx, stack, true);
            lo.call_routine(fx, Routine::Free, &[stack]);
            Vec::new()
        },
    );
}

/// `IoSubmit(entry)`: submits an io_uring entry from the running task and parks until its
/// completion. A call cancelled when it starts submits nothing; one cancelled while the
/// operation is in flight cancels it and waits for the acknowledgement. Gives whether the call
/// failed with cancellation, and the completion's result.
fn io_submit(lo: &mut Lowering, fx: &mut Fx) {
    let params = fx.entry();
    let entry = params[0];
    let record = current_task(lo, fx);
    let region = fx.load(PTR, record, task::REGION);
    let cancelled = origin(fx, region);
    let cancelled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, cancelled, 0);
    fx.when(cancelled, |fx| {
        let yes = fx.int(types::I8, 1);
        let zero = fx.int(types::I32, 0);
        fx.b.ins().return_(&[yes, zero]);
    });
    let op = fx.slot(operation::SIZE, 8);
    let zero = fx.ptr(0);
    let size = fx.ptr(operation::SIZE);
    lo.call_routine(fx, Routine::Fill, &[op, zero, size]);
    fx.store(record, op, operation::TASK);
    let word = fx.offset(record, task::IO);
    fx.b.ins().atomic_store(MemFlagsData::trusted(), op, word);
    host::submit(lo, fx, entry, Some(op));
    let cancelled = origin(fx, region);
    let cancelled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, cancelled, 0);
    fx.when(cancelled, |fx| host::cancel_operation(lo, fx, op));
    wait_delivered(lo, fx, op);
    fx.repeat(&[], |fx, _| {
        let seen =
            fx.b.ins()
                .atomic_cas(MemFlagsData::trusted(), word, op, zero);
        let claimed = fx.b.ins().icmp(IntCC::NotEqual, seen, op);
        fx.when(claimed, |fx| {
            lo.relax(fx);
        });
        (claimed, Vec::new())
    });
    let result = fx.load(types::I32, op, operation::RESULT);
    let refused = fx.b.ins().icmp_imm_s(IntCC::Equal, result, CANCELED);
    let cancelled = origin(fx, region);
    let cancelled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, cancelled, 0);
    let failed = fx.b.ins().band(refused, cancelled);
    fx.b.ins().return_(&[failed, result]);
}

/// Delivers an operation's completion with its result, by its kind: a waiting task wakes, a
/// deadline that expired cancels its region and wakes the task disarming it, and a component's
/// delayed messages that are due enter its intake.
pub fn deliver(lo: &mut Lowering, fx: &mut Fx, op: Value, result: Value) {
    let kind = fx.load(PTR, op, operation::KIND);
    let wait = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, kind::WAIT);
    fx.when(wait, |fx| {
        fx.store(result, op, operation::RESULT);
        let waiting = fx.load(PTR, op, operation::TASK);
        set_done(fx, op, operation::COMPLETED);
        wake(lo, fx, waiting);
        set_done(fx, op, operation::DELIVERED);
    });
    let deadline = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, kind::DEADLINE);
    fx.when(deadline, |fx| {
        set_done(fx, op, operation::COMPLETED);
        let expired = fx.b.ins().icmp_imm_s(IntCC::Equal, result, EXPIRED);
        fx.when(expired, |fx| {
            let region_record = fx.load(PTR, op, operation::TASK);
            lo.call_routine(fx, Routine::Cancel, &[region_record]);
        });
        let waiter = fx.offset(op, operation::WAITER);
        let waiter = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), waiter);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, waiter, 0);
        fx.when(present, |fx| wake(lo, fx, waiter));
        set_done(fx, op, operation::DELIVERED);
    });
    let after = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, kind::AFTER);
    fx.when(after, |fx| {
        super::super::supplied::tasks::components::timer_completed(lo, fx, op);
    });
}

/// Stores how far an operation's completion has been delivered.
fn set_done(fx: &mut Fx, op: Value, done: i64) {
    let word = fx.offset(op, operation::DONE);
    let done = fx.ptr(done as u64);
    fx.b.ins().atomic_store(MemFlagsData::trusted(), done, word);
}

/// Waits until an operation's completion has been delivered, the running task parking while it
/// has not arrived; on the seeded scheduler a completion queued is delivered at once.
pub fn wait_delivered(lo: &mut Lowering, fx: &mut Fx, op: Value) {
    let word = fx.offset(op, operation::DONE);
    fx.repeat(&[], |fx, _| {
        let done = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), word);
        let pending = fx.b.ins().icmp_imm_s(IntCC::Equal, done, 0);
        fx.when(pending, |fx| {
            lo.call_routine(fx, Routine::Park, &[]);
        });
        if lo.seeded() {
            let queued =
                fx.b.ins()
                    .icmp_imm_s(IntCC::Equal, done, operation::COMPLETED);
            fx.when(queued, |fx| super::seeded::reap(lo, fx));
        }
        let delivered =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, done, operation::DELIVERED);
        (fx.b.ins().bxor_imm_s(delivered, 1), Vec::new())
    });
}

impl Lowering<'_, '_> {
    /// Arms a timer that expires `ns` nanoseconds from now: a timeout of the relative time it
    /// writes at `spec` on the running thread's ring, or a timer of the virtual clock on the
    /// seeded scheduler and of the kernel's clock on a kernel.
    pub fn arm_timer(&mut self, fx: &mut Fx, op: Value, ns: Value, spec: Value) {
        if self.seeded() {
            super::seeded::arm(self, fx, op, ns);
            return;
        }
        if self.kernel() {
            self.kernel_arm(fx, op, ns);
            return;
        }
        let billion = fx.ptr(1_000_000_000);
        let seconds = fx.b.ins().udiv(ns, billion);
        let nanoseconds = fx.b.ins().urem(ns, billion);
        fx.store(seconds, spec, 0);
        fx.store(nanoseconds, spec, 8);
        host::submit_timeout(self, fx, op, spec);
    }

    /// Asks once for the cancellation of an operation whose record stays valid during the
    /// call; on the seeded scheduler and on a kernel, where every operation is a wait for a
    /// timer, one in flight completes cancelled at once.
    pub fn cancel_operation(&mut self, fx: &mut Fx, op: Value) {
        if self.seeded() {
            super::seeded::cancel(self, fx, op);
        } else if self.kernel() {
            self.kernel_cancel(fx, op);
        } else {
            host::cancel_operation(self, fx, op);
        }
    }

    /// Delivers the completions that have arrived: those on the running thread's ring, or
    /// those the seeded scheduler queued; a kernel's waits end by wakes alone.
    pub fn reap(&mut self, fx: &mut Fx) {
        if self.seeded() {
            super::seeded::reap(self, fx);
        } else if !self.kernel() {
            host::reap(self, fx, None);
        }
    }

    /// The nanoseconds of the clock that delays count: the monotonic clock, the virtual clock
    /// on the seeded scheduler, or the kernel's clock.
    pub fn now_ns(&mut self, fx: &mut Fx) -> Value {
        if self.seeded() {
            return super::seeded::now(self, fx);
        }
        match self.kernel() {
            true => self.call_routine(fx, Routine::ClockNow, &[])[0],
            false => self.platform_call(fx, "now_ns", &[]).result.scalar(),
        }
    }
}
