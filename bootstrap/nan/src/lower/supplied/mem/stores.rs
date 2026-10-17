//! Allocators, budgets and stores: heaps, boxes, shared values, arenas, pools and once maps.
//!
//! An allocator or heap is a cell handle, and a budget the holding of one. A box is the address
//! of its contents. A shared value is the address of its count, its allocator handle, its reach
//! and its contents. An arena or once map is the address of the header of its segments. A pool
//! is a vector of slots, each its generation, whether it is occupied and its value, then the
//! count of occupied slots.

use cranelift_codegen::ir::{
    AtomicRmwOp, InstBuilder, MemFlagsData, Value, condcodes::IntCC, types,
};

use super::super::super::Lowering;
use super::super::super::body::Held;
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::{Routine, holdings, report, tasks};
use super::super::super::transfer;
use super::super::{Supply, hw};
use super::maps::Entries;
use super::segments;
use super::vector::{self, Element};
use crate::check::ty::Ty;

/// The offsets of a shared value's count, allocator handle and reach, and of its contents.
mod shared {
    pub const COUNT: u64 = 0;
    pub const ALLOC: u64 = 8;
    pub const REACH: u64 = 16;
    pub const HEADER: u64 = 24;
}

/// The offset of a pool's count of occupied slots.
pub const POOL_COUNT: u64 = 32;

/// The layout of a pool slot holding a value of a layout: its size and alignment, and the
/// offset of the value after the generation and the occupied flag.
fn slot_shape(lo: &mut Lowering, value: &Ty) -> (Element, u64) {
    let layout = lo.layout(value);
    let align = layout.align.max(4);
    let at = 8u64.next_multiple_of(layout.align.max(1));
    let size = (at + layout.size).next_multiple_of(align);
    (Element { size, align }, at)
}

/// The offset of a shared value's contents of a type.
fn contents_at(lo: &mut Lowering, value: &Ty) -> u64 {
    shared::HEADER.next_multiple_of(lo.layout(value).align.max(1))
}

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    let owner = supply.path.rsplit_once('.').map_or("", |(owner, _)| owner);
    match owner {
        "mem::Alloc" | "mem::Budget" => budget(lo, fx, supply),
        "mem::Heap" => heap(lo, fx, supply),
        "mem::Box" => boxed_member(lo, fx, supply),
        "mem::Shared" => shared(lo, fx, supply),
        "mem::Arena" => arena(lo, fx, supply),
        "mem::ArenaCursor" => arena_next(lo, fx, supply),
        "mem::Id" => id(lo, fx, supply),
        "mem::Pool" => pool(lo, fx, supply),
        "mem::PoolCursor" => pool_next(lo, fx, supply),
        "mem::OnceMap" => once_map(lo, fx, supply),
        _ => return false,
    }
    true
}

/// The cell handle of an allocator or heap held at an address.
fn handle(fx: &mut Fx, held: Held) -> Value {
    fx.load(PTR, held.address(), 0)
}

/// The cell a new store draws through: its allocator's, or a new root cell over the region its
/// `over` takes.
fn store_cell(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> Value {
    match supply.member() {
        "over" => hw::region_cell(lo, fx, supply.param(0)),
        _ => handle(fx, supply.param(0)),
    }
}

/// Returns a cell handle as an allocator or heap.
fn ret_handle(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, cell: Value) {
    let sret = supply.sret.expect("a handle's address");
    fx.store(cell, sret, 0);
    supply.done(lo, fx);
}

fn budget(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let receiver = supply.param(0).address();
    let cell = match supply.path.starts_with("mem::Budget.") {
        true => holdings::payload(fx, receiver),
        false => fx.load(PTR, receiver, 0),
    };
    match supply.member() {
        "split" => {
            let limit = supply.param(1).scalar();
            let budget = lo.call_routine(fx, Routine::Split, &[cell, limit])[0];
            let kind = fx.ptr(holdings::kind::CLOSE_CELL as u64);
            let holding = lo.call_routine(fx, Routine::Hold, &[kind, budget])[0];
            ret_handle(lo, fx, supply, holding);
        }
        "alloc" => ret_handle(lo, fx, supply, cell),
        "limit" | "charged" => {
            let routine = match supply.member() {
                "limit" => Routine::Limit,
                _ => Routine::Charged,
            };
            let bytes = lo.call_routine(fx, routine, &[cell])[0];
            supply.ret(lo, fx, Held::Scalar(bytes));
        }
        "close" => {
            lo.call_routine(fx, Routine::Close, &[cell]);
            let holding = fx.load(PTR, receiver, 0);
            lo.call_routine(fx, Routine::Unhold, &[holding]);
            supply.done(lo, fx);
        }
        member => panic!("no lowering of {}.{member}", supply.path),
    }
}

fn heap(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    match supply.member() {
        "of" | "alloc" | "over" => {
            let cell = store_cell(lo, fx, supply);
            ret_handle(lo, fx, supply, cell);
        }
        "boxed" | "try_boxed" => {
            let cell = handle(fx, supply.param(0));
            boxed(lo, fx, supply, cell);
        }
        member => panic!("no lowering of mem::Heap.{member}"),
    }
}

/// `boxed` and `try_boxed` of a store drawing through a cell: a box holding the value taken.
fn boxed(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, cell: Value) {
    let value_ty = supply.type_arg(supply.args.len() - 1);
    let value = supply.param(1);
    let layout = lo.layout(&value_ty);
    let size = fx.ptr(layout.size.max(1));
    let align = fx.ptr(layout.align.max(1));
    let address = lo.call_routine(fx, Routine::Alloc, &[cell, size, align])[0];
    let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, address, 0);
    fx.when(failed, |fx| {
        supply.exhausted(lo, fx, &[(value, value_ty.clone())])
    });
    lo.store_held(fx, address, value, &value_ty);
    let sret = supply.sret.expect("a box's address");
    fx.store(address, sret, 0);
    supply.done(lo, fx);
}

fn boxed_member(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let contents = fx.load(PTR, supply.param(0).address(), 0);
    match supply.member() {
        "contents" | "contents_mut" => supply.ret(lo, fx, Held::Address(contents)),
        "into_contents" => {
            let value_ty = supply.type_arg(0);
            let layout = lo.layout(&value_ty);
            let result = supply
                .sret
                .unwrap_or_else(|| fx.slot(layout.size, layout.align));
            let held = lo.held_at(fx, &value_ty, contents);
            lo.store_held(fx, result, held, &value_ty);
            lo.call_routine(fx, Routine::Free, &[contents]);
            let held = lo.held_at(fx, &value_ty, result);
            supply.ret(lo, fx, held);
        }
        member => panic!("no lowering of mem::Box.{member}"),
    }
}

/// Releases a box of a type at an address: its contents, then its memory. A box of a `Dyn`
/// releases its contents through the method table.
pub fn release_box(lo: &mut Lowering, fx: &mut Fx, ty: &Ty, address: Value) {
    let contents = fx.load(PTR, address, 0);
    match ty.type_arg(0) {
        Some(Ty::Dyn(_)) => {
            let table = fx.load(PTR, address, 8);
            let release = fx.load(PTR, table, 0);
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, release, 0);
            fx.when(present, |fx| {
                let mut signature =
                    cranelift_codegen::ir::Signature::new(super::super::super::CALL_CONV);
                signature
                    .params
                    .push(cranelift_codegen::ir::AbiParam::new(PTR));
                let signature = fx.b.import_signature(signature);
                fx.b.ins().call_indirect(signature, release, &[contents]);
            });
        }
        Some(inner) => {
            let inner = inner.clone();
            lo.release(fx, &inner, contents);
        }
        None => {}
    }
    lo.call_routine(fx, Routine::Free, &[contents]);
}

fn shared(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let value_ty = supply.type_arg(0);
    let at = contents_at(lo, &value_ty);
    match supply.member() {
        "of" | "try_of" => {
            let value = supply.param(0);
            let cell = handle(fx, supply.param(1));
            let record = new_shared(lo, fx, cell, &value_ty, value);
            let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
            fx.when(failed, |fx| {
                supply.exhausted(lo, fx, &[(value, value_ty.clone())])
            });
            let sret = supply.sret.expect("a shared value's address");
            fx.store(record, sret, 0);
            supply.done(lo, fx);
        }
        "share" => {
            let record = fx.load(PTR, supply.param(0).address(), 0);
            let one = fx.ptr(1);
            fx.b.ins()
                .atomic_rmw(PTR, MemFlagsData::trusted(), AtomicRmwOp::Add, record, one);
            let sret = supply.sret.expect("a shared value's address");
            fx.store(record, sret, 0);
            supply.done(lo, fx);
        }
        "contents" => {
            let record = fx.load(PTR, supply.param(0).address(), 0);
            let contents = fx.offset(record, at);
            supply.ret(lo, fx, Held::Address(contents));
        }
        "make_mut" => {
            let receiver = supply.param(0).address();
            let clone = supply.param(1).address();
            let fn_ty = super::lists::fn_param(supply, 1);
            let record = fx.load(PTR, receiver, 0);
            let count = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), record);
            let shared_more = fx.b.ins().icmp_imm_s(IntCC::UnsignedGreaterThan, count, 1);
            fx.when(shared_more, |fx| {
                let contents = fx.offset(record, at);
                let cell = fx.load(PTR, record, shared::ALLOC);
                let alloc = fx.slot(8, 8);
                fx.store(cell, alloc, 0);
                let held = lo.held_at(fx, &value_ty, contents);
                let copied = lo
                    .call_value(fx, &fn_ty, clone, &[held, Held::Address(alloc)])
                    .result;
                let fresh = new_shared(lo, fx, cell, &value_ty, copied);
                let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, fresh, 0);
                fx.when(failed, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
                release_shared(lo, fx, &value_ty, receiver);
                fx.store(fresh, receiver, 0);
            });
            let record = fx.load(PTR, receiver, 0);
            let reach = tasks::current_domain(lo, fx);
            fx.store(reach, record, shared::REACH);
            let contents = fx.offset(record, at);
            supply.ret(lo, fx, Held::Address(contents));
        }
        member => panic!("no lowering of mem::Shared.{member}"),
    }
}

/// A new shared value holding a value with a count of 1, drawn through a cell; zero on
/// exhaustion.
pub fn new_shared(
    lo: &mut Lowering,
    fx: &mut Fx,
    cell: Value,
    value_ty: &Ty,
    value: Held,
) -> Value {
    let at = contents_at(lo, value_ty);
    let layout = lo.layout(value_ty);
    let size = fx.ptr(at + layout.size);
    let align = fx.ptr(layout.align.max(8));
    let record = lo.call_routine(fx, Routine::Alloc, &[cell, size, align])[0];
    let drawn = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
    fx.when(drawn, |fx| {
        let one = fx.ptr(1);
        fx.store(one, record, shared::COUNT);
        fx.store(cell, record, shared::ALLOC);
        let reach = tasks::current_domain(lo, fx);
        fx.store(reach, record, shared::REACH);
        let contents = fx.offset(record, at);
        lo.store_held(fx, contents, value, value_ty);
    });
    record
}

/// Releases a shared value at an address: the last one releases the contents and frees them.
pub fn release_shared(lo: &mut Lowering, fx: &mut Fx, value_ty: &Ty, address: Value) {
    let record = fx.load(PTR, address, 0);
    let minus = fx.ptr(u64::MAX);
    let before = fx.b.ins().atomic_rmw(
        PTR,
        MemFlagsData::trusted(),
        AtomicRmwOp::Add,
        record,
        minus,
    );
    let last = fx.b.ins().icmp_imm_s(IntCC::Equal, before, 1);
    let at = contents_at(lo, value_ty);
    fx.when(last, |fx| {
        let contents = fx.offset(record, at);
        lo.release(fx, value_ty, contents);
        lo.call_routine(fx, Routine::Free, &[record]);
    });
}

/// Moves a shared value into another domain's reach: when its reach widens, its record and
/// contents follow, the contents' holdings raised to the new reach.
pub fn transfer_shared(lo: &mut Lowering, fx: &mut Fx, value_ty: &Ty, address: Value, lift: Value) {
    let lifting = fx.b.ins().icmp_imm_s(IntCC::NotEqual, lift, 0);
    fx.when(lifting, |fx| {
        let record = fx.load(PTR, address, 0);
        let reach = fx.offset(record, shared::REACH);
        let widened = transfer::widen(fx, reach, lift);
        fx.when(widened, |fx| {
            transfer::lift(lo, fx, record, lift);
            let at = contents_at(lo, value_ty);
            let contents = fx.offset(record, at);
            let owner = transfer::raised(fx, lift);
            lo.transfer(fx, value_ty, contents, lift, owner);
        });
    });
}

/// Moves a pool into another domain's reach: its storage and the value of each occupied slot.
pub fn transfer_pool(
    lo: &mut Lowering,
    fx: &mut Fx,
    element: &Ty,
    address: Value,
    lift: Value,
    owner: Value,
) {
    let (slot, value_at) = slot_shape(lo, element);
    let capacity = fx.load(PTR, address, vector::CAPACITY);
    let allocated = fx.b.ins().icmp_imm_s(IntCC::NotEqual, capacity, 0);
    fx.when(allocated, |fx| {
        let data = fx.load(PTR, address, vector::DATA);
        transfer::lift(lo, fx, data, lift);
    });
    if !lo.transfers(element) {
        return;
    }
    let data = fx.load(PTR, address, vector::DATA);
    let len = fx.load(PTR, address, vector::LEN);
    let zero = fx.ptr(0);
    fx.each(zero, len, |fx, index| {
        let at = slot.at(fx, data, index);
        let occupied = fx.load(types::I32, at, 4);
        let occupied = fx.b.ins().icmp_imm_s(IntCC::NotEqual, occupied, 0);
        fx.when(occupied, |fx| {
            let stored = fx.offset(at, value_at);
            lo.transfer(fx, element, stored, lift, owner);
        });
    });
}

/// The address of the contents of a box or shared value whose first word is `pointer`.
pub fn contents_address(lo: &mut Lowering, fx: &mut Fx, ty: &Ty, pointer: Value) -> Value {
    if lo.ck.is_std(ty, "mem::Shared") {
        let value_ty = ty.type_arg(0).cloned().expect("a shared value's type");
        let at = contents_at(lo, &value_ty);
        return fx.offset(pointer, at);
    }
    pointer
}

fn arena(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let element = supply.type_arg(0);
    let shape = Element::of(lo, &element);
    match supply.member() {
        "empty" | "over" => {
            let cell = store_cell(lo, fx, supply);
            let header = segments::header(lo, fx, cell);
            let sret = supply.sret.expect("an arena's address");
            fx.store(header, sret, 0);
            supply.done(lo, fx);
        }
        "len" => {
            let header = fx.load(PTR, supply.param(0).address(), 0);
            let len = fx.load(PTR, header, segments::LEN);
            supply.ret(lo, fx, Held::Scalar(len));
        }
        "push" | "try_push" => {
            let header = fx.load(PTR, supply.param(0).address(), 0);
            let value = supply.param(1);
            let lock = fx.offset(header, segments::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let room = segments::reserve_one(lo, fx, header, shape);
            let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, room, 0);
            fx.when(failed, |fx| {
                lo.call_routine(fx, Routine::Unlock, &[lock]);
                supply.exhausted(lo, fx, &[(value, element.clone())]);
            });
            let len = fx.load(PTR, header, segments::LEN);
            let at = segments::at(fx, header, len, shape);
            lo.store_held(fx, at, value, &element);
            let next = fx.b.ins().iadd_imm_s(len, 1);
            fx.store(next, header, segments::LEN);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            let id = fx.b.ins().ireduce(types::I32, len);
            let sret = supply.sret.expect("an id's address");
            fx.store(id, sret, 0);
            supply.done(lo, fx);
        }
        "at" | "at_mut" | "get" => {
            let header = fx.load(PTR, supply.param(0).address(), 0);
            let index = fx.load(types::I32, supply.param(1).address(), 0);
            let index = fx.b.ins().uextend(PTR, index);
            let len = fx.load(PTR, header, segments::LEN);
            let found = fx.b.ins().icmp(IntCC::UnsignedLessThan, index, len);
            if supply.member() == "get" {
                let address = fx.choose(
                    found,
                    &[PTR],
                    |fx| vec![segments::at(fx, header, index, shape)],
                    |fx| vec![fx.ptr(0)],
                )[0];
                projected(lo, fx, supply, &element, found, address);
            } else {
                let outside = fx.b.ins().bxor_imm_s(found, 1);
                fx.when(outside, |fx| report::raise_kind(lo, fx, "trap.bounds"));
                let at = segments::at(fx, header, index, shape);
                supply.ret(lo, fx, Held::Address(at));
            }
        }
        "cursor" => {
            let header = fx.load(PTR, supply.param(0).address(), 0);
            let sret = supply.sret.expect("a cursor's address");
            fx.store(header, sret, 0);
            let zero = fx.ptr(0);
            fx.store(zero, sret, 8);
            supply.done(lo, fx);
        }
        "boxed" | "try_boxed" => {
            let header = fx.load(PTR, supply.param(0).address(), 0);
            let cell = segments::alloc(fx, header);
            boxed(lo, fx, supply, cell);
        }
        member => panic!("no lowering of mem::Arena.{member}"),
    }
}

/// Returns a projection's `Option`: `Some` of the part at `address` when `found`, else `None`.
pub fn projected(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    value: &Ty,
    found: Value,
    address: Value,
) {
    let layout_ty = supply.abi.result.clone();
    let layout = lo.layout(&layout_ty);
    let result = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    let part = lo.is_part(value);
    let value = value.clone();
    lo.write_option_with(fx, &layout_ty, result, found, |lo, fx, payload| {
        if part {
            fx.store(address, payload, 0);
        } else {
            let held = lo.held_at(fx, &value, address);
            lo.store_held(fx, payload, held, &value);
        }
    });
    let held = lo.held_at(fx, &layout_ty, result);
    supply.ret(lo, fx, held);
}

/// `ArenaCursor.next`: the arena's header and the next index.
fn arena_next(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let element = supply.type_arg(0);
    let shape = Element::of(lo, &element);
    let cursor = supply.param(0).address();
    let header = fx.load(PTR, cursor, 0);
    let position = fx.load(PTR, cursor, 8);
    let len = fx.load(PTR, header, segments::LEN);
    let found = fx.b.ins().icmp(IntCC::UnsignedLessThan, position, len);
    let address = fx.choose(
        found,
        &[PTR],
        |fx| {
            let next = fx.b.ins().iadd_imm_s(position, 1);
            fx.store(next, cursor, 8);
            vec![segments::at(fx, header, position, shape)]
        },
        |fx| vec![fx.ptr(0)],
    )[0];
    projected(lo, fx, supply, &element, found, address);
}

fn id(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    match supply.member() {
        "of" => {
            let sret = supply.sret.expect("an id's address");
            fx.store(supply.param(0).scalar(), sret, 0);
            supply.done(lo, fx);
        }
        _ => {
            let index = fx.load(types::I32, supply.param(0).address(), 0);
            supply.ret(lo, fx, Held::Scalar(index));
        }
    }
}

fn pool(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let element = supply.type_arg(0);
    let (slot, value_at) = slot_shape(lo, &element);
    match supply.member() {
        "empty" | "over" => {
            let sret = supply.sret.expect("a pool's address");
            let cell = store_cell(lo, fx, supply);
            vector::empty(fx, sret, cell, slot);
            let zero = fx.ptr(0);
            fx.store(zero, sret, POOL_COUNT);
            supply.done(lo, fx);
        }
        "len" => {
            let count = fx.load(PTR, supply.param(0).address(), POOL_COUNT);
            supply.ret(lo, fx, Held::Scalar(count));
        }
        "insert" | "try_insert" => {
            let address = supply.param(0).address();
            let value = supply.param(1);
            let data = fx.load(PTR, address, vector::DATA);
            let len = fx.load(PTR, address, vector::LEN);
            let free = first_free(fx, data, len, slot);
            let appended = fx.b.ins().icmp(IntCC::Equal, free, len);
            fx.when(appended, |fx| {
                let needed = fx.b.ins().iadd_imm_s(len, 1);
                let room = vector::reserve(lo, fx, address, needed, slot);
                let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, room, 0);
                fx.when(failed, |fx| {
                    supply.exhausted(lo, fx, &[(value, element.clone())])
                });
                let data = fx.load(PTR, address, vector::DATA);
                let at = slot.at(fx, data, len);
                let zero = fx.int(types::I32, 0);
                fx.store(zero, at, 0);
                fx.store(needed, address, vector::LEN);
            });
            let data = fx.load(PTR, address, vector::DATA);
            let at = slot.at(fx, data, free);
            let occupied = fx.int(types::I32, 1);
            fx.store(occupied, at, 4);
            let stored = fx.offset(at, value_at);
            lo.store_held(fx, stored, value, &element);
            let count = fx.load(PTR, address, POOL_COUNT);
            let count = fx.b.ins().iadd_imm_s(count, 1);
            fx.store(count, address, POOL_COUNT);
            let generation = fx.load(types::I32, at, 0);
            let sret = supply.sret.expect("a handle's address");
            let index = fx.b.ins().ireduce(types::I32, free);
            fx.store(index, sret, 0);
            fx.store(generation, sret, 4);
            supply.done(lo, fx);
        }
        "reserve" | "try_reserve" => {
            let address = supply.param(0).address();
            let additional = supply.param(1).scalar();
            let len = fx.load(PTR, address, vector::LEN);
            let count = fx.load(PTR, address, POOL_COUNT);
            let free = fx.b.ins().isub(len, count);
            let short =
                fx.b.ins()
                    .icmp(IntCC::UnsignedGreaterThan, additional, free);
            fx.when(short, |fx| {
                let missing = fx.b.ins().isub(additional, free);
                let (needed, overflow) = fx.b.ins().uadd_overflow(len, missing);
                let room = fx.choose(
                    overflow,
                    &[types::I8],
                    |fx| vec![fx.int(types::I8, 0)],
                    |fx| vec![vector::reserve(lo, fx, address, needed, slot)],
                )[0];
                let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, room, 0);
                fx.when(failed, |fx| supply.exhausted(lo, fx, &[]));
            });
            supply.done(lo, fx);
        }
        member @ ("contains" | "get" | "get_mut" | "at" | "at_mut" | "remove") => {
            let address = supply.param(0).address();
            let handle = supply.param(1).address();
            let index = fx.load(types::I32, handle, 0);
            let index = fx.b.ins().uextend(PTR, index);
            let generation = fx.load(types::I32, handle, 4);
            let data = fx.load(PTR, address, vector::DATA);
            let len = fx.load(PTR, address, vector::LEN);
            let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, index, len);
            let live = fx.choose(
                inside,
                &[types::I8],
                |fx| {
                    let at = slot.at(fx, data, index);
                    let occupied = fx.load(types::I32, at, 4);
                    let occupied = fx.b.ins().icmp_imm_s(IntCC::NotEqual, occupied, 0);
                    let stored = fx.load(types::I32, at, 0);
                    let same = fx.b.ins().icmp(IntCC::Equal, stored, generation);
                    vec![fx.b.ins().band(occupied, same)]
                },
                |fx| vec![fx.int(types::I8, 0)],
            )[0];
            let at = slot.at(fx, data, index);
            let stored = fx.offset(at, value_at);
            match member {
                "contains" => supply.ret(lo, fx, Held::Scalar(live)),
                "get" | "get_mut" => projected(lo, fx, supply, &element, live, stored),
                "at" | "at_mut" => {
                    let stale = fx.b.ins().bxor_imm_s(live, 1);
                    fx.when(stale, |fx| report::raise_kind(lo, fx, "trap.stale-handle"));
                    supply.ret(lo, fx, Held::Address(stored));
                }
                _ => {
                    let stale = fx.b.ins().bxor_imm_s(live, 1);
                    fx.when(stale, |fx| report::raise_kind(lo, fx, "trap.stale-handle"));
                    let layout = lo.layout(&element);
                    let result = supply
                        .sret
                        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
                    let held = lo.held_at(fx, &element, stored);
                    lo.store_held(fx, result, held, &element);
                    let next = fx.b.ins().iadd_imm_s(generation, 1);
                    fx.store(next, at, 0);
                    let vacant = fx.int(types::I32, 0);
                    fx.store(vacant, at, 4);
                    let count = fx.load(PTR, address, POOL_COUNT);
                    let count = fx.b.ins().iadd_imm_s(count, -1);
                    fx.store(count, address, POOL_COUNT);
                    let held = lo.held_at(fx, &element, result);
                    supply.ret(lo, fx, held);
                }
            }
        }
        "cursor" => {
            let sret = supply.sret.expect("a cursor's address");
            fx.store(supply.param(0).address(), sret, 0);
            let zero = fx.ptr(0);
            fx.store(zero, sret, 8);
            supply.done(lo, fx);
        }
        "boxed" | "try_boxed" => {
            let cell = fx.load(PTR, supply.param(0).address(), vector::ALLOC);
            boxed(lo, fx, supply, cell);
        }
        member => panic!("no lowering of mem::Pool.{member}"),
    }
}

/// The index of the earliest slot not occupied, or the length when every slot is.
fn first_free(fx: &mut Fx, data: Value, len: Value, slot: Element) -> Value {
    let zero = fx.ptr(0);
    fx.repeat(&[zero], |fx, state| {
        let index = state[0];
        let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, index, len);
        let next = fx.choose(
            inside,
            &[PTR, types::I8],
            |fx| {
                let at = slot.at(fx, data, index);
                let occupied = fx.load(types::I32, at, 4);
                let occupied = fx.b.ins().icmp_imm_s(IntCC::NotEqual, occupied, 0);
                let next = fx.b.ins().iadd_imm_s(index, 1);
                vec![fx.b.ins().select(occupied, next, index), occupied]
            },
            |fx| vec![index, fx.int(types::I8, 0)],
        );
        (next[1], vec![next[0]])
    })[0]
}

/// `PoolCursor.next`: the pool's address and the next index; it skips slots not occupied.
fn pool_next(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let element = supply.type_arg(0);
    let (slot, value_at) = slot_shape(lo, &element);
    let cursor = supply.param(0).address();
    let pool = fx.load(PTR, cursor, 0);
    let data = fx.load(PTR, pool, vector::DATA);
    let len = fx.load(PTR, pool, vector::LEN);
    let start = fx.load(PTR, cursor, 8);
    let found = fx.repeat(&[start], |fx, state| {
        let index = state[0];
        let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, index, len);
        let next = fx.choose(
            inside,
            &[PTR, types::I8],
            |fx| {
                let at = slot.at(fx, data, index);
                let occupied = fx.load(types::I32, at, 4);
                let occupied = fx.b.ins().icmp_imm_s(IntCC::NotEqual, occupied, 0);
                let next = fx.b.ins().iadd_imm_s(index, 1);
                vec![
                    fx.b.ins().select(occupied, index, next),
                    fx.b.ins().bxor_imm_s(occupied, 1),
                ]
            },
            |fx| vec![index, fx.int(types::I8, 0)],
        );
        (next[1], vec![next[0]])
    })[0];
    let present = fx.b.ins().icmp(IntCC::UnsignedLessThan, found, len);
    let after = fx.b.ins().iadd_imm_s(found, 1);
    let position = fx.b.ins().select(present, after, len);
    fx.store(position, cursor, 8);
    let at = slot.at(fx, data, found);
    let stored = fx.offset(at, value_at);
    projected(lo, fx, supply, &element, present, stored);
}

/// Releases a pool at an address: its occupied values, then its storage.
pub fn release_pool(lo: &mut Lowering, fx: &mut Fx, element: &Ty, address: Value) {
    let (slot, value_at) = slot_shape(lo, element);
    if lo.needs_release(element) {
        let data = fx.load(PTR, address, vector::DATA);
        let len = fx.load(PTR, address, vector::LEN);
        let zero = fx.ptr(0);
        fx.each(zero, len, |fx, index| {
            let at = slot.at(fx, data, index);
            let occupied = fx.load(types::I32, at, 4);
            let occupied = fx.b.ins().icmp_imm_s(IntCC::NotEqual, occupied, 0);
            fx.when(occupied, |fx| {
                let stored = fx.offset(at, value_at);
                lo.release(fx, element, stored);
            });
        });
    }
    vector::free_storage(lo, fx, address);
}

/// A member of `mem::OnceMap[K, V]`: key-value entries in segments, found by `eq`.
fn once_map(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let (key, value) = (supply.type_arg(0), supply.type_arg(1));
    let entries = Entries::of_map(lo, key.clone(), value.clone());
    let value_at = lo
        .layouts
        .field(lo.ck, &entries.entry, None, "value")
        .offset;
    match supply.member() {
        "empty" => {
            let cell = handle(fx, supply.param(1));
            let header = segments::header(lo, fx, cell);
            let sret = supply.sret.expect("a once map's address");
            fx.store(header, sret, 0);
            supply.done(lo, fx);
        }
        "len" => {
            let header = fx.load(PTR, supply.param(0).address(), 0);
            let len = fx.load(PTR, header, segments::LEN);
            supply.ret(lo, fx, Held::Scalar(len));
        }
        "get" | "insert" | "try_insert" => {
            let header = fx.load(PTR, supply.param(0).address(), 0);
            let key_held = supply.param(1);
            let sought = match key_held {
                Held::Address(address) => address,
                held => {
                    let layout = lo.layout(&key);
                    let slot = fx.slot(layout.size, layout.align);
                    lo.store_held(fx, slot, held, &key);
                    slot
                }
            };
            let zero = fx.ptr(0);
            let (index, len) = scan_from(lo, fx, header, &entries, sought, zero);
            let found = fx.b.ins().icmp(IntCC::UnsignedLessThan, index, len);
            if supply.member() == "get" {
                let address = fx.choose(
                    found,
                    &[PTR],
                    |fx| {
                        let at = segments::at(fx, header, index, entries.shape);
                        vec![fx.offset(at, value_at)]
                    },
                    |fx| vec![fx.ptr(0)],
                )[0];
                projected(lo, fx, supply, &value, found, address);
                return;
            }
            let value_held = supply.param(2);
            let lock = fx.offset(header, segments::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let (rest, len) = scan_from(lo, fx, header, &entries, sought, len);
            let added = fx.b.ins().icmp(IntCC::UnsignedLessThan, rest, len);
            let index = fx.b.ins().select(found, index, rest);
            let found = fx.b.ins().bor(found, added);
            let address = fx.choose_in(
                lo,
                found,
                &[PTR],
                |lo, fx| {
                    lo.call_routine(fx, Routine::Unlock, &[lock]);
                    lo.release(fx, &key, sought);
                    if let Held::Address(taken) = value_held {
                        lo.release(fx, &value, taken);
                    }
                    let at = segments::at(fx, header, index, entries.shape);
                    vec![fx.offset(at, value_at)]
                },
                |lo, fx| {
                    let room = segments::reserve_one(lo, fx, header, entries.shape);
                    let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, room, 0);
                    let taken = [(key_held, key.clone()), (value_held, value.clone())];
                    fx.when(failed, |fx| {
                        lo.call_routine(fx, Routine::Unlock, &[lock]);
                        supply.exhausted(lo, fx, &taken);
                    });
                    let at = segments::at(fx, header, len, entries.shape);
                    let key_slot = fx.offset(at, entries.key_at);
                    lo.store_held(fx, key_slot, key_held, &key);
                    let value_slot = fx.offset(at, value_at);
                    lo.store_held(fx, value_slot, value_held, &value);
                    let next = fx.b.ins().iadd_imm_s(len, 1);
                    fx.store(next, header, segments::LEN);
                    lo.call_routine(fx, Routine::Unlock, &[lock]);
                    vec![value_slot]
                },
            )[0];
            supply.ret(lo, fx, Held::Address(address));
        }
        member => panic!("no lowering of mem::OnceMap.{member}"),
    }
}

/// The index of the entry of segmented storage from `start` whose key equals the one at
/// `sought`, or the length, and that length.
fn scan_from(
    lo: &mut Lowering,
    fx: &mut Fx,
    header: Value,
    entries: &Entries,
    sought: Value,
    start: Value,
) -> (Value, Value) {
    let len = fx.load(PTR, header, segments::LEN);
    let index = fx.repeat(&[start], |fx, state| {
        let index = state[0];
        let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, index, len);
        let next = fx.choose(
            inside,
            &[PTR, types::I8],
            |fx| {
                let at = segments::at(fx, header, index, entries.shape);
                let stored = fx.offset(at, entries.key_at);
                let equal = lo.equal(fx, &entries.key, stored, sought);
                let next = fx.b.ins().iadd_imm_s(index, 1);
                vec![
                    fx.b.ins().select(equal, index, next),
                    fx.b.ins().bxor_imm_s(equal, 1),
                ]
            },
            |fx| vec![index, fx.int(types::I8, 0)],
        );
        (next[1], vec![next[0]])
    })[0];
    (index, len)
}
