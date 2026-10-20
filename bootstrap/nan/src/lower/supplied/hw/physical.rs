//! Physical memory: regions, the aligned frames carved from them, the page-table roots owning a
//! frame, and the stores whose supply is a region's chunks. Each of these values is its
//! memory's physical address, which the entry root maps at the same address, and a region its
//! length too.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::super::Lowering;
use super::super::super::body::Held;
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::memory::CHUNK;
use super::super::super::runtime::{Routine, memory, report, state};
use super::super::Supply;
use super::super::mem::vector::{self, Element};
use super::cpu::{word, word_result};
use crate::check::ty::{Arg, Const};

/// The offsets of a region's base and length.
pub mod region {
    pub const BASE: u64 = 0;
    pub const LEN: u64 = 8;
}

/// `hw::Region`: its base and length, and `frames`, which appends each frame of a size lying
/// wholly inside it.
pub fn lower_region(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let held = supply.param(0).address();
    let base = fx.load(PTR, held, region::BASE);
    let len = fx.load(PTR, held, region::LEN);
    match supply.member() {
        "base" => supply.ret(lo, fx, Held::Scalar(base)),
        "byte_len" => supply.ret(lo, fx, Held::Scalar(len)),
        "frames" => {
            let size = size_arg(supply, 0);
            let frames = supply.param(1).address();
            let first = fx.b.ins().iadd_imm_s(base, size as i64 - 1);
            let first = fx.b.ins().band_imm_s(first, -(size as i64));
            let end = fx.b.ins().iadd(base, len);
            fx.repeat(&[first], |fx, at| {
                let at = at[0];
                let (past, wrapped) = {
                    let size = fx.ptr(size);
                    fx.b.ins().uadd_overflow(at, size)
                };
                let inside = fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, past, end);
                let kept = fx.b.ins().bxor_imm_s(wrapped, 1);
                let inside = fx.b.ins().band(inside, kept);
                fx.when(inside, |fx| push_frame(lo, fx, frames, at));
                (inside, vec![past])
            });
            supply.done(lo, fx);
        }
        member => panic!("no lowering of hw::Region.{member}"),
    }
}

/// `hw::PhysFrame[S]`: its base; `split` into frames of a smaller size T, in address order;
/// `merge` of S ÷ T contiguous frames, the first S-aligned, else the parts back; a projection
/// of an AnyBits value at an offset inside it; and the region of its memory.
pub fn lower_frame(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let size = size_arg(supply, 0);
    match supply.member() {
        "base" => {
            let base = word(fx, supply.param(0));
            supply.ret(lo, fx, Held::Scalar(base));
        }
        "split" => {
            let base = word(fx, supply.param(0));
            let part = size_arg(supply, 1);
            let parts = supply.param(1).address();
            let zero = fx.ptr(0);
            let count = fx.ptr(size / part);
            fx.each(zero, count, |fx, index| {
                let offset = fx.b.ins().imul_imm_s(index, part as i64);
                let at = fx.b.ins().iadd(base, offset);
                push_frame(lo, fx, parts, at);
            });
            supply.done(lo, fx);
        }
        "merge" => merge(lo, fx, supply, size),
        "contents_mut" => {
            let base = word(fx, supply.param(0));
            let value_ty = supply.type_arg(1);
            let layout = lo.layout(&value_ty);
            let offset = word(fx, supply.param(1));
            let room = fx.ptr(size.saturating_sub(layout.size));
            let beyond = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, offset, room);
            let too_large = fx.b.ins().iconst(
                cranelift_codegen::ir::types::I8,
                i64::from(layout.size > size),
            );
            let misaligned =
                fx.b.ins()
                    .band_imm_s(offset, layout.align.max(1) as i64 - 1);
            let misaligned = fx.b.ins().icmp_imm_s(IntCC::NotEqual, misaligned, 0);
            let out = fx.b.ins().bor(beyond, misaligned);
            let out = fx.b.ins().bor(out, too_large);
            fx.when(out, |fx| report::raise_kind(lo, fx, "trap.bounds"));
            let at = fx.b.ins().iadd(base, offset);
            supply.ret(lo, fx, Held::Address(at));
        }
        "into_region" => {
            let base = word(fx, supply.param(0));
            let held = fx.slot(16, 8);
            fx.store(base, held, region::BASE);
            let len = fx.ptr(size);
            fx.store(len, held, region::LEN);
            let result = lo.held_at(fx, &supply.abi.result, held);
            supply.ret(lo, fx, result);
        }
        member => panic!("no lowering of hw::PhysFrame.{member}"),
    }
}

/// `PhysFrame[S].merge(take parts)`: `Ok` of the frame the parts make up when they are exactly
/// S ÷ T contiguous frames in address order whose first is S-aligned, their list's storage then
/// freed, and otherwise `Err` of the parts unchanged.
fn merge(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, size: u64) {
    let part = size_arg(supply, 1);
    let parts = supply.param(0).address();
    let result_ty = supply.abi.result.clone();
    let len = fx.load(PTR, parts, vector::LEN);
    let data = fx.load(PTR, parts, vector::DATA);
    let counted =
        fx.b.ins()
            .icmp_imm_s(IntCC::Equal, len, (size / part) as i64);
    let first = fx.choose(
        counted,
        &[PTR],
        |fx| vec![fx.load(PTR, data, 0)],
        |fx| vec![fx.ptr(0)],
    )[0];
    let aligned = fx.b.ins().band_imm_s(first, size as i64 - 1);
    let aligned = fx.b.ins().icmp_imm_s(IntCC::Equal, aligned, 0);
    let aligned = fx.b.ins().band(counted, aligned);
    let whole = fx.var(types::I8, aligned);
    fx.when(aligned, |fx| {
        let one = fx.ptr(1);
        let count = fx.ptr(size / part);
        fx.each(one, count, |fx, index| {
            let slot = fx.b.ins().imul_imm_s(index, 8);
            let slot = fx.b.ins().iadd(data, slot);
            let base = fx.load(PTR, slot, 0);
            let offset = fx.b.ins().imul_imm_s(index, part as i64);
            let expected = fx.b.ins().iadd(first, offset);
            let follows = fx.b.ins().icmp(IntCC::Equal, base, expected);
            let so_far = fx.b.use_var(whole);
            let so_far = fx.b.ins().band(so_far, follows);
            fx.b.def_var(whole, so_far);
        });
    });
    let whole = fx.b.use_var(whole);
    fx.when(whole, |fx| {
        vector::free_storage(lo, fx, parts);
        let frame = fx.slot(8, 8);
        fx.store(first, frame, 0);
        let ok = lo.variant_value(fx, &result_ty, "Ok", &[Held::Address(frame)], None);
        supply.ret(lo, fx, ok);
    });
    let err = lo.variant_value(fx, &result_ty, "Err", &[Held::Address(parts)], None);
    supply.ret(lo, fx, err);
}

/// `hw::PageTableRoot`: `of` takes ownership of a frame for a top-level table, `into_frame`
/// gives it back.
pub fn page_table_root(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    match supply.member() {
        "of" | "into_frame" => {
            let base = word(fx, supply.param(0));
            word_result(lo, fx, supply, base);
        }
        member => panic!("no lowering of hw::PageTableRoot.{member}"),
    }
}

/// The new root cell over a region a store's `over` takes: its supply is the 64 KiB-aligned
/// chunks lying wholly inside the region, and its limit their total size.
pub fn region_cell(lo: &mut Lowering, fx: &mut Fx, region: Held) -> Value {
    let held = region.address();
    let base = fx.load(PTR, held, region::BASE);
    let len = fx.load(PTR, held, region::LEN);
    let end = fx.b.ins().iadd(base, len);
    let first = fx.b.ins().iadd_imm_s(base, CHUNK as i64 - 1);
    let first = fx.b.ins().band_imm_s(first, -(CHUNK as i64));
    let last = fx.b.ins().band_imm_s(end, -(CHUNK as i64));
    let some = fx.b.ins().icmp(IntCC::UnsignedLessThan, first, last);
    let span = fx.b.ins().isub(last, first);
    let zero = fx.ptr(0);
    let span = fx.b.ins().select(some, span, zero);
    let count = fx.b.ins().ushr_imm_s(span, 16);
    let state_address = lo.state(fx);
    let lock = fx.offset(state_address, state::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let cell = memory::region_cell(lo, fx, first, count);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    cell
}

/// Appends a frame at a base address to a list of frames, trapping when the list cannot grow.
fn push_frame(lo: &mut Lowering, fx: &mut Fx, list: Value, base: Value) {
    let element = Element { size: 8, align: 8 };
    let len = fx.load(PTR, list, vector::LEN);
    let needed = fx.b.ins().iadd_imm_s(len, 1);
    let room = vector::reserve(lo, fx, list, needed, element);
    let short = fx.b.ins().icmp_imm_s(IntCC::Equal, room, 0);
    fx.when(short, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
    let data = fx.load(PTR, list, vector::DATA);
    let at = element.at(fx, data, len);
    fx.store(base, at, 0);
    fx.store(needed, list, vector::LEN);
}

/// The frame size a const argument of the operation states.
fn size_arg(supply: &Supply, index: usize) -> u64 {
    match supply.args.get(index) {
        Some(Arg::Const(Const::Int(size))) => *size as u64,
        other => panic!("a frame size, found {other:?}"),
    }
}
