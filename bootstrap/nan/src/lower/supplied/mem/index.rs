//! The hash index by which `mem::HashMap` and `mem::OnceMap` find their keys: an open-addressing
//! table of 64-bit slots probed linearly, each 0 when vacant or a key's 32-bit hash fragment
//! above the index of its entry plus 1. A key's fragment folds the state of a `mem::KeyHasher`
//! its `hash` writes to, started from the FNV basis and the map's seed. A table record is two
//! words, the table's address and its slot count, both 0 before the first entry; the table, drawn
//! through the map's allocator, has at least twice as many slots as entries.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::super::Lowering;
use super::super::super::body::Held;
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::Routine;
use super::super::super::transfer;
use crate::check::ty::{Arg, Ty};

const TABLE: u64 = 0;
const SLOTS: u64 = 8;

/// The slot count of a first table.
const FIRST: u64 = 8;

const FNV_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

/// The hash fragment of the key of type `key` at an address, under a map's seed.
pub fn fragment(lo: &mut Lowering, fx: &mut Fx, key: &Ty, at: Value, seed: Value) -> Value {
    let hasher = lo.std_ty("mem", "KeyHasher", Vec::new());
    let state = fx.slot(8, 8);
    let basis = fx.b.ins().iconst(types::I64, FNV_BASIS as i64);
    let start = fx.b.ins().bxor(basis, seed);
    fx.store(start, state, 0);
    let held = lo.held_at(fx, key, at);
    let hasher_arg = vec![Arg::Ty(hasher)];
    lo.call_method(fx, key, "Hash", hasher_arg, &[held, Held::Address(state)]);
    let hashed = fx.load(types::I64, state, 0);
    let high = fx.b.ins().ushr_imm_s(hashed, 32);
    let folded = fx.b.ins().bxor(hashed, high);
    fx.b.ins().band_imm_s(folded, 0xFFFF_FFFF)
}

/// The entry of a key's fragment that `matches` accepts, `matches` telling whether the entry of
/// an index holds the sought key: its index and 1, or an unspecified index and 0.
pub fn find(
    lo: &mut Lowering,
    fx: &mut Fx,
    record: Value,
    fragment: Value,
    mut matches: impl FnMut(&mut Lowering, &mut Fx, Value) -> Value,
) -> (Value, Value) {
    let table = fx.load(PTR, record, TABLE);
    let slots = fx.load(PTR, record, SLOTS);
    let none = fx.b.ins().icmp_imm_s(IntCC::Equal, slots, 0);
    let found = fx.choose_in(
        lo,
        none,
        &[PTR, types::I8],
        |_, fx| vec![fx.ptr(0), fx.int(types::I8, 0)],
        |lo, fx| {
            let mask = fx.b.ins().iadd_imm_s(slots, -1);
            let start = fx.b.ins().band(fragment, mask);
            let (zero, no) = (fx.ptr(0), fx.int(types::I8, 0));
            let state = fx.repeat(&[start, zero, no], |fx, state| {
                let slot = slot_at(fx, table, state[0]);
                let vacant = fx.b.ins().icmp_imm_s(IntCC::Equal, slot, 0);
                let high = fx.b.ins().ushr_imm_s(slot, 32);
                let same = fx.b.ins().icmp(IntCC::Equal, high, fragment);
                let held = fx.b.ins().bxor_imm_s(vacant, 1);
                let candidate = fx.b.ins().band(same, held);
                let low = fx.b.ins().band_imm_s(slot, 0xFFFF_FFFF);
                let index = fx.b.ins().iadd_imm_s(low, -1);
                let hit = fx.choose(
                    candidate,
                    &[types::I8],
                    |fx| vec![matches(lo, fx, index)],
                    |fx| vec![fx.int(types::I8, 0)],
                )[0];
                let next = fx.b.ins().iadd_imm_s(state[0], 1);
                let next = fx.b.ins().band(next, mask);
                let stop = fx.b.ins().bor(vacant, hit);
                let again = fx.b.ins().bxor_imm_s(stop, 1);
                (again, vec![next, index, hit])
            });
            vec![state[1], state[2]]
        },
    );
    (found[0], found[1])
}

/// Makes room for `needed` entries, drawing a table twice as large as needed through an
/// allocator and moving the slots into it: nonzero when there is room, zero on exhaustion,
/// which leaves the table unchanged.
pub fn reserve(
    lo: &mut Lowering,
    fx: &mut Fx,
    record: Value,
    alloc: Value,
    needed: Value,
) -> Value {
    let slots = fx.load(PTR, record, SLOTS);
    let wanted = fx.b.ins().ishl_imm_s(needed, 1);
    let enough =
        fx.b.ins()
            .icmp(IntCC::UnsignedLessThanOrEqual, wanted, slots);
    fx.choose_in(
        lo,
        enough,
        &[types::I8],
        |_, fx| vec![fx.int(types::I8, 1)],
        |lo, fx| {
            let first = fx.ptr(FIRST);
            let base = fx.b.ins().umax(slots, first);
            let size = fx.repeat(&[base], |fx, state| {
                let short = fx.b.ins().icmp(IntCC::UnsignedLessThan, state[0], wanted);
                let doubled = fx.b.ins().ishl_imm_s(state[0], 1);
                let next = fx.b.ins().select(short, doubled, state[0]);
                (short, vec![next])
            })[0];
            let bytes = fx.b.ins().ishl_imm_s(size, 3);
            let align = fx.ptr(8);
            let fresh = lo.call_routine(fx, Routine::Alloc, &[alloc, bytes, align])[0];
            let drawn = fx.b.ins().icmp_imm_s(IntCC::NotEqual, fresh, 0);
            fx.when(drawn, |fx| {
                let zero = fx.ptr(0);
                lo.call_routine(fx, Routine::Fill, &[fresh, zero, bytes]);
                let table = fx.load(PTR, record, TABLE);
                let mask = fx.b.ins().iadd_imm_s(size, -1);
                let zero = fx.ptr(0);
                fx.each(zero, slots, |fx, position| {
                    let slot = slot_at(fx, table, position);
                    let held = fx.b.ins().icmp_imm_s(IntCC::NotEqual, slot, 0);
                    fx.when(held, |fx| {
                        let fragment = fx.b.ins().ushr_imm_s(slot, 32);
                        place(fx, fresh, mask, fragment, slot);
                    });
                });
                free(lo, fx, record);
                fx.store(fresh, record, TABLE);
                fx.store(size, record, SLOTS);
            });
            vec![drawn]
        },
    )[0]
}

/// Records entry `index` under its fragment, in a table with room for it.
pub fn insert(fx: &mut Fx, record: Value, fragment: Value, index: Value) {
    let table = fx.load(PTR, record, TABLE);
    let slots = fx.load(PTR, record, SLOTS);
    let mask = fx.b.ins().iadd_imm_s(slots, -1);
    let shifted = fx.b.ins().ishl_imm_s(fragment, 32);
    let tagged = fx.b.ins().iadd_imm_s(index, 1);
    let slot = fx.b.ins().bor(shifted, tagged);
    place(fx, table, mask, fragment, slot);
}

/// Drops the slot of entry `removed`, which the table holds under `fragment`, shifting back
/// each slot whose probe passed it, then numbers each later entry one lower, as removing the
/// entry moves the later ones down.
pub fn remove(fx: &mut Fx, record: Value, fragment: Value, removed: Value) {
    let table = fx.load(PTR, record, TABLE);
    let slots = fx.load(PTR, record, SLOTS);
    let mask = fx.b.ins().iadd_imm_s(slots, -1);
    let tagged = fx.b.ins().iadd_imm_s(removed, 1);
    let start = fx.b.ins().band(fragment, mask);
    let hole = fx.repeat(&[start], |fx, state| {
        let slot = slot_at(fx, table, state[0]);
        let low = fx.b.ins().band_imm_s(slot, 0xFFFF_FFFF);
        let other = fx.b.ins().icmp(IntCC::NotEqual, low, tagged);
        let next = fx.b.ins().iadd_imm_s(state[0], 1);
        let next = fx.b.ins().band(next, mask);
        let position = fx.b.ins().select(other, next, state[0]);
        (other, vec![position])
    })[0];
    let next = fx.b.ins().iadd_imm_s(hole, 1);
    let next = fx.b.ins().band(next, mask);
    let ended = fx.repeat(&[hole, next], |fx, state| {
        let (hole, scanned) = (state[0], state[1]);
        let slot = slot_at(fx, table, scanned);
        let held = fx.b.ins().icmp_imm_s(IntCC::NotEqual, slot, 0);
        let fragment = fx.b.ins().ushr_imm_s(slot, 32);
        let ideal = fx.b.ins().band(fragment, mask);
        let travelled = fx.b.ins().isub(scanned, ideal);
        let travelled = fx.b.ins().band(travelled, mask);
        let gap = fx.b.ins().isub(scanned, hole);
        let gap = fx.b.ins().band(gap, mask);
        let passed =
            fx.b.ins()
                .icmp(IntCC::UnsignedGreaterThanOrEqual, travelled, gap);
        let moves = fx.b.ins().band(held, passed);
        fx.when(moves, |fx| {
            let to = slot_address(fx, table, hole);
            fx.store(slot, to, 0);
        });
        let hole = fx.b.ins().select(moves, scanned, hole);
        let next = fx.b.ins().iadd_imm_s(scanned, 1);
        let next = fx.b.ins().band(next, mask);
        (held, vec![hole, next])
    })[0];
    let vacated = slot_address(fx, table, ended);
    let zero = fx.int(types::I64, 0);
    fx.store(zero, vacated, 0);
    let zero = fx.ptr(0);
    fx.each(zero, slots, |fx, position| {
        let at = slot_address(fx, table, position);
        let slot = fx.load(types::I64, at, 0);
        let low = fx.b.ins().band_imm_s(slot, 0xFFFF_FFFF);
        let later = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, low, tagged);
        fx.when(later, |fx| {
            let lower = fx.b.ins().iadd_imm_s(slot, -1);
            fx.store(lower, at, 0);
        });
    });
}

/// Empties the table, keeping its slots.
pub fn clear(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let table = fx.load(PTR, record, TABLE);
    let slots = fx.load(PTR, record, SLOTS);
    let bytes = fx.b.ins().ishl_imm_s(slots, 3);
    let zero = fx.ptr(0);
    lo.call_routine(fx, Routine::Fill, &[table, zero, bytes]);
}

/// Frees the table when there is one.
pub fn free(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let slots = fx.load(PTR, record, SLOTS);
    let drawn = fx.b.ins().icmp_imm_s(IntCC::NotEqual, slots, 0);
    fx.when(drawn, |fx| {
        let table = fx.load(PTR, record, TABLE);
        lo.call_routine(fx, Routine::Free, &[table]);
    });
}

/// Moves the table into another domain's reach.
pub fn lift(lo: &mut Lowering, fx: &mut Fx, record: Value, lift: Value) {
    let slots = fx.load(PTR, record, SLOTS);
    let drawn = fx.b.ins().icmp_imm_s(IntCC::NotEqual, slots, 0);
    fx.when(drawn, |fx| {
        let table = fx.load(PTR, record, TABLE);
        transfer::lift(lo, fx, table, lift);
    });
}

/// Writes a slot at the first vacant position from its fragment.
fn place(fx: &mut Fx, table: Value, mask: Value, fragment: Value, slot: Value) {
    let start = fx.b.ins().band(fragment, mask);
    let position = fx.repeat(&[start], |fx, state| {
        let held = slot_at(fx, table, state[0]);
        let taken = fx.b.ins().icmp_imm_s(IntCC::NotEqual, held, 0);
        let next = fx.b.ins().iadd_imm_s(state[0], 1);
        let next = fx.b.ins().band(next, mask);
        let position = fx.b.ins().select(taken, next, state[0]);
        (taken, vec![position])
    })[0];
    let at = slot_address(fx, table, position);
    fx.store(slot, at, 0);
}

fn slot_address(fx: &mut Fx, table: Value, position: Value) -> Value {
    let offset = fx.b.ins().ishl_imm_s(position, 3);
    fx.b.ins().iadd(table, offset)
}

fn slot_at(fx: &mut Fx, table: Value, position: Value) -> Value {
    let at = slot_address(fx, table, position);
    fx.load(types::I64, at, 0)
}
