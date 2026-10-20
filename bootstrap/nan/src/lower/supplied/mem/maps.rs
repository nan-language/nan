//! Maps and sets over vectors of their entries: a map's `mem::KeyValue` entries sorted by key,
//! a hash map's in insertion order found through its hash index, a set's values sorted; their
//! entries and cursors. A viewing cursor is the address of its next entry and the count left.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::super::Lowering;
use super::super::super::body::Held;
use super::super::super::fx::{Fx, PTR};
use super::super::Supply;
use super::index;
use super::lists::fn_param;
use super::vector::{self, Element};
use crate::check::ty::{FnTy, Ty};

/// How a container finds a key.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// By binary search through `cmp`.
    Sorted,
    /// Through its hash index, comparing by `eq`.
    Hashed,
}

/// Where a hash map holds its seed and its index's table record, after its vector.
const SEED: u64 = 32;
const INDEX: u64 = 40;

/// The entries of a container: their type, the type of the key each holds and where.
pub struct Entries {
    pub entry: Ty,
    pub key: Ty,
    pub key_at: u64,
    pub value: Option<(Ty, u64)>,
    pub shape: Element,
}

impl Entries {
    pub fn of_map(lo: &mut Lowering, key: Ty, value: Ty) -> Entries {
        let entry = lo.std_ty("mem", "KeyValue", vec![key.clone(), value.clone()]);
        let key_at = lo.layouts.field(lo.ck, &entry, None, "key").offset;
        let value_at = lo.layouts.field(lo.ck, &entry, None, "value").offset;
        let shape = Element::of(lo, &entry);
        Entries {
            entry,
            key,
            key_at,
            value: Some((value, value_at)),
            shape,
        }
    }

    pub fn of_set(lo: &mut Lowering, value: Ty) -> Entries {
        let shape = Element::of(lo, &value);
        Entries {
            entry: value.clone(),
            key: value,
            key_at: 0,
            value: None,
            shape,
        }
    }

    fn value(&self) -> (Ty, u64) {
        self.value.clone().expect("a map's values")
    }
}

/// Where a key held at an address lies among a container's entries: the index of the entry
/// holding it and 1, or the index where it would go and 0, with the key's hash fragment for a
/// hash map.
fn find(
    lo: &mut Lowering,
    fx: &mut Fx,
    entries: &Entries,
    order: Order,
    address: Value,
    key: Value,
) -> (Value, Value, Option<Value>) {
    let data = fx.load(PTR, address, vector::DATA);
    let len = fx.load(PTR, address, vector::LEN);
    match order {
        Order::Hashed => {
            let seed = fx.load(types::I64, address, SEED);
            let fragment = index::fragment(lo, fx, &entries.key, key, seed);
            let record = fx.offset(address, INDEX);
            let (at, found) = index::find(lo, fx, record, fragment, |lo, fx, index| {
                let entry = entries.shape.at(fx, data, index);
                let stored = fx.offset(entry, entries.key_at);
                lo.equal(fx, &entries.key, stored, key)
            });
            let index = fx.b.ins().select(found, at, len);
            (index, found, Some(fragment))
        }
        Order::Sorted => {
            let (index, found) = search(lo, fx, entries, data, len, |lo, fx, stored| {
                lo.order(fx, &entries.key, stored, key)
            });
            (index, found, None)
        }
    }
}

/// The binary search its section states: `compare` gives the order of the key held at an
/// address against the sought one, as an ordering index. Gives the index found or where it
/// would go, and whether it was found.
pub fn search(
    lo: &mut Lowering,
    fx: &mut Fx,
    entries: &Entries,
    data: Value,
    len: Value,
    mut compare: impl FnMut(&mut Lowering, &mut Fx, Value) -> Value,
) -> (Value, Value) {
    let zero = fx.ptr(0);
    let no = fx.int(types::I8, 0);
    let state = fx.repeat(&[zero, len, no], |fx, state| {
        let (low, high, found) = (state[0], state[1], state[2]);
        let open = fx.b.ins().icmp(IntCC::UnsignedLessThan, low, high);
        let next = fx.choose(
            open,
            &[PTR, PTR, types::I8, types::I8],
            |fx| {
                let sum = fx.b.ins().iadd(low, high);
                let middle = fx.b.ins().ushr_imm_s(sum, 1);
                let at = entries.shape.at(fx, data, middle);
                let stored = fx.offset(at, entries.key_at);
                let ordering = compare(lo, fx, stored);
                let less = fx.b.ins().icmp_imm_s(IntCC::Equal, ordering, 0);
                let equal = fx.b.ins().icmp_imm_s(IntCC::Equal, ordering, 1);
                let above = fx.b.ins().iadd_imm_s(middle, 1);
                let low = fx.b.ins().select(less, above, low);
                let low = fx.b.ins().select(equal, middle, low);
                let high = fx.b.ins().select(less, high, middle);
                let again = fx.b.ins().bxor_imm_s(equal, 1);
                vec![low, high, equal, again]
            },
            |fx| vec![low, high, found, fx.int(types::I8, 0)],
        );
        (next[3], next[..3].to_vec())
    });
    (state[0], state[2])
}

/// A member of `mem::Map[K, V]` or `mem::HashMap[K, V]`.
pub fn map(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, order: Order) {
    let (key, value) = (supply.type_arg(0), supply.type_arg(1));
    let entries = Entries::of_map(lo, key.clone(), value.clone());
    let (_, value_at) = entries.value();
    match supply.member() {
        "empty" => {
            let sret = supply.sret.expect("a map's address");
            let alloc = supply.params.last().expect("an allocator").address();
            let alloc = fx.load(PTR, alloc, 0);
            vector::empty(fx, sret, alloc, entries.shape);
            if order == Order::Hashed {
                let seed = supply.param(0).scalar();
                fx.store(seed, sret, SEED);
                let zero = fx.ptr(0);
                fx.store(zero, sret, INDEX);
                fx.store(zero, sret, INDEX + 8);
            }
            supply.done(lo, fx);
        }
        "len" => {
            let len = fx.load(PTR, supply.param(0).address(), vector::LEN);
            supply.ret(lo, fx, Held::Scalar(len));
        }
        "insert" | "try_insert" => {
            let address = supply.param(0).address();
            let (key_held, value_held) = (supply.param(1), supply.param(2));
            let sought = held_address(lo, fx, &key, key_held);
            let (index, found, fragment) = find(lo, fx, &entries, order, address, sought);
            let data = fx.load(PTR, address, vector::DATA);
            let len = fx.load(PTR, address, vector::LEN);
            let option = supply.abi.result.clone();
            let layout = lo.layout(&option);
            let result = supply
                .sret
                .unwrap_or_else(|| fx.slot(layout.size, layout.align));
            fx.choose_in(
                lo,
                found,
                &[],
                |lo, fx| {
                    let at = entries.shape.at(fx, data, index);
                    let old = fx.offset(at, value_at);
                    let one = fx.int(types::I8, 1);
                    let held = lo.held_at(fx, &value, old);
                    lo.write_option(fx, &option, result, one, held);
                    lo.store_held(fx, old, value_held, &value);
                    lo.release(fx, &key, sought);
                    Vec::new()
                },
                |lo, fx| {
                    let needed = fx.b.ins().iadd_imm_s(len, 1);
                    let room = vector::reserve(lo, fx, address, needed, entries.shape);
                    let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, room, 0);
                    let taken = [(key_held, key.clone()), (value_held, value.clone())];
                    fx.when(failed, |fx| supply.exhausted(lo, fx, &taken));
                    if let Some(fragment) = fragment {
                        let record = fx.offset(address, INDEX);
                        let alloc = fx.load(PTR, address, vector::ALLOC);
                        let room = index::reserve(lo, fx, record, alloc, needed);
                        let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, room, 0);
                        fx.when(failed, |fx| supply.exhausted(lo, fx, &taken));
                        index::insert(fx, record, fragment, index);
                    }
                    let at = vector::open_gap(lo, fx, address, index, entries.shape);
                    let key_slot = fx.offset(at, entries.key_at);
                    lo.store_held(fx, key_slot, key_held, &key);
                    let value_slot = fx.offset(at, value_at);
                    lo.store_held(fx, value_slot, value_held, &value);
                    let zero = fx.int(types::I8, 0);
                    lo.write_option(fx, &option, result, zero, Held::Empty);
                    Vec::new()
                },
            );
            let held = lo.held_at(fx, &option, result);
            supply.ret(lo, fx, held);
        }
        "remove" => {
            let address = supply.param(0).address();
            let sought = held_address(lo, fx, &key, supply.param(1));
            let (index, found, fragment) = find(lo, fx, &entries, order, address, sought);
            let data = fx.load(PTR, address, vector::DATA);
            let option = supply.abi.result.clone();
            let layout = lo.layout(&option);
            let result = supply
                .sret
                .unwrap_or_else(|| fx.slot(layout.size, layout.align));
            lo.write_option_with(fx, &option, result, found, |lo, fx, payload| {
                let at = entries.shape.at(fx, data, index);
                let stored = fx.offset(at, value_at);
                let held = lo.held_at(fx, &value, stored);
                lo.store_held(fx, payload, held, &value);
                let stored_key = fx.offset(at, entries.key_at);
                lo.release(fx, &key, stored_key);
                vector::close_gap(lo, fx, address, index, entries.shape);
                if let Some(fragment) = fragment {
                    let record = fx.offset(address, INDEX);
                    index::remove(fx, record, fragment, index);
                }
            });
            let held = lo.held_at(fx, &option, result);
            supply.ret(lo, fx, held);
        }
        "at" | "at_mut" | "get" | "get_mut" => {
            let address = supply.param(0).address();
            let sought = held_address(lo, fx, &key, supply.param(1));
            let (index, found, _) = find(lo, fx, &entries, order, address, sought);
            let data = fx.load(PTR, address, vector::DATA);
            let at = entries.shape.at(fx, data, index);
            let stored = fx.offset(at, value_at);
            match supply.member() {
                "at" | "at_mut" => supply.ret(lo, fx, Held::Address(stored)),
                _ => projected_option(lo, fx, supply, &value, found, stored),
            }
        }
        "get_by" | "get_by_mut" => {
            let address = supply.param(0).address();
            let compare = supply.param(1).address();
            let fn_ty = fn_param(supply, 1);
            let data = fx.load(PTR, address, vector::DATA);
            let len = fx.load(PTR, address, vector::LEN);
            let (index, found) = search_by(lo, fx, &entries, data, len, &fn_ty, compare);
            let at = entries.shape.at(fx, data, index);
            let stored = fx.offset(at, value_at);
            projected_option(lo, fx, supply, &value, found, stored);
        }
        "cursor" | "cursor_mut" => {
            let sret = supply.sret.expect("a cursor's address");
            vector::write_view(fx, sret, supply.param(0).address());
            supply.done(lo, fx);
        }
        "into_cursor" => {
            if order == Order::Hashed {
                let record = fx.offset(supply.param(0).address(), INDEX);
                index::free(lo, fx, record);
            }
            into_cursor(lo, fx, supply);
        }
        "retain" => {
            let address = supply.param(0).address();
            let keep = supply.param(1).address();
            let fn_ty = fn_param(supply, 1);
            let entry = entries.entry.clone();
            vector::retain(lo, fx, address, &entry, |lo, fx, at| {
                let stored_key = fx.offset(at, entries.key_at);
                let stored_value = fx.offset(at, value_at);
                let key_held = lo.held_at(fx, &key, stored_key);
                let value_held = lo.held_at(fx, &value, stored_value);
                lo.call_value(fx, &fn_ty, keep, &[key_held, value_held])
                    .result
                    .scalar()
            });
            if order == Order::Hashed {
                reindex(lo, fx, &entries, address);
            }
            supply.done(lo, fx);
        }
        member => panic!("no lowering of {}.{member}", supply.path),
    }
}

/// Records each entry of a hash map afresh in its index, after entries were dropped.
fn reindex(lo: &mut Lowering, fx: &mut Fx, entries: &Entries, address: Value) {
    let record = fx.offset(address, INDEX);
    index::clear(lo, fx, record);
    let seed = fx.load(types::I64, address, SEED);
    let data = fx.load(PTR, address, vector::DATA);
    let len = fx.load(PTR, address, vector::LEN);
    let zero = fx.ptr(0);
    fx.each(zero, len, |fx, position| {
        let entry = entries.shape.at(fx, data, position);
        let stored = fx.offset(entry, entries.key_at);
        let fragment = index::fragment(lo, fx, &entries.key, stored, seed);
        index::insert(fx, record, fragment, position);
    });
}

/// Frees a hash map's index, before its vector is released.
pub fn free_index(lo: &mut Lowering, fx: &mut Fx, address: Value) {
    let record = fx.offset(address, INDEX);
    index::free(lo, fx, record);
}

/// Moves a hash map's index into another domain's reach.
pub fn lift_index(lo: &mut Lowering, fx: &mut Fx, address: Value, lift: Value) {
    let record = fx.offset(address, INDEX);
    index::lift(lo, fx, record, lift);
}

/// A binary search calling a function value with each key it visits, which gives the key's
/// order against the sought one.
fn search_by(
    lo: &mut Lowering,
    fx: &mut Fx,
    entries: &Entries,
    data: Value,
    len: Value,
    fn_ty: &FnTy,
    compare: Value,
) -> (Value, Value) {
    let key = entries.key.clone();
    search(lo, fx, entries, data, len, |lo, fx, stored| {
        let held = lo.held_at(fx, &key, stored);
        let ordering = lo.call_value(fx, fn_ty, compare, &[held]).result;
        lo.ordering_index(fx, ordering)
    })
}

/// Returns the projection's `Option`: `Some` of the part at `stored` when `found`, else
/// `None`.
fn projected_option(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    value: &Ty,
    found: Value,
    stored: Value,
) {
    let layout_ty = supply.abi.result.clone();
    let layout = lo.layout(&layout_ty);
    let result = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    let part = supply.wrap.is_some_and(|wrap| lo.is_part(value, wrap));
    let value = value.clone();
    lo.write_option_with(fx, &layout_ty, result, found, |lo, fx, payload| {
        if part {
            fx.store(stored, payload, 0);
        } else {
            let held = lo.held_at(fx, &value, stored);
            lo.store_held(fx, payload, held, &value);
        }
    });
    let held = lo.held_at(fx, &layout_ty, result);
    supply.ret(lo, fx, held);
}

/// The address of a key held one way or another, in a new slot for a scalar.
fn held_address(lo: &mut Lowering, fx: &mut Fx, ty: &Ty, held: Held) -> Value {
    match held {
        Held::Address(address) => address,
        held => {
            let layout = lo.layout(ty);
            let slot = fx.slot(layout.size, layout.align);
            lo.store_held(fx, slot, held, ty);
            slot
        }
    }
}

/// `into_cursor`: the vector, then the position 0.
fn into_cursor(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let sret = supply.sret.expect("a cursor's address");
    let source = supply.param(0).address();
    lo.copy(fx, sret, source, 32, 8);
    let zero = fx.ptr(0);
    fx.store(zero, sret, 32);
    supply.done(lo, fx);
}

/// A member of `mem::Set[T]`.
pub fn set(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let value = supply.type_arg(0);
    let entries = Entries::of_set(lo, value.clone());
    match supply.member() {
        "empty" => {
            let sret = supply.sret.expect("a set's address");
            let alloc = fx.load(PTR, supply.param(0).address(), 0);
            vector::empty(fx, sret, alloc, entries.shape);
            supply.done(lo, fx);
        }
        "len" => {
            let len = fx.load(PTR, supply.param(0).address(), vector::LEN);
            supply.ret(lo, fx, Held::Scalar(len));
        }
        "insert" | "try_insert" | "remove" => {
            let address = supply.param(0).address();
            let held = supply.param(1);
            let sought = held_address(lo, fx, &value, held);
            let (index, found, _) = find(lo, fx, &entries, Order::Sorted, address, sought);
            let data = fx.load(PTR, address, vector::DATA);
            let len = fx.load(PTR, address, vector::LEN);
            let changed = if supply.member() == "remove" {
                fx.when(found, |fx| {
                    let at = entries.shape.at(fx, data, index);
                    lo.release(fx, &value, at);
                    vector::close_gap(lo, fx, address, index, entries.shape);
                });
                found
            } else {
                fx.choose_in(
                    lo,
                    found,
                    &[types::I8],
                    |lo, fx| {
                        lo.release(fx, &value, sought);
                        vec![fx.int(types::I8, 0)]
                    },
                    |lo, fx| {
                        let needed = fx.b.ins().iadd_imm_s(len, 1);
                        let room = vector::reserve(lo, fx, address, needed, entries.shape);
                        let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, room, 0);
                        fx.when(failed, |fx| {
                            supply.exhausted(lo, fx, &[(held, value.clone())])
                        });
                        let at = vector::open_gap(lo, fx, address, index, entries.shape);
                        lo.store_held(fx, at, held, &value);
                        vec![fx.int(types::I8, 1)]
                    },
                )[0]
            };
            supply.ret(lo, fx, Held::Scalar(changed));
        }
        "contains_by" => {
            let address = supply.param(0).address();
            let compare = supply.param(1).address();
            let fn_ty = fn_param(supply, 1);
            let data = fx.load(PTR, address, vector::DATA);
            let len = fx.load(PTR, address, vector::LEN);
            let (_, found) = search_by(lo, fx, &entries, data, len, &fn_ty, compare);
            supply.ret(lo, fx, Held::Scalar(found));
        }
        "cursor" => {
            let sret = supply.sret.expect("a cursor's address");
            vector::write_view(fx, sret, supply.param(0).address());
            supply.done(lo, fx);
        }
        "into_cursor" => into_cursor(lo, fx, supply),
        "retain" => {
            let address = supply.param(0).address();
            let keep = supply.param(1).address();
            let fn_ty = fn_param(supply, 1);
            vector::retain(lo, fx, address, &value.clone(), |lo, fx, at| {
                let held = lo.held_at(fx, &value, at);
                lo.call_value(fx, &fn_ty, keep, &[held]).result.scalar()
            });
            supply.done(lo, fx);
        }
        member => panic!("no lowering of mem::Set.{member}"),
    }
}

/// The parts of `mem::Entry[K, V]`: the address of its key, then of its value.
pub fn entry(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let entry = supply.param(0).address();
    let offset = match supply.member() {
        "key" => 0,
        _ => 8,
    };
    let part = fx.load(PTR, entry, offset);
    supply.ret(lo, fx, Held::Address(part));
}

/// `next` of a cursor viewing entries: an `Entry` of the next key-value entry, or of the next
/// set value.
pub fn view_next(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, entries: &Entries) {
    let cursor = supply.param(0).address();
    let next = fx.load(PTR, cursor, 0);
    let left = fx.load(PTR, cursor, 8);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, left, 0);
    let layout_ty = supply.abi.result.clone();
    let layout = lo.layout(&layout_ty);
    let result = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    let value = entries.value.clone();
    let element = entries.entry.clone();
    let part = supply.wrap.is_some_and(|wrap| lo.is_part(&element, wrap));
    let shape = entries.shape;
    lo.write_option_with(fx, &layout_ty, result, present, |lo, fx, payload| {
        match &value {
            Some((_, value_at)) => {
                fx.store(next, payload, 0);
                let value = fx.offset(next, *value_at);
                fx.store(value, payload, 8);
            }
            None if part => fx.store(next, payload, 0),
            None => {
                let held = lo.held_at(fx, &element, next);
                lo.store_held(fx, payload, held, &element);
            }
        }
        let after = fx.b.ins().iadd_imm_s(next, shape.size as i64);
        fx.store(after, cursor, 0);
        let rest = fx.b.ins().iadd_imm_s(left, -1);
        fx.store(rest, cursor, 8);
    });
    let held = lo.held_at(fx, &layout_ty, result);
    supply.ret(lo, fx, held);
}
