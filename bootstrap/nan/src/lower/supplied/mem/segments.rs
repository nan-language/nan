//! Storage whose elements never move, for the stores that give parts through a read receiver:
//! segment k holds 4 × 2^k elements and is drawn through the allocator when first needed. Its
//! header, in runtime memory, is its length, its allocator handle, each segment's address, the
//! lock its insertions hold, for a once map its seed and its hash index's table record, and its
//! reach, the domain holding the store, to which an insertion by a task of a domain nested in it
//! passes what it stores; reads by position take no lock, an element being written before the
//! length grows past it.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::super::Lowering;
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::{Routine, tasks};
use super::super::super::transfer;
use super::vector::Element;
use crate::check::ty::Ty;

pub const LEN: u64 = 0;
const ALLOC: u64 = 8;
const SEGMENTS: u64 = 16;
const COUNT: u64 = 58;
pub const LOCK: u64 = SEGMENTS + 8 * COUNT;
pub const SEED: u64 = LOCK + 8;
pub const INDEX: u64 = SEED + 8;
const REACH: u64 = INDEX + 16;
const SIZE: u64 = REACH + 8;

/// A new empty header drawing through an allocator handle, its reach the running task's domain.
pub fn header(lo: &mut Lowering, fx: &mut Fx, alloc: Value) -> Value {
    let size = fx.ptr(SIZE);
    let header = lo.record_alloc(fx, size);
    let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, header, 0);
    fx.when(failed, |fx| {
        super::super::super::runtime::report::raise_kind(lo, fx, "trap.exhausted");
    });
    fx.store(alloc, header, ALLOC);
    let reach = tasks::current_domain(lo, fx);
    fx.store(reach, header, REACH);
    header
}

/// The reach of the storage when the running task's domain is nested in it, which then holds
/// what the task stores; zero otherwise.
pub fn outer_reach(lo: &mut Lowering, fx: &mut Fx, header: Value) -> Value {
    let reach = fx.load(PTR, header, REACH);
    let domain = tasks::current_domain(lo, fx);
    let nested = tasks::nested_in(fx, domain, reach);
    let zero = fx.ptr(0);
    fx.b.ins().select(nested, reach, zero)
}

/// Passes element `index` to `reach`, unless it is zero: the chunks of its segment and of what
/// it holds, and its holdings.
pub fn lift_element(
    lo: &mut Lowering,
    fx: &mut Fx,
    header: Value,
    element: &Ty,
    index: Value,
    reach: Value,
) {
    let shape = Element::of(lo, element);
    let lifting = fx.b.ins().icmp_imm_s(IntCC::NotEqual, reach, 0);
    fx.when(lifting, |fx| {
        let address = at(fx, header, index, shape);
        transfer::lift(lo, fx, address, reach);
        let owner = transfer::raised(fx, reach);
        lo.transfer(fx, element, address, reach, owner);
    });
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

/// Moves every element and the segments into another domain's reach, which becomes the
/// storage's.
pub fn transfer(
    lo: &mut Lowering,
    fx: &mut Fx,
    header: Value,
    element: &Ty,
    lift: Value,
    owner: Value,
) {
    let lifting = fx.b.ins().icmp_imm_s(IntCC::NotEqual, lift, 0);
    fx.when(lifting, |fx| fx.store(lift, header, REACH));
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
