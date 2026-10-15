//! The collections the interpreter supplies: lists, bytes, bounded lists, maps, hash maps, sets
//! and once maps, with their entries and cursors. A map's and a set's storage holds its entries
//! ordered by `cmp` of their keys, a hash map's and a once map's in insertion order; a bounded
//! list holds its elements inline.

use std::cmp::Ordering;
use std::rc::Rc;

use super::super::value::{Loc, Val, View};
use super::super::{Exec, Flow, Interpreter, Site};
use super::stores::{
    append, capacity, cell_of, container, entries, exhausted, grow, held, holder, holding,
    interior, place, release, storage, store,
};
use crate::check::db::ItemId;
use crate::check::ty::{Arg, Const, Scalar, Ty};
use crate::nir::{Callable, FnRef, Span};

/// A member of a collection, of `mem::Entry` or of a collection's cursor; none for another
/// member.
pub fn member(
    interp: &mut Interpreter,
    owner: &str,
    member: &str,
    type_args: &[Arg],
    args: &[Val],
    at: (ItemId, Span),
    site: Site,
) -> Option<Exec<Flow>> {
    let arg = |index: usize| {
        type_args
            .get(index)
            .and_then(Arg::ty)
            .cloned()
            .unwrap_or(Ty::Unit)
    };
    match owner {
        "mem::List" => list(interp, owner, member, &arg(0), args, at, site),
        "mem::Bytes" => list(
            interp,
            owner,
            member,
            &Ty::Scalar(Scalar::U8),
            args,
            at,
            site,
        ),
        "mem::Bounded" => bounded(interp, member, type_args, args, at),
        "mem::Map" | "mem::HashMap" | "mem::OnceMap" => {
            let entry = entry_ty(interp, arg(0), arg(1));
            map(interp, owner, member, &arg(0), &entry, args, at, site)
        }
        "mem::Set" => set(interp, member, &arg(0), args, at, site),
        "mem::Entry" => {
            let entry = place(&interp.deref(&args[0]).fields()[0]);
            let part = match member {
                "key" => 0,
                "value" | "value_mut" => 1,
                _ => return None,
            };
            Some(Ok(Flow::Return(Val::Ref(entry.with(part)))))
        }
        _ => cursor(interp, owner, member, args),
    }
}

/// A member of `mem::List` or `mem::Bytes`, whose elements have type `element`.
fn list(
    interp: &mut Interpreter,
    owner: &str,
    member: &str,
    element: &Ty,
    args: &[Val],
    at: (ItemId, Span),
    site: Site,
) -> Option<Exec<Flow>> {
    let option = interp.ty_item("core", "Option");
    let fallible = member.starts_with("try_");
    Some(Ok(Flow::Return(match member {
        "empty" => {
            let cell = cell_of(interp, &args[0]);
            container(interp, owner, Vec::new(), cell)
        }
        "zeroed" | "try_zeroed" => {
            let cell = cell_of(interp, &args[1]);
            let zeros = vec![Val::Int(0); args[0].int() as usize];
            match holding(interp, owner, zeros, cell, element) {
                Some(bytes) => bytes,
                None => return Some(exhausted(interp, fallible, at)),
            }
        }
        "len" => Val::Int(entries(interp, &args[0]).len() as i128),
        "capacity" => Val::Int(capacity(interp, &args[0]) as i128),
        "push" | "try_push" | "append" | "try_append" => {
            let values = match member {
                "push" | "try_push" => vec![args[1].clone()],
                _ => {
                    let data = view_of(interp, &args[1]);
                    interp.view_values(&data)
                }
            };
            if append(interp, &place(&args[0]), &values, element).is_none() {
                return Some(exhausted(interp, fallible, at));
            }
            Val::Unit
        }
        "insert" | "try_insert" => {
            let receiver = place(&args[0]);
            let current = interp.memory.get(&receiver).clone();
            let mut elements = entries(interp, &current);
            let index = args[1].int() as usize;
            if index > elements.len() {
                return Some(Err(interp.trap("trap.requires", at)));
            }
            let Some(grown) = grow(interp, &current, elements.len() + 1, element) else {
                return Some(exhausted(interp, fallible, at));
            };
            elements.insert(index, args[2].clone());
            interp.memory.set(&receiver, grown);
            store(interp, &receiver, elements);
            Val::Unit
        }
        "reserve" | "try_reserve" => {
            let receiver = place(&args[0]);
            let current = interp.memory.get(&receiver).clone();
            let needed = entries(interp, &current).len() + args[1].int() as usize;
            let Some(grown) = grow(interp, &current, needed, element) else {
                return Some(exhausted(interp, fallible, at));
            };
            interp.memory.set(&receiver, grown);
            Val::Unit
        }
        "pop" => {
            let receiver = place(&args[0]);
            let mut elements = entries(interp, &args[0]);
            let popped = elements.pop();
            store(interp, &receiver, elements);
            match popped {
                Some(value) => Val::some(option, value),
                None => Val::none(option),
            }
        }
        "remove" => {
            let receiver = place(&args[0]);
            let mut elements = entries(interp, &args[0]);
            let index = args[1].int() as usize;
            if index >= elements.len() {
                return Some(Err(interp.trap("trap.requires", at)));
            }
            let removed = elements.remove(index);
            store(interp, &receiver, elements);
            removed
        }
        "truncate" => {
            let receiver = place(&args[0]);
            let mut elements = entries(interp, &args[0]);
            let len = args[1].int() as usize;
            if len < elements.len() {
                for released in elements.split_off(len) {
                    release(interp, &released);
                }
                store(interp, &receiver, elements);
            }
            Val::Unit
        }
        "at" | "at_mut" => {
            let storage = storage(interp, &args[0]);
            let index = args[1].int();
            let len = interp.memory.get(&storage).fields().len();
            if index < 0 || index as usize >= len {
                return Some(Err(interp.bounds_trap(site, at)));
            }
            Val::Ref(storage.with(index as u32))
        }
        "view" | "view_mut" => Val::View(view_of(interp, &args[0])),
        "cursor_mut" => {
            let cursor = interp.ty_item("core", "SliceCursorMut");
            let view = view_of(interp, &args[0]);
            Val::Struct(cursor, Rc::new(vec![Val::View(view)]))
        }
        "into_cursor" => {
            let cursor = interp.ty_item("mem", "ListCursor");
            Val::Struct(cursor, Rc::new(vec![interp.deref(&args[0]), Val::Int(0)]))
        }
        "retain" => return Some(retain(interp, &args[0], &args[1], 1, at, site)),
        "sort_by" => return Some(sort_by(interp, &args[0], &args[1], at, site)),
        _ => return None,
    })))
}

/// A member of `mem::Bounded[T, N]`, which holds its elements inline as an array.
fn bounded(
    interp: &mut Interpreter,
    member: &str,
    type_args: &[Arg],
    args: &[Val],
    at: (ItemId, Span),
) -> Option<Exec<Flow>> {
    let option = interp.ty_item("core", "Option");
    let bound = match type_args.get(1) {
        Some(Arg::Const(Const::Int(bound))) => *bound as usize,
        _ => panic!("a bounded list's capacity"),
    };
    let elements = |interp: &Interpreter| interp.deref(&args[0]).fields()[0].fields().to_vec();
    Some(Ok(Flow::Return(match member {
        "empty" => {
            let item = interp.ty_item("mem", "Bounded");
            Val::Struct(item, Rc::new(vec![Val::Array(Rc::new(Vec::new()))]))
        }
        "len" => Val::Int(elements(interp).len() as i128),
        "push" | "pop" => {
            let array = place(&args[0]).with(0);
            let mut held = elements(interp);
            let result = match member {
                "push" if held.len() >= bound => {
                    return Some(Err(interp.trap("trap.requires", at)));
                }
                "push" => {
                    held.push(args[1].clone());
                    Val::Unit
                }
                _ => match held.pop() {
                    Some(value) => Val::some(option, value),
                    None => Val::none(option),
                },
            };
            interp.memory.set(&array, Val::Array(Rc::new(held)));
            result
        }
        "at" | "at_mut" => {
            let view = inline_view(interp, &args[0]);
            let index = args[1].int();
            if index < 0 || index as usize >= view.len {
                return Some(Err(interp.trap("trap.bounds", at)));
            }
            Val::Ref(view.array.with(index as u32))
        }
        "view" | "view_mut" => Val::View(inline_view(interp, &args[0])),
        "cursor_mut" => {
            let cursor = interp.ty_item("core", "SliceCursorMut");
            let view = inline_view(interp, &args[0]);
            Val::Struct(cursor, Rc::new(vec![Val::View(view)]))
        }
        "into_cursor" => {
            let cursor = interp.ty_item("mem", "BoundedCursor");
            let array = Val::Array(Rc::new(elements(interp)));
            Val::Struct(cursor, Rc::new(vec![array, Val::Int(0)]))
        }
        _ => return None,
    })))
}

/// A member of `mem::Map`, `mem::HashMap` or `mem::OnceMap`, whose keys have type `key` and
/// entries type `entry`.
#[allow(clippy::too_many_arguments)]
fn map(
    interp: &mut Interpreter,
    owner: &str,
    member: &str,
    key: &Ty,
    entry: &Ty,
    args: &[Val],
    at: (ItemId, Span),
    site: Site,
) -> Option<Exec<Flow>> {
    let option = interp.ty_item("core", "Option");
    let fallible = member.starts_with("try_");
    let ordered = owner == "mem::Map";
    let once = owner == "mem::OnceMap";
    let receiver_container = |interp: &Interpreter| match once {
        true => held(interp, &args[0]),
        false => interp.deref(&args[0]),
    };
    let mut flow = || -> Exec<Flow> {
        Ok(Flow::Return(match member {
            "empty" => {
                let cell = cell_of(interp, args.last().expect("an allocator"));
                let inner = container(interp, owner, Vec::new(), cell);
                match once {
                    true => interior(interp, owner, inner),
                    false => inner,
                }
            }
            "len" => Val::Int(entries(interp, &receiver_container(interp)).len() as i128),
            "insert" | "try_insert" => {
                let receiver = match once {
                    true => holder(interp, &args[0]),
                    false => place(&args[0]),
                };
                let current = interp.memory.get(&receiver).clone();
                let sought = interp.deref(&args[1]);
                let (index, found) = find(interp, &current, key, &sought, ordered, true, at, site)?;
                let slot = storage(interp, &current).with(index as u32);
                if found {
                    release(interp, &args[1]);
                    if once {
                        release(interp, &args[2]);
                        return Ok(Flow::Return(Val::Ref(slot.with(1))));
                    }
                    let old = interp.memory.get(&slot.with(1)).clone();
                    interp.memory.set(&slot.with(1), args[2].clone());
                    return Ok(Flow::Return(Val::some(option, old)));
                }
                let mut held = entries(interp, &current);
                let Some(grown) = grow(interp, &current, held.len() + 1, entry) else {
                    release(interp, &args[1]);
                    release(interp, &args[2]);
                    return exhausted(interp, fallible, at);
                };
                let keyed = key_value(interp, args[1].clone(), args[2].clone());
                held.insert(index, keyed);
                interp.memory.set(&receiver, grown);
                store(interp, &receiver, held);
                match once {
                    true => Val::Ref(slot.with(1)),
                    false => Val::none(option),
                }
            }
            "remove" => {
                let receiver = place(&args[0]);
                let current = interp.memory.get(&receiver).clone();
                let sought = interp.deref(&args[1]);
                let (index, found) = find(interp, &current, key, &sought, ordered, true, at, site)?;
                if !found {
                    return Ok(Flow::Return(Val::none(option)));
                }
                let mut held = entries(interp, &current);
                let removed = held.remove(index);
                store(interp, &receiver, held);
                release(interp, &removed.fields()[0]);
                Val::some(option, removed.fields()[1].clone())
            }
            "get" | "get_mut" | "at" | "at_mut" => {
                let current = receiver_container(interp);
                let sought = interp.deref(&args[1]);
                let (index, found) = find(interp, &current, key, &sought, ordered, true, at, site)?;
                let value = storage(interp, &current).with(index as u32).with(1);
                match (member, found) {
                    ("get" | "get_mut", true) => Val::some(option, Val::Ref(value)),
                    ("get" | "get_mut", false) => Val::none(option),
                    (_, true) => Val::Ref(value),
                    (_, false) => return Err(interp.trap("trap.requires", at)),
                }
            }
            "get_by" | "get_by_mut" => {
                let current = interp.deref(&args[0]);
                match search(interp, &current, &args[1], true, at, site)? {
                    Some(index) => {
                        let value = storage(interp, &current).with(index as u32).with(1);
                        Val::some(option, Val::Ref(value))
                    }
                    None => Val::none(option),
                }
            }
            "cursor" | "cursor_mut" => {
                let name = match (ordered, member) {
                    (true, "cursor") => "MapCursor",
                    (true, _) => "MapCursorMut",
                    (false, "cursor") => "HashMapCursor",
                    (false, _) => "HashMapCursorMut",
                };
                let cursor = interp.ty_item("mem", name);
                let view = view_of(interp, &args[0]);
                Val::Struct(cursor, Rc::new(vec![Val::View(view)]))
            }
            "into_cursor" => {
                let name = match ordered {
                    true => "MapIntoCursor",
                    false => "HashMapIntoCursor",
                };
                let cursor = interp.ty_item("mem", name);
                Val::Struct(cursor, Rc::new(vec![interp.deref(&args[0]), Val::Int(0)]))
            }
            "retain" => return retain(interp, &args[0], &args[1], 2, at, site),
            _ => unreachable!("a map member"),
        }))
    };
    match member {
        "empty" | "len" | "insert" | "try_insert" | "remove" | "get" | "get_mut" | "at"
        | "at_mut" | "get_by" | "get_by_mut" | "cursor" | "cursor_mut" | "into_cursor"
        | "retain" => Some(flow()),
        _ => None,
    }
}

/// A member of `mem::Set[T]`, whose values have type `value`.
fn set(
    interp: &mut Interpreter,
    member: &str,
    value: &Ty,
    args: &[Val],
    at: (ItemId, Span),
    site: Site,
) -> Option<Exec<Flow>> {
    let fallible = member.starts_with("try_");
    let mut flow = || -> Exec<Flow> {
        Ok(Flow::Return(match member {
            "empty" => {
                let cell = cell_of(interp, &args[0]);
                container(interp, "mem::Set", Vec::new(), cell)
            }
            "len" => Val::Int(entries(interp, &args[0]).len() as i128),
            "insert" | "try_insert" | "remove" => {
                let receiver = place(&args[0]);
                let current = interp.memory.get(&receiver).clone();
                let sought = interp.deref(&args[1]);
                let (index, found) = find(interp, &current, value, &sought, true, false, at, site)?;
                let mut held = entries(interp, &current);
                match (member, found) {
                    ("remove", false) => Val::Bool(false),
                    ("remove", true) => {
                        let removed = held.remove(index);
                        store(interp, &receiver, held);
                        release(interp, &removed);
                        Val::Bool(true)
                    }
                    (_, true) => {
                        release(interp, &args[1]);
                        Val::Bool(false)
                    }
                    _ => {
                        let Some(grown) = grow(interp, &current, held.len() + 1, value) else {
                            release(interp, &args[1]);
                            return exhausted(interp, fallible, at);
                        };
                        held.insert(index, args[1].clone());
                        interp.memory.set(&receiver, grown);
                        store(interp, &receiver, held);
                        Val::Bool(true)
                    }
                }
            }
            "contains_by" => {
                let current = interp.deref(&args[0]);
                Val::Bool(search(interp, &current, &args[1], false, at, site)?.is_some())
            }
            "cursor" => {
                let cursor = interp.ty_item("mem", "SetCursor");
                let view = view_of(interp, &args[0]);
                Val::Struct(cursor, Rc::new(vec![Val::View(view)]))
            }
            "into_cursor" => {
                let cursor = interp.ty_item("mem", "SetIntoCursor");
                Val::Struct(cursor, Rc::new(vec![interp.deref(&args[0]), Val::Int(0)]))
            }
            "retain" => return retain(interp, &args[0], &args[1], 1, at, site),
            _ => unreachable!("a set member"),
        }))
    };
    match member {
        "empty" | "len" | "insert" | "try_insert" | "remove" | "contains_by" | "cursor"
        | "into_cursor" | "retain" => Some(flow()),
        _ => None,
    }
}

/// The `next` of a collection's cursor; none for another member.
fn cursor(interp: &mut Interpreter, owner: &str, member: &str, args: &[Val]) -> Option<Exec<Flow>> {
    if member != "next" {
        return None;
    }
    let next = match owner {
        "mem::SetCursor" => super::slice_next(interp, &args[0]),
        "mem::BoundedCursor" => super::array_next(interp, &args[0]),
        "mem::MapCursor" | "mem::MapCursorMut" | "mem::HashMapCursor" | "mem::HashMapCursorMut" => {
            let next = super::slice_next(interp, &args[0]);
            match next {
                Val::Variant(option, 0, payload) => {
                    let entry = interp.ty_item("mem", "Entry");
                    let entry = Val::Struct(entry, Rc::new(vec![payload[0].clone()]));
                    Val::some(option, entry)
                }
                none => none,
            }
        }
        "mem::ListCursor"
        | "mem::MapIntoCursor"
        | "mem::HashMapIntoCursor"
        | "mem::SetIntoCursor" => {
            let cursor = place(&args[0]);
            let Val::Struct(item, fields) = interp.memory.get(&cursor).clone() else {
                panic!("an owning cursor value");
            };
            let option = interp.ty_item("core", "Option");
            let position = fields[1].int() as usize;
            match entries(interp, &fields[0]).get(position).cloned() {
                Some(element) => {
                    let state = vec![fields[0].clone(), Val::Int(position as i128 + 1)];
                    interp
                        .memory
                        .set(&cursor, Val::Struct(item, Rc::new(state)));
                    Val::some(option, element)
                }
                None => Val::none(option),
            }
        }
        _ => return None,
    };
    Some(Ok(Flow::Return(next)))
}

/// `retain`: calls `keep` on each element in order, with its key and value for an entry of
/// `parts` parts, and releases those it rejects.
fn retain(
    interp: &mut Interpreter,
    receiver: &Val,
    keep: &Val,
    parts: usize,
    at: (ItemId, Span),
    site: Site,
) -> Exec<Flow> {
    let Val::Fn(keep) = interp.deref(keep) else {
        panic!("a keep function value");
    };
    let receiver = place(receiver);
    let mut kept = Vec::new();
    for element in entries(interp, &interp.memory.get(&receiver).clone()) {
        let args = match parts {
            1 => vec![element.clone()],
            _ => element.fields().to_vec(),
        };
        let keeps = match interp.invoke(&keep, args, at, site)? {
            Flow::Return(value) => value.bool(),
            failed => return Ok(failed),
        };
        match keeps {
            true => kept.push(element),
            false => release(interp, &element),
        }
    }
    store(interp, &receiver, kept);
    Ok(Flow::Return(Val::Unit))
}

/// `sort_by`: the stable binary insertion sort its section states, calling `order` in its
/// order.
fn sort_by(
    interp: &mut Interpreter,
    receiver: &Val,
    order: &Val,
    at: (ItemId, Span),
    site: Site,
) -> Exec<Flow> {
    let Val::Fn(order) = interp.deref(order) else {
        panic!("an order function value");
    };
    let receiver = place(receiver);
    let mut elements = entries(interp, &interp.memory.get(&receiver).clone());
    for index in 1..elements.len() {
        let moved = elements[index].clone();
        let (mut low, mut high) = (0, index);
        while low < high {
            let middle = (low + high) / 2;
            let args = vec![moved.clone(), elements[middle].clone()];
            match ordering(&returned(interp.invoke(&order, args, at, site)?)) {
                Ordering::Less => high = middle,
                _ => low = middle + 1,
            }
        }
        let element = elements.remove(index);
        elements.insert(low, element);
    }
    store(interp, &receiver, elements);
    Ok(Flow::Return(Val::Unit))
}

/// The ordered lookup its section states: a binary search calling `order` on the keys, or on
/// the values of a set, of a container; the index found, if any.
fn search(
    interp: &mut Interpreter,
    container: &Val,
    order: &Val,
    keyed: bool,
    at: (ItemId, Span),
    site: Site,
) -> Exec<Option<usize>> {
    let Val::Fn(order) = interp.deref(order) else {
        panic!("an order function value");
    };
    let held = entries(interp, container);
    let (mut low, mut high) = (0, held.len());
    while low < high {
        let middle = (low + high) / 2;
        let sought = match keyed {
            true => held[middle].fields()[0].clone(),
            false => held[middle].clone(),
        };
        match ordering(&returned(interp.invoke(&order, vec![sought], at, site)?)) {
            Ordering::Less => low = middle + 1,
            Ordering::Greater => high = middle,
            Ordering::Equal => return Ok(Some(middle)),
        }
    }
    Ok(None)
}

/// Where a key lies in a container of entries, or of values when not `keyed`: the index of the
/// one holding it and `true`, or the index where it would go and `false`. An ordered container
/// compares keys by `cmp`, another by `eq`.
#[allow(clippy::too_many_arguments)]
fn find(
    interp: &mut Interpreter,
    container: &Val,
    key: &Ty,
    sought: &Val,
    ordered: bool,
    keyed: bool,
    at: (ItemId, Span),
    site: Site,
) -> Exec<(usize, bool)> {
    let held = entries(interp, container);
    let key_of = |element: &Val| match keyed {
        true => element.fields()[0].clone(),
        false => element.clone(),
    };
    if !ordered {
        for (index, entry) in held.iter().enumerate() {
            let args = vec![key_of(entry), sought.clone()];
            if conformance(interp, "Eq", "eq", key, args, at, site)?.bool() {
                return Ok((index, true));
            }
        }
        return Ok((held.len(), false));
    }
    let (mut low, mut high) = (0, held.len());
    while low < high {
        let middle = (low + high) / 2;
        let args = vec![key_of(&held[middle]), sought.clone()];
        match ordering(&conformance(interp, "Ord", "cmp", key, args, at, site)?) {
            Ordering::Less => low = middle + 1,
            Ordering::Greater => high = middle,
            Ordering::Equal => return Ok((middle, true)),
        }
    }
    Ok((low, false))
}

/// Calls a method of a core interface on values of a type, through its conformance.
pub fn conformance(
    interp: &mut Interpreter,
    interface: &str,
    method: &str,
    ty: &Ty,
    args: Vec<Val>,
    at: (ItemId, Span),
    site: Site,
) -> Exec<Val> {
    let interface = interp.ty_item("core", interface);
    let index = interp
        .ck
        .interface_method(interface, method)
        .expect("a core interface method");
    let fn_ref = FnRef {
        callable: Callable::Method {
            interface,
            method: index as u32,
        },
        args: Rc::from(vec![Arg::Ty(ty.clone())]),
    };
    Ok(returned(interp.call_function(&fn_ref, args, at, site)?))
}

/// The value a call of a function that cannot fail returned.
fn returned(flow: Flow) -> Val {
    match flow {
        Flow::Return(value) => value,
        _ => panic!("a function that cannot fail ending otherwise than by returning"),
    }
}

/// The order a `core::Ordering` value states.
pub fn ordering(value: &Val) -> Ordering {
    match value {
        Val::Variant(_, 0, _) => Ordering::Less,
        Val::Variant(_, 1, _) => Ordering::Equal,
        Val::Variant(_, 2, _) => Ordering::Greater,
        Val::Ref(_) => panic!("an ordering reference"),
        other => panic!("an ordering, not {other:?}"),
    }
}

/// The type of a map's entries, `mem::KeyValue[K, V]`.
fn entry_ty(interp: &Interpreter, key: Ty, value: Ty) -> Ty {
    let item = interp.ty_item("mem", "KeyValue");
    Ty::item(item, vec![Arg::Ty(key), Arg::Ty(value)])
}

/// A `mem::KeyValue` of a key and a value.
fn key_value(interp: &Interpreter, key: Val, value: Val) -> Val {
    let item = interp.ty_item("mem", "KeyValue");
    Val::Struct(item, Rc::new(vec![key, value]))
}

/// The view of a container's elements, or of a slice's.
fn view_of(interp: &mut Interpreter, value: &Val) -> View {
    match interp.deref(value) {
        Val::Struct(..) => {
            let storage = storage(interp, value);
            let len = interp.memory.get(&storage).fields().len();
            View {
                array: storage,
                start: 0,
                len,
            }
        }
        _ => interp.view(value),
    }
}

/// The view of a bounded list's elements: in its place for a reference, else in a new
/// read-only extent.
fn inline_view(interp: &mut Interpreter, receiver: &Val) -> View {
    let array = match receiver {
        Val::Ref(loc) => loc.with(0),
        value => {
            let elements = value.fields()[0].clone();
            Loc {
                extent: interp.memory.alloc(elements, false, 0),
                path: Vec::new(),
            }
        }
    };
    let len = interp.memory.get(&array).fields().len();
    View {
        array,
        start: 0,
        len,
    }
}
