//! The support routines: the fixed routines of the implementation that lowered code, compiler
//! stubs and seams call, each named `_NS`, its routine's name, `_` and a suffix. Their state is
//! the writable data object `_NSstate` and the thread-local `_NSthread`, which addresses the
//! record of the executor thread running.
//!
//! `mem_op` copies, fills and compares; `cap_table` keeps the cells, chunks and capability
//! tables; `stack_switch` keeps tasks, their stacks and the executors' run queues; `trap` writes
//! trap reports and abandons domains; `settle` settles abandoned domains; `replay` runs the
//! seeded scheduler of test images.

pub mod holdings;
pub mod locks;
pub mod memory;
pub mod regions;
pub mod report;
pub mod seeded;
pub mod settle;
pub mod system;
pub mod tables;
pub mod tasks;

use cranelift_codegen::ir::{
    AbiParam, AtomicRmwOp, InstBuilder, MemFlagsData, Signature, Value, condcodes::IntCC, types,
};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};

use super::fx::{Fx, PTR};
use super::machine::Stub;
use super::{CALL_CONV, Lowering};

/// A support routine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Routine {
    /// `(dst, src, len)`: copies bytes between ranges that may overlap.
    Copy,
    /// `(dst, byte, len)`: writes one byte value over a range.
    Fill,
    /// `(left, right, len) -> i32`: compares two ranges as unsigned bytes.
    Compare,
    /// `(cell, size, align) -> address`: an allocation through an allocator in the running
    /// task's domain, zero on exhaustion.
    Alloc,
    /// `(cell, size, align, domain) -> address`: an allocation in a domain's chunks.
    AllocIn,
    /// `(address)`: frees an allocation.
    Free,
    /// `(size) -> address`: a zeroed runtime record of `size` bytes, zero on exhaustion.
    RecordAlloc,
    /// `(address)`: frees a runtime record.
    RecordFree,
    /// `(address, size) -> address`: a runtime record moved to `size` bytes, keeping its
    /// leading bytes; zero on exhaustion, the record then unchanged.
    RecordResize,
    /// `(cell, limit) -> cell`: a budget's new cell under an allocator's.
    Split,
    /// `(cell)`: closes a cell.
    Close,
    /// `(cell) -> bytes`: a cell's charge.
    Charged,
    /// `(cell) -> bytes`: a cell's limit.
    Limit,
    /// `(size) -> address`: the environment of a task closure, in runtime memory.
    TaskEnv,
    /// `(address)`: frees a task closure's environment.
    TaskEnvFree,
    /// `(address, domain) -> lifted`: passes the chunks an allocation lies in to a domain when
    /// a domain nested in it owns them; nonzero when they passed.
    Lift,
    /// `(kind, payload) -> holding`: a new holding the running task's domain owns.
    Hold,
    /// `(holding)`: ends a holding released.
    Unhold,
    /// `(holding, domain)`: passes a holding to a domain, or to none for zero.
    Rehome,
    /// `(world)`: fills a root grant's capabilities and allocator.
    MintRoot,
    /// `(handle) -> entry`: a capability's table entry, zero when stale.
    Check,
    /// `(handle, kind) -> handle`: a new entry deriving from a capability's entry.
    Derive,
    /// `(handle)`: revokes an entry and every entry deriving from it.
    Revoke,
    /// `(runner, key, keyed) -> handle`: a new outbox of a runner.
    OutboxMint,
    /// `(handle)`: makes an outbox stale.
    OutboxRetire,
    /// `(handle) -> runner`: the runner of a live outbox, taking a reference to its record;
    /// zero for a stale outbox.
    OutboxClaim,
    /// `(kind, operands, count, caller) -> Never`: traps the running task.
    TrapRaise,
    /// `(location) -> Never`: halts the kernel for a lock its CPU acquires again.
    Reacquired,
    /// `(domain)`: settles an abandoned `sys::Domain` body.
    Settle,
    /// `(signal, info, context)`: the handler of the stop signal.
    Preempt,
    /// `(signal, info, context)`: the handler of a fault, which reports a stack overflow.
    Overflow,
    /// `(signal, info, context)`: the handler of an exceeded processor time limit, which ends a
    /// test run.
    CpuLimit,
    /// `() -> Never`: traps with `trap.stack-overflow`, run on a thread's trap stack.
    OverflowRaise,
    /// `(task) -> Never`: stops the running task, which a stop signal interrupted.
    Stop,
    /// `(report, bits, how)`: writes a scalar in a trap report as its writing says.
    ReportScalar,
    /// `(report, start, len, bytes)`: writes the bytes of a range in a trap report as a text
    /// literal, or as a byte-string literal when `bytes` is not 0.
    ReportText,
    /// `(text, len)`: writes bytes to standard error.
    WriteErr,
    /// `(glue, code, env, extra, stack, cell, domain, region) -> task`: a new task, not yet
    /// ready, its stack drawn through a cell; zero on exhaustion.
    Spawn,
    /// `(task)`: makes a task ready.
    Wake,
    /// `()`: suspends the running task until a wake.
    Park,
    /// `(task)`: the first code a task runs on its own stack.
    TaskStart,
    /// `(entry) -> (cancelled, result)`: submits an io_uring entry and waits for its
    /// completion or its acknowledged cancellation.
    IoSubmit,
    /// `() -> Never`: runs the executor's scheduler on the calling thread.
    Schedule,
    /// `(thread) -> Never`: the start routine of an executor thread.
    ThreadMain,
    /// `(size) -> region`: a cancelling construct's new record of `size` bytes, enclosed by
    /// the running task's region.
    RegionOpen,
    /// `(region)`: cancels a construct.
    Cancel,
    /// `() -> origin`: the origin a cancellation of the running call holds, zero when no
    /// construct holding it has cancelled.
    Origin,
    /// `(held)`: releases the cowns an `await when` holds.
    CownRelease,
    /// `(status) -> Never`: ends the process.
    Exit,
    /// `()`: sets up the runtime, nonzero when it cannot.
    Init,
    /// `(lock)`: takes a lock word.
    Lock,
    /// `(lock)`: releases a lock word.
    Unlock,
    /// `() -> thread`: the record of the executor thread running the caller, which a call
    /// reads anew after each suspension as a task may resume on another thread.
    Thread,
    /// `()`: a scheduling point of the running task on the seeded scheduler.
    Yield,
    /// `(seed, stream, index) -> value`: value `index` of a stream of a seed.
    Stream,
    /// `(choices, low, high, signed) -> value`: a draw of a property test's case.
    Draw,
}

impl Routine {
    pub fn name(self) -> &'static str {
        match self {
            Routine::Copy => "_NSmem_op_copy",
            Routine::Fill => "_NSmem_op_fill",
            Routine::Compare => "_NSmem_op_compare",
            Routine::Alloc => "_NScap_table_alloc",
            Routine::AllocIn => "_NScap_table_alloc_in",
            Routine::Free => "_NScap_table_free",
            Routine::RecordAlloc => "_NScap_table_record_alloc",
            Routine::RecordFree => "_NScap_table_record_free",
            Routine::RecordResize => "_NScap_table_record_resize",
            Routine::Split => "_NScap_table_split",
            Routine::Close => "_NScap_table_close",
            Routine::Charged => "_NScap_table_charged",
            Routine::Limit => "_NScap_table_limit",
            Routine::TaskEnv => "_NScap_table_task_env",
            Routine::TaskEnvFree => "_NScap_table_task_env_free",
            Routine::Lift => "_NScap_table_lift",
            Routine::Hold => "_NScap_table_hold",
            Routine::Unhold => "_NScap_table_unhold",
            Routine::Rehome => "_NScap_table_rehome",
            Routine::MintRoot => "_NScap_table_mint_root",
            Routine::Check => "_NScap_table_check",
            Routine::Derive => "_NScap_table_derive",
            Routine::Revoke => "_NScap_table_revoke",
            Routine::OutboxMint => "_NScap_table_outbox_mint",
            Routine::OutboxRetire => "_NScap_table_outbox_retire",
            Routine::OutboxClaim => "_NScap_table_outbox_claim",
            Routine::TrapRaise => "_NStrap_raise",
            Routine::Reacquired => "_NStrap_reacquired",
            Routine::ReportScalar => "_NStrap_scalar",
            Routine::Settle => "_NSsettle_domain",
            Routine::Preempt => "_NStrap_preempt",
            Routine::Overflow => "_NStrap_overflow",
            Routine::CpuLimit => "_NSreplay_cpu_limit",
            Routine::OverflowRaise => "_NStrap_overflow_raise",
            Routine::Stop => "_NSstack_switch_stop",
            Routine::ReportText => "_NStrap_text",
            Routine::WriteErr => "_NStrap_write_err",
            Routine::Spawn => "_NSstack_switch_spawn",
            Routine::Wake => "_NSstack_switch_wake",
            Routine::Park => "_NSstack_switch_park",
            Routine::TaskStart => "_NSstack_switch_task_start",
            Routine::IoSubmit => "_NSstack_switch_io_submit",
            Routine::Schedule => "_NSstack_switch_schedule",
            Routine::ThreadMain => "_NSstack_switch_thread_main",
            Routine::RegionOpen => "_NSstack_switch_region_open",
            Routine::Cancel => "_NSstack_switch_cancel",
            Routine::Origin => "_NSstack_switch_origin",
            Routine::CownRelease => "_NSstack_switch_cown_release",
            Routine::Exit => "_NSstack_switch_exit",
            Routine::Init => "_NSstack_switch_init",
            Routine::Lock => "_NSstack_switch_lock",
            Routine::Unlock => "_NSstack_switch_unlock",
            Routine::Thread => "_NSstack_switch_thread",
            Routine::Yield => "_NSreplay_yield",
            Routine::Stream => "_NSreplay_stream",
            Routine::Draw => "_NSreplay_draw",
        }
    }

    /// The parameter and result types of the routine.
    fn types(
        self,
    ) -> (
        &'static [cranelift_codegen::ir::Type],
        &'static [cranelift_codegen::ir::Type],
    ) {
        const P: cranelift_codegen::ir::Type = types::I64;
        match self {
            Routine::Copy | Routine::Fill => (&[P, P, P], &[]),
            Routine::OutboxMint => (&[P, P, P], &[P]),
            Routine::Compare => (&[P, P, P], &[types::I32]),
            Routine::Alloc => (&[P, P, P], &[P]),
            Routine::AllocIn => (&[P, P, P, P], &[P]),
            Routine::Split | Routine::Derive | Routine::Hold => (&[P, P], &[P]),
            Routine::Lift => (&[P, P], &[types::I8]),
            Routine::Rehome => (&[P, P], &[]),
            Routine::Charged
            | Routine::Limit
            | Routine::TaskEnv
            | Routine::Check
            | Routine::OutboxClaim
            | Routine::RegionOpen
            | Routine::RecordAlloc => (&[P], &[P]),
            Routine::RecordResize => (&[P, P], &[P]),
            Routine::Free
            | Routine::RecordFree
            | Routine::Close
            | Routine::TaskEnvFree
            | Routine::Unhold
            | Routine::MintRoot
            | Routine::Revoke
            | Routine::OutboxRetire
            | Routine::Wake
            | Routine::TaskStart
            | Routine::Cancel
            | Routine::CownRelease
            | Routine::Exit
            | Routine::ThreadMain
            | Routine::Lock
            | Routine::Unlock
            | Routine::Settle
            | Routine::Reacquired
            | Routine::Stop => (&[P], &[]),
            Routine::Preempt | Routine::Overflow | Routine::CpuLimit => (&[types::I32, P, P], &[]),
            Routine::OverflowRaise => (&[], &[]),
            Routine::TrapRaise | Routine::ReportText => (&[P, P, P, P], &[]),
            Routine::ReportScalar => (&[P, P, P], &[]),
            Routine::WriteErr => (&[P, P], &[]),
            Routine::Spawn => (&[P, P, P, P, P, P, P, P], &[P]),
            Routine::Park | Routine::Schedule | Routine::Yield => (&[], &[]),
            Routine::Stream => (&[P, P, P], &[P]),
            Routine::Draw => (&[P, P, P, P], &[P]),
            Routine::Origin | Routine::Init | Routine::Thread => (&[], &[P]),
            Routine::IoSubmit => (&[P], &[types::I8, types::I32]),
        }
    }

    pub fn signature(self) -> Signature {
        let (params, returns) = self.types();
        let mut signature = Signature::new(CALL_CONV);
        signature
            .params
            .extend(params.iter().map(|ty| AbiParam::new(*ty)));
        signature
            .returns
            .extend(returns.iter().map(|ty| AbiParam::new(*ty)));
        signature
    }
}

/// The offsets of the runtime state's fields.
pub mod state {
    /// The lock of the cell and capability tables and of chunks.
    pub const LOCK: u64 = 0;
    /// The lock of the run queue, and its first and last tasks.
    pub const QUEUE_LOCK: u64 = 8;
    pub const READY_HEAD: u64 = 16;
    pub const READY_TAIL: u64 = 24;
    /// The executor thread records and their count.
    pub const THREADS: u64 = 32;
    pub const THREAD_COUNT: u64 = 40;
    /// The cell table: its records and the count of slots used.
    pub const CELLS: u64 = 48;
    pub const CELL_COUNT: u64 = 56;
    /// The capability table: its records and the count of slots used.
    pub const ENTRIES: u64 = 64;
    pub const ENTRY_COUNT: u64 = 72;
    /// The process domain's record.
    pub const PROCESS: u64 = 80;
    /// The entry task.
    pub const ENTRY_TASK: u64 = 88;
    /// The signal number of the first termination request, zero before one.
    pub const TERMINATION: u64 = 96;
    /// The operation reading termination requests.
    pub const SIGNALS: u64 = 104;
    /// Every live task, linked through their records, and the lock of that list.
    pub const TASKS: u64 = 120;
    pub const TASKS_LOCK: u64 = 128;
    /// The milliseconds of the monotonic clock when the root grant was minted.
    pub const MINTED: u64 = 136;
    /// The last rank a cown took.
    pub const RANKS: u64 = 144;
    /// The chunk map's table of roots.
    pub const CHUNK_MAP: u64 = 152;
    /// The index plus one of the first free capability entry, zero when none is free.
    pub const FREE_ENTRY: u64 = 160;
    /// The last id a cancelling construct took.
    pub const REGIONS: u64 = 168;
    /// The construct that termination requests cancel, whose region is the entry task.
    pub const ROOT: u64 = 176;
    /// On the seeded scheduler: the run's seed, the index of the scheduling stream's next
    /// value, the virtual clock's milliseconds and its first timer, the last order a task or
    /// timer took, whether a scheduling point found two ready tasks, and the first and last
    /// of the completions not yet delivered.
    pub const SEED: u64 = 184;
    pub const PICKS: u64 = 192;
    pub const NOW: u64 = 200;
    pub const TIMERS: u64 = 208;
    pub const ORDER: u64 = 216;
    pub const CONTENDED: u64 = 224;
    pub const COMPLETIONS: u64 = 232;
    pub const COMPLETIONS_TAIL: u64 = 240;
    /// On a machine target: the two values the platform entered the image with, the
    /// start-information address on `x86_64-bare` and the image handle and system table on
    /// `x86_64-uefi`; the installed anchor's address, zero before one; the location record of
    /// the `anchor` call that installed it; the number of CPUs that started their executor;
    /// nonzero once a kernel halt begins; the number of CPUs it has stopped; and on
    /// `x86_64-uefi`, once `exit_services` ends the firmware's boot services, the record of its
    /// final memory map, the map's size and the size of each of its descriptors.
    pub const PLATFORM: u64 = 248;
    pub const PLATFORM_TABLE: u64 = 256;
    pub const ANCHOR: u64 = 264;
    pub const ANCHOR_CALL: u64 = 272;
    pub const STARTED_CPUS: u64 = 280;
    pub const HALTING: u64 = 288;
    pub const STOPPED_CPUS: u64 = 296;
    pub const MEMORY_MAP: u64 = 304;
    pub const MAP_SIZE: u64 = 312;
    pub const MAP_STRIDE: u64 = 320;
    /// The single chunks returned to the platform's supply and kept mapped for the next draws,
    /// zeroed, linked through their first words, and their count.
    pub const KEPT_CHUNKS: u64 = 328;
    pub const KEPT_COUNT: u64 = 336;
    pub const SIZE: u64 = 384;
}

impl Lowering<'_, '_> {
    /// The function of a routine, declared now and defined once lowering reaches the end of
    /// its queue.
    pub fn routine(&mut self, routine: Routine) -> FuncId {
        if let Some(id) = self.routines.get(&routine) {
            return *id;
        }
        let id = self
            .module
            .declare_function(routine.name(), Linkage::Local, &routine.signature())
            .expect("a support routine");
        self.routines.insert(routine, id);
        self.queue.push(super::Pending::Routine(routine, id));
        id
    }

    /// Calls a routine, giving its results.
    pub fn call_routine(&mut self, fx: &mut Fx, routine: Routine, args: &[Value]) -> Vec<Value> {
        let id = self.routine(routine);
        let reference = self.func_ref(fx, id);
        let call = fx.b.ins().call(reference, args);
        fx.b.inst_results(call).to_vec()
    }

    /// The address of the runtime state.
    pub fn state(&mut self, fx: &mut Fx) -> Value {
        let id = self.state_data();
        let global = self.data_ref(fx, id);
        fx.b.ins().symbol_value(PTR, global)
    }

    fn state_data(&mut self) -> DataId {
        if let Some(id) = self.state_id {
            return id;
        }
        let id = self
            .module
            .declare_data("_NSstate", Linkage::Local, true, false)
            .expect("the runtime state");
        let mut description = DataDescription::new();
        description.define_zeroinit(state::SIZE as usize);
        description.set_align(64);
        self.module
            .define_data(id, &description)
            .expect("the runtime state");
        self.state_id = Some(id);
        id
    }

    /// The address of the thread-local word holding the running executor thread's record, on
    /// `x86_64-host`.
    pub fn thread_slot(&mut self, fx: &mut Fx) -> Value {
        let id = match self.thread_id {
            Some(id) => id,
            None => {
                let id = self
                    .module
                    .declare_data("_NSthread", Linkage::Local, true, true)
                    .expect("the thread record word");
                let mut description = DataDescription::new();
                description.define_zeroinit(8);
                description.set_align(8);
                self.module
                    .define_data(id, &description)
                    .expect("the thread record word");
                self.thread_id = Some(id);
                id
            }
        };
        let global = self.data_ref(fx, id);
        fx.b.ins().tls_value(PTR, global)
    }

    /// A zeroed runtime record of `size` bytes, zero on exhaustion.
    pub fn record_alloc(&mut self, fx: &mut Fx, size: Value) -> Value {
        self.call_routine(fx, Routine::RecordAlloc, &[size])[0]
    }

    /// Frees a runtime record.
    pub fn record_free(&mut self, fx: &mut Fx, address: Value) {
        self.call_routine(fx, Routine::RecordFree, &[address]);
    }

    /// A runtime record moved to `size` bytes, zero on exhaustion.
    pub fn record_resize(&mut self, fx: &mut Fx, address: Value, size: Value) -> Value {
        self.call_routine(fx, Routine::RecordResize, &[address, size])[0]
    }

    /// The record of the running executor thread, zero outside one: on a machine target the
    /// running CPU's, which its record holds.
    pub fn thread(&mut self, fx: &mut Fx) -> Value {
        match self.machine() {
            true => self.call_stub(fx, Stub::Thread, &[])[0],
            false => self.call_routine(fx, Routine::Thread, &[])[0],
        }
    }

    /// A C library function, by name, which only an `x86_64-host` image links.
    pub fn libc(
        &mut self,
        name: &str,
        params: &[cranelift_codegen::ir::Type],
        returns: &[cranelift_codegen::ir::Type],
    ) -> FuncId {
        assert!(
            !self.machine(),
            "the C library function {name} in a machine image"
        );
        let mut signature = Signature::new(CALL_CONV);
        signature
            .params
            .extend(params.iter().map(|ty| AbiParam::new(*ty)));
        signature
            .returns
            .extend(returns.iter().map(|ty| AbiParam::new(*ty)));
        self.module
            .declare_function(name, Linkage::Import, &signature)
            .unwrap_or_else(|error| panic!("the C library function {name}: {error}"))
    }

    /// Calls a C library function, giving its results.
    pub fn call_libc(
        &mut self,
        fx: &mut Fx,
        name: &str,
        args: &[Value],
        returns: &[cranelift_codegen::ir::Type],
    ) -> Vec<Value> {
        let params: Vec<cranelift_codegen::ir::Type> = args
            .iter()
            .map(|arg| fx.b.func.dfg.value_type(*arg))
            .collect();
        let id = self.libc(name, &params, returns);
        let reference = self.func_ref(fx, id);
        let call = fx.b.ins().call(reference, args);
        fx.b.inst_results(call).to_vec()
    }

    /// Relaxes the processor in a spin loop: on `x86_64-host` it yields the processor to the
    /// system, on a machine target it hints the spin with `pause`.
    pub fn relax(&mut self, fx: &mut Fx) {
        if self.machine() {
            self.call_stub(fx, Stub::Pause, &[]);
        } else {
            self.call_libc(fx, "sched_yield", &[], &[types::I32]);
        }
    }

    /// Copies `size` bytes, inline when small.
    pub fn copy(&mut self, fx: &mut Fx, dst: Value, src: Value, size: u64, align: u64) {
        if size <= 64 {
            fx.copy_small(dst, src, size, align);
            return;
        }
        let len = fx.ptr(size);
        self.call_routine(fx, Routine::Copy, &[dst, src, len]);
    }

    /// Whether two views of bytes hold the same bytes.
    pub fn views_equal(
        &mut self,
        fx: &mut Fx,
        left: super::body::Held,
        right: super::body::Held,
    ) -> Value {
        let (left, right) = (left.address(), right.address());
        let left_len = fx.load(PTR, left, 8);
        let right_len = fx.load(PTR, right, 8);
        let same_len = fx.b.ins().icmp(IntCC::Equal, left_len, right_len);
        let result = fx.choose(
            same_len,
            &[types::I8],
            |fx| {
                let a = fx.load(PTR, left, 0);
                let b = fx.load(PTR, right, 0);
                let order = self.call_routine(fx, Routine::Compare, &[a, b, left_len]);
                vec![fx.b.ins().icmp_imm_s(IntCC::Equal, order[0], 0)]
            },
            |fx| vec![fx.int(types::I8, 0)],
        );
        result[0]
    }
}

/// Defines a routine's body.
pub fn define(lo: &mut Lowering, fx: &mut Fx, routine: Routine) {
    match routine {
        Routine::Copy | Routine::Fill | Routine::Compare => memory::bytes(lo, fx, routine),
        Routine::Alloc
        | Routine::AllocIn
        | Routine::Free
        | Routine::Split
        | Routine::Close
        | Routine::Charged
        | Routine::Limit
        | Routine::TaskEnv
        | Routine::TaskEnvFree
        | Routine::Lift => memory::define(lo, fx, routine),
        Routine::Hold | Routine::Unhold | Routine::Rehome => holdings::define(lo, fx, routine),
        Routine::MintRoot
        | Routine::Check
        | Routine::Derive
        | Routine::Revoke
        | Routine::OutboxMint
        | Routine::OutboxRetire
        | Routine::OutboxClaim => tables::define(lo, fx, routine),
        Routine::TrapRaise
        | Routine::Reacquired
        | Routine::ReportScalar
        | Routine::ReportText
        | Routine::WriteErr
        | Routine::OverflowRaise => report::define(lo, fx, routine),
        Routine::Settle => settle::define(lo, fx, routine),
        Routine::Spawn
        | Routine::Wake
        | Routine::Park
        | Routine::TaskStart
        | Routine::IoSubmit
        | Routine::Schedule
        | Routine::ThreadMain
        | Routine::Thread
        | Routine::Stop => tasks::define(lo, fx, routine),
        Routine::RegionOpen | Routine::Cancel | Routine::Origin => regions::define(lo, fx, routine),
        Routine::CownRelease => super::supplied::tasks::cowns::release_routine(lo, fx),
        Routine::Yield | Routine::Stream => seeded::define(lo, fx, routine),
        Routine::Draw => super::supplied::generate::define_draw(lo, fx),
        Routine::Lock | Routine::Unlock => locks::define(lo, fx, routine),
        Routine::Init
        | Routine::Exit
        | Routine::RecordAlloc
        | Routine::RecordFree
        | Routine::RecordResize => system::define(lo, fx, routine),
        Routine::Preempt | Routine::Overflow | Routine::CpuLimit => {
            super::host::define(lo, fx, routine)
        }
    }
}

/// Atomically adds to a word, giving its previous value.
pub fn fetch_add(fx: &mut Fx, address: Value, amount: Value) -> Value {
    fx.b.ins().atomic_rmw(
        PTR,
        MemFlagsData::trusted(),
        AtomicRmwOp::Add,
        address,
        amount,
    )
}
