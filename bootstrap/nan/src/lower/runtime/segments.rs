//! Stack segments: the stacks tasks draw, those the grown calls of the bundles a kernel loads run
//! on, and those nested calls run on. A segment starts with a header naming the
//! stack the task ran on before it, its own length and whether it is mapped rather than drawn
//! through a cell, then the routine reserve, then the room grown and nested calls use.

use cranelift_codegen::ir::{InstBuilder, TrapCode, Value, condcodes::IntCC, types};

use super::super::fx::{Fx, PTR};
use super::super::stack::STUB_CALL_BOUND;
use super::super::{Lowering, nested, supplied};
use super::tasks::{self, task};
use super::{Routine, memory, report, system};
use crate::workspace::Arch;

/// The bytes of a segment's header: the segment the task ran on before it, zero for its own
/// stack, the segment's length and whether it is mapped.
const HEADER: u64 = 24;

/// The offsets in the record `Grow` keeps in its frame while a segment runs: `Grow`'s context,
/// the thunk and its record, and the segment's context once it switches back.
const SWITCH_THUNK: u64 = 24;
const SWITCH_RECORD: u64 = 32;
const SWITCH_BACK: u64 = 40;
const SWITCH: u64 = 64;

/// A stack of `length` bytes: mapped for the zero cell, drawn through a cell for a domain
/// otherwise, its guard region made inaccessible when `guarded`; zero when it cannot be had.
pub fn draw(
    lo: &mut Lowering,
    fx: &mut Fx,
    cell: Value,
    domain: Value,
    length: Value,
    guarded: bool,
) -> Value {
    let mapped = fx.b.ins().icmp_imm_s(IntCC::Equal, cell, 0);
    fx.choose_in(
        lo,
        mapped,
        &[PTR],
        |lo, fx| vec![system::map_stack(lo, fx, length)],
        |lo, fx| {
            let align = fx.ptr(memory::CHUNK);
            let drawn = lo.call_routine(fx, Routine::AllocIn, &[cell, length, align, domain])[0];
            if guarded {
                let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, drawn, 0);
                fx.when(present, |fx| system::protect_guard(lo, fx, drawn, false));
            }
            vec![drawn]
        },
    )[0]
}

/// Returns a stack `draw` gave through a cell, its guard region made accessible again when
/// `guarded`.
pub fn give_back(
    lo: &mut Lowering,
    fx: &mut Fx,
    cell: Value,
    stack: Value,
    length: Value,
    guarded: bool,
) {
    let mapped = fx.b.ins().icmp_imm_s(IntCC::Equal, cell, 0);
    fx.choose_in(
        lo,
        mapped,
        &[],
        |lo, fx| {
            system::unmap_stack(lo, fx, stack, length);
            Vec::new()
        },
        |lo, fx| {
            if guarded {
                system::protect_guard(lo, fx, stack, true);
            }
            lo.call_routine(fx, Routine::Free, &[stack]);
            Vec::new()
        },
    );
}

/// The bounds of the stack a task runs on: the segment given unless it is zero, the bounds of
/// the task's own stack otherwise.
pub fn bounds(fx: &mut Fx, segment: Value, low: Value, high: Value) -> (Value, Value) {
    let grown = fx.b.ins().icmp_imm_s(IntCC::NotEqual, segment, 0);
    let bounds = fx.choose(
        grown,
        &[PTR, PTR],
        |fx| {
            let length = fx.load(PTR, segment, 8);
            vec![segment, fx.b.ins().iadd(segment, length)]
        },
        |_| vec![low, high],
    );
    (bounds[0], bounds[1])
}

/// Returns the segments an ended task's record still names, which a trap left it on.
pub fn release(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let newest = fx.load(PTR, record, task::SEGMENT);
    let grown = fx.b.ins().icmp_imm_s(IntCC::NotEqual, newest, 0);
    fx.when(grown, |fx| {
        fx.repeat(&[newest], |fx, state| {
            let segment = state[0];
            let older = fx.load(PTR, segment, 0);
            let length = fx.load(PTR, segment, 8);
            let mapped = fx.load(PTR, segment, 16);
            return_segment(lo, fx, segment, length, mapped);
            let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, older, 0);
            (more, vec![older])
        });
    });
}

/// `Grow(thunk, record, need)`: calls the thunk with the record on the stack it runs on, unless
/// that is the running task's and holds less than `need` bytes and the stub call bound above
/// its limit below `Grow`'s frame. Then the thunk runs on a new segment drawn through the
/// task's stack cell, of the default task stack or the room it needs if more, which is returned
/// once the thunk returns; when the segment cannot be drawn, the task traps
/// (`trap.exhausted`).
pub fn grow(lo: &mut Lowering, fx: &mut Fx) {
    let params = fx.entry();
    let (thunk, record, need) = (params[0], params[1], params[2]);
    let here = fx.slot(8, 8);
    let task = tasks::current_task(lo, fx);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, task, 0);
    let short = fx.choose(
        present,
        &[types::I8],
        |fx| {
            let limit = fx.load(PTR, task, task::LIMIT);
            let top = fx.load(PTR, task, task::TOP);
            let above =
                fx.b.ins()
                    .icmp(IntCC::UnsignedGreaterThanOrEqual, here, limit);
            let below = fx.b.ins().icmp(IntCC::UnsignedLessThan, here, top);
            let room = fx.b.ins().isub(here, limit);
            let wanted = fx.b.ins().iadd_imm_s(need, STUB_CALL_BOUND as i64);
            let lacking = fx.b.ins().icmp(IntCC::UnsignedLessThan, room, wanted);
            let inside = fx.b.ins().band(above, below);
            vec![fx.b.ins().band(inside, lacking)]
        },
        |fx| vec![fx.int(types::I8, 0)],
    )[0];
    fx.choose_in(
        lo,
        short,
        &[],
        |lo, fx| {
            on_segment(lo, fx, task, thunk, record, need);
            Vec::new()
        },
        |_, fx| {
            let signature = fx.b.import_signature(supplied::tasks::record_signature());
            fx.b.ins().call_indirect(signature, thunk, &[record]);
            Vec::new()
        },
    );
    fx.b.ins().return_(&[]);
}

/// Runs a thunk with its record on a new segment of a task's stack, then returns the segment;
/// when the segment cannot be drawn, the task traps (`trap.exhausted`).
fn on_segment(
    lo: &mut Lowering,
    fx: &mut Fx,
    task: Value,
    thunk: Value,
    record: Value,
    need: Value,
) {
    let cell = fx.load(PTR, task, task::STACK_CELL);
    let domain = fx.load(PTR, task, task::DOMAIN);
    let length = segment_length(lo, fx, need);
    let segment = draw(lo, fx, cell, domain, length, false);
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, segment, 0);
    fx.when(missing, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
    let mapped = fx.b.ins().icmp_imm_s(IntCC::Equal, cell, 0);
    let mapped = fx.b.ins().uextend(PTR, mapped);
    run_on_segment(lo, fx, task, (segment, length, mapped), thunk, record);
}

/// Returns a segment: unmapped when `mapped`, freed otherwise.
fn return_segment(lo: &mut Lowering, fx: &mut Fx, segment: Value, length: Value, mapped: Value) {
    let mapped = fx.b.ins().icmp_imm_s(IntCC::NotEqual, mapped, 0);
    fx.choose_in(
        lo,
        mapped,
        &[],
        |lo, fx| {
            system::unmap_stack(lo, fx, segment, length);
            Vec::new()
        },
        |lo, fx| {
            lo.call_routine(fx, Routine::Free, &[segment]);
            Vec::new()
        },
    );
}

/// The length of a segment for a call needing `need` bytes: its header, the routine reserve and
/// the default task stack, or the room the call needs if more.
fn segment_length(lo: &mut Lowering, fx: &mut Fx, need: Value) -> Value {
    let reserve = tasks::reserve(lo);
    let wanted = fx.b.ins().iadd_imm_s(need, STUB_CALL_BOUND as i64);
    let default = fx.ptr(tasks::STACK);
    let usable = fx.b.ins().umax(wanted, default);
    let length =
        fx.b.ins()
            .iadd_imm_s(usable, (HEADER + reserve + 15) as i64);
    fx.b.ins().band_imm_s(length, -16)
}

/// Runs a thunk with its record on a segment of a task's stack, given with its length and
/// whether it is mapped, the newest the task's record names while it runs, then returns the
/// segment.
fn run_on_segment(
    lo: &mut Lowering,
    fx: &mut Fx,
    task: Value,
    (segment, length, mapped): (Value, Value, Value),
    thunk: Value,
    record: Value,
) {
    let reserve = tasks::reserve(lo);
    let limit = fx.load(PTR, task, task::LIMIT);
    let top = fx.load(PTR, task, task::TOP);
    let older = fx.load(PTR, task, task::SEGMENT);
    fx.store(older, segment, 0);
    fx.store(length, segment, 8);
    fx.store(mapped, segment, 16);
    let grown_limit = fx.b.ins().iadd_imm_s(segment, (HEADER + reserve) as i64);
    let grown_top = fx.b.ins().iadd(segment, length);
    fx.store(grown_limit, task, task::LIMIT);
    fx.store(grown_top, task, task::TOP);
    fx.store(segment, task, task::SEGMENT);
    switch_onto(lo, fx, grown_top, thunk, record);
    fx.store(limit, task, task::LIMIT);
    fx.store(top, task, task::TOP);
    fx.store(older, task, task::SEGMENT);
    return_segment(lo, fx, segment, length, mapped);
}

/// Runs a thunk with its record on the stack whose top is `top`, through `GrownStart`, and
/// comes back once it returns.
fn switch_onto(lo: &mut Lowering, fx: &mut Fx, top: Value, thunk: Value, record: Value) {
    let switch = fx.slot(SWITCH, 16);
    fx.store(thunk, switch, SWITCH_THUNK);
    fx.store(record, switch, SWITCH_RECORD);
    // The stack's first context: on x86_64 eight below its top over a zeroed return slot, so
    // `GrownStart` is entered as a call leaves the stack, and the frame pointer of the routine
    // switching, which `GrownStart`'s frame record links to.
    let start = match lo.ck.db.target.arch() {
        Some(Arch::Aarch64) => top,
        _ => {
            let start = fx.b.ins().iadd_imm_s(top, -8);
            let zero = fx.ptr(0);
            fx.store(zero, start, 0);
            start
        }
    };
    let fresh = fx.slot(24, 8);
    fx.store(start, fresh, 0);
    let frame = fx.b.ins().get_frame_pointer(PTR);
    fx.store(frame, fresh, 8);
    let grown_start = lo.routine(Routine::GrownStart);
    let grown_start = lo.func_ref(fx, grown_start);
    let grown_start = fx.b.ins().func_addr(PTR, grown_start);
    fx.store(grown_start, fresh, 16);
    lo.switch_stacks(fx, switch, fresh, switch);
}

/// `Nested(function, first, alloc, third, need)`: calls the function with its arguments through
/// the thunk of nested calls, on the stack it runs on when that is the running task's and holds
/// `need` bytes and the stub call bound above its limit below `Nested`'s frame. Otherwise the
/// call runs on a new segment drawn through `alloc`, of the task's stack when a task runs,
/// returned once it returns; a segment that cannot be drawn gives `OutOfMemory`'s code.
pub fn nested(lo: &mut Lowering, fx: &mut Fx) {
    let params = fx.entry();
    let need = params[4];
    let held = fx.slot(nested::record::SIZE, 8);
    let parts = [nested::record::FUNCTION]
        .into_iter()
        .chain(nested::record::ARGS);
    for (offset, value) in parts.zip(params.iter().copied()) {
        fx.store(value, held, offset);
    }
    let out_of_memory = fx.ptr(supplied::codecs::failure::OUT_OF_MEMORY as u64);
    fx.store(out_of_memory, held, nested::record::CODE);
    let thunk = lo.nested_thunk();
    let thunk = lo.func_ref(fx, thunk);
    let thunk = fx.b.ins().func_addr(PTR, thunk);
    let here = fx.slot(8, 8);
    let task = tasks::current_task(lo, fx);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, task, 0);
    fx.choose_in(
        lo,
        present,
        &[],
        |lo, fx| {
            let short = short_of(fx, task, here, need);
            fx.choose_in(
                lo,
                short,
                &[],
                |lo, fx| {
                    let length = segment_length(lo, fx, need);
                    let domain = fx.load(PTR, task, task::DOMAIN);
                    let align = fx.ptr(memory::CHUNK);
                    let drawn =
                        lo.call_routine(fx, Routine::AllocIn, &[params[2], length, align, domain]);
                    let segment = drawn[0];
                    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, segment, 0);
                    fx.when(present, |fx| {
                        let drawn = fx.ptr(0);
                        run_on_segment(lo, fx, task, (segment, length, drawn), thunk, held);
                    });
                    Vec::new()
                },
                |_, fx| {
                    let signature = fx.b.import_signature(supplied::tasks::record_signature());
                    fx.b.ins().call_indirect(signature, thunk, &[held]);
                    Vec::new()
                },
            );
            Vec::new()
        },
        |lo, fx| {
            let length = segment_length(lo, fx, need);
            let align = fx.ptr(memory::CHUNK);
            let segment = lo.call_routine(fx, Routine::Alloc, &[params[2], length, align])[0];
            let drawn = fx.b.ins().icmp_imm_s(IntCC::NotEqual, segment, 0);
            fx.when(drawn, |fx| {
                let top = fx.b.ins().iadd(segment, length);
                switch_onto(lo, fx, top, thunk, held);
                lo.call_routine(fx, Routine::Free, &[segment]);
            });
            Vec::new()
        },
    );
    let code = fx.load(PTR, held, nested::record::CODE);
    fx.b.ins().return_(&[code]);
}

/// Whether a running task's stack lacks `need` bytes and the stub call bound above its limit
/// below `here`, or `here` lies outside it.
fn short_of(fx: &mut Fx, task: Value, here: Value, need: Value) -> Value {
    let limit = fx.load(PTR, task, task::LIMIT);
    let top = fx.load(PTR, task, task::TOP);
    let above =
        fx.b.ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, here, limit);
    let below = fx.b.ins().icmp(IntCC::UnsignedLessThan, here, top);
    let room = fx.b.ins().isub(here, limit);
    let wanted = fx.b.ins().iadd_imm_s(need, STUB_CALL_BOUND as i64);
    let lacking = fx.b.ins().icmp(IntCC::UnsignedLessThan, room, wanted);
    let inside = fx.b.ins().band(above, below);
    let roomy = fx.b.ins().bxor_imm_s(lacking, 1);
    let fits = fx.b.ins().band(inside, roomy);
    fx.b.ins().bxor_imm_s(fits, 1)
}

/// `GrownStart(record)`: links its frame record to `Grow`'s, so walks of the stack pass from
/// the segment to the stack it grew from, calls the thunk with its record, and switches back to
/// `Grow`.
pub fn grown_start(lo: &mut Lowering, fx: &mut Fx) {
    let switch = fx.entry()[0];
    let frame = fx.b.ins().get_frame_pointer(PTR);
    let grow = lo.routine(Routine::Grow);
    let grow = lo.func_ref(fx, grow);
    let grow = fx.b.ins().func_addr(PTR, grow);
    fx.store(grow, frame, 8);
    let thunk = fx.load(PTR, switch, SWITCH_THUNK);
    let record = fx.load(PTR, switch, SWITCH_RECORD);
    let signature = fx.b.import_signature(supplied::tasks::record_signature());
    fx.b.ins().call_indirect(signature, thunk, &[record]);
    let back = fx.offset(switch, SWITCH_BACK);
    let zero = fx.ptr(0);
    lo.switch_stacks(fx, back, switch, zero);
    fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
}
