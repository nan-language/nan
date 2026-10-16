//! Generation: `core::Choices` and its draws, the generators std supplies, and those derives
//! write for structs and enums.
//!
//! A `Choices` value is the address of the run's choice record: the number of generator calls
//! enclosing the one running, the innermost constraint frame and the node of it the next draw
//! stands at, the draws made, the index of the draw stream's next value, the case's index, and
//! the forced draws of a rerun. A constraint frame holds a table of difference bounds, each
//! drawn integer's value once drawn, scratch distances, and what popping it restores: the frame
//! enclosing it and that frame's node.
//!
//! A table is read-only data: its counts of drawn integers, edges, exclusions and nodes, then
//! each drawn integer's bounds, each edge's nodes and weight, each exclusion's drawn integer and
//! value in ascending order, and each node's drawn integer (zero for none), first child and
//! child count. Its nodes form the tree of the positions a generation reaches through fields
//! and lengths: a struct's node has a child per field, a list's, bytes', bounded list's or
//! string's one for its length.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, condcodes::IntCC, types};
use cranelift_module::{DataDescription, DataId, Linkage, Module};

use super::super::Lowering;
use super::super::abi::{Abi, Repr, Returned};
use super::super::body::Held;
use super::super::call::{Invoked, adapt_value};
use super::super::fx::{Fx, PTR};
use super::super::runtime::{Routine, seeded};
use super::super::test_image::{self, frame as frames};
use super::Supply;
use crate::check::db::{ItemId, TypeKey};
use crate::check::generation::Constraints;
use crate::check::sig::SigKind;
use crate::check::ty::{Arg, Const, Scalar, Ty};
use crate::nir::FnRef;

/// The offsets of the choice record's fields.
pub mod choices {
    pub const DEPTH: u64 = 0;
    pub const FRAME: u64 = 8;
    pub const CURSOR: u64 = 16;
    pub const DRAWS: u64 = 24;
    pub const STREAM: u64 = 32;
    pub const CASE: u64 = 40;
    pub const FORCED: u64 = 48;
    pub const FORCED_COUNT: u64 = 56;
    pub const SIZE: u64 = 64;
}

/// The offsets of a constraint frame's fields.
mod frame {
    pub const TABLE: u64 = 0;
    pub const VALUES: u64 = 8;
    pub const UPPER: u64 = 16;
    pub const LOWER: u64 = 24;
    pub const OUTER: u64 = 32;
    pub const OUTER_CURSOR: u64 = 40;
    pub const SIZE: u64 = 48;
}

/// The offsets of a table's counts and the sizes of its records.
mod table {
    pub const DRAWN: u64 = 0;
    pub const EDGES: u64 = 8;
    pub const EXCLUDED: u64 = 16;
    pub const HEADER: u64 = 32;
    pub const BOUND: u64 = 32;
    pub const EDGE: u64 = 32;
    pub const EXCLUSION: u64 = 32;
    pub const NODE: u64 = 24;
}

/// The stream serving draws.
const DRAWS: u64 = 0;

/// The depth of enclosing generator calls from which collections draw no elements and enums
/// only their variants that do not hold themselves.
const DEEP: i64 = 8;

/// The most elements a collection without a bound draws.
const ELEMENTS: i64 = 64;

/// A node of a table's tree before it is laid out: its drawn integer, zero for none, and its
/// children.
struct Node {
    drawn: u64,
    children: Vec<Node>,
}

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    let signed = match supply.path.as_str() {
        "core::Choices.draw" => false,
        "core::Choices.draw_signed" => true,
        _ => return false,
    };
    let record = fx.load(PTR, supply.param(0).address(), 0);
    let low = supply.param(1).scalar();
    let high = supply.param(2).scalar();
    let value = draw(lo, fx, record, low, high, signed);
    supply.ret(lo, fx, Held::Scalar(value));
    true
}

/// A draw between two bounds, signed or not, by the routine making it.
fn draw(
    lo: &mut Lowering,
    fx: &mut Fx,
    record: Value,
    low: Value,
    high: Value,
    signed: bool,
) -> Value {
    let signed = fx.ptr(u64::from(signed));
    lo.call_routine(fx, Routine::Draw, &[record, low, high, signed])[0]
}

/// A 128-bit constant.
fn wide(fx: &mut Fx, value: i128) -> Value {
    let low = fx.int(types::I64, value as i64);
    let high = fx.int(types::I64, (value >> 64) as i64);
    fx.b.ins().iconcat(low, high)
}

/// The 128-bit integer at an address.
fn load_wide(fx: &mut Fx, address: Value, offset: u64) -> Value {
    let low = fx.load(types::I64, address, offset);
    let high = fx.load(types::I64, address, offset + 8);
    fx.b.ins().iconcat(low, high)
}

fn store_wide(fx: &mut Fx, value: Value, address: Value, offset: u64) {
    let (low, high) = fx.b.ins().isplit(value);
    fx.store(low, address, offset);
    fx.store(high, address, offset + 8);
}

/// The smaller of two 128-bit integers, by a branch: the backend has no 128-bit minimum.
fn smaller(fx: &mut Fx, left: Value, right: Value) -> Value {
    let less = fx.b.ins().icmp(IntCC::SignedLessThan, left, right);
    fx.choose(less, &[types::I128], |_| vec![left], |_| vec![right])[0]
}

/// The larger of two 128-bit integers, by a branch.
fn larger(fx: &mut Fx, left: Value, right: Value) -> Value {
    let more = fx.b.ins().icmp(IntCC::SignedGreaterThan, left, right);
    fx.choose(more, &[types::I128], |_| vec![left], |_| vec![right])[0]
}

/// `Draw(record, low, high, signed) -> value`: a draw's value. Its bounds narrow to the values
/// extending the drawn integers of its frame drawn so far to a solution of the frame's
/// constraints, those its exclusions skip set apart; a rerun's draw takes its forced value
/// within them, past the forced ones its shrink point; case 0 takes the low bound, case 1 the
/// high bound, and a later case, as the draw stream's next value is even, a boundary value or
/// any value, chosen uniformly by its next. The draw is recorded on standard output with its
/// bounds and the value it gives once its exclusions are skipped.
pub fn define_draw(lo: &mut Lowering, fx: &mut Fx) {
    let params = fx.entry();
    let (record, low, high, signed) = (params[0], params[1], params[2], params[3]);
    let signed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, signed, 0);
    let low = extend(fx, low, signed);
    let high = extend(fx, high, signed);
    let frame_record = fx.load(PTR, record, choices::FRAME);
    let cursor = fx.load(PTR, record, choices::CURSOR);
    let framed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, frame_record, 0);
    let placed =
        fx.b.ins()
            .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, cursor, 0);
    let constrained = fx.b.ins().band(framed, placed);
    let drawn = fx.choose(
        constrained,
        &[PTR],
        |fx| {
            let table_address = fx.load(PTR, frame_record, frame::TABLE);
            let nodes = nodes_at(fx, table_address);
            let node = fx.b.ins().imul_imm_s(cursor, table::NODE as i64);
            let node = fx.b.ins().iadd(nodes, node);
            vec![fx.load(PTR, node, 0)]
        },
        |fx| vec![fx.ptr(0)],
    )[0];
    let constrained = fx.b.ins().icmp_imm_s(IntCC::NotEqual, drawn, 0);
    let bounds = fx.choose(
        constrained,
        &[types::I128, types::I128],
        |fx| {
            let (narrow_low, narrow_high) = narrowed(fx, frame_record, drawn);
            let low = larger(fx, low, narrow_low);
            let high = smaller(fx, high, narrow_high);
            let skipped = excluded_within(fx, frame_record, drawn, low, high);
            let high = fx.b.ins().isub(high, skipped);
            vec![low, high]
        },
        |_| vec![low, high],
    );
    let (low, high) = (bounds[0], bounds[1]);
    let drawn_value = choose(lo, fx, record, low, high);
    let value = fx.choose(
        constrained,
        &[types::I128],
        |fx| {
            let value = shifted(fx, frame_record, drawn, drawn_value);
            let values = fx.load(PTR, frame_record, frame::VALUES);
            let slot = fx.b.ins().imul_imm_s(drawn, 32);
            let slot = fx.b.ins().iadd(values, slot);
            let one = fx.ptr(1);
            fx.store(one, slot, 0);
            store_wide(fx, value, slot, 16);
            vec![value]
        },
        |_| vec![drawn_value],
    )[0];
    let entry = fx.slot(64, 8);
    store_wide(fx, drawn_value, entry, 0);
    store_wide(fx, low, entry, 16);
    store_wide(fx, high, entry, 32);
    store_wide(fx, value, entry, 48);
    let size = fx.ptr(64);
    test_image::write_frame(lo, fx, frames::DRAW, &[(entry, size)]);
    let draws = fx.load(PTR, record, choices::DRAWS);
    let draws = fx.b.ins().iadd_imm_s(draws, 1);
    fx.store(draws, record, choices::DRAWS);
    let value = fx.b.ins().ireduce(types::I64, value);
    fx.b.ins().return_(&[value]);
}

/// A bound's 128-bit value, from its bits read as signed or not.
fn extend(fx: &mut Fx, bits: Value, signed: Value) -> Value {
    let negative = fx.b.ins().sextend(types::I128, bits);
    let positive = fx.b.ins().uextend(types::I128, bits);
    fx.b.ins().select(signed, negative, positive)
}

/// The address of a table's nodes.
fn nodes_at(fx: &mut Fx, table_address: Value) -> Value {
    let drawn = fx.load(PTR, table_address, table::DRAWN);
    let edges = fx.load(PTR, table_address, table::EDGES);
    let excluded = fx.load(PTR, table_address, table::EXCLUDED);
    let at = fx.b.ins().imul_imm_s(drawn, table::BOUND as i64);
    let edge_bytes = fx.b.ins().imul_imm_s(edges, table::EDGE as i64);
    let at = fx.b.ins().iadd(at, edge_bytes);
    let excluded_bytes = fx.b.ins().imul_imm_s(excluded, table::EXCLUSION as i64);
    let at = fx.b.ins().iadd(at, excluded_bytes);
    let at = fx.b.ins().iadd_imm_s(at, table::HEADER as i64);
    fx.b.ins().iadd(table_address, at)
}

/// The bounds of a frame's drawn integer that extend the values drawn so far to a solution of
/// the frame's difference bounds: the shortest distances from and to node 0, each value drawn
/// fixing its node's distance to 0 both ways.
fn narrowed(fx: &mut Fx, frame_record: Value, drawn: Value) -> (Value, Value) {
    let upper = fx.load(PTR, frame_record, frame::UPPER);
    let lower = fx.load(PTR, frame_record, frame::LOWER);
    distances(fx, frame_record, upper, false);
    distances(fx, frame_record, lower, true);
    let at = fx.b.ins().imul_imm_s(drawn, 16);
    let to_upper = fx.b.ins().iadd(upper, at);
    let high = load_wide(fx, to_upper, 0);
    let to_lower = fx.b.ins().iadd(lower, at);
    let low = load_wide(fx, to_lower, 0);
    let low = fx.b.ins().ineg(low);
    (low, high)
}

/// Writes the shortest distances from node 0 over a frame's edges, reversed when `inward`, with
/// the values drawn so far as edges both ways between their nodes and node 0, by relaxing every
/// edge once per node.
fn distances(fx: &mut Fx, frame_record: Value, out: Value, inward: bool) {
    let table_address = fx.load(PTR, frame_record, frame::TABLE);
    let values = fx.load(PTR, frame_record, frame::VALUES);
    let drawn = fx.load(PTR, table_address, table::DRAWN);
    let edge_count = fx.load(PTR, table_address, table::EDGES);
    let bounds_bytes = fx.b.ins().imul_imm_s(drawn, table::BOUND as i64);
    let edges = fx.b.ins().iadd_imm_s(table_address, table::HEADER as i64);
    let edges = fx.b.ins().iadd(edges, bounds_bytes);
    let nodes = fx.b.ins().iadd_imm_s(drawn, 1);
    let far = wide(fx, i128::MAX / 4);
    let zero = fx.ptr(0);
    fx.each(zero, nodes, |fx, node| {
        let at = fx.b.ins().imul_imm_s(node, 16);
        let at = fx.b.ins().iadd(out, at);
        store_wide(fx, far, at, 0);
    });
    let origin = wide(fx, 0);
    store_wide(fx, origin, out, 0);
    let relax = |fx: &mut Fx, from: Value, to: Value, weight: Value| {
        let (from, to) = if inward { (to, from) } else { (from, to) };
        let from_at = fx.b.ins().imul_imm_s(from, 16);
        let from_at = fx.b.ins().iadd(out, from_at);
        let to_at = fx.b.ins().imul_imm_s(to, 16);
        let to_at = fx.b.ins().iadd(out, to_at);
        let base = load_wide(fx, from_at, 0);
        let reached = fx.b.ins().iadd(base, weight);
        let current = load_wide(fx, to_at, 0);
        let best = smaller(fx, reached, current);
        store_wide(fx, best, to_at, 0);
    };
    fx.each(zero, nodes, |fx, _| {
        let zero = fx.ptr(0);
        fx.each(zero, edge_count, |fx, index| {
            let edge = fx.b.ins().imul_imm_s(index, table::EDGE as i64);
            let edge = fx.b.ins().iadd(edges, edge);
            let from = fx.load(PTR, edge, 0);
            let to = fx.load(PTR, edge, 8);
            let weight = load_wide(fx, edge, 16);
            relax(fx, from, to, weight);
        });
        let one = fx.ptr(1);
        fx.each(one, nodes, |fx, node| {
            let slot = fx.b.ins().imul_imm_s(node, 32);
            let slot = fx.b.ins().iadd(values, slot);
            let assigned = fx.load(PTR, slot, 0);
            let assigned = fx.b.ins().icmp_imm_s(IntCC::NotEqual, assigned, 0);
            fx.when(assigned, |fx| {
                let value = load_wide(fx, slot, 16);
                let origin = fx.ptr(0);
                relax(fx, origin, node, value);
                let back = fx.b.ins().ineg(value);
                relax(fx, node, origin, back);
            });
        });
    });
}

/// The number of values a frame's exclusions take from a drawn integer's bounds.
fn excluded_within(
    fx: &mut Fx,
    frame_record: Value,
    drawn: Value,
    low: Value,
    high: Value,
) -> Value {
    let (start, count) = exclusions(fx, frame_record);
    let none = wide(fx, 0);
    let skipped = fx.var(types::I128, none);
    let zero = fx.ptr(0);
    fx.each(zero, count, |fx, index| {
        let at = fx.b.ins().imul_imm_s(index, table::EXCLUSION as i64);
        let at = fx.b.ins().iadd(start, at);
        let excluding = fx.load(PTR, at, 0);
        let mine = fx.b.ins().icmp(IntCC::Equal, excluding, drawn);
        let value = load_wide(fx, at, 16);
        let above = fx.b.ins().icmp(IntCC::SignedGreaterThanOrEqual, value, low);
        let below = fx.b.ins().icmp(IntCC::SignedLessThanOrEqual, value, high);
        let within = fx.b.ins().band(above, below);
        let counted = fx.b.ins().band(mine, within);
        fx.when(counted, |fx| {
            let so_far = fx.b.use_var(skipped);
            let one = wide(fx, 1);
            let more = fx.b.ins().iadd(so_far, one);
            fx.b.def_var(skipped, more);
        });
    });
    fx.b.use_var(skipped)
}

/// The value a drawn integer takes from a draw's result: one more per value its exclusions
/// take, in ascending order, at or below the result so far.
fn shifted(fx: &mut Fx, frame_record: Value, drawn: Value, value: Value) -> Value {
    let (start, count) = exclusions(fx, frame_record);
    let result = fx.var(types::I128, value);
    let zero = fx.ptr(0);
    fx.each(zero, count, |fx, index| {
        let at = fx.b.ins().imul_imm_s(index, table::EXCLUSION as i64);
        let at = fx.b.ins().iadd(start, at);
        let excluding = fx.load(PTR, at, 0);
        let mine = fx.b.ins().icmp(IntCC::Equal, excluding, drawn);
        let excluded = load_wide(fx, at, 16);
        let so_far = fx.b.use_var(result);
        let reached =
            fx.b.ins()
                .icmp(IntCC::SignedLessThanOrEqual, excluded, so_far);
        let moves = fx.b.ins().band(mine, reached);
        let one = wide(fx, 1);
        let next = fx.b.ins().iadd(so_far, one);
        let next = fx.b.ins().select(moves, next, so_far);
        fx.b.def_var(result, next);
    });
    fx.b.use_var(result)
}

/// The address and count of a frame's table's exclusions.
fn exclusions(fx: &mut Fx, frame_record: Value) -> (Value, Value) {
    let table_address = fx.load(PTR, frame_record, frame::TABLE);
    let drawn = fx.load(PTR, table_address, table::DRAWN);
    let edges = fx.load(PTR, table_address, table::EDGES);
    let count = fx.load(PTR, table_address, table::EXCLUDED);
    let at = fx.b.ins().imul_imm_s(drawn, table::BOUND as i64);
    let edge_bytes = fx.b.ins().imul_imm_s(edges, table::EDGE as i64);
    let at = fx.b.ins().iadd(at, edge_bytes);
    let at = fx.b.ins().iadd_imm_s(at, table::HEADER as i64);
    (fx.b.ins().iadd(table_address, at), count)
}

/// The value of a draw within bounds, by the run's case or forced draws.
fn choose(lo: &mut Lowering, fx: &mut Fx, record: Value, low: Value, high: Value) -> Value {
    let forced = fx.load(PTR, record, choices::FORCED);
    let rerun = fx.b.ins().icmp_imm_s(IntCC::NotEqual, forced, 0);
    fx.choose_in(
        lo,
        rerun,
        &[types::I128],
        |_, fx| vec![forced_value(fx, record, low, high)],
        |lo, fx| {
            let case = fx.load(PTR, record, choices::CASE);
            let first = fx.b.ins().icmp_imm_s(IntCC::Equal, case, 0);
            let second = fx.b.ins().icmp_imm_s(IntCC::Equal, case, 1);
            fx.choose_in(
                lo,
                first,
                &[types::I128],
                |_, _| vec![low],
                |lo, fx| {
                    fx.choose_in(
                        lo,
                        second,
                        &[types::I128],
                        |_, _| vec![high],
                        |lo, fx| vec![chosen(lo, fx, record, low, high)],
                    )
                },
            )
        },
    )[0]
}

/// The next value of the draw stream.
fn next_value(lo: &mut Lowering, fx: &mut Fx, record: Value) -> Value {
    let index = fx.load(PTR, record, choices::STREAM);
    let next = fx.b.ins().iadd_imm_s(index, 1);
    fx.store(next, record, choices::STREAM);
    let stream = fx.ptr(DRAWS);
    seeded::stream(lo, fx, stream, index)
}

/// A later case's value: as the draw stream's next value is even, a boundary value chosen
/// uniformly among the bounds, their neighbours within them and 0 within them, ascending and
/// without repetition; otherwise any value within the bounds, chosen uniformly.
fn chosen(lo: &mut Lowering, fx: &mut Fx, record: Value, low: Value, high: Value) -> Value {
    let parity = next_value(lo, fx, record);
    let even = fx.b.ins().band_imm_s(parity, 1);
    let even = fx.b.ins().icmp_imm_s(IntCC::Equal, even, 0);
    let pick = next_value(lo, fx, record);
    fx.choose_in(
        lo,
        even,
        &[types::I128],
        |_, fx| {
            let candidates = fx.slot(5 * 16, 8);
            let one = wide(fx, 1);
            let above_low = fx.b.ins().iadd(low, one);
            let below_high = fx.b.ins().isub(high, one);
            let zero = wide(fx, 0);
            let none = fx.ptr(0);
            let count = fx.var(PTR, none);
            for candidate in [low, above_low, zero, below_high, high] {
                let above =
                    fx.b.ins()
                        .icmp(IntCC::SignedGreaterThanOrEqual, candidate, low);
                let below =
                    fx.b.ins()
                        .icmp(IntCC::SignedLessThanOrEqual, candidate, high);
                let within = fx.b.ins().band(above, below);
                fx.when(within, |fx| {
                    let written = fx.b.use_var(count);
                    let zero = fx.ptr(0);
                    let yes = fx.int(types::I8, 1);
                    let fresh = fx.repeat(&[zero, yes], |fx, state| {
                        let (index, fresh) = (state[0], state[1]);
                        let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, index, written);
                        let going = fx.b.ins().band(inside, fresh);
                        let next = fx.choose(
                            going,
                            &[PTR, types::I8],
                            |fx| {
                                let at = fx.b.ins().imul_imm_s(index, 16);
                                let at = fx.b.ins().iadd(candidates, at);
                                let known = load_wide(fx, at, 0);
                                let differs = fx.b.ins().icmp(IntCC::NotEqual, known, candidate);
                                vec![fx.b.ins().iadd_imm_s(index, 1), differs]
                            },
                            |_| vec![index, fresh],
                        );
                        (going, next)
                    })[1];
                    fx.when(fresh, |fx| {
                        let at = fx.b.ins().imul_imm_s(written, 16);
                        let at = fx.b.ins().iadd(candidates, at);
                        store_wide(fx, candidate, at, 0);
                        let grown = fx.b.ins().iadd_imm_s(written, 1);
                        fx.b.def_var(count, grown);
                    });
                });
            }
            let written = fx.b.use_var(count);
            sort_wide(fx, candidates, written);
            let position = fx.b.ins().urem(pick, written);
            let at = fx.b.ins().imul_imm_s(position, 16);
            let at = fx.b.ins().iadd(candidates, at);
            vec![load_wide(fx, at, 0)]
        },
        |_, fx| {
            let span = fx.b.ins().isub(high, low);
            let span = fx.b.ins().ireduce(types::I64, span);
            let size = fx.b.ins().iadd_imm_s(span, 1);
            let whole = fx.b.ins().icmp_imm_s(IntCC::Equal, size, 0);
            let one = fx.ptr(1);
            let divisor = fx.b.ins().select(whole, one, size);
            let offset = fx.b.ins().urem(pick, divisor);
            let offset = fx.b.ins().select(whole, pick, offset);
            let offset = fx.b.ins().uextend(types::I128, offset);
            vec![fx.b.ins().iadd(low, offset)]
        },
    )[0]
}

/// Sorts `count` 128-bit integers at an address ascending.
fn sort_wide(fx: &mut Fx, at: Value, count: Value) {
    let zero = fx.ptr(0);
    fx.each(zero, count, |fx, _| {
        let one = fx.ptr(1);
        fx.each(one, count, |fx, index| {
            let previous = fx.b.ins().iadd_imm_s(index, -1);
            let left_at = fx.b.ins().imul_imm_s(previous, 16);
            let left_at = fx.b.ins().iadd(at, left_at);
            let right_at = fx.b.ins().imul_imm_s(index, 16);
            let right_at = fx.b.ins().iadd(at, right_at);
            let left = load_wide(fx, left_at, 0);
            let right = load_wide(fx, right_at, 0);
            let swap = fx.b.ins().icmp(IntCC::SignedGreaterThan, left, right);
            fx.when(swap, |fx| {
                store_wide(fx, right, left_at, 0);
                store_wide(fx, left, right_at, 0);
            });
        });
    });
}

/// A rerun's draw: the forced value moved within the bounds to the nearer one, or past the
/// forced values the shrink point, the value within the bounds nearest 0.
fn forced_value(fx: &mut Fx, record: Value, low: Value, high: Value) -> Value {
    let draws = fx.load(PTR, record, choices::DRAWS);
    let count = fx.load(PTR, record, choices::FORCED_COUNT);
    let recorded = fx.b.ins().icmp(IntCC::UnsignedLessThan, draws, count);
    fx.choose(
        recorded,
        &[types::I128],
        |fx| {
            let forced = fx.load(PTR, record, choices::FORCED);
            let at = fx.b.ins().imul_imm_s(draws, 8);
            let at = fx.b.ins().iadd(forced, at);
            let text = fx.load(PTR, at, 0);
            let value = signed_decimal(fx, text);
            let value = larger(fx, value, low);
            vec![smaller(fx, value, high)]
        },
        |fx| {
            let zero = wide(fx, 0);
            let value = larger(fx, zero, low);
            vec![smaller(fx, value, high)]
        },
    )[0]
}

/// The signed decimal number a NUL-terminated text writes, as a 128-bit integer.
fn signed_decimal(fx: &mut Fx, text: Value) -> Value {
    let first = fx.load(types::I8, text, 0);
    let negative = fx.b.ins().icmp_imm_s(IntCC::Equal, first, i64::from(b'-'));
    let skip = fx.b.ins().uextend(PTR, negative);
    let start = fx.b.ins().iadd(text, skip);
    let zero = wide(fx, 0);
    let magnitude = fx.repeat(&[start, zero], |fx, state| {
        let (at, value) = (state[0], state[1]);
        let byte = fx.load(types::I8, at, 0);
        let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, byte, 0);
        let digit = fx.b.ins().uextend(types::I64, byte);
        let digit = fx.b.ins().iadd_imm_s(digit, -i64::from(b'0'));
        let digit = fx.b.ins().uextend(types::I128, digit);
        let ten = wide(fx, 10);
        let shifted = fx.b.ins().imul(value, ten);
        let grown = fx.b.ins().iadd(shifted, digit);
        let value = fx.b.ins().select(more, grown, value);
        let next = fx.b.ins().iadd_imm_s(at, 1);
        (more, vec![next, value])
    })[1];
    let flipped = fx.b.ins().ineg(magnitude);
    fx.b.ins().select(negative, flipped, magnitude)
}

/// Builds the tree of a value of a type at a path, holding a node for each position a drawn
/// integer's path reaches through fields or a length.
fn node(lo: &mut Lowering, ty: &Ty, path: &str, constraints: &Constraints) -> Node {
    let drawn = constraints
        .drawn
        .iter()
        .position(|(known, _)| known == path)
        .map_or(0, |index| index as u64 + 1);
    let below = |suffix: &str| {
        constraints
            .drawn
            .iter()
            .any(|(known, _)| known.starts_with(&format!("{path}{suffix}")))
    };
    let mut children = Vec::new();
    if let Ty::Item(item, _) = ty
        && below(".")
    {
        let length = match lo.ck.db.canonical(*item).as_str() {
            "mem::List" | "mem::Bytes" | "mem::Bounded" => Some(".len()"),
            "text::String" => Some(".as_str().char_count()"),
            _ => None,
        };
        match (length, &lo.ck.sig(*item).kind) {
            (Some(length), _) => {
                children.push(node(lo, &Ty::Unit, &format!("{path}{length}"), constraints))
            }
            (None, SigKind::Struct { fields }) => {
                for field in fields.clone() {
                    let field_ty = lo.ck.field_type(ty, &field.name).unwrap_or(Ty::Unit);
                    children.push(node(
                        lo,
                        &field_ty,
                        &format!("{path}.{}", field.name),
                        constraints,
                    ));
                }
            }
            _ => {}
        }
    }
    Node { drawn, children }
}

/// Lays out a tree's nodes, each node's children after it contiguously.
fn lay_out(root: Node) -> Vec<(u64, u64, u64)> {
    let mut nodes = vec![(root.drawn, 0, 0)];
    let mut pending = vec![(0usize, root.children)];
    while let Some((at, children)) = pending.pop() {
        let first = nodes.len() as u64;
        nodes[at].1 = first;
        nodes[at].2 = children.len() as u64;
        for child in &children {
            nodes.push((child.drawn, 0, 0));
        }
        for (offset, child) in children.into_iter().enumerate() {
            pending.push((first as usize + offset, child.children));
        }
    }
    nodes
}

/// The table of constraints over a tree.
fn table_data(lo: &mut Lowering, constraints: &Constraints, nodes: &[(u64, u64, u64)]) -> DataId {
    let mut bytes = Vec::new();
    for count in [
        constraints.drawn.len(),
        constraints.edges.len(),
        constraints.excluded.len(),
        nodes.len(),
    ] {
        bytes.extend_from_slice(&(count as u64).to_le_bytes());
    }
    for (_, (low, high)) in &constraints.drawn {
        bytes.extend_from_slice(&low.to_le_bytes());
        bytes.extend_from_slice(&high.to_le_bytes());
    }
    for (from, to, weight) in &constraints.edges {
        bytes.extend_from_slice(&(*from as u64).to_le_bytes());
        bytes.extend_from_slice(&(*to as u64).to_le_bytes());
        bytes.extend_from_slice(&weight.to_le_bytes());
    }
    let mut excluded = constraints.excluded.clone();
    excluded.sort();
    for (drawn, value) in excluded {
        bytes.extend_from_slice(&(drawn as u64).to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    for (drawn, first, count) in nodes {
        for word in [*drawn, *first, *count] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
    }
    lo.data_object("generation", bytes)
}

/// The table of a property test's generation, its root's children its parameters; none when
/// its generation has no constraint.
fn case_table(lo: &mut Lowering, item: ItemId, params: &[Ty]) -> Option<DataId> {
    let constraints = lo.ck.generation(item)?;
    if constraints.drawn.is_empty() {
        return None;
    }
    let root = Node {
        drawn: 0,
        children: params
            .iter()
            .enumerate()
            .map(|(index, ty)| node(lo, ty, &format!("#{index}"), &constraints))
            .collect(),
    };
    let nodes = lay_out(root);
    Some(table_data(lo, &constraints, &nodes))
}

/// The table of the predicates of a struct type, its root the struct; none when it has none.
fn struct_table(lo: &mut Lowering, ty: &Ty) -> Option<DataId> {
    let constraints = lo.ck.struct_generation(ty)?;
    let Ty::Item(item, _) = ty else {
        return None;
    };
    let SigKind::Struct { fields } = &lo.ck.sig(*item).kind else {
        return None;
    };
    let fields = fields.clone();
    let root = Node {
        drawn: 0,
        children: fields
            .iter()
            .map(|field| {
                let field_ty = lo.ck.field_type(ty, &field.name).unwrap_or(Ty::Unit);
                node(lo, &field_ty, &field.name, &constraints)
            })
            .collect(),
    };
    let nodes = lay_out(root);
    Some(table_data(lo, &constraints, &nodes))
}

/// Pushes a constraint frame of a table onto the choice record, at its root.
pub fn push_frame(lo: &mut Lowering, fx: &mut Fx, record: Value, table_id: DataId) {
    let global = lo.data_ref(fx, table_id);
    let table_address = fx.b.ins().symbol_value(PTR, global);
    let one = fx.ptr(1);
    let size = fx.ptr(frame::SIZE);
    let pushed = lo.call_libc(fx, "calloc", &[one, size], &[PTR])[0];
    super::sims::exhausted_unless(lo, fx, pushed);
    let drawn = fx.load(PTR, table_address, table::DRAWN);
    let nodes = fx.b.ins().iadd_imm_s(drawn, 1);
    for (field, width) in [(frame::VALUES, 32), (frame::UPPER, 16), (frame::LOWER, 16)] {
        let width = fx.ptr(width);
        let area = lo.call_libc(fx, "calloc", &[nodes, width], &[PTR])[0];
        super::sims::exhausted_unless(lo, fx, area);
        fx.store(area, pushed, field);
    }
    fx.store(table_address, pushed, frame::TABLE);
    let outer = fx.load(PTR, record, choices::FRAME);
    fx.store(outer, pushed, frame::OUTER);
    let cursor = fx.load(PTR, record, choices::CURSOR);
    fx.store(cursor, pushed, frame::OUTER_CURSOR);
    fx.store(pushed, record, choices::FRAME);
    let root = fx.ptr(0);
    fx.store(root, record, choices::CURSOR);
}

/// Pops the innermost constraint frame, restoring the frame and node it was pushed at.
fn pop_frame(fx: &mut Fx, record: Value) {
    let top = fx.load(PTR, record, choices::FRAME);
    let outer = fx.load(PTR, top, frame::OUTER);
    let cursor = fx.load(PTR, top, frame::OUTER_CURSOR);
    fx.store(outer, record, choices::FRAME);
    fx.store(cursor, record, choices::CURSOR);
}

/// Moves the cursor to child `index` of node `node`, to none when the node is none or has no
/// such child.
pub fn descend(fx: &mut Fx, record: Value, node: Value, index: u64) {
    let none = fx.int(PTR, -1);
    let placed =
        fx.b.ins()
            .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, node, 0);
    let child = fx.choose(
        placed,
        &[PTR],
        |fx| {
            let top = fx.load(PTR, record, choices::FRAME);
            let table_address = fx.load(PTR, top, frame::TABLE);
            let nodes = nodes_at(fx, table_address);
            let at = fx.b.ins().imul_imm_s(node, table::NODE as i64);
            let at = fx.b.ins().iadd(nodes, at);
            let first = fx.load(PTR, at, 8);
            let count = fx.load(PTR, at, 16);
            let has =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedGreaterThan, count, index as i64);
            let child = fx.b.ins().iadd_imm_s(first, index as i64);
            vec![fx.b.ins().select(has, child, none)]
        },
        |_| vec![none],
    )[0];
    fx.store(child, record, choices::CURSOR);
}

/// Places the cursor at no node: the next draws are those no constraint names.
fn unplace(fx: &mut Fx, record: Value) {
    let none = fx.int(PTR, -1);
    fx.store(none, record, choices::CURSOR);
}

/// Counts a generator call entering: its depth among enclosing ones, before it counts.
fn enter(fx: &mut Fx, record: Value) -> Value {
    let depth = fx.load(PTR, record, choices::DEPTH);
    let deeper = fx.b.ins().iadd_imm_s(depth, 1);
    fx.store(deeper, record, choices::DEPTH);
    depth
}

fn leave(fx: &mut Fx, record: Value, depth: Value) {
    fx.store(depth, record, choices::DEPTH);
}

/// Generates a value of a type through its generator, into a new slot.
pub fn generate(
    lo: &mut Lowering,
    fx: &mut Fx,
    ty: &Ty,
    choices_value: Value,
    alloc: Value,
) -> Value {
    let layout = lo.layout(ty);
    let generate = lo.core_interface("Generate");
    let fn_ref = FnRef {
        callable: crate::nir::Callable::Method {
            interface: generate,
            method: 0,
        },
        args: vec![Arg::Ty(ty.clone())].into(),
    };
    let instance = lo.callable_instance(&fn_ref, false);
    let abi = lo.abi(&instance);
    let id = lo.instance(instance);
    let alloc_slot = fx.slot(8, 8);
    fx.store(alloc, alloc_slot, 0);
    let alloc_ty = lo.std_ty("mem", "Alloc", Vec::new());
    let alloc_held = lo.held_at(fx, &alloc_ty, alloc_slot);
    let result = lo
        .invoke(
            fx,
            Invoked::Direct(id),
            &abi,
            &[Held::Address(choices_value), alloc_held],
        )
        .result;
    let slot = fx.slot(layout.size, layout.align);
    lo.store_held(fx, slot, result, ty);
    slot
}

/// The parameters a generator receives: the address of the `Choices` value, the allocator's
/// cell, and where its result goes.
fn generator_params(fx: &mut Fx, abi: &Abi) -> (Value, Value, Option<Value>) {
    let incoming = fx.entry();
    let mut incoming = incoming.into_iter();
    let sret = (abi.returned == Returned::Indirect).then(|| incoming.next().expect("a result"));
    let mut params = Vec::new();
    for (_, repr) in &abi.params {
        params.push(match repr {
            Repr::Empty => None,
            _ => incoming.next(),
        });
    }
    let choices_value = params[0].expect("the choices");
    let alloc = match abi.params[1].1 {
        Repr::Scalar(_) => params[1].expect("an allocator"),
        _ => fx.load(PTR, params[1].expect("an allocator"), 0),
    };
    (choices_value, alloc, sret)
}

/// Returns a generated value at an address as a generator's form gives it.
fn give(lo: &mut Lowering, fx: &mut Fx, abi: &Abi, sret: Option<Value>, ty: &Ty, at: Value) {
    match abi.returned {
        Returned::Indirect => {
            let layout = lo.layout(ty);
            lo.copy(
                fx,
                sret.expect("a result address"),
                at,
                layout.size,
                layout.align,
            );
            fx.b.ins().return_(&[]);
        }
        Returned::Scalar(_) => {
            let held = lo.held_at(fx, ty, at);
            let nty = crate::nir::NTy::Owned(ty.clone());
            let repr = lo.owned_repr(ty);
            let scalar = adapt_value(lo, fx, held, &nty, repr).scalar();
            fx.b.ins().return_(&[scalar]);
        }
        _ => {
            fx.b.ins().return_(&[]);
        }
    }
}

/// The address of the choice record a `Choices` value at an address names.
fn record_of(fx: &mut Fx, choices_value: Value) -> Value {
    fx.load(PTR, choices_value, 0)
}

/// Defines the generator std supplies for a type: integer scalars between their minimum and
/// maximum, `bool`, `char`, `Unit` and floats by their bits; arrays of Copy elements in index
/// order; collections and strings by a length, then their elements; boxes, Untrusted values
/// and ids by their contents.
pub fn conformance(lo: &mut Lowering, fx: &mut Fx, abi: &Abi, ty: &Ty) {
    let (choices_value, alloc, sret) = generator_params(fx, abi);
    let record = record_of(fx, choices_value);
    let depth = enter(fx, record);
    let layout = lo.layout(ty);
    let slot = fx.slot(layout.size, layout.align);
    match ty {
        Ty::Unit => {}
        Ty::Scalar(scalar) => {
            let value = scalar_value(lo, fx, record, *scalar);
            fx.store(value, slot, 0);
        }
        Ty::Item(item, args) => {
            let path = lo.ck.db.canonical(*item);
            let arg = |index: usize| args.get(index).and_then(Arg::ty).cloned();
            let count = |index: usize| match args.get(index) {
                Some(Arg::Const(Const::Int(count))) => *count as i64,
                _ => 0,
            };
            match path.as_str() {
                "core::Array" => {
                    let element = arg(0).expect("an element type");
                    let element_layout = lo.layout(&element);
                    unplace(fx, record);
                    for index in 0..count(1) as u64 {
                        let value = generate(lo, fx, &element, choices_value, alloc);
                        let at = fx.offset(slot, index * element_layout.size);
                        lo.copy(fx, at, value, element_layout.size, element_layout.align);
                    }
                }
                "core::Untrusted" => {
                    let inner = arg(0).expect("an Untrusted type argument");
                    unplace(fx, record);
                    let value = generate(lo, fx, &inner, choices_value, alloc);
                    lo.copy(fx, slot, value, layout.size, layout.align);
                }
                "mem::Id" => {
                    unplace(fx, record);
                    let index = scalar_value(lo, fx, record, Scalar::U32);
                    fx.store(index, slot, 0);
                }
                "mem::Box" => {
                    let inner = arg(0).expect("a boxed type");
                    unplace(fx, record);
                    let value = generate(lo, fx, &inner, choices_value, alloc);
                    let heap_ty = lo.std_ty("mem", "Heap", Vec::new());
                    let held_alloc = alloc_held(lo, fx, alloc);
                    let heap = call_member(lo, fx, &heap_ty, "of", &[], &[held_alloc]);
                    let held = lo.held_at(fx, &inner, value);
                    let boxed = call_member(lo, fx, &heap_ty, "boxed", &[inner], &[heap, held]);
                    lo.store_held(fx, slot, boxed, ty);
                }
                _ => sequence(lo, fx, record, (choices_value, alloc), ty, slot, depth),
            }
        }
        other => panic!("no generator of {other:?}"),
    }
    leave(fx, record, depth);
    give(lo, fx, abi, sret, ty, slot);
}

/// A drawn scalar: an integer between its minimum and maximum, `bool` from 0 or 1, a `char`
/// from 0 to 1_112_063 skipping the surrogates, a float by its bits.
fn scalar_value(lo: &mut Lowering, fx: &mut Fx, record: Value, scalar: Scalar) -> Value {
    let ir = match lo.layout(&Ty::Scalar(scalar)).shape {
        super::super::layout::Shape::Scalar { ty, .. } => ty,
        _ => panic!("a scalar's layout"),
    };
    match scalar {
        Scalar::Bool => {
            let zero = fx.ptr(0);
            let one = fx.ptr(1);
            let value = draw(lo, fx, record, zero, one, false);
            fx.b.ins().ireduce(types::I8, value)
        }
        Scalar::Char => {
            let zero = fx.ptr(0);
            let top = fx.ptr(1_112_063);
            let value = draw(lo, fx, record, zero, top, false);
            let past =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, value, 0xD800);
            let moved = fx.b.ins().iadd_imm_s(value, 0x800);
            let value = fx.b.ins().select(past, moved, value);
            fx.b.ins().ireduce(types::I32, value)
        }
        Scalar::F32 | Scalar::F64 => {
            let width = if scalar == Scalar::F32 { 32 } else { 64 };
            let zero = fx.ptr(0);
            let top = fx.int(PTR, if width == 32 { u32::MAX as i64 } else { -1 });
            let bits = draw(lo, fx, record, zero, top, false);
            let bits = match width {
                32 => fx.b.ins().ireduce(types::I32, bits),
                _ => bits,
            };
            fx.b.ins().bitcast(ir, MemFlagsData::new(), bits)
        }
        _ => {
            let (low, high) = scalar.range();
            let signed = scalar.is_signed();
            let low = fx.int(PTR, low as i64);
            let high = fx.int(PTR, high as i64);
            let value = draw(lo, fx, record, low, high, signed);
            match ir {
                types::I64 => value,
                narrow => fx.b.ins().ireduce(narrow, value),
            }
        }
    }
}

/// The `mem::Alloc` value of a cell, held as its parameters take it.
fn alloc_held(lo: &mut Lowering, fx: &mut Fx, alloc: Value) -> Held {
    let slot = fx.slot(8, 8);
    fx.store(alloc, slot, 0);
    let alloc_ty = lo.std_ty("mem", "Alloc", Vec::new());
    lo.held_at(fx, &alloc_ty, slot)
}

/// Calls a member function of a type, its own type arguments after the type's.
fn call_member(
    lo: &mut Lowering,
    fx: &mut Fx,
    owner: &Ty,
    name: &str,
    own: &[Ty],
    args: &[Held],
) -> Held {
    let Ty::Item(item, type_args) = owner else {
        panic!("a member of {owner:?}");
    };
    let function = *lo
        .ck
        .db
        .members(TypeKey::Item(*item), name)
        .first()
        .unwrap_or_else(|| panic!("the member {name} of {owner:?}"));
    let mut fn_args: Vec<Arg> = type_args.iter().cloned().collect();
    fn_args.extend(own.iter().cloned().map(Arg::Ty));
    let instance = lo.callable_instance(&FnRef::item(function, fn_args.into()), false);
    let abi = lo.abi(&instance);
    let id = lo.instance(instance);
    lo.invoke(fx, Invoked::Direct(id), &abi, args).result
}

/// A collection or string: a length drawn at the length's node, as a bound allows or none
/// under `DEEP` enclosing generator calls, then that many elements, entries or chars; a hash
/// map draws its seed first.
fn sequence(
    lo: &mut Lowering,
    fx: &mut Fx,
    record: Value,
    (choices_value, alloc): (Value, Value),
    ty: &Ty,
    slot: Value,
    depth: Value,
) {
    let Ty::Item(item, args) = ty else {
        panic!("a sequence of {ty:?}");
    };
    let path = lo.ck.db.canonical(*item);
    let arg = |index: usize| args.get(index).and_then(Arg::ty).cloned();
    let bound = |index: usize| match args.get(index) {
        Some(Arg::Const(Const::Int(count))) => *count as i64,
        _ => 0,
    };
    let node = fx.load(PTR, record, choices::CURSOR);
    let held_alloc = alloc_held(lo, fx, alloc);
    let (empty, limit, element): (Held, i64, Option<Ty>) = match path.as_str() {
        "mem::List" | "mem::Set" => {
            let empty = call_member(lo, fx, ty, "empty", &[], &[held_alloc]);
            (empty, ELEMENTS, arg(0))
        }
        "mem::Bytes" => {
            let empty = call_member(lo, fx, ty, "empty", &[], &[held_alloc]);
            (empty, ELEMENTS, Some(Ty::Scalar(Scalar::U8)))
        }
        "text::String" => {
            let empty = call_member(lo, fx, ty, "empty", &[], &[held_alloc]);
            (empty, ELEMENTS, Some(Ty::Scalar(Scalar::Char)))
        }
        "mem::Map" => {
            let empty = call_member(lo, fx, ty, "empty", &[], &[held_alloc]);
            (empty, ELEMENTS, None)
        }
        "mem::HashMap" => {
            unplace(fx, record);
            let seed = scalar_value(lo, fx, record, Scalar::U64);
            let empty = call_member(lo, fx, ty, "empty", &[], &[Held::Scalar(seed), held_alloc]);
            (empty, ELEMENTS, None)
        }
        "mem::Bounded" => {
            let empty = call_member(lo, fx, ty, "empty", &[], &[]);
            (empty, bound(1), arg(0))
        }
        "text::Fixed" => {
            let empty = call_member(lo, fx, ty, "empty", &[], &[]);
            (empty, bound(0), None)
        }
        other => panic!("no generator of {other}"),
    };
    lo.store_held(fx, slot, empty, ty);
    descend(fx, record, node, 0);
    let deep =
        fx.b.ins()
            .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, depth, DEEP);
    let most = fx.ptr(limit as u64);
    let none = fx.ptr(0);
    let high = fx.b.ins().select(deep, none, most);
    let zero = fx.ptr(0);
    let len = draw(lo, fx, record, zero, high, false);
    unplace(fx, record);
    let path = path.clone();
    fx.each(zero, len, |fx, _| {
        let receiver = Held::Address(slot);
        match path.as_str() {
            "mem::Map" | "mem::HashMap" => {
                let (key, value) = (arg(0).expect("a key type"), arg(1).expect("a value type"));
                let key_at = generate(lo, fx, &key, choices_value, alloc);
                let value_at = generate(lo, fx, &value, choices_value, alloc);
                let key_held = lo.held_at(fx, &key, key_at);
                let value_held = lo.held_at(fx, &value, value_at);
                let replaced =
                    call_member(lo, fx, ty, "insert", &[], &[receiver, key_held, value_held]);
                let option = lo.std_ty("core", "Option", vec![value]);
                let layout = lo.layout(&option);
                let held_at = fx.slot(layout.size, layout.align);
                lo.store_held(fx, held_at, replaced, &option);
                lo.release(fx, &option, held_at);
            }
            "text::Fixed" => {
                let low = fx.ptr(0x20);
                let high = fx.ptr(0x7E);
                let code = draw(lo, fx, record, low, high, false);
                let code = fx.b.ins().ireduce(types::I32, code);
                call_member(lo, fx, ty, "push", &[], &[receiver, Held::Scalar(code)]);
            }
            _ => {
                let element = element.clone().expect("an element type");
                let at = generate(lo, fx, &element, choices_value, alloc);
                let held = lo.held_at(fx, &element, at);
                let name = if path == "mem::Set" { "insert" } else { "push" };
                call_member(lo, fx, ty, name, &[], &[receiver, held]);
            }
        }
    });
}

/// Defines the generator a derive writes for a struct or enum: a struct's fields in
/// declaration order, each at its node, then the struct, its own predicates' frame pushed when
/// no frame reaches it; an enum's variant by a draw among the eligible ones, then its payload.
pub fn derived(lo: &mut Lowering, fx: &mut Fx, abi: &Abi, ty: &Ty) {
    let (choices_value, alloc, sret) = generator_params(fx, abi);
    let record = record_of(fx, choices_value);
    let depth = enter(fx, record);
    let layout = lo.layout(ty);
    let slot = fx.slot(layout.size, layout.align);
    let Ty::Item(item, _) = ty else {
        panic!("a derived generator of {ty:?}");
    };
    match lo.ck.sig(*item).kind.clone() {
        SigKind::Struct { fields } => {
            let own = struct_table(lo, ty);
            let cursor = fx.load(PTR, record, choices::CURSOR);
            let pushed = match own {
                Some(table_id) => {
                    let unplaced = fx.b.ins().icmp_imm_s(IntCC::SignedLessThan, cursor, 0);
                    fx.when(unplaced, |fx| push_frame(lo, fx, record, table_id));
                    Some(unplaced)
                }
                None => None,
            };
            let node = fx.load(PTR, record, choices::CURSOR);
            for (index, field) in fields.iter().enumerate() {
                let field_ty = lo.ck.field_type(ty, &field.name).expect("a field's type");
                descend(fx, record, node, index as u64);
                let value = generate(lo, fx, &field_ty, choices_value, alloc);
                let at = lo.layouts.field(lo.ck, ty, None, &field.name);
                let size = lo.layout(&field_ty);
                let target = fx.offset(slot, at.offset);
                lo.copy(fx, target, value, size.size, size.align);
            }
            fx.store(node, record, choices::CURSOR);
            if let Some(pushed) = pushed {
                fx.when(pushed, |fx| pop_frame(fx, record));
            }
        }
        SigKind::Enum { variants } => {
            let all: Vec<usize> = (0..variants.len()).collect();
            let eligible: Vec<usize> = all
                .iter()
                .copied()
                .filter(|index| !holds_itself(lo, ty, &variants[*index].payload, *item))
                .collect();
            let cursor = fx.load(PTR, record, choices::CURSOR);
            unplace(fx, record);
            let deep =
                fx.b.ins()
                    .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, depth, DEEP);
            let count_all = fx.ptr(all.len() as u64 - 1);
            let count_eligible = fx.ptr(eligible.len().max(1) as u64 - 1);
            let high = fx.b.ins().select(deep, count_eligible, count_all);
            let zero = fx.ptr(0);
            let position = draw(lo, fx, record, zero, high, false);
            let names = lo.variant_names(*item);
            for (index, variant) in variants.iter().enumerate() {
                let deep_position = eligible.iter().position(|chosen| *chosen == index);
                let shallow = fx.b.ins().icmp_imm_s(IntCC::Equal, position, index as i64);
                let deep_chosen = match deep_position {
                    Some(at) => fx.b.ins().icmp_imm_s(IntCC::Equal, position, at as i64),
                    None => fx.int(types::I8, 0),
                };
                let this = fx.b.ins().select(deep, deep_chosen, shallow);
                let payload = payload_types(lo, ty, &variant.payload);
                let name = names[index].clone();
                fx.when(this, |fx| {
                    let mut held = Vec::new();
                    for payload_ty in &payload {
                        let at = generate(lo, fx, payload_ty, choices_value, alloc);
                        held.push(lo.held_at(fx, payload_ty, at));
                    }
                    let value = lo.variant_value(fx, ty, &name, &held, false);
                    lo.store_held(fx, slot, value, ty);
                });
            }
            fx.store(cursor, record, choices::CURSOR);
        }
        other => panic!("a derived generator of {other:?}"),
    }
    leave(fx, record, depth);
    give(lo, fx, abi, sret, ty, slot);
}

/// The types of a variant's payload for an instantiation of its enum.
fn payload_types(lo: &Lowering, ty: &Ty, payload: &crate::check::sig::Payload) -> Vec<Ty> {
    let subst = lo.subst(
        match ty {
            Ty::Item(item, _) => *item,
            _ => panic!("an enum"),
        },
        ty.args(),
    );
    match payload {
        crate::check::sig::Payload::None => Vec::new(),
        crate::check::sig::Payload::Single(payload) => vec![subst.ty(payload)],
        crate::check::sig::Payload::Labelled(fields) => {
            fields.iter().map(|(_, field)| subst.ty(field)).collect()
        }
    }
}

/// Whether a variant's payload holds its enum as a constituent type at any depth.
fn holds_itself(
    lo: &Lowering,
    ty: &Ty,
    payload: &crate::check::sig::Payload,
    enumeration: ItemId,
) -> bool {
    payload_types(lo, ty, payload)
        .iter()
        .any(|payload| reaches(lo, payload, enumeration, &mut Vec::new()))
}

/// Whether a type is an item or holds it as a constituent type, through fields, payloads and
/// type arguments.
fn reaches(lo: &Lowering, ty: &Ty, target: ItemId, seen: &mut Vec<ItemId>) -> bool {
    let Ty::Item(item, args) = ty else {
        return false;
    };
    if *item == target {
        return true;
    }
    if seen.contains(item) {
        return false;
    }
    seen.push(*item);
    args.iter()
        .filter_map(Arg::ty)
        .any(|arg| reaches(lo, arg, target, seen))
        || lo.ck.constituents(*item).iter().any(|constituent| {
            let subst = lo.subst(*item, args);
            reaches(lo, &subst.ty(constituent), target, seen)
        })
}

/// Generates the parameters of a property test's case into slots, each at its node of the
/// test's table, through a new choice record of the run's case and forced draws.
pub fn case(
    lo: &mut Lowering,
    fx: &mut Fx,
    item: ItemId,
    params: &[Ty],
    alloc: Value,
    run: (Value, Value, Value),
) -> Vec<Value> {
    let one = fx.ptr(1);
    let size = fx.ptr(choices::SIZE);
    let record = lo.call_libc(fx, "calloc", &[one, size], &[PTR])[0];
    super::sims::exhausted_unless(lo, fx, record);
    let (case_index, forced, forced_count) = run;
    fx.store(case_index, record, choices::CASE);
    fx.store(forced, record, choices::FORCED);
    fx.store(forced_count, record, choices::FORCED_COUNT);
    unplace(fx, record);
    let table_id = case_table(lo, item, params);
    if let Some(table_id) = table_id {
        push_frame(lo, fx, record, table_id);
    }
    let choices_value = fx.slot(8, 8);
    fx.store(record, choices_value, 0);
    let mut slots = Vec::new();
    for (index, ty) in params.iter().enumerate() {
        match table_id {
            Some(_) => {
                let root = fx.ptr(0);
                descend(fx, record, root, index as u64);
            }
            None => unplace(fx, record),
        }
        slots.push(generate(lo, fx, ty, choices_value, alloc));
    }
    slots
}

impl Lowering<'_, '_> {
    /// A read-only data object holding bytes, aligned to 8.
    fn data_object(&mut self, kind: &str, bytes: Vec<u8>) -> DataId {
        self.glue += 1;
        let name = format!("_NSdata_{kind}{}", self.glue);
        let id = self
            .module
            .declare_data(&name, Linkage::Local, false, false)
            .expect("a data object");
        let mut description = DataDescription::new();
        description.define(bytes.into_boxed_slice());
        description.set_align(8);
        self.module
            .define_data(id, &description)
            .expect("defining data");
        id
    }
}
