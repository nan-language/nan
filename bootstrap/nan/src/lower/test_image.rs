//! Test images: a package's tests lowered for their runs on a run target, each run a process on
//! `x86_64-host` and a boot of the emulator on a machine target. The seam reads which test and
//! run to perform and starts the entry task, which mints the run's simulated world, runs the test
//! as the first task of a new test domain on the seeded scheduler, then writes the run's outcome
//! as a frame on standard output, or the serial port of a machine, and ends the run.
//!
//! A frame is its kind's byte, its payload's length as 4 bytes little-endian, then the payload.
//! An outcome's payload is its code, whether the test's target ran and whether a scheduling
//! point found two ready tasks, then the trap report, or for an escaped failure its variant's
//! canonical path, a NUL and its rendering.

use std::collections::HashMap;

use cranelift_codegen::ir::{
    AbiParam, InstBuilder, Signature, TrapCode, Value, condcodes::IntCC, types,
};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};

use super::body::Held;
use super::fx::{Fx, PTR};
use super::machine::Stub;
use super::runtime::tasks::{self, domain, trap_record};
use super::runtime::{Routine, state, system};
use super::supplied::tasks::domains;
use super::{CALL_CONV, Instance, Lowered, Lowering, lines};
use crate::check::Checker;
use crate::check::db::{ItemId, PackageId};
use crate::check::ty::{FnTy, LimitSet, Ty};
use crate::nir::{Builtin, Callable, Role};
use crate::syntax::ast::FnKind;

/// What a test image lowers: tests of one package, in the order the seam numbers them.
pub struct Tests {
    pub tests: Vec<Test>,
    pub roots: Vec<PackageId>,
    /// The package names of the final closure with the prefix their symbols carry.
    pub prefixes: HashMap<String, String>,
}

pub struct Test {
    pub item: ItemId,
    pub target: Target,
}

/// What a test's target names, each of whose instances marks that it ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Function(ItemId),
    /// A method a derive writes for an owner, by name.
    Derived(ItemId, String),
}

/// The exit status of a run that used more processor time than a test run may.
pub const TIMED_OUT: i32 = 124;

/// The texts naming a run before any forced draws: its test's number, its index and seed, and
/// its replay token.
const RUN_TEXTS: u64 = 4;

/// The emulator configuration file holding the texts that name a machine run.
pub const RUN_FILE: &str = "opt/nan/run";

/// What a machine test image writes to the serial port before its frames, after the firmware's
/// own output.
pub const RUN_START: &[u8] = b"\0nan-run\0";

/// The kinds of frames.
pub mod frame {
    pub const TRACE: u8 = 1;
    pub const DRAW: u8 = 2;
    pub const OUTCOME: u8 = 5;
}

/// The codes of outcomes.
pub mod outcome {
    pub const PASSED: u8 = 0;
    pub const TRAPPED: u8 = 1;
    pub const ESCAPED: u8 = 2;
    pub const STALLED: u8 = 3;
}

/// The offsets of the test state's fields: the test and run the seam read, the replay token,
/// the forced draws of a rerun and their count, the run's world, and the failure that escaped
/// the body.
mod test_state {
    pub const TEST: u64 = 0;
    pub const RUN: u64 = 8;
    pub const TOKEN: u64 = 16;
    pub const TOKEN_LEN: u64 = 24;
    pub const FORCED: u64 = 32;
    pub const FORCED_COUNT: u64 = 40;
    pub const WORLD: u64 = 48;
    pub const ESCAPED: u64 = 56;
    pub const ESCAPE: u64 = 64;
    pub const ESCAPE_LEN: u64 = 72;
    pub const SIZE: u64 = 80;
}

/// What lowering keeps for a test image: the distinct targets of its tests, whose instances
/// mark that they ran, each test's target among them, and the image's writable data.
pub struct Runtime {
    targets: Vec<Target>,
    test_targets: Vec<usize>,
    ran: Option<DataId>,
    state: Option<DataId>,
}

/// Lowers the tests of a package with the support routines their runs need.
pub fn lower(ck: &Checker, tests: &Tests) -> Lowered {
    let mut targets: Vec<Target> = Vec::new();
    let test_targets = tests
        .tests
        .iter()
        .map(
            |test| match targets.iter().position(|known| *known == test.target) {
                Some(index) => index,
                None => {
                    targets.push(test.target.clone());
                    targets.len() - 1
                }
            },
        )
        .collect();
    let runtime = Runtime {
        targets,
        test_targets,
        ran: None,
        state: None,
    };
    let mut lowering = Lowering::new(
        ck,
        tests.prefixes.clone(),
        tests.roots.clone(),
        Some(runtime),
    );
    let entry = entry_glue(&mut lowering, tests);
    match lowering.machine() {
        true => machine_seam(&mut lowering, entry),
        false => super::host::test_seam(&mut lowering, entry),
    }
    lowering.drain();
    lowering.finish(None)
}

/// Starts the run of a test image that `count` NUL-terminated texts name, their addresses at
/// `texts`: the `RUN_TEXTS` naming the run, its test's number, its index and seed and its replay
/// token, then for a rerun its forced draws. Sets up the runtime, then starts the entry task,
/// which runs `entry`; ends with status 71 when the runtime cannot start.
pub fn start_run(
    lo: &mut Lowering,
    fx: &mut Fx,
    entry: cranelift_module::FuncId,
    count: Value,
    texts: Value,
) {
    let failed = lo.call_routine(fx, Routine::Init, &[])[0];
    let unavailable = fx.b.ins().icmp_imm_s(IntCC::NotEqual, failed, 0);
    fx.when(unavailable, |fx| system::unavailable(lo, fx));
    let text = |fx: &mut Fx, index: u64| fx.load(PTR, texts, 8 * index);
    let mut run = [fx.ptr(0); 3];
    for (field, index) in run.iter_mut().zip(0..) {
        let text = text(fx, index);
        *field = decimal(fx, text);
    }
    let token = text(fx, RUN_TEXTS - 1);
    let len = text_len(fx, token);
    let rerun =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedGreaterThan, count, RUN_TEXTS as i64);
    let none = fx.ptr(0);
    let first = fx.b.ins().iadd_imm_s(texts, 8 * RUN_TEXTS as i64);
    let forced = fx.b.ins().select(rerun, first, none);
    let left = fx.b.ins().iadd_imm_s(count, -(RUN_TEXTS as i64));
    let left = fx.b.ins().select(rerun, left, none);
    arguments(lo, fx, run, (token, len), (forced, left));
    let state_address = lo.state(fx);
    let process = fx.load(PTR, state_address, state::PROCESS);
    let root = fx.load(PTR, state_address, state::ROOT);
    let glue = lo.func_ref(fx, entry);
    let glue = fx.b.ins().func_addr(PTR, glue);
    let zero = fx.ptr(0);
    let stack = fx.ptr(tasks::STACK);
    let record = lo.call_routine(
        fx,
        Routine::Spawn,
        &[glue, zero, zero, zero, stack, zero, process, root],
    )[0];
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
    fx.when(missing, |fx| system::unavailable(lo, fx));
    fx.store(record, state_address, state::ENTRY_TASK);
    lo.call_routine(fx, Routine::Wake, &[record]);
    lo.call_routine(fx, Routine::Schedule, &[]);
    fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
}

/// The length of a NUL-terminated text.
fn text_len(fx: &mut Fx, text: Value) -> Value {
    let zero = fx.ptr(0);
    fx.repeat(&[zero], |fx, state| {
        let at = fx.b.ins().iadd(text, state[0]);
        let byte = fx.load(types::I8, at, 0);
        let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, byte, 0);
        let next = fx.b.ins().iadd_imm_s(state[0], 1);
        let len = fx.b.ins().select(more, next, state[0]);
        (more, vec![len])
    })[0]
}

/// The unsigned decimal number a NUL-terminated text writes.
fn decimal(fx: &mut Fx, text: Value) -> Value {
    let zero = fx.ptr(0);
    fx.repeat(&[text, zero], |fx, state| {
        let (at, value) = (state[0], state[1]);
        let byte = fx.load(types::I8, at, 0);
        let byte = fx.b.ins().uextend(PTR, byte);
        let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, byte, 0);
        let digit = fx.b.ins().iadd_imm_s(byte, -i64::from(b'0'));
        let shifted = fx.b.ins().imul_imm_s(value, 10);
        let grown = fx.b.ins().iadd(shifted, digit);
        let value = fx.b.ins().select(more, grown, value);
        let next = fx.b.ins().iadd_imm_s(at, 1);
        (more, vec![next, value])
    })[1]
}

/// The entry seam of a machine test image, whose boot glue ignores the platform's values,
/// disables maskable interrupts, the firmware's included, and reads the texts naming its run,
/// each NUL-terminated, from the emulator configuration file `RUN_FILE`.
fn machine_seam(lo: &mut Lowering, entry: FuncId) {
    let mut signature = Signature::new(CALL_CONV);
    signature.params.push(AbiParam::new(PTR));
    signature.params.push(AbiParam::new(PTR));
    let boot = lo
        .module
        .declare_function("_NStest_boot", Linkage::Local, &signature)
        .expect("the test boot glue");
    lo.define(
        boot,
        signature,
        lines::HIDDEN | lines::CRITICAL,
        |lo, fx| {
            fx.entry();
            lo.call_stub(fx, Stub::Cli, &[]);
            let start = lo.view_literal(fx, RUN_START).address();
            let (text, len) = (fx.load(PTR, start, 0), fx.load(PTR, start, 8));
            super::runtime::system::write_stream(lo, fx, 1, text, len);
            let (bytes, len) = lo.fw_cfg_file(fx, RUN_FILE);
            let (count, texts) = texts(lo, fx, bytes, len);
            start_run(lo, fx, entry, count, texts);
        },
    );
    lo.entry_seam(boot, super::machine::BOOT_STACK);
}

/// The NUL-terminated texts of a run file's bytes, which a NUL follows: their count and an array
/// of their addresses, with `RUN_TEXTS` more entries addressing that last NUL, standing for texts
/// the file lacks.
fn texts(lo: &mut Lowering, fx: &mut Fx, bytes: Value, len: Value) -> (Value, Value) {
    let zero = fx.ptr(0);
    let count = fx.var(PTR, zero);
    fx.each(zero, len, |fx, index| {
        let at = fx.b.ins().iadd(bytes, index);
        let byte = fx.load(types::I8, at, 0);
        let ends = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, 0);
        let ends = fx.b.ins().uextend(PTR, ends);
        let counted = fx.b.use_var(count);
        let counted = fx.b.ins().iadd(counted, ends);
        fx.b.def_var(count, counted);
    });
    let count = fx.b.use_var(count);
    let slots = fx.b.ins().iadd_imm_s(count, RUN_TEXTS as i64);
    let size = fx.b.ins().imul_imm_s(slots, 8);
    let array = lo.record_alloc(fx, size);
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, array, 0);
    fx.when(missing, |fx| system::unavailable(lo, fx));
    let empty = fx.b.ins().iadd(bytes, len);
    fx.each(zero, slots, |fx, index| {
        let slot = fx.b.ins().imul_imm_s(index, 8);
        let slot = fx.b.ins().iadd(array, slot);
        fx.store(empty, slot, 0);
    });
    let start = fx.var(PTR, bytes);
    let filled = fx.var(PTR, zero);
    fx.each(zero, len, |fx, index| {
        let at = fx.b.ins().iadd(bytes, index);
        let byte = fx.load(types::I8, at, 0);
        let ends = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, 0);
        fx.when(ends, |fx| {
            let index = fx.b.use_var(filled);
            let slot = fx.b.ins().imul_imm_s(index, 8);
            let slot = fx.b.ins().iadd(array, slot);
            let text = fx.b.use_var(start);
            fx.store(text, slot, 0);
            let index = fx.b.ins().iadd_imm_s(index, 1);
            fx.b.def_var(filled, index);
            let next = fx.b.ins().iadd_imm_s(at, 1);
            fx.b.def_var(start, next);
        });
    });
    (count, array)
}

impl Lowering<'_, '_> {
    /// Whether the image is a test image, whose tasks run on the seeded scheduler.
    pub fn seeded(&self) -> bool {
        self.test.is_some()
    }

    /// The index of the target an instance's calls run, when it is one of the image's tests'.
    pub fn target_slot(&self, instance: &Instance) -> Option<u32> {
        let runtime = self.test.as_ref()?;
        let target = match instance {
            Instance::Nir {
                item,
                role: Role::Body,
                ..
            }
            | Instance::Supplied { item, .. } => Target::Function(*item),
            Instance::Derived { owner, method, .. } => {
                Target::Derived(*owner, (*method).to_owned())
            }
            _ => return None,
        };
        runtime
            .targets
            .iter()
            .position(|known| *known == target)
            .map(|index| index as u32)
    }

    /// The index of the target a direct call runs, when it is one of the image's tests', for
    /// the calls lowered where they are written.
    pub fn called_target(&self, callable: &Callable) -> Option<u32> {
        let runtime = self.test.as_ref()?;
        runtime
            .targets
            .iter()
            .position(|known| match (known, callable) {
                (Target::Function(target), Callable::Item { item, .. }) => target == item,
                (Target::Function(target), Callable::Builtin(Builtin::Intrinsic(path))) => {
                    self.ck.db.canonical(*target) == *path
                }
                _ => false,
            })
            .map(|index| index as u32)
    }

    /// The writable bytes recording which targets ran, one per target.
    pub fn ran_data(&mut self) -> DataId {
        let runtime = self.test.as_mut().expect("a test image");
        if let Some(id) = runtime.ran {
            return id;
        }
        let count = runtime.targets.len();
        let id = self
            .module
            .declare_data("_NSreplay_ran", Linkage::Local, true, false)
            .expect("the targets run");
        let mut description = DataDescription::new();
        description.define_zeroinit(count.max(1));
        self.module
            .define_data(id, &description)
            .expect("the targets run");
        self.test.as_mut().expect("a test image").ran = Some(id);
        id
    }

    /// Marks that the target at a slot ran.
    pub fn mark_ran(&mut self, fx: &mut Fx, slot: u32) {
        let id = self.ran_data();
        let global = self.data_ref(fx, id);
        let base = fx.b.ins().symbol_value(PTR, global);
        let one = fx.int(types::I8, 1);
        fx.store(one, base, u64::from(slot));
    }

    /// The address of the test state.
    fn test_state(&mut self, fx: &mut Fx) -> Value {
        let runtime = self.test.as_mut().expect("a test image");
        let id = match runtime.state {
            Some(id) => id,
            None => {
                let id = self
                    .module
                    .declare_data("_NSreplay_test", Linkage::Local, true, false)
                    .expect("the test state");
                let mut description = DataDescription::new();
                description.define_zeroinit(test_state::SIZE as usize);
                description.set_align(8);
                self.module
                    .define_data(id, &description)
                    .expect("the test state");
                self.test.as_mut().expect("a test image").state = Some(id);
                id
            }
        };
        let global = self.data_ref(fx, id);
        fx.b.ins().symbol_value(PTR, global)
    }
}

/// Records the run the seam read: the test's number, the run's index and seed, its replay
/// token, and for a rerun the texts of its forced draws and their count, zero otherwise.
pub fn arguments(
    lo: &mut Lowering,
    fx: &mut Fx,
    run: [Value; 3],
    token: (Value, Value),
    forced: (Value, Value),
) {
    let at = lo.test_state(fx);
    fx.store(run[0], at, test_state::TEST);
    fx.store(run[1], at, test_state::RUN);
    fx.store(token.0, at, test_state::TOKEN);
    fx.store(token.1, at, test_state::TOKEN_LEN);
    fx.store(forced.0, at, test_state::FORCED);
    fx.store(forced.1, at, test_state::FORCED_COUNT);
    let state_address = lo.state(fx);
    fx.store(run[2], state_address, state::SEED);
}

/// The run's replay token: its address and length.
pub fn token(lo: &mut Lowering, fx: &mut Fx) -> (Value, Value) {
    let at = lo.test_state(fx);
    (
        fx.load(PTR, at, test_state::TOKEN),
        fx.load(PTR, at, test_state::TOKEN_LEN),
    )
}

/// The address of the run's one `host::World`.
pub fn world(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let at = lo.test_state(fx);
    fx.load(PTR, at, test_state::WORLD)
}

/// The type of the function value a test's run starts its domain with.
fn run_type() -> FnTy {
    FnTy {
        kind: FnKind::Task,
        params: Vec::new(),
        result: Ty::Unit,
        fails: None,
        wait: true,
        limits: LimitSet::default(),
        convention: None,
    }
}

/// The glue the entry task runs: mints the run's world, runs the numbered test as the first
/// task of a test domain nested in the process domain, then writes the outcome and ends the
/// process.
fn entry_glue(lo: &mut Lowering, tests: &Tests) -> FuncId {
    let runs: Vec<FuncId> = (0..tests.tests.len())
        .map(|index| run_glue(lo, tests, index))
        .collect();
    let mut signature = Signature::new(CALL_CONV);
    signature.params.push(AbiParam::new(PTR));
    let id = lo
        .module
        .declare_function("_NSreplay_entry", Linkage::Local, &signature)
        .expect("the test entry glue");
    lo.define(id, signature, lines::HIDDEN, |lo, fx| {
        fx.entry();
        let world = super::supplied::sims::mint_world(lo, fx);
        let at = lo.test_state(fx);
        fx.store(world, at, test_state::WORLD);
        let test = fx.load(PTR, at, test_state::TEST);
        let function = fx.slot(16, 8);
        let zero = fx.ptr(0);
        fx.store(zero, function, 0);
        fx.store(zero, function, 8);
        let slots = lo.test.as_ref().expect("a test image").test_targets.clone();
        let slot = fx.slot(8, 8);
        for (index, run) in runs.iter().enumerate() {
            let this = fx.b.ins().icmp_imm_s(IntCC::Equal, test, index as i64);
            fx.when(this, |fx| {
                let run = lo.func_ref(fx, *run);
                let code = fx.b.ins().func_addr(PTR, run);
                fx.store(code, function, 0);
                let target = fx.ptr(slots[index] as u64);
                fx.store(target, slot, 0);
            });
        }
        let world_ty = lo.std_ty("host", "World", Vec::new());
        let alloc = lo.layouts.field(lo.ck, &world_ty, None, "alloc").offset;
        let cell = fx.load(PTR, world, alloc);
        let domains::Nested { record, trap, .. } =
            domains::run_nested(lo, fx, &run_type(), function, cell, false);
        let target = fx.load(PTR, slot, 0);
        let id = lo.ran_data();
        let global = lo.data_ref(fx, id);
        let ran = fx.b.ins().symbol_value(PTR, global);
        let ran = fx.b.ins().iadd(ran, target);
        let ran = fx.load(types::I8, ran, 0);
        let abandoned = fx.load(PTR, record, domain::ABANDONED);
        let abandoned = fx.b.ins().icmp_imm_s(IntCC::NotEqual, abandoned, 0);
        fx.when(abandoned, |fx| {
            lo.call_routine(fx, Routine::Settle, &[record]);
            let len = fx.load(PTR, trap, trap_record::LEN);
            let bytes = fx.offset(trap, trap_record::BYTES);
            finish(lo, fx, outcome::TRAPPED, ran, &[(bytes, len)]);
        });
        let at = lo.test_state(fx);
        let escaped = fx.load(PTR, at, test_state::ESCAPED);
        let escaped = fx.b.ins().icmp_imm_s(IntCC::NotEqual, escaped, 0);
        fx.when(escaped, |fx| {
            let text = fx.load(PTR, at, test_state::ESCAPE);
            let len = fx.load(PTR, at, test_state::ESCAPE_LEN);
            finish(lo, fx, outcome::ESCAPED, ran, &[(text, len)]);
        });
        finish(lo, fx, outcome::PASSED, ran, &[]);
    });
    id
}

/// Writes the run's outcome and ends the process.
fn finish(lo: &mut Lowering, fx: &mut Fx, code: u8, ran: Value, parts: &[(Value, Value)]) {
    let head = fx.slot(3, 1);
    let code = fx.int(types::I8, i64::from(code));
    fx.store(code, head, 0);
    fx.store(ran, head, 1);
    let state_address = lo.state(fx);
    let contended = fx.load(types::I8, state_address, state::CONTENDED);
    fx.store(contended, head, 2);
    let three = fx.ptr(3);
    let mut all = vec![(head, three)];
    all.extend_from_slice(parts);
    write_frame(lo, fx, frame::OUTCOME, &all);
    let status = fx.ptr(0);
    lo.call_routine(fx, Routine::Exit, &[status]);
    fx.b.ins()
        .trap(cranelift_codegen::ir::TrapCode::user(1).expect("a trap code"));
}

/// Ends a run in which no task is ready and no deadline is pending.
pub fn stalled(lo: &mut Lowering, fx: &mut Fx) {
    let id = lo.ran_data();
    let global = lo.data_ref(fx, id);
    let ran = fx.b.ins().symbol_value(PTR, global);
    let at = lo.test_state(fx);
    let test = fx.load(PTR, at, test_state::TEST);
    let slots = lo.test.as_ref().expect("a test image").test_targets.clone();
    let mut slot = fx.ptr(0);
    for (index, target) in slots.iter().enumerate() {
        let this = fx.b.ins().icmp_imm_s(IntCC::Equal, test, index as i64);
        let candidate = fx.ptr(*target as u64);
        slot = fx.b.ins().select(this, candidate, slot);
    }
    let ran = fx.b.ins().iadd(ran, slot);
    let ran = fx.load(types::I8, ran, 0);
    finish(lo, fx, outcome::STALLED, ran, &[]);
}

/// Writes a frame of a kind whose payload is the concatenation of byte ranges.
pub fn write_frame(lo: &mut Lowering, fx: &mut Fx, kind: u8, parts: &[(Value, Value)]) {
    let mut len = fx.ptr(0);
    for (_, part) in parts {
        len = fx.b.ins().iadd(len, *part);
    }
    let header = fx.slot(5, 1);
    let kind = fx.int(types::I8, i64::from(kind));
    fx.store(kind, header, 0);
    let len = fx.b.ins().ireduce(types::I32, len);
    fx.b.ins().store(super::fx::unaligned(), len, header, 1);
    let five = fx.ptr(5);
    super::runtime::system::write_stream(lo, fx, 1, header, five);
    for (start, part) in parts {
        super::runtime::system::write_stream(lo, fx, 1, *start, *part);
    }
}

/// The glue a test's run starts its domain with, a function value's code: generates the case of
/// a property test, calls the test's body with it and releases the parameters the body reads,
/// then returns, a failure escaping the body having been recorded.
fn run_glue(lo: &mut Lowering, tests: &Tests, index: usize) -> FuncId {
    let fn_ty = run_type();
    let abi = lo.fn_abi(&fn_ty);
    let item = tests.tests[index].item;
    let id = lo
        .module
        .declare_function(
            &format!("_NSreplay_run_{index}"),
            Linkage::Local,
            &abi.signature(),
        )
        .expect("a test's run glue");
    let params: Vec<Ty> = lo.ck.typed(item).map_or_else(Vec::new, |body| {
        body.params
            .iter()
            .map(|param| body.local(*param).ty.clone())
            .collect()
    });
    lo.define(id, abi.signature(), lines::HIDDEN, |lo, fx| {
        fx.entry();
        let body = Instance::Nir {
            item,
            role: Role::Body,
            index: 0,
            args: crate::check::ty::no_args(),
        };
        let body_abi = lo.abi(&body);
        let mut args = Vec::new();
        if !params.is_empty() {
            let world_address = world(lo, fx);
            let world_ty = lo.std_ty("host", "World", Vec::new());
            let alloc_at = lo.layouts.field(lo.ck, &world_ty, None, "alloc").offset;
            let alloc = fx.load(PTR, world_address, alloc_at);
            let at = lo.test_state(fx);
            let run = (
                fx.load(PTR, at, test_state::RUN),
                fx.load(PTR, at, test_state::FORCED),
                fx.load(PTR, at, test_state::FORCED_COUNT),
            );
            let slots = super::supplied::generate::case(lo, fx, item, &params, alloc, run);
            for ((ty, slot), (nty, _)) in params.iter().zip(&slots).zip(&body_abi.params) {
                args.push(match nty {
                    crate::nir::NTy::Owned(_) => lo.held_at(fx, ty, *slot),
                    _ => Held::Address(*slot),
                });
            }
            let id = lo.instance(body.clone());
            lo.invoke(fx, super::call::Invoked::Direct(id), &body_abi, &args);
            for ((ty, slot), (nty, _)) in params.iter().zip(&slots).zip(&body_abi.params) {
                if nty.is_reference() {
                    lo.release(fx, ty, *slot);
                }
            }
        } else {
            let id = lo.instance(body);
            lo.invoke(fx, super::call::Invoked::Direct(id), &body_abi, &args);
        }
        let passed = fx.int(types::I8, 0);
        fx.b.ins().return_(&[passed]);
    });
    id
}

/// Records a failure of a type escaping a test's body: its variant's canonical path and, when
/// its type implements `Render`, its rendering within a render budget of 4,096 bytes.
pub fn escape(lo: &mut Lowering, fx: &mut Fx, held: Held, ty: &Ty) {
    let address = match held {
        Held::Address(address) => address,
        other => {
            let layout = lo.layout(ty);
            let slot = fx.slot(layout.size, layout.align);
            lo.store_held(fx, slot, other, ty);
            slot
        }
    };
    let path = lo.ck.db.canonical(match ty {
        Ty::Item(item, _) => *item,
        other => panic!("an escaped failure of {other:?}"),
    });
    let names = match ty {
        Ty::Item(item, _) => lo.variant_names(*item),
        _ => Vec::new(),
    };
    let text = fx.slot(8 + 4_096, 8);
    let zero = fx.ptr(0);
    let len = fx.var(PTR, zero);
    let append = |lo: &mut Lowering, fx: &mut Fx, start: Value, size: Value| {
        let at = fx.b.use_var(len);
        let room = fx.ptr(4_096);
        let left = fx.b.ins().isub(room, at);
        let size = fx.b.ins().umin(size, left);
        let target = fx.b.ins().iadd(text, at);
        lo.call_routine(fx, Routine::Copy, &[target, start, size]);
        let grown = fx.b.ins().iadd(at, size);
        fx.b.def_var(len, grown);
    };
    if names.is_empty() {
        let view = lo.view_literal(fx, path.as_bytes()).address();
        let (start, size) = (fx.load(PTR, view, 0), fx.load(PTR, view, 8));
        append(lo, fx, start, size);
    } else {
        let index = lo.variant_index(fx, ty, address);
        for (position, name) in names.iter().enumerate() {
            let this = fx.b.ins().icmp_imm_s(IntCC::Equal, index, position as i64);
            let written = format!("{path}.{name}");
            fx.when(this, |fx| {
                let view = lo.view_literal(fx, written.as_bytes()).address();
                let (start, size) = (fx.load(PTR, view, 0), fx.load(PTR, view, 8));
                append(lo, fx, start, size);
            });
        }
    }
    let nul = lo.view_literal(fx, b"\0").address();
    let (start, size) = (fx.load(PTR, nul, 0), fx.load(PTR, nul, 8));
    append(lo, fx, start, size);
    let render = lo.core_interface("Render");
    if lo.ck.implements(ty, render) {
        let value = lo.held_at(fx, ty, address);
        let (start, size) = super::supplied::render::render_value(lo, fx, ty, value, 4_096);
        append(lo, fx, start, size);
    }
    let size = fx.b.use_var(len);
    let copy = lo.record_alloc(fx, size);
    lo.call_routine(fx, Routine::Copy, &[copy, text, size]);
    let at = lo.test_state(fx);
    fx.store(copy, at, test_state::ESCAPE);
    fx.store(size, at, test_state::ESCAPE_LEN);
    let yes = fx.ptr(1);
    fx.store(yes, at, test_state::ESCAPED);
}
