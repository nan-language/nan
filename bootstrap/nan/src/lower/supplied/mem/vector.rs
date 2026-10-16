//! Growable vectors: the storage, length, capacity and allocator handle that lists, bytes,
//! strings and the entries of maps and sets keep, grown as growth states. An empty vector's
//! storage is a non-null address its elements' alignment gives, holding no allocation.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::super::Lowering;
use super::super::super::body::Held;
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::Routine;
use super::super::super::transfer;
use crate::check::ty::Ty;

pub const DATA: u64 = 0;
pub const LEN: u64 = 8;
pub const CAPACITY: u64 = 16;
pub const ALLOC: u64 = 24;

/// The size and alignment of a vector's elements.
#[derive(Clone, Copy)]
pub struct Element {
    pub size: u64,
    pub align: u64,
}

impl Element {
    pub fn of(lo: &mut Lowering, ty: &Ty) -> Element {
        let layout = lo.layout(ty);
        Element {
            size: layout.size,
            align: layout.align.max(1),
        }
    }

    /// The address of element `index` of the storage at `data`.
    pub fn at(self, fx: &mut Fx, data: Value, index: Value) -> Value {
        let offset = fx.b.ins().imul_imm_s(index, self.size as i64);
        fx.b.ins().iadd(data, offset)
    }

    /// The bytes of `count` elements.
    pub fn bytes(self, fx: &mut Fx, count: Value) -> Value {
        fx.b.ins().imul_imm_s(count, self.size as i64)
    }
}

/// Writes an empty vector drawing through an allocator handle at an address.
pub fn empty(fx: &mut Fx, address: Value, alloc: Value, element: Element) {
    let dangling = fx.ptr(element.align);
    fx.store(dangling, address, DATA);
    let zero = fx.ptr(0);
    fx.store(zero, address, LEN);
    fx.store(zero, address, CAPACITY);
    fx.store(alloc, address, ALLOC);
}

/// Makes room for `needed` elements in the vector at an address: nonzero when it has room,
/// zero on exhaustion, which leaves the vector unchanged. A new capacity is the largest of
/// twice the old one, the needed length and 4.
pub fn reserve(
    lo: &mut Lowering,
    fx: &mut Fx,
    address: Value,
    needed: Value,
    element: Element,
) -> Value {
    let capacity = fx.load(PTR, address, CAPACITY);
    let enough =
        fx.b.ins()
            .icmp(IntCC::UnsignedLessThanOrEqual, needed, capacity);
    fx.choose(
        enough,
        &[types::I8],
        |fx| vec![fx.int(types::I8, 1)],
        |fx| {
            let doubled = fx.b.ins().ishl_imm_s(capacity, 1);
            let wanted = fx.b.ins().umax(doubled, needed);
            let four = fx.ptr(4);
            let wanted = fx.b.ins().umax(wanted, four);
            vec![resize(lo, fx, address, wanted, element)]
        },
    )[0]
}

/// Moves the vector's elements to a new allocation of `capacity` elements, freeing the old
/// one: nonzero on success, zero on exhaustion, which leaves the vector unchanged.
pub fn resize(
    lo: &mut Lowering,
    fx: &mut Fx,
    address: Value,
    capacity: Value,
    element: Element,
) -> Value {
    let size = fx.ptr(element.size);
    let (bytes, overflow) = fx.b.ins().umul_overflow(capacity, size);
    fx.choose(
        overflow,
        &[types::I8],
        |fx| vec![fx.int(types::I8, 0)],
        |fx| {
            let one = fx.ptr(1);
            let bytes = fx.b.ins().umax(bytes, one);
            let alloc = fx.load(PTR, address, ALLOC);
            let align = fx.ptr(element.align);
            let fresh = lo.call_routine(fx, Routine::Alloc, &[alloc, bytes, align])[0];
            let drawn = fx.b.ins().icmp_imm_s(IntCC::NotEqual, fresh, 0);
            fx.when(drawn, |fx| {
                let data = fx.load(PTR, address, DATA);
                let len = fx.load(PTR, address, LEN);
                let used = element.bytes(fx, len);
                lo.call_routine(fx, Routine::Copy, &[fresh, data, used]);
                free_storage(lo, fx, address);
                fx.store(fresh, address, DATA);
                fx.store(capacity, address, CAPACITY);
            });
            vec![drawn]
        },
    )[0]
}

/// Frees a vector's storage when it holds an allocation.
pub fn free_storage(lo: &mut Lowering, fx: &mut Fx, address: Value) {
    let capacity = fx.load(PTR, address, CAPACITY);
    let allocated = fx.b.ins().icmp_imm_s(IntCC::NotEqual, capacity, 0);
    fx.when(allocated, |fx| {
        let data = fx.load(PTR, address, DATA);
        lo.call_routine(fx, Routine::Free, &[data]);
    });
}

/// Releases each element of a type from `from` to the length, then the storage.
pub fn release(lo: &mut Lowering, fx: &mut Fx, address: Value, element: &Ty, from: Value) {
    release_elements(lo, fx, address, element, from);
    free_storage(lo, fx, address);
}

/// Releases each element of a type from `from` to the length.
pub fn release_elements(lo: &mut Lowering, fx: &mut Fx, address: Value, element: &Ty, from: Value) {
    if !lo.needs_release(element) {
        return;
    }
    let shape = Element::of(lo, element);
    let data = fx.load(PTR, address, DATA);
    let len = fx.load(PTR, address, LEN);
    fx.each(from, len, |fx, index| {
        let at = shape.at(fx, data, index);
        lo.release(fx, element, at);
    });
}

/// Moves a vector into another domain's reach: its storage, and each element from `from`, or
/// from the first.
pub fn transfer(
    lo: &mut Lowering,
    fx: &mut Fx,
    address: Value,
    element: &Ty,
    from: Option<Value>,
    lift: Value,
    owner: Value,
) {
    let capacity = fx.load(PTR, address, CAPACITY);
    let allocated = fx.b.ins().icmp_imm_s(IntCC::NotEqual, capacity, 0);
    fx.when(allocated, |fx| {
        let data = fx.load(PTR, address, DATA);
        transfer::lift(lo, fx, data, lift);
    });
    if !lo.transfers(element) {
        return;
    }
    let shape = Element::of(lo, element);
    let data = fx.load(PTR, address, DATA);
    let len = fx.load(PTR, address, LEN);
    let from = from.unwrap_or_else(|| fx.ptr(0));
    fx.each(from, len, |fx, index| {
        let at = shape.at(fx, data, index);
        lo.transfer(fx, element, at, lift, owner);
    });
}

/// Appends a value as the last element, in room already reserved.
pub fn push_reserved(lo: &mut Lowering, fx: &mut Fx, address: Value, value: Held, element: &Ty) {
    let shape = Element::of(lo, element);
    let data = fx.load(PTR, address, DATA);
    let len = fx.load(PTR, address, LEN);
    let at = shape.at(fx, data, len);
    lo.store_held(fx, at, value, element);
    let len = fx.b.ins().iadd_imm_s(len, 1);
    fx.store(len, address, LEN);
}

/// Opens a gap of one element at `index`, in room already reserved: the elements from it move
/// up by one and the length grows by one. Gives the gap's address.
pub fn open_gap(
    lo: &mut Lowering,
    fx: &mut Fx,
    address: Value,
    index: Value,
    element: Element,
) -> Value {
    let data = fx.load(PTR, address, DATA);
    let len = fx.load(PTR, address, LEN);
    let at = element.at(fx, data, index);
    let after = fx.b.ins().iadd_imm_s(at, element.size as i64);
    let moved = fx.b.ins().isub(len, index);
    let moved = element.bytes(fx, moved);
    lo.call_routine(fx, Routine::Copy, &[after, at, moved]);
    let len = fx.b.ins().iadd_imm_s(len, 1);
    fx.store(len, address, LEN);
    at
}

/// Closes the element at `index`, whose value has moved out: the elements after it move down
/// by one and the length shrinks by one.
pub fn close_gap(lo: &mut Lowering, fx: &mut Fx, address: Value, index: Value, element: Element) {
    let data = fx.load(PTR, address, DATA);
    let len = fx.load(PTR, address, LEN);
    let at = element.at(fx, data, index);
    let after = fx.b.ins().iadd_imm_s(at, element.size as i64);
    let rest = fx.b.ins().isub(len, index);
    let rest = fx.b.ins().iadd_imm_s(rest, -1);
    let moved = element.bytes(fx, rest);
    lo.call_routine(fx, Routine::Copy, &[at, after, moved]);
    let len = fx.b.ins().iadd_imm_s(len, -1);
    fx.store(len, address, LEN);
}

/// Writes the view of a vector's elements at an address.
pub fn write_view(fx: &mut Fx, view: Value, address: Value) {
    let data = fx.load(PTR, address, DATA);
    let len = fx.load(PTR, address, LEN);
    fx.store(data, view, 0);
    fx.store(len, view, 8);
}

/// Keeps the elements a predicate keeps, in order, releasing the others; `keep` receives each
/// element's address and gives whether to keep it.
pub fn retain(
    lo: &mut Lowering,
    fx: &mut Fx,
    address: Value,
    element: &Ty,
    mut keep: impl FnMut(&mut Lowering, &mut Fx, Value) -> Value,
) {
    let shape = Element::of(lo, element);
    let data = fx.load(PTR, address, DATA);
    let len = fx.load(PTR, address, LEN);
    let zero = fx.ptr(0);
    let end = fx.repeat(&[zero, zero], |fx, state| {
        let (index, kept) = (state[0], state[1]);
        let more = fx.b.ins().icmp(IntCC::UnsignedLessThan, index, len);
        let next = fx.choose(
            more,
            &[PTR, PTR],
            |fx| {
                let at = shape.at(fx, data, index);
                let keeps = keep(lo, fx, at);
                let kept = fx.choose_in(
                    lo,
                    keeps,
                    &[PTR],
                    |lo, fx| {
                        let target = shape.at(fx, data, kept);
                        let size = fx.ptr(shape.size);
                        lo.call_routine(fx, Routine::Copy, &[target, at, size]);
                        vec![fx.b.ins().iadd_imm_s(kept, 1)]
                    },
                    |lo, fx| {
                        lo.release(fx, element, at);
                        vec![kept]
                    },
                )[0];
                vec![fx.b.ins().iadd_imm_s(index, 1), kept]
            },
            |_| vec![index, kept],
        );
        (more, next)
    });
    fx.store(end[1], address, LEN);
}
