//! The system the runtime runs on: the memory of its records and of the chunks cells draw, task
//! stacks, idling, its standard streams, its start and its exit. On `x86_64-host` the host module
//! provides them over the C library and Linux; on a machine target the machine module does, over
//! the boot heap, the serial and exit ports and the CPUs.

use cranelift_codegen::ir::{InstBuilder, TrapCode, Value, condcodes::IntCC};

use super::super::Lowering;
use super::super::fx::{Fx, PTR};
use super::super::host;
use super::super::machine::Stub;
use super::super::machine::heap;
use super::regions::region;
use super::{Routine, memory, state, tables, tasks};

/// The text written when the runtime cannot start.
const UNAVAILABLE: &str = "nan: runtime unavailable\n";

/// The tables `Init` sets up, by the field of the runtime state addressing each, its size and
/// the name of the data a machine image reserves for it.
const TABLES: [(u64, u64, &str); 5] = [
    (
        state::CELLS,
        memory::CELLS * memory::CELL_RECORD,
        "_NScells",
    ),
    (state::CHUNK_MAP, memory::MAP_ROOTS * 8, "_NSchunk_map"),
    (
        state::ENTRIES,
        tables::ENTRIES * tables::ENTRY_RECORD,
        "_NSentries",
    ),
    (state::PROCESS, tasks::domain::SIZE, "_NSprocess"),
    (state::ROOT, region::SIZE, "_NSroot_region"),
];

/// Defines `Init`, `Exit` and the record routines.
pub fn define(lo: &mut Lowering, fx: &mut Fx, routine: Routine) {
    match (routine, lo.machine()) {
        (Routine::Init, _) => init(lo, fx),
        (Routine::Exit, true) => {
            let status = fx.entry()[0];
            lo.exit_machine(fx, status);
        }
        (_, true) => heap::define(lo, fx, routine),
        (_, false) => host::define(lo, fx, routine),
    }
}

/// `Init()`: the cell, chunk and capability tables, the process domain and the root region,
/// which a machine image reserves, then the executors of the platform; nonzero when the runtime
/// cannot start.
fn init(lo: &mut Lowering, fx: &mut Fx) {
    fx.entry();
    let state_address = lo.state(fx);
    for (field, size, name) in TABLES {
        let address = match lo.machine() {
            true => lo.reserved(fx, name, size, 64),
            false => {
                let size = fx.ptr(size);
                let address = lo.record_alloc(fx, size);
                unavailable_unless(fx, address);
                address
            }
        };
        fx.store(address, state_address, field);
    }
    let one = fx.ptr(1);
    let root = fx.load(PTR, state_address, state::ROOT);
    fx.store(one, root, region::ID);
    fx.store(one, state_address, state::REGIONS);
    match lo.machine() {
        true => lo.machine_init(fx),
        false => host::start_threads(lo, fx),
    }
    let zero = fx.ptr(0);
    fx.b.ins().return_(&[zero]);
}

/// Returns 1 from `Init` when an address is zero.
pub fn unavailable_unless(fx: &mut Fx, address: Value) {
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, address, 0);
    fx.when(missing, |fx| {
        let one = fx.ptr(1);
        fx.b.ins().return_(&[one]);
    });
}

/// Writes that the runtime is unavailable and ends with status 71.
pub fn unavailable(lo: &mut Lowering, fx: &mut Fx) {
    let text = lo.view_literal(fx, UNAVAILABLE.as_bytes()).address();
    let start = fx.load(PTR, text, 0);
    let len = fx.load(PTR, text, 8);
    lo.call_routine(fx, Routine::WriteErr, &[start, len]);
    let status = fx.ptr(71);
    lo.call_routine(fx, Routine::Exit, &[status]);
    fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
}

/// Writes a byte range to a standard stream: on `x86_64-host` to its descriptor, on a machine
/// target to the serial port, which carries both streams.
pub fn write_stream(lo: &mut Lowering, fx: &mut Fx, fd: i64, text: Value, len: Value) {
    match lo.machine() {
        true => lo.serial_write(fx, text, len),
        false => host::write_all(lo, fx, fd, text, len),
    }
}

/// `count` zero-filled chunks aligned to their size, zero when the supply runs out.
pub fn map_chunks(lo: &mut Lowering, fx: &mut Fx, count: Value) -> Value {
    match lo.machine() {
        true => heap::map_chunks(lo, fx, count),
        false => host::map_chunks(lo, fx, count),
    }
}

/// Returns `count` chunks from `base` to the supply.
pub fn unmap_chunks(lo: &mut Lowering, fx: &mut Fx, base: Value, count: Value) {
    match lo.machine() {
        true => heap::unmap_chunks(lo, fx, base, count),
        false => {
            let len = fx.b.ins().imul_imm_s(count, memory::CHUNK as i64);
            host::unmap(lo, fx, base, len);
        }
    }
}

/// A task stack of `len` bytes whose lowest page is an inaccessible guard on `x86_64-host`, zero
/// when the system refuses; a machine target draws it as a runtime record, its guard region
/// unprotected since its stacks have static bounds.
pub fn map_stack(lo: &mut Lowering, fx: &mut Fx, len: Value) -> Value {
    match lo.machine() {
        true => lo.record_alloc(fx, len),
        false => host::map_stack(lo, fx, len),
    }
}

/// Returns a task stack `map_stack` gave.
pub fn unmap_stack(lo: &mut Lowering, fx: &mut Fx, stack: Value, len: Value) {
    match lo.machine() {
        true => lo.record_free(fx, stack),
        false => host::unmap(lo, fx, stack, len),
    }
}

/// Makes the guard region at the base of a stack drawn through a cell inaccessible, or
/// accessible again, on `x86_64-host`.
pub fn protect_guard(lo: &mut Lowering, fx: &mut Fx, base: Value, accessible: bool) {
    if !lo.machine() {
        host::protect_guard(lo, fx, base, accessible);
    }
}

/// Waits until a task may be ready: on `x86_64-host` in the running thread's ring, handling the
/// completions that arrive; on a machine target the CPU halts until an interrupt.
pub fn idle(lo: &mut Lowering, fx: &mut Fx) {
    match lo.machine() {
        true => {
            lo.call_stub(fx, Stub::IdleWait, &[]);
        }
        false => host::idle(lo, fx),
    }
}

/// Wakes an executor thread waiting idle, other than the running one, so a task made ready runs;
/// a machine CPU waiting idle wakes on its own timer.
pub fn wake_idle(lo: &mut Lowering, fx: &mut Fx) {
    if !lo.machine() {
        host::wake_idle(lo, fx);
    }
}
