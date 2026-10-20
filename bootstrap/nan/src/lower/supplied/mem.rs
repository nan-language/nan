//! Memory: allocators, budgets and stores, lists, bytes, maps and sets, the release of the std
//! types holding memory, and the conformances the toolchain supplies to sequences and
//! collections.

mod index;
pub mod lists;
pub mod maps;
mod segments;
pub mod stores;
pub mod vector;

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::Lowering;
use super::super::abi::{Abi, Repr, Returned};
use super::super::body::Held;
use super::super::data::const_arg;
use super::super::fx::{Fx, PTR};
use super::super::runtime::Routine;
use super::super::transfer;
use super::Supply;
use crate::check::ty::{Arg, Scalar, Ty};
use maps::{Entries, Order};
use vector::Element;

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    let owner = supply.path.rsplit_once('.').map_or("", |(owner, _)| owner);
    match owner {
        "mem::List" => lists::list(lo, fx, supply, &supply.type_arg(0)),
        "mem::Bytes" => lists::list(lo, fx, supply, &lists::byte()),
        "mem::Bounded" => lists::bounded(lo, fx, supply),
        "mem::ListCursor"
        | "mem::MapIntoCursor"
        | "mem::HashMapIntoCursor"
        | "mem::SetIntoCursor" => {
            let element = owning_element(lo, owner, supply);
            lists::vector_next(lo, fx, supply, &element);
        }
        "mem::BoundedCursor" => lists::bounded_next(lo, fx, supply),
        "mem::Map" => maps::map(lo, fx, supply, Order::Sorted),
        "mem::HashMap" => maps::map(lo, fx, supply, Order::Hashed),
        "mem::Set" => maps::set(lo, fx, supply),
        "mem::Entry" => maps::entry(lo, fx, supply),
        "mem::MapCursor" | "mem::MapCursorMut" | "mem::HashMapCursor" | "mem::HashMapCursorMut" => {
            let entries = Entries::of_map(lo, supply.type_arg(0), supply.type_arg(1));
            maps::view_next(lo, fx, supply, &entries);
        }
        "mem::SetCursor" => {
            let entries = Entries::of_set(lo, supply.type_arg(0));
            maps::view_next(lo, fx, supply, &entries);
        }
        _ => return stores::lower(lo, fx, supply),
    }
    true
}

/// The element an owning cursor moves out.
fn owning_element(lo: &mut Lowering, owner: &str, supply: &Supply) -> Ty {
    match owner {
        "mem::ListCursor" | "mem::SetIntoCursor" => supply.type_arg(0),
        _ => lo.std_ty(
            "mem",
            "KeyValue",
            vec![supply.type_arg(0), supply.type_arg(1)],
        ),
    }
}

/// Whether moving a value of a mem type into another domain's reach does anything, none for a
/// type std declares in full.
pub fn transfers(lo: &mut Lowering, path: &str, ty: &Ty) -> Option<bool> {
    Some(match path {
        "mem::List"
        | "mem::Bytes"
        | "mem::Map"
        | "mem::HashMap"
        | "mem::Set"
        | "mem::Pool"
        | "mem::Arena"
        | "mem::OnceMap"
        | "mem::Box"
        | "mem::Shared"
        | "mem::ListCursor"
        | "mem::MapIntoCursor"
        | "mem::HashMapIntoCursor"
        | "mem::SetIntoCursor"
        | "mem::Budget" => true,
        "mem::Bounded" | "mem::BoundedCursor" => {
            let element = ty.type_arg(0).cloned().expect("an element type");
            lo.transfers(&element)
        }
        _ => return None,
    })
}

/// Moves a value of a mem type at an address into another domain's reach; false for a type std
/// declares in full.
pub fn transfer(
    lo: &mut Lowering,
    fx: &mut Fx,
    path: &str,
    ty: &Ty,
    address: Value,
    lift: Value,
    owner: Value,
) -> bool {
    let arg = |index: usize| ty.type_arg(index).cloned().expect("a type argument");
    let entry = |lo: &mut Lowering| lo.std_ty("mem", "KeyValue", vec![arg(0), arg(1)]);
    match path {
        "mem::List" | "mem::Set" => vector::transfer(lo, fx, address, &arg(0), None, lift, owner),
        "mem::Bytes" => vector::transfer(lo, fx, address, &lists::byte(), None, lift, owner),
        "mem::Map" | "mem::HashMap" => {
            let entry = entry(lo);
            vector::transfer(lo, fx, address, &entry, None, lift, owner);
            if path == "mem::HashMap" {
                maps::lift_index(lo, fx, address, lift);
            }
        }
        "mem::ListCursor" | "mem::SetIntoCursor" => {
            let position = fx.load(PTR, address, 32);
            vector::transfer(lo, fx, address, &arg(0), Some(position), lift, owner);
        }
        "mem::MapIntoCursor" | "mem::HashMapIntoCursor" => {
            let entry = entry(lo);
            let position = fx.load(PTR, address, 32);
            vector::transfer(lo, fx, address, &entry, Some(position), lift, owner);
        }
        "mem::Bounded" => {
            let len_at = lo.layouts.field(lo.ck, ty, None, "len").offset;
            let items_at = lo.layouts.field(lo.ck, ty, None, "items").offset;
            transfer_inline(
                lo,
                fx,
                &arg(0),
                address,
                (len_at, items_at, None),
                lift,
                owner,
            );
        }
        "mem::BoundedCursor" => {
            let position_at = lo.layouts.field(lo.ck, ty, None, "position").offset;
            let bounded_at = lo.layouts.field(lo.ck, ty, None, "bounded").offset;
            let bounded = lo.layouts.field(lo.ck, ty, None, "bounded").ty;
            let len_at = lo.layouts.field(lo.ck, &bounded, None, "len").offset + bounded_at;
            let items_at = lo.layouts.field(lo.ck, &bounded, None, "items").offset + bounded_at;
            let shape = (len_at, items_at, Some(position_at));
            transfer_inline(lo, fx, &arg(0), address, shape, lift, owner);
        }
        "mem::Pool" => stores::transfer_pool(lo, fx, &arg(0), address, lift, owner),
        "mem::Arena" => {
            let header = fx.load(PTR, address, 0);
            segments::transfer(lo, fx, header, &arg(0), lift, owner);
        }
        "mem::OnceMap" => {
            let header = fx.load(PTR, address, 0);
            let entry = entry(lo);
            segments::transfer(lo, fx, header, &entry, lift, owner);
            let record = fx.offset(header, segments::INDEX);
            index::lift(lo, fx, record, lift);
        }
        "mem::Box" => {
            let contents = fx.load(PTR, address, 0);
            transfer::lift(lo, fx, contents, lift);
            lo.transfer(fx, &arg(0), contents, lift, owner);
        }
        "mem::Shared" => stores::transfer_shared(lo, fx, &arg(0), address, lift),
        "mem::Budget" => transfer::rehome(lo, fx, address, owner),
        _ => return false,
    }
    true
}

/// Moves the elements a bounded list or its cursor holds inline, given the offsets of its
/// length, its items and its cursor's position.
fn transfer_inline(
    lo: &mut Lowering,
    fx: &mut Fx,
    element: &Ty,
    address: Value,
    (len_at, items_at, position_at): (u64, u64, Option<u64>),
    lift: Value,
    owner: Value,
) {
    let shape = Element::of(lo, element);
    let len = fx.load(PTR, address, len_at);
    let from = match position_at {
        Some(at) => fx.load(PTR, address, at),
        None => fx.ptr(0),
    };
    let items = fx.offset(address, items_at);
    fx.each(from, len, |fx, index| {
        let at = shape.at(fx, items, index);
        lo.transfer(fx, element, at, lift, owner);
    });
}

/// Whether releasing a value of a mem type does anything, none for a type std declares in
/// full.
pub fn needs_release(lo: &mut Lowering, path: &str, ty: &Ty) -> Option<bool> {
    Some(match path {
        "mem::List"
        | "mem::Bytes"
        | "mem::Map"
        | "mem::HashMap"
        | "mem::Set"
        | "mem::Pool"
        | "mem::Arena"
        | "mem::OnceMap"
        | "mem::Box"
        | "mem::Shared"
        | "mem::ListCursor"
        | "mem::MapIntoCursor"
        | "mem::HashMapIntoCursor"
        | "mem::SetIntoCursor" => true,
        "mem::Bounded" | "mem::BoundedCursor" => {
            let element = ty.type_arg(0).cloned().expect("an element type");
            lo.needs_release(&element)
        }
        _ => return None,
    })
}

/// Releases a value of a mem type at an address; false for a type std declares in full.
pub fn release(lo: &mut Lowering, fx: &mut Fx, path: &str, ty: &Ty, address: Value) -> bool {
    let arg = |index: usize| ty.type_arg(index).cloned().expect("a type argument");
    match path {
        "mem::List" | "mem::Set" => {
            let zero = fx.ptr(0);
            vector::release(lo, fx, address, &arg(0), zero);
        }
        "mem::Bytes" => vector::free_storage(lo, fx, address),
        "mem::Map" | "mem::HashMap" => {
            if path == "mem::HashMap" {
                maps::free_index(lo, fx, address);
            }
            let entry = lo.std_ty("mem", "KeyValue", vec![arg(0), arg(1)]);
            let zero = fx.ptr(0);
            vector::release(lo, fx, address, &entry, zero);
        }
        "mem::ListCursor" | "mem::SetIntoCursor" => lists::release_cursor(lo, fx, address, &arg(0)),
        "mem::MapIntoCursor" | "mem::HashMapIntoCursor" => {
            let entry = lo.std_ty("mem", "KeyValue", vec![arg(0), arg(1)]);
            lists::release_cursor(lo, fx, address, &entry);
        }
        "mem::Bounded" => {
            let len_at = lo.layouts.field(lo.ck, ty, None, "len").offset;
            let items_at = lo.layouts.field(lo.ck, ty, None, "items").offset;
            release_inline(lo, fx, &arg(0), address, len_at, items_at, None);
        }
        "mem::BoundedCursor" => {
            let position_at = lo.layouts.field(lo.ck, ty, None, "position").offset;
            let bounded_at = lo.layouts.field(lo.ck, ty, None, "bounded").offset;
            let bounded = lo.layouts.field(lo.ck, ty, None, "bounded").ty;
            let len_at = lo.layouts.field(lo.ck, &bounded, None, "len").offset + bounded_at;
            let items_at = lo.layouts.field(lo.ck, &bounded, None, "items").offset + bounded_at;
            release_inline(
                lo,
                fx,
                &arg(0),
                address,
                len_at,
                items_at,
                Some(position_at),
            );
        }
        "mem::Pool" => stores::release_pool(lo, fx, &arg(0), address),
        "mem::Arena" => {
            let header = fx.load(PTR, address, 0);
            segments::release(lo, fx, header, &arg(0));
        }
        "mem::OnceMap" => {
            let header = fx.load(PTR, address, 0);
            let record = fx.offset(header, segments::INDEX);
            index::free(lo, fx, record);
            let entry = lo.std_ty("mem", "KeyValue", vec![arg(0), arg(1)]);
            segments::release(lo, fx, header, &entry);
        }
        "mem::Box" => stores::release_box(lo, fx, ty, address),
        "mem::Shared" => stores::release_shared(lo, fx, &arg(0), address),
        _ => return false,
    }
    true
}

/// Releases the elements an inline list holds, from its position when it is a cursor's.
fn release_inline(
    lo: &mut Lowering,
    fx: &mut Fx,
    element: &Ty,
    address: Value,
    len_at: u64,
    items_at: u64,
    position_at: Option<u64>,
) {
    let shape = Element::of(lo, element);
    let len = fx.load(PTR, address, len_at);
    let from = match position_at {
        Some(at) => fx.load(PTR, address, at),
        None => fx.ptr(0),
    };
    let items = fx.offset(address, items_at);
    fx.each(from, len, |fx, index| {
        let at = shape.at(fx, items, index);
        lo.release(fx, element, at);
    });
}

/// A sequence a conformance walks: the address of its first element, its length and its
/// elements' type.
struct Sequence {
    element: Ty,
    /// Whether its hash starts with its length.
    counted: bool,
    /// Whether its elements are bytes that one write hashes, as a text's are.
    text: bool,
}

/// The sequence a std type is, none for a type whose conformances are not sequences'.
fn sequence(lo: &mut Lowering, ty: &Ty) -> Option<Sequence> {
    let Ty::Item(item, _) = ty else {
        return None;
    };
    if !lo.ck.db.is_std(*item) {
        return None;
    }
    let path = lo.ck.db.canonical(*item);
    let arg = |index: usize| ty.type_arg(index).cloned().expect("a type argument");
    let byte = Ty::Scalar(Scalar::U8);
    Some(match path.as_str() {
        "core::Str" | "text::String" | "text::Fixed" => Sequence {
            element: byte,
            counted: true,
            text: true,
        },
        "mem::Bytes" => Sequence {
            element: byte,
            counted: true,
            text: false,
        },
        "core::Slice" | "core::SliceMut" | "mem::List" | "mem::Set" | "mem::Bounded" => Sequence {
            element: arg(0),
            counted: true,
            text: false,
        },
        "core::Array" => Sequence {
            element: arg(0),
            counted: false,
            text: false,
        },
        "mem::Map" | "mem::HashMap" => Sequence {
            element: lo.std_ty("mem", "KeyValue", vec![arg(0), arg(1)]),
            counted: true,
            text: false,
        },
        _ => return None,
    })
}

/// The address of the first element and the length of a sequence value of a type at an address.
fn elements(lo: &mut Lowering, fx: &mut Fx, ty: &Ty, address: Value) -> (Value, Value) {
    if lo.ck.is_std(ty, "core::Array") {
        let count = const_arg(&ty.args()[1]) as u64;
        return (address, fx.ptr(count));
    }
    if lo.ck.is_std(ty, "mem::Bounded") || lo.ck.is_std(ty, "text::Fixed") {
        let (len_name, items_name) = match lo.ck.is_std(ty, "text::Fixed") {
            true => ("len", "bytes"),
            false => ("len", "items"),
        };
        let len_at = lo.layouts.field(lo.ck, ty, None, len_name).offset;
        let items_at = lo.layouts.field(lo.ck, ty, None, items_name).offset;
        let len = fx.load(PTR, address, len_at);
        return (fx.offset(address, items_at), len);
    }
    (fx.load(PTR, address, 0), fx.load(PTR, address, 8))
}

/// Defines `eq`, `cmp`, `hash` or `default` of a sequence or collection type; false for another
/// type.
pub fn conformance(lo: &mut Lowering, fx: &mut Fx, abi: &Abi, ty: &Ty, method: &str) -> bool {
    let Some(sequence) = sequence(lo, ty) else {
        return false;
    };
    let incoming = fx.entry();
    let mut incoming = incoming.into_iter();
    let sret = (abi.returned == Returned::Indirect).then(|| incoming.next().expect("a result"));
    let params: Vec<Held> = abi
        .params
        .iter()
        .map(|(_, repr)| match repr {
            Repr::Scalar(_) => Held::Scalar(incoming.next().expect("a parameter")),
            Repr::Address => Held::Address(incoming.next().expect("a parameter")),
            Repr::Empty => Held::Empty,
        })
        .collect();
    let shape = Element::of(lo, &sequence.element);
    match method {
        "eq" | "cmp" => {
            let (left, left_len) = elements(lo, fx, ty, params[0].address());
            let (right, right_len) = elements(lo, fx, ty, params[1].address());
            let order = if sequence.text {
                text_order(lo, fx, left, left_len, right, right_len)
            } else {
                walk_order(
                    lo,
                    fx,
                    &sequence.element,
                    shape,
                    method,
                    (left, left_len),
                    (right, right_len),
                )
            };
            let held = match method {
                "eq" => Held::Scalar(fx.b.ins().icmp_imm_s(IntCC::Equal, order, 1)),
                _ => {
                    let less = fx.b.ins().icmp_imm_s(IntCC::Equal, order, 0);
                    let greater = fx.b.ins().icmp_imm_s(IntCC::Equal, order, 2);
                    lo.ordering(fx, less, greater)
                }
            };
            match held {
                Held::Scalar(value) => {
                    fx.b.ins().return_(&[value]);
                }
                Held::Address(source) => {
                    let target = sret.expect("an ordering's address");
                    let layout = lo.layout(&abi.result);
                    lo.copy(fx, target, source, layout.size, layout.align);
                    fx.b.ins().return_(&[]);
                }
                Held::Empty => {
                    fx.b.ins().return_(&[]);
                }
            }
        }
        "hash" => {
            let hasher_ty = abi.params[1].0.ty().clone();
            let hasher = params[1];
            let (start, len) = elements(lo, fx, ty, params[0].address());
            if sequence.counted {
                let count = fx.slot(8, 8);
                fx.store(len, count, 0);
                let eight = fx.ptr(8);
                write_bytes(lo, fx, &hasher_ty, hasher, count, eight);
            }
            if sequence.text {
                write_bytes(lo, fx, &hasher_ty, hasher, start, len);
            } else {
                let element = sequence.element.clone();
                let zero = fx.ptr(0);
                fx.each(zero, len, |fx, index| {
                    let at = shape.at(fx, start, index);
                    hash_part(lo, fx, &element, at, &hasher_ty, hasher);
                });
            }
            fx.b.ins().return_(&[]);
        }
        "default" => {
            let target = sret.expect("a default's address");
            let element = sequence.element.clone();
            let (start, len) = elements(lo, fx, ty, target);
            let zero = fx.ptr(0);
            fx.each(zero, len, |fx, index| {
                let value = lo.call_method(fx, &element, "Default", Vec::new(), &[]);
                let at = shape.at(fx, start, index);
                lo.store_held(fx, at, value, &element);
            });
            fx.b.ins().return_(&[]);
        }
        other => panic!("the conformance {other} of {ty:?}"),
    }
    true
}

/// Hashes a part of a type at an address: a key-value entry's key then value, another value
/// through its `Hash`.
fn hash_part(lo: &mut Lowering, fx: &mut Fx, ty: &Ty, at: Value, hasher_ty: &Ty, hasher: Held) {
    if lo.ck.is_std(ty, "mem::KeyValue") {
        for name in ["key", "value"] {
            let field = lo.layouts.field(lo.ck, ty, None, name);
            let part = fx.offset(at, field.offset);
            hash_part(lo, fx, &field.ty, part, hasher_ty, hasher);
        }
        return;
    }
    let held = lo.held_at(fx, ty, at);
    lo.call_method(
        fx,
        ty,
        "Hash",
        vec![Arg::Ty(hasher_ty.clone())],
        &[held, hasher],
    );
}

/// Writes `len` bytes from an address to a hasher in one write.
fn write_bytes(
    lo: &mut Lowering,
    fx: &mut Fx,
    hasher_ty: &Ty,
    hasher: Held,
    start: Value,
    len: Value,
) {
    let view = fx.slot(16, 8);
    fx.store(start, view, 0);
    fx.store(len, view, 8);
    lo.call_method(
        fx,
        hasher_ty,
        "Hasher",
        Vec::new(),
        &[hasher, Held::Address(view)],
    );
}

/// The order of two byte texts as an ordering index: by their common prefix's bytes, then by
/// length.
fn text_order(
    lo: &mut Lowering,
    fx: &mut Fx,
    left: Value,
    left_len: Value,
    right: Value,
    right_len: Value,
) -> Value {
    let common = fx.b.ins().umin(left_len, right_len);
    let compared = lo.call_routine(fx, Routine::Compare, &[left, right, common])[0];
    let before = fx.b.ins().icmp_imm_s(IntCC::SignedLessThan, compared, 0);
    let after = fx.b.ins().icmp_imm_s(IntCC::SignedGreaterThan, compared, 0);
    let shorter =
        fx.b.ins()
            .icmp(IntCC::UnsignedLessThan, left_len, right_len);
    let longer =
        fx.b.ins()
            .icmp(IntCC::UnsignedGreaterThan, left_len, right_len);
    let one = fx.int(types::I8, 1);
    let zero = fx.int(types::I8, 0);
    let two = fx.int(types::I8, 2);
    let by_len = fx.b.ins().select(shorter, zero, one);
    let by_len = fx.b.ins().select(longer, two, by_len);
    let by_bytes = fx.b.ins().select(after, two, by_len);
    fx.b.ins().select(before, zero, by_bytes)
}

/// The order of two sequences as an ordering index, element by element through `eq` or `cmp`,
/// then by length; `eq` only tells equal (1) from not (0 or 2).
fn walk_order(
    lo: &mut Lowering,
    fx: &mut Fx,
    element: &Ty,
    shape: Element,
    method: &str,
    (left, left_len): (Value, Value),
    (right, right_len): (Value, Value),
) -> Value {
    let common = fx.b.ins().umin(left_len, right_len);
    let same_len = fx.b.ins().icmp(IntCC::Equal, left_len, right_len);
    if method == "eq" {
        let start = fx.int(types::I8, 1);
        let zero = fx.ptr(0);
        let state = fx.choose(
            same_len,
            &[types::I8],
            |fx| {
                let walked = fx.repeat(&[zero, start], |fx, state| {
                    let (index, equal) = (state[0], state[1]);
                    let more = fx.b.ins().icmp(IntCC::UnsignedLessThan, index, common);
                    let going = fx.b.ins().band(more, equal);
                    let next = fx.choose(
                        going,
                        &[PTR, types::I8],
                        |fx| {
                            let a = shape.at(fx, left, index);
                            let b = shape.at(fx, right, index);
                            let same = elements_equal(lo, fx, element, a, b);
                            vec![fx.b.ins().iadd_imm_s(index, 1), same]
                        },
                        |_| vec![index, equal],
                    );
                    (going, next)
                });
                vec![walked[1]]
            },
            |fx| vec![fx.int(types::I8, 0)],
        );
        return state[0];
    }
    let zero = fx.ptr(0);
    let undecided = fx.int(types::I8, 1);
    let walked = fx.repeat(&[zero, undecided], |fx, state| {
        let (index, order) = (state[0], state[1]);
        let more = fx.b.ins().icmp(IntCC::UnsignedLessThan, index, common);
        let open = fx.b.ins().icmp_imm_s(IntCC::Equal, order, 1);
        let going = fx.b.ins().band(more, open);
        let next = fx.choose(
            going,
            &[PTR, types::I8],
            |fx| {
                let a = shape.at(fx, left, index);
                let b = shape.at(fx, right, index);
                let order = lo.order(fx, element, a, b);
                vec![fx.b.ins().iadd_imm_s(index, 1), order]
            },
            |_| vec![index, order],
        );
        (going, next)
    });
    let order = walked[1];
    let open = fx.b.ins().icmp_imm_s(IntCC::Equal, order, 1);
    let shorter =
        fx.b.ins()
            .icmp(IntCC::UnsignedLessThan, left_len, right_len);
    let zero8 = fx.int(types::I8, 0);
    let two = fx.int(types::I8, 2);
    let one = fx.int(types::I8, 1);
    let by_len = fx.b.ins().select(shorter, zero8, two);
    let by_len = fx.b.ins().select(same_len, one, by_len);
    fx.b.ins().select(open, by_len, order)
}

/// Whether two elements at addresses are equal: a key-value entry by key then value, another
/// through its `Eq`.
fn elements_equal(lo: &mut Lowering, fx: &mut Fx, ty: &Ty, a: Value, b: Value) -> Value {
    if lo.ck.is_std(ty, "mem::KeyValue") {
        let mut equal = fx.int(types::I8, 1);
        for name in ["key", "value"] {
            let field = lo.layouts.field(lo.ck, ty, None, name);
            let left = fx.offset(a, field.offset);
            let right = fx.offset(b, field.offset);
            let same = elements_equal(lo, fx, &field.ty, left, right);
            equal = fx.b.ins().band(equal, same);
        }
        return equal;
    }
    lo.equal(fx, ty, a, b)
}
