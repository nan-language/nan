//! Storage whose elements never move, for the stores that give parts through a read receiver:
//! segment k holds 4 × 2^k elements and is drawn through the allocator when first needed. Its
//! header, in runtime memory, is its length, its allocator handle, each segment's address and
//! the lock its insertions hold; reads take no lock, an element being written before the length
//! grows past it.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::super::Lowering;
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::Routine;
use super::super::super::transfer;
use super::vector::Element;
use crate::check::ty::Ty;

pub const LEN: u64 = 0;
const ALLOC: u64 = 8;
const SEGMENTS: u64 = 16;
const COUNT: u64 = 58;
pub const LOCK: u64 = SEGMENTS + 8 * COUNT;
const SIZE: u64 = LOCK + 8;

/// A new empty header drawing through an allocator handle.
pub fn header(lo: &mut Lowering, fx: &mut Fx, alloc: Value) -> Value {
    let size = fx.ptr(SIZE);
    let header = lo.record_alloc(fx, size);
    let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, header, 0);
    fx.when(failed, |fx| {
        super::super::super::runtime::report::raise_kind(lo, fx, "trap.exhausted");
    });
    fx.store(alloc, header, ALLOC);
    header
}

/// The segment holding element `index`.
fn segment_of(fx: &mut Fx, index: Value) -> Value {
    let quarter = fx.b.ins().ushr_imm_s(index, 2);
    let q = fx.b.ins().iadd_imm_s(quarter, 1);
    let zeros = fx.b.ins().clz(q);
    let top = fx.ptr(63);
    fx.b.ins().isub(top, zeros)
}

/// The address of element `index`, in a segment already drawn.
pub fn at(fx: &mut Fx, header: Value, index: Value, element: Element) -> Value {
    let segment = segment_of(fx, index);
    let one = fx.ptr(1);
    let size = fx.b.ins().ishl(one, segment);
    let first = fx.b.ins().iadd_imm_s(size, -1);
    let first = fx.b.ins().ishl_imm_s(first, 2);
    let position = fx.b.ins().isub(index, first);
    let slot = fx.b.ins().ishl_imm_s(segment, 3);
    let slot = fx.b.ins().iadd(header, slot);
    let base = fx.load(PTR, slot, SEGMENTS);
    element.at(fx, base, position)
}

/// Makes room for one more element: nonzero when there is room, zero on exhaustion.
pub fn reserve_one(lo: &mut Lowering, fx: &mut Fx, header: Value, element: Element) -> Value {
    let len = fx.load(PTR, header, LEN);
    let segment = segment_of(fx, len);
    let slot = fx.b.ins().ishl_imm_s(segment, 3);
    let slot = fx.b.ins().iadd(header, slot);
    let base = fx.load(PTR, slot, SEGMENTS);
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, base, 0);
    fx.choose(
        missing,
        &[types::I8],
        |fx| {
            let one = fx.ptr(1);
            let count = fx.b.ins().ishl(one, segment);
            let count = fx.b.ins().ishl_imm_s(count, 2);
            let bytes = element.bytes(fx, count);
            let bytes = fx.b.ins().umax(bytes, one);
            let alloc = fx.load(PTR, header, ALLOC);
            let align = fx.ptr(element.align);
            let drawn = lo.call_routine(fx, Routine::Alloc, &[alloc, bytes, align])[0];
            fx.store(drawn, slot, SEGMENTS);
            vec![fx.b.ins().icmp_imm_s(IntCC::NotEqual, drawn, 0)]
        },
        |fx| vec![fx.int(types::I8, 1)],
    )[0]
}

/// Releases every element, the segments and the header.
pub fn release(lo: &mut Lowering, fx: &mut Fx, header: Value, element: &Ty) {
    let shape = Element::of(lo, element);
    if lo.needs_release(element) {
        let len = fx.load(PTR, header, LEN);
        let zero = fx.ptr(0);
        fx.each(zero, len, |fx, index| {
            let address = at(fx, header, index, shape);
            lo.release(fx, element, address);
        });
    }
    let zero = fx.ptr(0);
    let count = fx.ptr(COUNT);
    fx.each(zero, count, |fx, segment| {
        let slot = fx.b.ins().ishl_imm_s(segment, 3);
        let slot = fx.b.ins().iadd(header, slot);
        let base = fx.load(PTR, slot, SEGMENTS);
        let drawn = fx.b.ins().icmp_imm_s(IntCC::NotEqual, base, 0);
        fx.when(drawn, |fx| {
            lo.call_routine(fx, Routine::Free, &[base]);
        });
    });
    lo.record_free(fx, header);
}

/// Moves every element and the segments into another domain's reach.
pub fn transfer(
    lo: &mut Lowering,
    fx: &mut Fx,
    header: Value,
    element: &Ty,
    lift: Value,
    owner: Value,
) {
    let shape = Element::of(lo, element);
    if lo.transfers(element) {
        let len = fx.load(PTR, header, LEN);
        let zero = fx.ptr(0);
        fx.each(zero, len, |fx, index| {
            let address = at(fx, header, index, shape);
            lo.transfer(fx, element, address, lift, owner);
        });
    }
    let zero = fx.ptr(0);
    let count = fx.ptr(COUNT);
    fx.each(zero, count, |fx, segment| {
        let slot = fx.b.ins().ishl_imm_s(segment, 3);
        let slot = fx.b.ins().iadd(header, slot);
        let base = fx.load(PTR, slot, SEGMENTS);
        let drawn = fx.b.ins().icmp_imm_s(IntCC::NotEqual, base, 0);
        fx.when(drawn, |fx| {
            transfer::lift(lo, fx, base, lift);
        });
    });
}

/// The allocator handle the storage draws through.
pub fn alloc(fx: &mut Fx, header: Value) -> Value {
    fx.load(PTR, header, ALLOC)
}
