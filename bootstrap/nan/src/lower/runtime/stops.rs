//! Where a task asked to stop may stop: at a point of its stack outside every frame of a
//! function a stopping task completes before it stops.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::fx::{Fx, PTR};
use super::super::{Lowering, lines};

/// The registers of a task interrupted on its stack: its instruction, stack and frame pointers,
/// its link register where the architecture has one, the bounds of its own stack's usable part,
/// and in an image keeping segments the newest segment it runs on, zero for none.
pub struct Interrupted {
    pub ip: Value,
    pub sp: Value,
    pub fp: Value,
    pub link: Option<Value>,
    pub low: Value,
    pub high: Value,
    pub segment: Option<Value>,
}

/// Whether a code address lies in a function a stopping task does not stop inside; an address
/// outside lowered code counts as one when `unknown` holds.
fn critical_at(lo: &mut Lowering, fx: &mut Fx, address: Value, unknown: bool) -> Value {
    let (start, flags) = super::report::function_at(lo, fx, address);
    let found = fx.b.ins().icmp_imm_s(IntCC::NotEqual, start, 0);
    let critical = fx.b.ins().band_imm_s(flags, i64::from(lines::CRITICAL));
    let critical = fx.b.ins().icmp_imm_s(IntCC::NotEqual, critical, 0);
    let outside = fx.int(types::I8, i64::from(unknown));
    fx.b.ins().select(found, critical, outside)
}

/// Whether an interrupted task has no frame of a function it does not stop inside.
pub fn stoppable(lo: &mut Lowering, fx: &mut Fx, at: &Interrupted) -> Value {
    let in_ip = critical_at(lo, fx, at.ip, true);
    // The return address of a function interrupted in its prologue or epilogue, before its frame
    // pointer is set or after it is restored, lives in the link register where the architecture
    // has one, and otherwise in the two words at the stack pointer.
    let busy = match at.link {
        Some(link) => {
            let in_link = critical_at(lo, fx, link, false);
            fx.b.ins().bor(in_ip, in_link)
        }
        None => {
            let top = fx.load(PTR, at.sp, 0);
            let in_top = critical_at(lo, fx, top, false);
            let next = fx.load(PTR, at.sp, 8);
            let in_next = critical_at(lo, fx, next, false);
            let busy = fx.b.ins().bor(in_ip, in_top);
            fx.b.ins().bor(busy, in_next)
        }
    };
    let free = fx.b.ins().bxor_imm_s(busy, 1);
    fx.choose_in(
        lo,
        free,
        &[types::I8],
        |lo, fx| match at.segment {
            None => {
                let walked = fx.repeat(&[at.fp, free], |fx, state| {
                    let (frame, free) = (state[0], state[1]);
                    let inside = within(fx, frame, at.low, at.high);
                    let result = fx.choose_in(
                        lo,
                        inside,
                        &[PTR, types::I8],
                        |lo, fx| frame_free(lo, fx, frame),
                        |_, fx| vec![fx.ptr(0), free],
                    );
                    let more = fx.b.ins().band(inside, result[1]);
                    (more, result)
                });
                vec![walked[1]]
            }
            Some(segment) => {
                // A frame outside the segment the walk is in lies on an older one, or on the
                // task's own stack once none is left.
                let walked = fx.repeat(&[at.fp, free, segment], |fx, state| {
                    let (frame, free, segment) = (state[0], state[1], state[2]);
                    let (low, high) = super::segments::bounds(fx, segment, at.low, at.high);
                    let inside = within(fx, frame, low, high);
                    let grown = fx.b.ins().icmp_imm_s(IntCC::NotEqual, segment, 0);
                    let result = fx.choose_in(
                        lo,
                        inside,
                        &[PTR, types::I8, PTR, types::I8],
                        |lo, fx| {
                            let mut walked = frame_free(lo, fx, frame);
                            walked.extend([segment, walked[1]]);
                            walked
                        },
                        |_, fx| {
                            let older = fx.choose(
                                grown,
                                &[PTR],
                                |fx| vec![fx.load(PTR, segment, 0)],
                                |fx| vec![fx.ptr(0)],
                            )[0];
                            vec![frame, free, older, grown]
                        },
                    );
                    (result[3], result[..3].to_vec())
                });
                vec![walked[1]]
            }
        },
        |_, fx| vec![fx.int(types::I8, 0)],
    )[0]
}

/// Whether a frame record lies within the bounds of a stack.
fn within(fx: &mut Fx, frame: Value, low: Value, high: Value) -> Value {
    let above =
        fx.b.ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, frame, low);
    let end = fx.b.ins().iadd_imm_s(frame, 16);
    let below = fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, end, high);
    fx.b.ins().band(above, below)
}

/// The frame record a frame record links to, and whether its function is one a stopping task
/// may stop outside of, as the base frame's is.
fn frame_free(lo: &mut Lowering, fx: &mut Fx, frame: Value) -> Vec<Value> {
    let address = fx.load(PTR, frame, 8);
    let base = fx.b.ins().icmp_imm_s(IntCC::Equal, address, 0);
    let critical = critical_at(lo, fx, address, true);
    let free = fx.b.ins().bxor_imm_s(critical, 1);
    let free = fx.b.ins().bor(base, free);
    vec![fx.load(PTR, frame, 0), free]
}
