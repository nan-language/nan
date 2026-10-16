//! Text: owned strings, inline fixed texts, their writers, and interners.
//!
//! A string is a vector of bytes; a fixed text is its length and its bytes inline. An interner
//! is the address of its record, drawn through its allocator: its lock, its allocator handle,
//! its count of texts, the address and capacity of its table of texts, and the address and size
//! of its index. Each interned text is a copy drawn through the allocator, which never moves;
//! the table holds each copy's address and length by symbol index, and each slot of the index,
//! a power of two of them probed linearly from a text's hash, holds a symbol index plus one,
//! zero when empty.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::Lowering;
use super::super::body::Held;
use super::super::data::const_arg;
use super::super::fx::{Fx, PTR};
use super::super::runtime::{Routine, report};
use super::super::transfer;
use super::Supply;
use super::mem::lists;
use super::mem::vector::{self, Element};
use super::render;

mod interner {
    pub const LOCK: u64 = 0;
    pub const ALLOC: u64 = 8;
    pub const COUNT: u64 = 16;
    pub const TEXTS: u64 = 24;
    pub const CAPACITY: u64 = 32;
    pub const SLOTS: u64 = 40;
    pub const SLOT_COUNT: u64 = 48;
    pub const SIZE: u64 = 56;
}

/// The bytes of an entry of an interner's table: a copy's address and length.
const TEXT_ENTRY: u64 = 16;

/// The most texts an interner holds: one per `u32` index.
const MOST_TEXTS: i64 = 1 << 32;

const BYTE: Element = Element { size: 1, align: 1 };

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    let owner = supply.path.rsplit_once('.').map_or("", |(owner, _)| owner);
    match owner {
        "text::String" => string(lo, fx, supply),
        "text::Fixed" => fixed(lo, fx, supply),
        "text::Interner" => interner(lo, fx, supply),
        _ => return false,
    }
    true
}

/// Whether releasing a text type does anything, none for a type std declares in full.
pub fn needs_release(path: &str) -> Option<bool> {
    match path {
        "text::String" | "text::Interner" => Some(true),
        "text::Fixed" => Some(false),
        _ => None,
    }
}

/// Whether moving a text type into another domain's reach does anything, none for a type std
/// declares in full.
pub fn transfers(path: &str) -> Option<bool> {
    match path {
        "text::String" | "text::Interner" => Some(true),
        _ => None,
    }
}

/// Moves a value of a text type at an address into another domain's reach; false for a type
/// std declares in full.
pub fn transfer(lo: &mut Lowering, fx: &mut Fx, path: &str, address: Value, lift: Value) -> bool {
    match path {
        "text::String" => {
            let none = fx.ptr(0);
            vector::transfer(lo, fx, address, &lists::byte(), None, lift, none);
        }
        "text::Interner" => {
            let record = fx.load(PTR, address, 0);
            transfer::lift(lo, fx, record, lift);
            let count = fx.load(PTR, record, interner::COUNT);
            let zero = fx.ptr(0);
            fx.each(zero, count, |fx, index| {
                let entry = text_entry(fx, record, index);
                let text = fx.load(PTR, entry, 0);
                transfer::lift(lo, fx, text, lift);
            });
            for (field, size) in [
                (interner::TEXTS, interner::CAPACITY),
                (interner::SLOTS, interner::SLOT_COUNT),
            ] {
                let held = fx.load(PTR, record, size);
                let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, held, 0);
                fx.when(present, |fx| {
                    let storage = fx.load(PTR, record, field);
                    transfer::lift(lo, fx, storage, lift);
                });
            }
        }
        _ => return false,
    }
    true
}

/// Releases a value of a text type at an address; false for a type std declares in full.
pub fn release(lo: &mut Lowering, fx: &mut Fx, path: &str, address: Value) -> bool {
    match path {
        "text::String" => vector::free_storage(lo, fx, address),
        "text::Interner" => release_interner(lo, fx, address),
        _ => return false,
    }
    true
}

fn string(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let address = supply.param(0).address();
    match supply.member() {
        "empty" => {
            let sret = supply.sret.expect("a string's address");
            let alloc = fx.load(PTR, address, 0);
            vector::empty(fx, sret, alloc, BYTE);
        }
        "as_str" => {
            let sret = supply.sret.expect("a text's address");
            vector::write_view(fx, sret, address);
        }
        "push" | "try_push" => {
            let buffer = fx.slot(4, 1);
            let len = render::encode_utf8(fx, supply.param(1).scalar(), buffer);
            lists::append(lo, fx, supply, address, buffer, len);
        }
        "append" | "try_append" => {
            let text = supply.param(1).address();
            let start = fx.load(PTR, text, 0);
            let len = fx.load(PTR, text, 8);
            lists::append(lo, fx, supply, address, start, len);
        }
        "clear" => {
            let zero = fx.ptr(0);
            fx.store(zero, address, vector::LEN);
        }
        "into_bytes" => {
            let sret = supply.sret.expect("a bytes' address");
            lo.copy(fx, sret, address, 32, 8);
        }
        "write" | "try_write" => string_write(lo, fx, supply),
        member => panic!("no lowering of text::String.{member}"),
    }
    supply.done(lo, fx);
}

/// `String.write(value, budget)` and `try_write`: renders once to measure the output, then
/// again into storage drawn for exactly that output.
fn string_write(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let address = supply.param(0).address();
    let value = supply.param(1).address();
    let budget = supply.param(2).scalar();
    let none = fx.ptr(0);
    let measure = render::new_renderer(fx, none, budget);
    render::render_dyn(fx, value, measure);
    let written = render::written(fx, measure);
    let elided = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, written, budget);
    let three = fx.ptr(3);
    let dots = fx.b.ins().umin(budget, three);
    let zero = fx.ptr(0);
    let marker = fx.b.ins().select(elided, dots, zero);
    let room = fx.b.ins().isub(budget, dots);
    let end = fx.choose_in(
        lo,
        elided,
        &[PTR],
        |_, fx| vec![elided_end(fx, value, room)],
        |_, _| vec![written],
    )[0];
    let output = fx.b.ins().iadd(end, marker);
    let len = fx.load(PTR, address, vector::LEN);
    let (needed, overflow) = fx.b.ins().uadd_overflow(len, output);
    let drawn = fx.choose_in(
        lo,
        overflow,
        &[types::I8],
        |_, fx| vec![fx.int(types::I8, 0)],
        |lo, fx| vec![vector::reserve(lo, fx, address, needed, BYTE)],
    )[0];
    let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, drawn, 0);
    fx.when(failed, |fx| supply.exhausted(lo, fx, &[]));
    let data = fx.load(PTR, address, vector::DATA);
    let target = fx.b.ins().iadd(data, len);
    let keep = render::new_renderer(fx, target, end);
    render::render_dyn(fx, value, keep);
    let marked = fx.b.ins().icmp_imm_s(IntCC::NotEqual, marker, 0);
    fx.when(marked, |fx| {
        let dots = lo.view_literal(fx, b"...").address();
        let dots = fx.load(PTR, dots, 0);
        let at = fx.b.ins().iadd(target, end);
        lo.call_routine(fx, Routine::Copy, &[at, dots, marker]);
    });
    fx.store(needed, address, vector::LEN);
}

/// The length of the longest prefix of an elided rendering of a `Dyn[Render]` value that ends
/// at a char boundary within `room` bytes, read from the bytes the rendering holds around
/// `room`.
fn elided_end(fx: &mut Fx, value: Value, room: Value) -> Value {
    let three = fx.ptr(3);
    let back = fx.b.ins().umin(room, three);
    let skip = fx.b.ins().isub(room, back);
    let window = fx.slot(4, 1);
    let four = fx.ptr(4);
    let renderer = render::window_renderer(fx, window, four, skip);
    render::render_dyn(fx, value, renderer);
    fx.repeat(&[room], |fx, state| {
        let end = state[0];
        let inside = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, end, skip);
        let continuation = fx.choose(
            inside,
            &[types::I8],
            |fx| {
                let offset = fx.b.ins().isub(end, skip);
                let at = fx.b.ins().iadd(window, offset);
                let byte = fx.load(types::I8, at, 0);
                let high = fx.b.ins().band_imm_s(byte, 0xC0);
                vec![fx.b.ins().icmp_imm_s(IntCC::Equal, high, 0x80)]
            },
            |fx| vec![fx.int(types::I8, 0)],
        )[0];
        let earlier = fx.b.ins().iadd_imm_s(end, -1);
        let next = fx.b.ins().select(continuation, earlier, end);
        (continuation, vec![next])
    })[0]
}

fn fixed(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let fixed_ty = supply.owner.clone().expect("a fixed text type");
    let capacity = const_arg(&fixed_ty.args()[0]) as u64;
    let len_at = lo.layouts.field(lo.ck, &fixed_ty, None, "len").offset;
    let bytes_at = lo.layouts.field(lo.ck, &fixed_ty, None, "bytes").offset;
    if supply.member() == "empty" {
        let sret = supply.sret.expect("a fixed text's address");
        let zero = fx.ptr(0);
        fx.store(zero, sret, len_at);
        supply.done(lo, fx);
        return;
    }
    let address = supply.param(0).address();
    let len = fx.load(PTR, address, len_at);
    let bytes = fx.offset(address, bytes_at);
    let end = fx.b.ins().iadd(bytes, len);
    match supply.member() {
        "as_str" => {
            let sret = supply.sret.expect("a text's address");
            fx.store(bytes, sret, 0);
            fx.store(len, sret, 8);
        }
        "push" => {
            let added = render::encode_utf8(fx, supply.param(1).scalar(), end);
            let len = fx.b.ins().iadd(len, added);
            fx.store(len, address, len_at);
        }
        "append" => {
            let text = supply.param(1).address();
            let start = fx.load(PTR, text, 0);
            let added = fx.load(PTR, text, 8);
            lo.call_routine(fx, Routine::Copy, &[end, start, added]);
            let len = fx.b.ins().iadd(len, added);
            fx.store(len, address, len_at);
        }
        "clear" => {
            let zero = fx.ptr(0);
            fx.store(zero, address, len_at);
        }
        "write" | "try_write" => {
            let value = supply.param(1).address();
            let budget = supply.param(2).scalar();
            let full = fx.ptr(capacity);
            let room = fx.b.ins().isub(full, len);
            let budget = fx.b.ins().umin(budget, room);
            let renderer = render::new_renderer(fx, end, budget);
            render::render_dyn(fx, value, renderer);
            let added = render::finish(lo, fx, renderer);
            let len = fx.b.ins().iadd(len, added);
            fx.store(len, address, len_at);
        }
        member => panic!("no lowering of text::Fixed.{member}"),
    }
    supply.done(lo, fx);
}

fn interner(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    if supply.member() == "empty" {
        let alloc = fx.load(PTR, supply.param(0).address(), 0);
        let size = fx.ptr(interner::SIZE);
        let align = fx.ptr(8);
        let record = lo.call_routine(fx, Routine::Alloc, &[alloc, size, align])[0];
        let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
        fx.when(missing, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
        let zero = fx.ptr(0);
        lo.call_routine(fx, Routine::Fill, &[record, zero, size]);
        fx.store(alloc, record, interner::ALLOC);
        let sret = supply.sret.expect("an interner's address");
        fx.store(record, sret, 0);
        supply.done(lo, fx);
        return;
    }
    let record = fx.load(PTR, supply.param(0).address(), 0);
    let lock = fx.offset(record, interner::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    match supply.member() {
        "len" => {
            let count = fx.load(PTR, record, interner::COUNT);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            supply.ret(lo, fx, Held::Scalar(count));
        }
        "intern" | "try_intern" | "lookup" => {
            let text = supply.param(1).address();
            let start = fx.load(PTR, text, 0);
            let len = fx.load(PTR, text, 8);
            let hash = hash_text(fx, start, len);
            let found = find(lo, fx, record, hash, start, len);
            if supply.member() == "lookup" {
                lo.call_routine(fx, Routine::Unlock, &[lock]);
                let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, found, 0);
                let index = fx.b.ins().iadd_imm_s(found, -1);
                let option = supply.abi.result.clone();
                let layout = lo.layout(&option);
                let result = supply
                    .sret
                    .unwrap_or_else(|| fx.slot(layout.size, layout.align));
                lo.write_option_with(fx, &option, result, present, |_, fx, payload| {
                    let index = fx.b.ins().ireduce(types::I32, index);
                    fx.store(index, payload, 0);
                });
                let held = lo.held_at(fx, &option, result);
                supply.ret(lo, fx, held);
                return;
            }
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, found, 0);
            fx.when(present, |fx| {
                lo.call_routine(fx, Routine::Unlock, &[lock]);
                let index = fx.b.ins().iadd_imm_s(found, -1);
                return_symbol(lo, fx, supply, index);
            });
            let index = insert(lo, fx, supply, record, hash, start, len);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            return_symbol(lo, fx, supply, index);
        }
        "resolve" => {
            let symbol = supply.param(1);
            let index = match symbol {
                Held::Scalar(index) => index,
                held => fx.load(types::I32, held.address(), 0),
            };
            let index = fx.b.ins().uextend(PTR, index);
            let count = fx.load(PTR, record, interner::COUNT);
            let outside =
                fx.b.ins()
                    .icmp(IntCC::UnsignedGreaterThanOrEqual, index, count);
            fx.when(outside, |fx| {
                lo.call_routine(fx, Routine::Unlock, &[lock]);
                report::raise_kind(lo, fx, "trap.bounds");
            });
            let entry = text_entry(fx, record, index);
            let start = fx.load(PTR, entry, 0);
            let len = fx.load(PTR, entry, 8);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            let sret = supply.sret.expect("a text's address");
            fx.store(start, sret, 0);
            fx.store(len, sret, 8);
            supply.done(lo, fx);
        }
        member => panic!("no lowering of text::Interner.{member}"),
    }
}

/// Returns the symbol of an index.
fn return_symbol(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, index: Value) {
    let symbol = supply.abi.result.clone();
    let layout = lo.layout(&symbol);
    let slot = fx.slot(layout.size, layout.align);
    let index = fx.b.ins().ireduce(types::I32, index);
    fx.store(index, slot, 0);
    let held = lo.held_at(fx, &symbol, slot);
    supply.ret(lo, fx, held);
}

/// The FNV-1a hash of bytes.
fn hash_text(fx: &mut Fx, start: Value, len: Value) -> Value {
    let basis = fx.int(PTR, 0xcbf2_9ce4_8422_2325_u64 as i64);
    let zero = fx.ptr(0);
    fx.repeat(&[zero, basis], |fx, state| {
        let (at, hash) = (state[0], state[1]);
        let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, at, len);
        let next = fx.choose(
            inside,
            &[PTR, PTR],
            |fx| {
                let address = fx.b.ins().iadd(start, at);
                let byte = fx.load(types::I8, address, 0);
                let byte = fx.b.ins().uextend(PTR, byte);
                let mixed = fx.b.ins().bxor(hash, byte);
                let mixed = fx.b.ins().imul_imm_s(mixed, 0x0100_0000_01b3);
                vec![fx.b.ins().iadd_imm_s(at, 1), mixed]
            },
            |_| vec![at, hash],
        );
        (inside, next)
    })[1]
}

/// The address of the table entry of a symbol index.
fn text_entry(fx: &mut Fx, record: Value, index: Value) -> Value {
    let texts = fx.load(PTR, record, interner::TEXTS);
    let offset = fx.b.ins().imul_imm_s(index, TEXT_ENTRY as i64);
    fx.b.ins().iadd(texts, offset)
}

/// The symbol index plus one of an interned text equal to the given bytes, zero when none is.
fn find(
    lo: &mut Lowering,
    fx: &mut Fx,
    record: Value,
    hash: Value,
    start: Value,
    len: Value,
) -> Value {
    let slots = fx.load(PTR, record, interner::SLOTS);
    let slot_count = fx.load(PTR, record, interner::SLOT_COUNT);
    let indexed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, slot_count, 0);
    fx.choose_in(
        lo,
        indexed,
        &[PTR],
        |lo, fx| {
            let mask = fx.b.ins().iadd_imm_s(slot_count, -1);
            let first = fx.b.ins().band(hash, mask);
            let found = fx.repeat(&[first, first], |fx, state| {
                let position = state[0];
                let offset = fx.b.ins().imul_imm_s(position, 4);
                let at = fx.b.ins().iadd(slots, offset);
                let held = fx.load(types::I32, at, 0);
                let held = fx.b.ins().uextend(PTR, held);
                let empty = fx.b.ins().icmp_imm_s(IntCC::Equal, held, 0);
                let same = fx.choose_in(
                    lo,
                    empty,
                    &[types::I8],
                    |_, fx| vec![fx.int(types::I8, 0)],
                    |lo, fx| {
                        let index = fx.b.ins().iadd_imm_s(held, -1);
                        let entry = text_entry(fx, record, index);
                        let other = fx.load(PTR, entry, 0);
                        let other_len = fx.load(PTR, entry, 8);
                        let same_len = fx.b.ins().icmp(IntCC::Equal, other_len, len);
                        fx.choose_in(
                            lo,
                            same_len,
                            &[types::I8],
                            |lo, fx| {
                                let order =
                                    lo.call_routine(fx, Routine::Compare, &[other, start, len]);
                                vec![fx.b.ins().icmp_imm_s(IntCC::Equal, order[0], 0)]
                            },
                            |_, fx| vec![fx.int(types::I8, 0)],
                        )
                    },
                )[0];
                let stop = fx.b.ins().bor(empty, same);
                let next = fx.b.ins().iadd_imm_s(position, 1);
                let next = fx.b.ins().band(next, mask);
                let zero = fx.ptr(0);
                let result = fx.b.ins().select(same, held, zero);
                (fx.b.ins().bxor_imm_s(stop, 1), vec![next, result])
            });
            vec![found[1]]
        },
        |_, fx| vec![fx.ptr(0)],
    )[0]
}

/// Puts a symbol index plus one in the first empty slot from a hash of an index with room.
fn place(fx: &mut Fx, slots: Value, slot_count: Value, hash: Value, held: Value) {
    let mask = fx.b.ins().iadd_imm_s(slot_count, -1);
    let first = fx.b.ins().band(hash, mask);
    fx.repeat(&[first], |fx, state| {
        let position = state[0];
        let offset = fx.b.ins().imul_imm_s(position, 4);
        let at = fx.b.ins().iadd(slots, offset);
        let current = fx.load(types::I32, at, 0);
        let empty = fx.b.ins().icmp_imm_s(IntCC::Equal, current, 0);
        fx.when(empty, |fx| {
            let held = fx.b.ins().ireduce(types::I32, held);
            fx.store(held, at, 0);
        });
        let next = fx.b.ins().iadd_imm_s(position, 1);
        let next = fx.b.ins().band(next, mask);
        (fx.b.ins().bxor_imm_s(empty, 1), vec![next])
    });
}

/// Interns a text absent from an interner, whose lock the caller holds: draws its copy and any
/// larger table and index first, so that exhaustion, which unlocks and traps or fails, changes
/// nothing. Gives the new symbol index.
#[allow(clippy::too_many_arguments)]
fn insert(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    record: Value,
    hash: Value,
    start: Value,
    len: Value,
) -> Value {
    let lock = fx.offset(record, interner::LOCK);
    let alloc = fx.load(PTR, record, interner::ALLOC);
    let count = fx.load(PTR, record, interner::COUNT);
    let capacity = fx.load(PTR, record, interner::CAPACITY);
    let slot_count = fx.load(PTR, record, interner::SLOT_COUNT);
    let exhausted = |lo: &mut Lowering, fx: &mut Fx, drawn: &[Value]| {
        for address in drawn {
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, *address, 0);
            fx.when(present, |fx| {
                lo.call_routine(fx, Routine::Free, &[*address]);
            });
        }
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        supply.exhausted(lo, fx, &[]);
    };
    let full = fx.b.ins().icmp_imm_s(IntCC::Equal, count, MOST_TEXTS);
    fx.when(full, |fx| exhausted(lo, fx, &[]));
    let one = fx.ptr(1);
    let copy_size = fx.b.ins().umax(len, one);
    let copy = lo.call_routine(fx, Routine::Alloc, &[alloc, copy_size, one])[0];
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, copy, 0);
    fx.when(missing, |fx| exhausted(lo, fx, &[]));
    let grows = fx.b.ins().icmp(IntCC::Equal, count, capacity);
    let doubled = fx.b.ins().ishl_imm_s(capacity, 1);
    let eight = fx.ptr(8);
    let wanted = fx.b.ins().umax(doubled, eight);
    let table = fx.choose_in(
        lo,
        grows,
        &[PTR],
        |lo, fx| {
            let bytes = fx.b.ins().imul_imm_s(wanted, TEXT_ENTRY as i64);
            let align = fx.ptr(8);
            let table = lo.call_routine(fx, Routine::Alloc, &[alloc, bytes, align])[0];
            let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, table, 0);
            fx.when(missing, |fx| exhausted(lo, fx, &[copy]));
            vec![table]
        },
        |_, fx| vec![fx.ptr(0)],
    )[0];
    let after = fx.b.ins().iadd_imm_s(count, 1);
    let loaded = fx.b.ins().ishl_imm_s(after, 1);
    let rehash =
        fx.b.ins()
            .icmp(IntCC::UnsignedGreaterThan, loaded, slot_count);
    let doubled_slots = fx.b.ins().ishl_imm_s(slot_count, 1);
    let sixteen = fx.ptr(16);
    let new_slot_count = fx.b.ins().umax(doubled_slots, sixteen);
    let index = fx.choose_in(
        lo,
        rehash,
        &[PTR],
        |lo, fx| {
            let bytes = fx.b.ins().imul_imm_s(new_slot_count, 4);
            let align = fx.ptr(4);
            let index = lo.call_routine(fx, Routine::Alloc, &[alloc, bytes, align])[0];
            let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, index, 0);
            fx.when(missing, |fx| exhausted(lo, fx, &[copy, table]));
            let zero = fx.ptr(0);
            lo.call_routine(fx, Routine::Fill, &[index, zero, bytes]);
            vec![index]
        },
        |_, fx| vec![fx.ptr(0)],
    )[0];
    fx.when(grows, |fx| {
        let old = fx.load(PTR, record, interner::TEXTS);
        let bytes = fx.b.ins().imul_imm_s(count, TEXT_ENTRY as i64);
        lo.call_routine(fx, Routine::Copy, &[table, old, bytes]);
        let had = fx.b.ins().icmp_imm_s(IntCC::NotEqual, capacity, 0);
        fx.when(had, |fx| {
            lo.call_routine(fx, Routine::Free, &[old]);
        });
        fx.store(table, record, interner::TEXTS);
        fx.store(wanted, record, interner::CAPACITY);
    });
    lo.call_routine(fx, Routine::Copy, &[copy, start, len]);
    let entry = text_entry(fx, record, count);
    fx.store(copy, entry, 0);
    fx.store(len, entry, 8);
    fx.when(rehash, |fx| {
        let old = fx.load(PTR, record, interner::SLOTS);
        let zero = fx.ptr(0);
        fx.each(zero, count, |fx, existing| {
            let entry = text_entry(fx, record, existing);
            let text = fx.load(PTR, entry, 0);
            let text_len = fx.load(PTR, entry, 8);
            let hash = hash_text(fx, text, text_len);
            let held = fx.b.ins().iadd_imm_s(existing, 1);
            place(fx, index, new_slot_count, hash, held);
        });
        let had = fx.b.ins().icmp_imm_s(IntCC::NotEqual, slot_count, 0);
        fx.when(had, |fx| {
            lo.call_routine(fx, Routine::Free, &[old]);
        });
        fx.store(index, record, interner::SLOTS);
        fx.store(new_slot_count, record, interner::SLOT_COUNT);
    });
    let slots = fx.load(PTR, record, interner::SLOTS);
    let slot_count = fx.load(PTR, record, interner::SLOT_COUNT);
    place(fx, slots, slot_count, hash, after);
    fx.store(after, record, interner::COUNT);
    count
}

/// Releases an interner: its texts, table, index and record.
fn release_interner(lo: &mut Lowering, fx: &mut Fx, address: Value) {
    let record = fx.load(PTR, address, 0);
    let count = fx.load(PTR, record, interner::COUNT);
    let zero = fx.ptr(0);
    fx.each(zero, count, |fx, index| {
        let entry = text_entry(fx, record, index);
        let text = fx.load(PTR, entry, 0);
        lo.call_routine(fx, Routine::Free, &[text]);
    });
    for (field, size) in [
        (interner::TEXTS, interner::CAPACITY),
        (interner::SLOTS, interner::SLOT_COUNT),
    ] {
        let held = fx.load(PTR, record, size);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, held, 0);
        fx.when(present, |fx| {
            let storage = fx.load(PTR, record, field);
            lo.call_routine(fx, Routine::Free, &[storage]);
        });
    }
    lo.call_routine(fx, Routine::Free, &[record]);
}
