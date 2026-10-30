//! Simulated capabilities: in a test image every capability is a sim, as only `testing::world()`
//! and the `sys::sim` constructors mint them there, so each target-dependent operation of a sys
//! capability or resource acts on the sim its entry or holding names. The run's world, minted
//! before its test starts, holds a root cell and an entry per capability.
//!
//! A sim is an object of the sim table, named by its index, which an entry's first state word
//! or a resource's holding holds: a file system, an open file, a network, a listener, a
//! connection's endpoint, a random stream or an input. An awaited operation that the sim's state
//! does not decide waits in it: its operation record lies in the sim's waiter slot until the
//! sim completes it, or until its cancellation withdraws it.

pub mod fs;
pub mod net;

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};
use cranelift_module::{DataDescription, Linkage, Module};

use super::super::body::Held;
use super::super::fx::{Fx, PTR};
use super::super::runtime::regions::origin;
use super::super::runtime::tables::{self, entry, kind};
use super::super::runtime::tasks::{self, CANCELED, kind as op_kind, operation, task};
use super::super::runtime::{Routine, memory, report, seeded, state};
use super::super::test_image::{self, frame};
use super::super::{Glue, Lowering};
use super::Supply;
use crate::check::ty::Ty;

/// The kinds of sim objects, each object's first word.
pub mod object {
    pub const FS: i64 = 1;
    pub const FILE: i64 = 2;
    pub const NET: i64 = 3;
    pub const LISTENER: i64 = 4;
    pub const ENDPOINT: i64 = 5;
    pub const RANDOM: i64 = 6;
    pub const INPUT: i64 = 7;
    pub const KIND: u64 = 0;
}

/// The offsets of the sim table's fields: its records' addresses, their count and capacity.
mod table {
    pub const RECORDS: u64 = 0;
    pub const COUNT: u64 = 8;
    pub const CAPACITY: u64 = 16;
    pub const SIZE: u64 = 24;
}

/// The offsets of a random stream's fields: its seed and the index of its next byte.
mod random {
    pub const SEED: u64 = 8;
    pub const NEXT: u64 = 16;
    pub const SIZE: u64 = 24;
}

/// The offsets of an input's fields: its bytes, their count and the count read.
mod input {
    pub const DATA: u64 = 8;
    pub const LEN: u64 = 16;
    pub const READ: u64 = 24;
    pub const SIZE: u64 = 32;
}

/// The stream of random bytes.
const RANDOM_STREAM: u64 = 2;

/// The render budget of a log record's value.
const LOG_BUDGET: u64 = 1_024;

/// The bytes of a log record's line buffer.
const LINE: u64 = 4_096;

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    match supply.path.as_str() {
        "testing::world" => {
            let world_ty = lo.std_ty("host", "World", Vec::new());
            let layout = lo.layout(&world_ty);
            let world = test_image::world(lo, fx);
            let sret = supply.sret.expect("a world's address");
            lo.copy(fx, sret, world, layout.size, layout.align);
            supply.ret(lo, fx, Held::Address(sret));
        }
        "sys::sim::fs" => {
            let limit = supply.param(0).scalar();
            let cell = fx.load(PTR, supply.param(1).address(), 0);
            let sim = fs::new(lo, fx, limit, cell);
            capability(lo, fx, supply, kind::FS, sim);
        }
        "sys::sim::net" => {
            let sim = net::new(lo, fx);
            capability(lo, fx, supply, kind::NET, sim);
        }
        "sys::sim::random" => {
            let sim = random_stream(lo, fx, supply.param(0).scalar());
            capability(lo, fx, supply, kind::RANDOM, sim);
        }
        "sys::sim::stdio" => {
            let (start, len) = super::sys::view(fx, supply.param(0));
            let cell = fx.load(PTR, supply.param(1).address(), 0);
            let sim = new_input(lo, fx, start, len, cell);
            capability(lo, fx, supply, kind::STDIO, sim);
        }
        _ => return false,
    }
    true
}

/// Returns a new capability of a kind whose entry names a sim.
fn capability(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, entry_kind: i64, sim: Value) {
    let kind_value = fx.ptr(entry_kind as u64);
    let handle = tables::mint(lo, fx, kind_value);
    let record = tables::entry_at(lo, fx, handle);
    fx.store(sim, record, entry::STATE);
    let sret = supply.sret.expect("a capability's address");
    fx.store(handle, sret, 0);
    supply.done(lo, fx);
}

/// Mints the run's world in runtime memory: its address. Its file system has no size limit
/// and stores through its allocator, whose root cell has none either; its random stream is the
/// run's, and its wall clock reads the virtual clock.
pub fn mint_world(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let world_ty = lo.std_ty("host", "World", Vec::new());
    let layout = lo.layout(&world_ty);
    let size = fx.ptr(layout.size);
    let world = lo.record_alloc(fx, size);
    let limit = fx.int(PTR, -1);
    let parent = fx.int(PTR, -1);
    let cell = memory::new_cell(lo, fx, limit, parent);
    let unlimited = fx.int(PTR, -1);
    let state_address = lo.state(fx);
    let seed = fx.load(PTR, state_address, state::SEED);
    let fs = fs::new(lo, fx, unlimited, cell);
    let net = net::new(lo, fx);
    let random = random_stream(lo, fx, seed);
    let none = fx.ptr(0);
    let entries = [
        ("fs", kind::FS, fs),
        ("net", kind::NET, net),
        ("clock", kind::CLOCK, none),
        ("random", kind::RANDOM, random),
        ("log", kind::LOG, none),
        ("stdio", kind::STDIO, none),
        ("exec", kind::EXECUTOR, cell),
        ("wall", kind::WALL, none),
    ];
    let mut handles = Vec::new();
    for (field, entry_kind, sim) in entries {
        let kind_value = fx.ptr(entry_kind as u64);
        let handle = tables::mint(lo, fx, kind_value);
        let record = tables::entry_at(lo, fx, handle);
        fx.store(sim, record, entry::STATE);
        let at = lo.layouts.field(lo.ck, &world_ty, None, field).offset;
        fx.store(handle, world, at);
        handles.push((handle, record));
    }
    fx.store(handles[2].0, handles[6].1, entry::STATE + 8);
    tables::grant_memory(lo, fx, world, cell);
    world
}

/// The address of the sim table.
fn sims_table(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let id = match lo.sims {
        Some(id) => id,
        None => {
            let id = lo
                .module
                .declare_data("_NSreplay_sims", Linkage::Local, true, false)
                .expect("the sim table");
            let mut description = DataDescription::new();
            description.define_zeroinit(table::SIZE as usize);
            description.set_align(8);
            lo.module
                .define_data(id, &description)
                .expect("the sim table");
            lo.sims = Some(id);
            id
        }
    };
    let global = lo.data_ref(fx, id);
    fx.b.ins().symbol_value(PTR, global)
}

/// A new zeroed sim object of a kind and size in runtime memory: its index and record.
pub fn new_object(lo: &mut Lowering, fx: &mut Fx, object_kind: i64, size: u64) -> (Value, Value) {
    let bytes = fx.ptr(size);
    let record = lo.record_alloc(fx, bytes);
    exhausted_unless(lo, fx, record);
    let kind_value = fx.ptr(object_kind as u64);
    fx.store(kind_value, record, object::KIND);
    let sims = sims_table(lo, fx);
    let count = fx.load(PTR, sims, table::COUNT);
    let capacity = fx.load(PTR, sims, table::CAPACITY);
    let full =
        fx.b.ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, count, capacity);
    fx.when(full, |fx| {
        let doubled = fx.b.ins().ishl_imm_s(capacity, 1);
        let least = fx.ptr(16);
        let grown = fx.b.ins().umax(doubled, least);
        let bytes = fx.b.ins().imul_imm_s(grown, 8);
        let records = fx.load(PTR, sims, table::RECORDS);
        let moved = lo.record_resize(fx, records, bytes);
        exhausted_unless(lo, fx, moved);
        fx.store(moved, sims, table::RECORDS);
        fx.store(grown, sims, table::CAPACITY);
    });
    let records = fx.load(PTR, sims, table::RECORDS);
    let slot = fx.b.ins().imul_imm_s(count, 8);
    let slot = fx.b.ins().iadd(records, slot);
    fx.store(record, slot, 0);
    let next = fx.b.ins().iadd_imm_s(count, 1);
    fx.store(next, sims, table::COUNT);
    (next, record)
}

/// The record of the sim object an index names.
pub fn object_at(lo: &mut Lowering, fx: &mut Fx, index: Value) -> Value {
    let sims = sims_table(lo, fx);
    let records = fx.load(PTR, sims, table::RECORDS);
    let slot = fx.b.ins().iadd_imm_s(index, -1);
    let slot = fx.b.ins().imul_imm_s(slot, 8);
    let slot = fx.b.ins().iadd(records, slot);
    fx.load(PTR, slot, 0)
}

/// Traps with `trap.exhausted` when runtime memory could not be drawn.
pub fn exhausted_unless(lo: &mut Lowering, fx: &mut Fx, address: Value) {
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, address, 0);
    fx.when(missing, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
}

/// Draws `size` bytes through a cell for storage a sim keeps whatever domain draws it,
/// trapping on exhaustion; zero for no bytes.
pub fn draw(lo: &mut Lowering, fx: &mut Fx, cell: Value, size: Value) -> Value {
    let empty = fx.b.ins().icmp_imm_s(IntCC::Equal, size, 0);
    fx.choose_in(
        lo,
        empty,
        &[PTR],
        |_, fx| vec![fx.ptr(0)],
        |lo, fx| {
            let state_address = lo.state(fx);
            let process = fx.load(PTR, state_address, state::PROCESS);
            let one = fx.ptr(1);
            let drawn = lo.call_routine(fx, Routine::AllocIn, &[cell, size, one, process])[0];
            exhausted_unless(lo, fx, drawn);
            vec![drawn]
        },
    )[0]
}

/// Waits at a cancellation point for the operation that `enqueue` records in a sim, or among the
/// timers of the virtual clock or a kernel's clock without a waiter slot, the running task
/// parking until it completes; gives whether the wait ended by the cancellation of the call. A
/// call cancelled at its start waits for nothing.
pub fn wait(
    lo: &mut Lowering,
    fx: &mut Fx,
    slot: Option<Value>,
    enqueue: impl FnOnce(&mut Lowering, &mut Fx, Value),
) -> Value {
    let record = tasks::current_task(lo, fx);
    let region = fx.load(PTR, record, task::REGION);
    let early = origin(fx, region);
    let early = fx.b.ins().icmp_imm_s(IntCC::NotEqual, early, 0);
    fx.choose_in(
        lo,
        early,
        &[types::I8],
        |_, fx| vec![fx.int(types::I8, 1)],
        |lo, fx| {
            let op = fx.slot(operation::SIZE, 8);
            let zero = fx.ptr(0);
            let size = fx.ptr(operation::SIZE);
            lo.call_routine(fx, Routine::Fill, &[op, zero, size]);
            fx.store(record, op, operation::TASK);
            let wait = fx.ptr(op_kind::WAIT as u64);
            fx.store(wait, op, operation::KIND);
            if let Some(slot) = slot {
                fx.store(slot, op, operation::WAITER);
                let withdraw = withdraw_glue(lo);
                let withdraw = lo.func_ref(fx, withdraw);
                let withdraw = fx.b.ins().func_addr(PTR, withdraw);
                fx.store(withdraw, op, operation::WITHDRAW);
            }
            fx.store(op, record, task::IO);
            enqueue(lo, fx, op);
            let cancelled = origin(fx, region);
            let cancelled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, cancelled, 0);
            fx.when(cancelled, |fx| lo.cancel_operation(fx, op));
            tasks::wait_delivered(lo, fx, op);
            let current = tasks::current_task(lo, fx);
            let zero = fx.ptr(0);
            fx.store(zero, current, task::IO);
            let result = fx.load(types::I32, op, operation::RESULT);
            vec![fx.b.ins().icmp_imm_s(IntCC::Equal, result, CANCELED)]
        },
    )[0]
}

/// The glue withdrawing a waiting operation from the waiter slot its record names.
fn withdraw_glue(lo: &mut Lowering) -> cranelift_module::FuncId {
    lo.glue_function(
        Glue::SimWithdraw,
        super::tasks::record_signature(),
        |_, fx| {
            let params = fx.entry();
            let op = params[0];
            let slot = fx.load(PTR, op, operation::WAITER);
            let waiting = fx.load(PTR, slot, 0);
            let same = fx.b.ins().icmp(IntCC::Equal, waiting, op);
            fx.when(same, |fx| {
                let zero = fx.ptr(0);
                fx.store(zero, slot, 0);
            });
            fx.b.ins().return_(&[]);
        },
    )
}

/// Records a waiting operation in a waiter slot.
pub fn enqueue(fx: &mut Fx, slot: Value, op: Value) {
    fx.store(op, slot, 0);
}

/// Completes the operation waiting in a waiter slot, if any: its task becomes ready.
pub fn wake(lo: &mut Lowering, fx: &mut Fx, slot: Value) {
    let op = fx.load(PTR, slot, 0);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, op, 0);
    fx.when(present, |fx| {
        let zero = fx.ptr(0);
        fx.store(zero, slot, 0);
        let ready = fx.int(types::I32, 0);
        seeded::complete(lo, fx, op, ready);
    });
}

/// Fails an operation with the cancellation of its failure type.
pub fn fail_cancelled(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let origin = super::tasks::origin(lo, fx);
    super::tasks::fail_cancelled(lo, fx, supply, origin);
}

/// Fails an operation with the cancellation of its failure type when its call is cancelled at
/// its start: the cancellation point of an operation its sim decides at once.
pub fn cancellation_point(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let early = super::tasks::origin(lo, fx);
    let cancelled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, early, 0);
    fx.when(cancelled, |fx| fail_cancelled(lo, fx, supply));
}

/// Writes byte ranges to the test trace as one entry.
pub fn trace(lo: &mut Lowering, fx: &mut Fx, parts: &[(Value, Value)]) {
    test_image::write_frame(lo, fx, frame::TRACE, parts);
}

/// A new random stream of a seed: its index.
fn random_stream(lo: &mut Lowering, fx: &mut Fx, seed: Value) -> Value {
    let (index, record) = new_object(lo, fx, object::RANDOM, random::SIZE);
    fx.store(seed, record, random::SEED);
    index
}

/// `sys::Random.fill` of the stream at `index`: its next bytes, least significant first of each
/// value.
pub fn random_fill(lo: &mut Lowering, fx: &mut Fx, index: Value, buf: Held) {
    let record = object_at(lo, fx, index);
    let seed = fx.load(PTR, record, random::SEED);
    let first = fx.load(PTR, record, random::NEXT);
    let (start, len) = super::sys::view(fx, buf);
    let zero = fx.ptr(0);
    fx.each(zero, len, |fx, offset| {
        let at = fx.b.ins().iadd(first, offset);
        let word = fx.b.ins().ushr_imm_s(at, 3);
        let stream = fx.ptr(RANDOM_STREAM);
        let value = lo.call_routine(fx, Routine::Stream, &[seed, stream, word])[0];
        let shift = fx.b.ins().band_imm_s(at, 7);
        let shift = fx.b.ins().imul_imm_s(shift, 8);
        let shifted = fx.b.ins().ushr(value, shift);
        let byte = fx.b.ins().ireduce(types::I8, shifted);
        let target = fx.b.ins().iadd(start, offset);
        fx.store(byte, target, 0);
    });
    let next = fx.b.ins().iadd(first, len);
    fx.store(next, record, random::NEXT);
}

/// Appends a text to a line buffer of `LINE` bytes from `at`, each line feed as `\n`: the
/// line's new length, which stays within the buffer.
fn append_escaped(fx: &mut Fx, line: Value, at: Value, text: Value, len: Value) -> Value {
    let zero = fx.ptr(0);
    let done = fx.repeat(&[zero, at], |fx, state| {
        let (index, at) = (state[0], state[1]);
        let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, index, len);
        let room =
            fx.b.ins()
                .icmp_imm_s(IntCC::UnsignedLessThanOrEqual, at, LINE as i64 - 2);
        let going = fx.b.ins().band(inside, room);
        let next = fx.choose(
            going,
            &[PTR, PTR],
            |fx| {
                let place = fx.b.ins().iadd(text, index);
                let byte = fx.load(types::I8, place, 0);
                let feed = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'\n'));
                let target = fx.b.ins().iadd(line, at);
                let written = fx.choose(
                    feed,
                    &[PTR],
                    |fx| {
                        let slash = fx.int(types::I8, i64::from(b'\\'));
                        let letter = fx.int(types::I8, i64::from(b'n'));
                        fx.store(slash, target, 0);
                        fx.store(letter, target, 1);
                        vec![fx.ptr(2)]
                    },
                    |fx| {
                        fx.store(byte, target, 0);
                        vec![fx.ptr(1)]
                    },
                )[0];
                vec![
                    fx.b.ins().iadd_imm_s(index, 1),
                    fx.b.ins().iadd(at, written),
                ]
            },
            |_| vec![index, at],
        );
        (going, next)
    });
    done[1]
}

/// Writes a log record to the test trace: the level, `: `, the message, a space and the
/// rendering of its value, each line feed written as `\n`, then a line feed.
pub fn log_line(
    lo: &mut Lowering,
    fx: &mut Fx,
    level: &str,
    message: Held,
    value_ty: &Ty,
    value: Held,
) {
    let (buffer, rendered) = super::render::render_value(lo, fx, value_ty, value, LOG_BUDGET);
    let line = fx.slot(LINE + 1, 8);
    let mut at = fx.ptr(0);
    let (message_start, message_len) = super::sys::view(fx, message);
    let separator = format!("{level}: ");
    let parts = [
        literal(lo, fx, separator.as_bytes()),
        (message_start, message_len),
        literal(lo, fx, b" "),
        (buffer, rendered),
    ];
    for (start, len) in parts {
        at = append_escaped(fx, line, at, start, len);
    }
    let end = fx.b.ins().iadd(line, at);
    let feed = fx.int(types::I8, i64::from(b'\n'));
    fx.store(feed, end, 0);
    let len = fx.b.ins().iadd_imm_s(at, 1);
    trace(lo, fx, &[(line, len)]);
}

/// The start and length of a literal's bytes.
fn literal(lo: &mut Lowering, fx: &mut Fx, bytes: &[u8]) -> (Value, Value) {
    let view = lo.view_literal(fx, bytes).address();
    (fx.load(PTR, view, 0), fx.load(PTR, view, 8))
}

/// A new input holding a copy of bytes drawn through a cell: its index.
fn new_input(lo: &mut Lowering, fx: &mut Fx, start: Value, len: Value, cell: Value) -> Value {
    let (index, record) = new_object(lo, fx, object::INPUT, input::SIZE);
    let copy = draw(lo, fx, cell, len);
    lo.call_routine(fx, Routine::Copy, &[copy, start, len]);
    fx.store(copy, record, input::DATA);
    fx.store(len, record, input::LEN);
    index
}

/// `sys::Stdio.read_in` of the input at `index`: its next bytes, none once it has ended or for
/// the world's empty input at index zero.
pub fn stdio_read(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, index: Value) {
    cancellation_point(lo, fx, supply);
    let has_input = fx.b.ins().icmp_imm_s(IntCC::NotEqual, index, 0);
    let count = fx.choose_in(
        lo,
        has_input,
        &[PTR],
        |lo, fx| {
            let record = object_at(lo, fx, index);
            let (start, room) = super::sys::view(fx, supply.param(1));
            let data = fx.load(PTR, record, input::DATA);
            let len = fx.load(PTR, record, input::LEN);
            let read = fx.load(PTR, record, input::READ);
            let left = fx.b.ins().isub(len, read);
            let count = fx.b.ins().umin(left, room);
            let from = fx.b.ins().iadd(data, read);
            lo.call_routine(fx, Routine::Copy, &[start, from, count]);
            let read = fx.b.ins().iadd(read, count);
            fx.store(read, record, input::READ);
            vec![count]
        },
        |_, fx| vec![fx.ptr(0)],
    )[0];
    supply.ret(lo, fx, Held::Scalar(count));
}

/// `sys::Stdio.write_out` and `write_err`: the bytes enter the test trace.
pub fn stdio_write(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    cancellation_point(lo, fx, supply);
    let (start, len) = super::sys::view(fx, supply.param(1));
    trace(lo, fx, &[(start, len)]);
    supply.done(lo, fx);
}

/// Releases the open file or listener a holding's payload names, as its end of scope does.
pub fn release(lo: &mut Lowering, fx: &mut Fx, index: Value) {
    let record = object_at(lo, fx, index);
    let object_kind = fx.load(PTR, record, object::KIND);
    let listener =
        fx.b.ins()
            .icmp_imm_s(IntCC::Equal, object_kind, object::LISTENER);
    fx.when(listener, |fx| net::close_listener(lo, fx, record));
}

/// Abandons the connection a holding's payload names: its peer is reset.
pub fn abandon(lo: &mut Lowering, fx: &mut Fx, index: Value) {
    let record = object_at(lo, fx, index);
    net::end(lo, fx, record, net::ended::RESET);
}
