//! The index of a runner's keyed tasks by key: an open-addressing table of node addresses,
//! each slot zero when vacant, probed linearly from the high half of the key's product with
//! `MULTIPLIER`. It holds the node of each key not superseded. A table is a runtime record
//! holding its slot count and how many nodes it holds, then its slots, at least twice as many
//! as its nodes. The caller of each operation holds the runner's lock.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::super::super::Lowering;
use super::super::super::super::fx::{Fx, PTR};
use super::keyed::node;
use super::runner;

const SLOTS: u64 = 0;
const USED: u64 = 8;
const FIRST_SLOT: u64 = 16;

/// The slot count of a first table.
const FIRST: u64 = 8;

const MULTIPLIER: i64 = 0x9E37_79B9_7F4A_7C15_u64 as i64;

/// Makes room for one more node in a runner's index, drawing a table twice as large and moving
/// the nodes into it: nonzero when there is room, zero on exhaustion, which leaves the index
/// unchanged.
pub fn reserve(lo: &mut Lowering, fx: &mut Fx, record: Value) -> Value {
    let table = fx.load(PTR, record, runner::KEYS);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, table, 0);
    let counts = fx.choose(
        present,
        &[PTR, PTR],
        |fx| vec![fx.load(PTR, table, SLOTS), fx.load(PTR, table, USED)],
        |fx| vec![fx.ptr(0), fx.ptr(0)],
    );
    let (slots, used) = (counts[0], counts[1]);
    let wanted = fx.b.ins().iadd_imm_s(used, 1);
    let wanted = fx.b.ins().ishl_imm_s(wanted, 1);
    let enough =
        fx.b.ins()
            .icmp(IntCC::UnsignedLessThanOrEqual, wanted, slots);
    fx.choose_in(
        lo,
        enough,
        &[types::I8],
        |_, fx| vec![fx.int(types::I8, 1)],
        |lo, fx| {
            let doubled = fx.b.ins().ishl_imm_s(slots, 1);
            let first = fx.ptr(FIRST);
            let size = fx.b.ins().umax(doubled, first);
            let bytes = fx.b.ins().ishl_imm_s(size, 3);
            let bytes = fx.b.ins().iadd_imm_s(bytes, FIRST_SLOT as i64);
            let fresh = lo.record_alloc(fx, bytes);
            let drawn = fx.b.ins().icmp_imm_s(IntCC::NotEqual, fresh, 0);
            fx.when(drawn, |fx| {
                fx.store(size, fresh, SLOTS);
                fx.store(used, fresh, USED);
                fx.when(present, |fx| {
                    let mask = fx.b.ins().iadd_imm_s(size, -1);
                    let zero = fx.ptr(0);
                    fx.each(zero, slots, |fx, position| {
                        let held = slot_at(fx, table, position);
                        let taken = fx.b.ins().icmp_imm_s(IntCC::NotEqual, held, 0);
                        fx.when(taken, |fx| place(fx, fresh, mask, held));
                    });
                    lo.record_free(fx, table);
                });
                fx.store(fresh, record, runner::KEYS);
            });
            vec![drawn]
        },
    )[0]
}

/// Records a node under its key, in an index with room for it and no node of that key.
pub fn insert(fx: &mut Fx, record: Value, node: Value) {
    let table = fx.load(PTR, record, runner::KEYS);
    let slots = fx.load(PTR, table, SLOTS);
    let mask = fx.b.ins().iadd_imm_s(slots, -1);
    place(fx, table, mask, node);
    let used = fx.load(PTR, table, USED);
    let used = fx.b.ins().iadd_imm_s(used, 1);
    fx.store(used, table, USED);
}

/// Removes the node of a key from a runner's index, giving it, or zero when the index holds
/// none, shifting back each later node whose probe passed its slot.
pub fn take(lo: &mut Lowering, fx: &mut Fx, record: Value, key: Value) -> Value {
    let table = fx.load(PTR, record, runner::KEYS);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, table, 0);
    fx.choose_in(
        lo,
        present,
        &[PTR],
        |_, fx| {
            let slots = fx.load(PTR, table, SLOTS);
            let mask = fx.b.ins().iadd_imm_s(slots, -1);
            let start = home(fx, key, mask);
            let position = fx.repeat(&[start], |fx, state| {
                let held = slot_at(fx, table, state[0]);
                let taken = fx.b.ins().icmp_imm_s(IntCC::NotEqual, held, 0);
                let other = fx.choose(
                    taken,
                    &[types::I8],
                    |fx| {
                        let held_key = fx.load(PTR, held, node::KEY);
                        vec![fx.b.ins().icmp(IntCC::NotEqual, held_key, key)]
                    },
                    |fx| vec![fx.int(types::I8, 0)],
                )[0];
                let next = fx.b.ins().iadd_imm_s(state[0], 1);
                let next = fx.b.ins().band(next, mask);
                let position = fx.b.ins().select(other, next, state[0]);
                (other, vec![position])
            })[0];
            let found = slot_at(fx, table, position);
            let taken = fx.b.ins().icmp_imm_s(IntCC::NotEqual, found, 0);
            fx.when(taken, |fx| {
                remove(fx, table, mask, position);
                let used = fx.load(PTR, table, USED);
                let used = fx.b.ins().iadd_imm_s(used, -1);
                fx.store(used, table, USED);
            });
            vec![found]
        },
        |_, fx| vec![fx.ptr(0)],
    )[0]
}

/// Runs `visit` on each node of a runner's index, then frees the index.
pub fn drain(
    lo: &mut Lowering,
    fx: &mut Fx,
    record: Value,
    mut visit: impl FnMut(&mut Lowering, &mut Fx, Value),
) {
    let table = fx.load(PTR, record, runner::KEYS);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, table, 0);
    fx.when(present, |fx| {
        let slots = fx.load(PTR, table, SLOTS);
        let zero = fx.ptr(0);
        fx.each(zero, slots, |fx, position| {
            let held = slot_at(fx, table, position);
            let taken = fx.b.ins().icmp_imm_s(IntCC::NotEqual, held, 0);
            fx.when(taken, |fx| visit(lo, fx, held));
        });
        lo.record_free(fx, table);
        let zero = fx.ptr(0);
        fx.store(zero, record, runner::KEYS);
    });
}

/// The slot a key's probe starts from, in a table whose slot count is `mask` plus one.
fn home(fx: &mut Fx, key: Value, mask: Value) -> Value {
    let multiplier = fx.int(types::I64, MULTIPLIER);
    let product = fx.b.ins().imul(key, multiplier);
    let high = fx.b.ins().ushr_imm_s(product, 32);
    fx.b.ins().band(high, mask)
}

/// Writes a node at the first vacant slot from its key's.
fn place(fx: &mut Fx, table: Value, mask: Value, node: Value) {
    let key = fx.load(PTR, node, node::KEY);
    let start = home(fx, key, mask);
    let position = fx.repeat(&[start], |fx, state| {
        let held = slot_at(fx, table, state[0]);
        let taken = fx.b.ins().icmp_imm_s(IntCC::NotEqual, held, 0);
        let next = fx.b.ins().iadd_imm_s(state[0], 1);
        let next = fx.b.ins().band(next, mask);
        let position = fx.b.ins().select(taken, next, state[0]);
        (taken, vec![position])
    })[0];
    let at = slot_address(fx, table, position);
    fx.store(node, at, 0);
}

/// Vacates slot `hole`, moving back into it each later node of its run whose probe passed it.
fn remove(fx: &mut Fx, table: Value, mask: Value, hole: Value) {
    let next = fx.b.ins().iadd_imm_s(hole, 1);
    let next = fx.b.ins().band(next, mask);
    let ended = fx.repeat(&[hole, next], |fx, state| {
        let (hole, scanned) = (state[0], state[1]);
        let held = slot_at(fx, table, scanned);
        let taken = fx.b.ins().icmp_imm_s(IntCC::NotEqual, held, 0);
        let ideal = fx.choose(
            taken,
            &[PTR],
            |fx| {
                let held_key = fx.load(PTR, held, node::KEY);
                vec![home(fx, held_key, mask)]
            },
            |fx| vec![fx.ptr(0)],
        )[0];
        let travelled = fx.b.ins().isub(scanned, ideal);
        let travelled = fx.b.ins().band(travelled, mask);
        let gap = fx.b.ins().isub(scanned, hole);
        let gap = fx.b.ins().band(gap, mask);
        let passed =
            fx.b.ins()
                .icmp(IntCC::UnsignedGreaterThanOrEqual, travelled, gap);
        let moves = fx.b.ins().band(taken, passed);
        fx.when(moves, |fx| {
            let to = slot_address(fx, table, hole);
            fx.store(held, to, 0);
        });
        let hole = fx.b.ins().select(moves, scanned, hole);
        let next = fx.b.ins().iadd_imm_s(scanned, 1);
        let next = fx.b.ins().band(next, mask);
        (taken, vec![hole, next])
    })[0];
    let vacated = slot_address(fx, table, ended);
    let zero = fx.ptr(0);
    fx.store(zero, vacated, 0);
}

fn slot_address(fx: &mut Fx, table: Value, position: Value) -> Value {
    let offset = fx.b.ins().ishl_imm_s(position, 3);
    let slots = fx.b.ins().iadd_imm_s(table, FIRST_SLOT as i64);
    fx.b.ins().iadd(slots, offset)
}

fn slot_at(fx: &mut Fx, table: Value, position: Value) -> Value {
    let at = slot_address(fx, table, position);
    fx.load(PTR, at, 0)
}
