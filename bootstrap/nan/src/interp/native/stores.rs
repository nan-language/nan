//! The stores the interpreter supplies: allocators and budgets over the run's cells, the
//! storage of growable containers, and heaps, boxes, shared values, arenas and pools.
//!
//! A growable container holds its storage, the cell it draws through, its capacity and its
//! allocation. A type with interior mutability holds a reference to an extent of its own
//! holding such a container, which its operations replace through a read receiver.

use std::rc::Rc;

use super::super::budget::CHUNK;
use super::super::memory::layout;
use super::super::value::{Loc, Val, View};
use super::super::{Exec, Flow, Interpreter, Site};
use crate::check::db::ItemId;
use crate::check::ty::{Arg, Ty};
use crate::nir::Span;

/// The limit of the root cell an interpreter run's `testing::world()` allocator names.
pub const WORLD_LIMIT: u128 = 16 * 1024 * 1024;

/// The positions of a container's parts.
const STORAGE: usize = 0;
const CELL: usize = 1;
const CAPACITY: usize = 2;
const ALLOCATION: usize = 3;

/// The position of a pool's part after its container's: the indexes of its free slots in the
/// order they were freed.
const FREED: usize = 4;

/// The positions of a pool slot's parts: its generation and its occupant.
const GENERATION: usize = 0;
const OCCUPANT: usize = 1;

/// The generation of a retired pool slot, which never holds an occupant again.
const RETIRED: i128 = u32::MAX as i128;

/// The positions of a shared value's or cown's parts: its contents, its count, the cell its box
/// was drawn through and the box's allocation.
const CONTENTS: usize = 0;
const COUNT: usize = 1;
const SHARED_CELL: usize = 2;
const SHARED_ALLOCATION: usize = 3;

/// A member of `mem::Alloc`, `mem::Budget`, `mem::Heap`, `mem::Box`, `mem::Shared`,
/// `mem::Arena`, `mem::Id` or `mem::Pool`, or of their cursors; none for another member.
pub fn member(
    interp: &mut Interpreter,
    owner: &str,
    member: &str,
    type_args: &[Arg],
    args: &[Val],
    at: (ItemId, Span),
    site: Site,
) -> Option<Exec<Flow>> {
    let element = type_args
        .first()
        .and_then(Arg::ty)
        .cloned()
        .unwrap_or(Ty::Unit);
    let option = interp.ty_item("core", "Option");
    Some(Ok(Flow::Return(match (owner, member) {
        ("mem::Alloc", "split") => {
            let parent = cell_of(interp, &args[0]);
            let cell = interp.budgets.split(parent, args[1].int() as u128);
            let budget = interp.ty_item("mem", "Budget");
            Val::Struct(budget, Rc::new(vec![Val::Int(i128::from(cell))]))
        }
        ("mem::Budget" | "mem::Heap", "alloc") => allocator(interp, cell_of(interp, &args[0])),
        ("mem::Budget", "charged") => {
            Val::Int(interp.budgets.charged(cell_of(interp, &args[0])) as i128)
        }
        ("mem::Budget", "limit") => {
            Val::Int(interp.budgets.limit(cell_of(interp, &args[0])) as i128)
        }
        ("mem::Budget", "close") => {
            let cell = cell_of(interp, &args[0]);
            interp.budgets.close(cell);
            Val::Unit
        }
        ("mem::Heap", "of") => {
            let heap = interp.ty_item("mem", "Heap");
            let cell = cell_of(interp, &args[0]);
            Val::Struct(heap, Rc::new(vec![Val::Int(i128::from(cell))]))
        }
        ("mem::Heap" | "mem::Arena" | "mem::Pool", "boxed" | "try_boxed") => {
            let store = interp.deref(&args[0]);
            let cell = match owner {
                "mem::Heap" => cell_of(interp, &store),
                "mem::Arena" => held(interp, &store).fields()[CELL].int() as u32,
                _ => store.fields()[CELL].int() as u32,
            };
            let ty = type_args
                .last()
                .and_then(Arg::ty)
                .cloned()
                .unwrap_or(Ty::Unit);
            match boxed(interp, cell, args[1].clone(), &ty) {
                Some(boxed) => boxed,
                None => return Some(exhausted(interp, member == "try_boxed", at)),
            }
        }
        ("mem::Box", "contents" | "contents_mut") => interp.deref(&args[0]).fields()[0].clone(),
        ("mem::Box", "into_contents") => {
            let boxed = interp.deref(&args[0]);
            let value = interp.deref(&boxed.fields()[0]);
            unbox(interp, &boxed);
            value
        }
        ("mem::Shared" | "task::Cown", "of" | "try_of") => {
            let cell = cell_of(interp, &args[1]);
            match counted(interp, owner, cell, args[0].clone(), &element) {
                Some(shared) => shared,
                None => return Some(exhausted(interp, member == "try_of", at)),
            }
        }
        ("mem::Shared" | "task::Cown", "share") => {
            let shared = interp.deref(&args[0]);
            let count = place(&shared.fields()[COUNT]);
            let next = Val::Int(interp.memory.get(&count).int() + 1);
            interp.memory.set(&count, next);
            shared
        }
        ("mem::Shared", "contents") => interp.deref(&args[0]).fields()[CONTENTS].clone(),
        ("mem::Shared", "make_mut") => return Some(make_mut(interp, args, &element, at, site)),
        ("mem::Arena", "empty") => {
            let cell = cell_of(interp, &args[0]);
            let inner = container(interp, owner, Vec::new(), cell);
            interior(interp, owner, inner)
        }
        ("mem::Arena", "len") => Val::Int(entries(interp, &held(interp, &args[0])).len() as i128),
        ("mem::Arena", "push" | "try_push") => {
            let holder = holder(interp, &args[0]);
            let id = interp
                .memory
                .get(&storage_of(interp, &holder))
                .fields()
                .len();
            if append(interp, &holder, &args[1..2], &element).is_none() {
                return Some(exhausted(interp, member == "try_push", at));
            }
            let item = interp.ty_item("mem", "Id");
            Val::Struct(item, Rc::new(vec![Val::Int(id as i128)]))
        }
        ("mem::Arena", "at" | "at_mut" | "get") => {
            let arena = held(interp, &args[0]);
            let index = interp.deref(&args[1]).fields()[0].int() as usize;
            let storage = storage(interp, &arena);
            let found = index < interp.memory.get(&storage).fields().len();
            let entry = storage.with(index as u32);
            match (member, found) {
                ("get", true) => Val::some(option, Val::Ref(entry)),
                ("get", false) => Val::none(option),
                (_, true) => Val::Ref(entry),
                (_, false) => return Some(Err(interp.trap("trap.bounds", at))),
            }
        }
        ("mem::Arena", "cursor") => {
            let arena = held(interp, &args[0]);
            let storage = storage(interp, &arena);
            let len = interp.memory.get(&storage).fields().len();
            let cursor = interp.ty_item("mem", "ArenaCursor");
            let view = View {
                array: storage,
                start: 0,
                len,
            };
            Val::Struct(cursor, Rc::new(vec![Val::View(view)]))
        }
        ("mem::ArenaCursor", "next") => super::slice_next(interp, &args[0]),
        ("mem::Id", "of") => {
            let id = interp.ty_item("mem", "Id");
            Val::Struct(id, Rc::new(vec![args[0].clone()]))
        }
        ("mem::Id", "index") => interp.deref(&args[0]).fields()[0].clone(),
        ("mem::Pool", "empty") => {
            let cell = cell_of(interp, &args[0]);
            pool(interp, Vec::new(), cell)
        }
        ("mem::Pool", "len") => {
            let pool = interp.deref(&args[0]);
            let slots = entries(interp, &pool);
            Val::Int(slots.iter().filter(|slot| occupied(slot)).count() as i128)
        }
        ("mem::Pool", "insert" | "try_insert") => {
            let pool = place(&args[0]);
            match insert_slot(interp, &pool, args[1].clone(), &element) {
                Some(handle) => handle,
                None => return Some(exhausted(interp, member == "try_insert", at)),
            }
        }
        ("mem::Pool", "reserve" | "try_reserve") => {
            let pool = place(&args[0]);
            let current = interp.memory.get(&pool).clone();
            let free = current.fields()[FREED].fields().len();
            let needed =
                entries(interp, &current).len() + (args[1].int() as usize).saturating_sub(free);
            let Some(grown) = grow(interp, &current, needed, &element) else {
                return Some(exhausted(interp, member == "try_reserve", at));
            };
            interp.memory.set(&pool, grown);
            Val::Unit
        }
        ("mem::Pool", "contains" | "get" | "get_mut" | "at" | "at_mut" | "remove") => {
            let pool = interp.deref(&args[0]);
            let storage = storage(interp, &pool);
            let handle = interp.deref(&args[1]);
            let (index, generation) = (handle.fields()[0].int() as usize, &handle.fields()[1]);
            let live = interp
                .memory
                .get(&storage)
                .fields()
                .get(index)
                .is_some_and(|slot| occupied(slot) && slot.fields()[GENERATION] == *generation);
            let occupant = storage.with(index as u32).with(OCCUPANT as u32);
            match (member, live) {
                ("contains", _) => Val::Bool(live),
                ("get" | "get_mut", true) => Val::some(option, Val::Ref(occupant)),
                ("get" | "get_mut", false) => Val::none(option),
                (_, false) => return Some(Err(interp.trap("trap.stale-handle", at))),
                ("remove", true) => {
                    let removed = interp.memory.get(&occupant).clone();
                    let next = generation.int() + 1;
                    interp.memory.set(
                        &storage.with(index as u32),
                        slot(Val::Int(next), Val::Absent),
                    );
                    if next != RETIRED {
                        let freed = place(&args[0]).with(FREED as u32);
                        let mut indexes = interp.memory.get(&freed).fields().to_vec();
                        indexes.push(Val::Int(index as i128));
                        interp.memory.set(&freed, Val::Array(Rc::new(indexes)));
                    }
                    removed
                }
                _ => Val::Ref(occupant),
            }
        }
        ("mem::Pool", "cursor") => {
            let pool = interp.deref(&args[0]);
            let cursor = interp.ty_item("mem", "PoolCursor");
            let storage = Val::Ref(storage(interp, &pool));
            Val::Struct(cursor, Rc::new(vec![storage, Val::Int(0)]))
        }
        ("mem::PoolCursor", "next") => {
            let cursor = place(&args[0]);
            let Val::Struct(item, fields) = interp.memory.get(&cursor).clone() else {
                panic!("a pool cursor value");
            };
            let storage = place(&fields[0]);
            let slots = interp.memory.get(&storage).fields().to_vec();
            let start = fields[1].int() as usize;
            let next = (start..slots.len()).find(|index| occupied(&slots[*index]));
            let position = next.map_or(slots.len(), |index| index + 1);
            let state = vec![fields[0].clone(), Val::Int(position as i128)];
            interp
                .memory
                .set(&cursor, Val::Struct(item, Rc::new(state)));
            match next {
                Some(index) => {
                    let occupant = storage.with(index as u32).with(OCCUPANT as u32);
                    Val::some(option, Val::Ref(occupant))
                }
                None => Val::none(option),
            }
        }
        _ => return None,
    })))
}

/// A new pool drawing through a cell, holding slots none of which is free.
pub fn pool(interp: &mut Interpreter, slots: Vec<Val>, cell: u32) -> Val {
    let Val::Struct(item, parts) = container(interp, "mem::Pool", slots, cell) else {
        panic!("a pool value");
    };
    let mut parts = parts.as_ref().clone();
    parts.push(Val::Array(Rc::new(Vec::new())));
    Val::Struct(item, Rc::new(parts))
}

/// Places a value in the slot freed earliest of the pool at a place, or in a new slot when none
/// is free, and returns its handle; none on exhaustion.
fn insert_slot(interp: &mut Interpreter, pool: &Loc, value: Val, element: &Ty) -> Option<Val> {
    let current = interp.memory.get(pool).clone();
    let mut slots = entries(interp, &current);
    let index = match current.fields()[FREED].fields().split_first() {
        Some((first, rest)) => {
            let rest = Val::Array(Rc::new(rest.to_vec()));
            interp.memory.set(&pool.with(FREED as u32), rest);
            first.int() as usize
        }
        None => {
            let grown = grow(interp, &current, slots.len() + 1, element)?;
            interp.memory.set(pool, grown);
            slots.push(slot(Val::Int(0), Val::Absent));
            slots.len() - 1
        }
    };
    let generation = slots[index].fields()[GENERATION].clone();
    slots[index] = slot(generation.clone(), value);
    store(interp, pool, slots);
    let handle = interp.ty_item("mem", "Handle");
    Some(Val::Struct(
        handle,
        Rc::new(vec![Val::Int(index as i128), generation]),
    ))
}

fn slot(generation: Val, occupant: Val) -> Val {
    Val::Array(Rc::new(vec![generation, occupant]))
}

fn occupied(slot: &Val) -> bool {
    slot.fields()[OCCUPANT] != Val::Absent
}

/// A new box holding a value, drawn through a cell; none on exhaustion.
pub fn boxed(interp: &mut Interpreter, cell: u32, value: Val, ty: &Ty) -> Option<Val> {
    let (size, align) = layout(interp.ck, ty);
    let allocation = interp.budgets.allocate(cell, size.max(1), align)?;
    let extent = interp.memory.alloc(value, true, size);
    let item = interp.ty_item("mem", "Box");
    Some(Val::Struct(
        item,
        Rc::new(vec![
            Val::Ref(Loc {
                extent,
                path: Vec::new(),
            }),
            Val::Int(i128::from(allocation)),
        ]),
    ))
}

/// Ends a box whose contents moved out or were released: its memory returns and its extent
/// ends.
fn unbox(interp: &mut Interpreter, boxed: &Val) {
    interp.budgets.free(boxed.fields()[1].int() as u32);
    interp.memory.kill(place(&boxed.fields()[0]).extent);
}

/// A new shared value or cown, of the std type at `path`, holding a value with a count of 1,
/// drawn through a cell; none on exhaustion.
pub fn counted(
    interp: &mut Interpreter,
    path: &str,
    cell: u32,
    value: Val,
    ty: &Ty,
) -> Option<Val> {
    let contents = boxed(interp, cell, value, ty)?;
    let count = interp.memory.alloc(Val::Int(1), true, 0);
    let (module, name) = path.rsplit_once("::").expect("a std path");
    let item = interp.ty_item(module, name);
    let parts = contents.fields();
    Some(Val::Struct(
        item,
        Rc::new(vec![
            parts[0].clone(),
            Val::Ref(Loc {
                extent: count,
                path: Vec::new(),
            }),
            Val::Int(i128::from(cell)),
            parts[1].clone(),
        ]),
    ))
}

/// `make_mut`: a writable view of the contents, which a count above 1 first replaces with a
/// clone in a new box of its own.
fn make_mut(
    interp: &mut Interpreter,
    args: &[Val],
    element: &Ty,
    at: (ItemId, Span),
    site: Site,
) -> Exec<Flow> {
    let receiver = place(&args[0]);
    let current = interp.memory.get(&receiver).clone();
    let count = place(&current.fields()[COUNT]);
    if interp.memory.get(&count).int() > 1 {
        let Val::Fn(clone) = interp.deref(&args[1]) else {
            panic!("a clone function value");
        };
        let cell = current.fields()[SHARED_CELL].int() as u32;
        let alloc = allocator(interp, cell);
        let contents = current.fields()[CONTENTS].clone();
        let copied = match interp.invoke(&clone, vec![contents, alloc], at, site)? {
            Flow::Return(value) => value,
            failed => return Ok(failed),
        };
        let Some(replacement) = counted(interp, "mem::Shared", cell, copied, element) else {
            return Err(interp.trap("trap.exhausted", at));
        };
        release(interp, &current);
        interp.memory.set(&receiver, replacement);
    }
    let contents = interp.memory.get(&receiver).fields()[CONTENTS].clone();
    Ok(Flow::Return(contents))
}

/// The cell an allocator, budget or heap names.
pub fn cell_of(interp: &Interpreter, value: &Val) -> u32 {
    interp.deref(value).fields()[0].int() as u32
}

/// An allocator naming a cell.
pub fn allocator(interp: &Interpreter, cell: u32) -> Val {
    let alloc = interp.ty_item("mem", "Alloc");
    Val::Struct(alloc, Rc::new(vec![Val::Int(i128::from(cell))]))
}

/// The place a reference designates.
pub fn place(value: &Val) -> Loc {
    match value {
        Val::Ref(loc) => loc.clone(),
        other => panic!("a reference, not {other:?}"),
    }
}

/// A new container of the std type at `path`, drawing through a cell, holding elements
/// without memory of its own.
pub fn container(interp: &mut Interpreter, path: &str, elements: Vec<Val>, cell: u32) -> Val {
    let (module, name) = path.rsplit_once("::").expect("a std path");
    let item = interp.ty_item(module, name);
    let extent = interp.memory.alloc(Val::Array(Rc::new(elements)), true, 0);
    Val::Struct(
        item,
        Rc::new(vec![
            Val::Ref(Loc {
                extent,
                path: Vec::new(),
            }),
            Val::Int(i128::from(cell)),
            Val::Int(0),
            Val::Int(-1),
        ]),
    )
}

/// A value of the std type at `path` with interior mutability, holding a container in an extent
/// of its own.
pub fn interior(interp: &mut Interpreter, path: &str, inner: Val) -> Val {
    let (module, name) = path.rsplit_once("::").expect("a std path");
    let item = interp.ty_item(module, name);
    let extent = interp.memory.alloc(inner, true, 0);
    Val::Struct(
        item,
        Rc::new(vec![Val::Ref(Loc {
            extent,
            path: Vec::new(),
        })]),
    )
}

/// The place of the container a value with interior mutability holds.
pub fn holder(interp: &Interpreter, value: &Val) -> Loc {
    place(&interp.deref(value).fields()[0])
}

/// The container a value with interior mutability holds.
pub fn held(interp: &Interpreter, value: &Val) -> Val {
    interp.memory.get(&holder(interp, value)).clone()
}

/// The place of a container's storage.
pub fn storage(interp: &Interpreter, container: &Val) -> Loc {
    place(&interp.deref(container).fields()[STORAGE])
}

/// The place of the storage of the container at a place.
pub fn storage_of(interp: &Interpreter, container: &Loc) -> Loc {
    storage(interp, &interp.memory.get(container).clone())
}

/// A container's elements.
pub fn entries(interp: &Interpreter, container: &Val) -> Vec<Val> {
    interp
        .memory
        .get(&storage(interp, container))
        .fields()
        .to_vec()
}

/// A container's capacity.
pub fn capacity(interp: &Interpreter, container: &Val) -> usize {
    interp.deref(container).fields()[CAPACITY].int() as usize
}

/// A new container of the std type at `path` holding elements, drawing through a cell, its
/// capacity their number; none on exhaustion.
pub fn holding(
    interp: &mut Interpreter,
    path: &str,
    elements: Vec<Val>,
    cell: u32,
    element: &Ty,
) -> Option<Val> {
    let empty = container(interp, path, Vec::new(), cell);
    let sized = resized(interp, &empty, elements.len(), element)?;
    let storage = storage(interp, &sized);
    interp.memory.set(&storage, Val::Array(Rc::new(elements)));
    Some(sized)
}

/// Appends elements to the container at a place, growing it; none on exhaustion, which leaves
/// it unchanged.
pub fn append(interp: &mut Interpreter, place: &Loc, values: &[Val], element: &Ty) -> Option<()> {
    let current = interp.memory.get(place).clone();
    let mut elements = entries(interp, &current);
    let grown = grow(interp, &current, elements.len() + values.len(), element)?;
    elements.extend_from_slice(values);
    interp.memory.set(place, grown);
    store(interp, place, elements);
    Some(())
}

/// Replaces the elements of the container at a place.
pub fn store(interp: &mut Interpreter, place: &Loc, elements: Vec<Val>) {
    let storage = storage_of(interp, place);
    interp.memory.set(&storage, Val::Array(Rc::new(elements)));
}

/// The end of a growing operation finding memory exhausted: a `try_` form fails with
/// `mem::OutOfMemory.Exhausted`, another traps.
pub fn exhausted(interp: &Interpreter, fallible: bool, at: (ItemId, Span)) -> Exec<Flow> {
    if fallible {
        let error = interp.ty_item("mem", "OutOfMemory");
        return Ok(Flow::Fail(Val::Variant(error, 0, Rc::new(Vec::new()))));
    }
    Err(interp.trap("trap.exhausted", at))
}

/// The container with a capacity for `needed` elements: unchanged when it has one, otherwise
/// grown to the largest of twice its capacity, the needed length and 4; none on exhaustion.
pub fn grow(interp: &mut Interpreter, container: &Val, needed: usize, element: &Ty) -> Option<Val> {
    let capacity = capacity(interp, container);
    if needed <= capacity {
        return Some(interp.deref(container));
    }
    resized(
        interp,
        container,
        (2 * capacity).max(needed).max(4),
        element,
    )
}

/// The container with a capacity, in a new allocation replacing its old one; none on
/// exhaustion.
fn resized(
    interp: &mut Interpreter,
    container: &Val,
    capacity: usize,
    element: &Ty,
) -> Option<Val> {
    let Val::Struct(item, parts) = interp.deref(container) else {
        panic!("a container value");
    };
    let (size, align) = layout(interp.ck, element);
    let cell = parts[CELL].int() as u32;
    let bytes = size.saturating_mul(capacity as u64).max(1);
    let allocation = match capacity {
        0 => -1,
        _ => i128::from(
            interp
                .budgets
                .allocate(cell, bytes.min(u64::MAX / CHUNK), align)?,
        ),
    };
    if parts[ALLOCATION].int() >= 0 {
        interp.budgets.free(parts[ALLOCATION].int() as u32);
    }
    let mut parts = parts.as_ref().clone();
    parts[CAPACITY] = Val::Int(capacity as i128);
    parts[ALLOCATION] = Val::Int(allocation);
    Some(Val::Struct(item, Rc::new(parts)))
}

/// Releases what a value owns: the elements and memory of the std containers, stores, boxes
/// and shared values it holds, and what those elements own.
pub fn release(interp: &mut Interpreter, value: &Val) {
    match value {
        Val::Struct(item, fields) => match interp.ck.db.canonical(*item).as_str() {
            "mem::List" | "mem::Bytes" | "mem::Map" | "mem::HashMap" | "mem::Set"
            | "text::String" => release_container(interp, value),
            "mem::Pool" => {
                for slot in entries(interp, value) {
                    release(interp, &slot.fields()[OCCUPANT]);
                }
                release_container(interp, value);
            }
            path @ ("mem::Arena" | "mem::OnceMap" | "text::Interner") => {
                let inner = held(interp, value);
                if path == "text::Interner" {
                    for entry in entries(interp, &inner) {
                        interp.budgets.free(entry.fields()[1].int() as u32);
                    }
                }
                release_container(interp, &inner);
                interp.memory.kill(holder(interp, value).extent);
            }
            "mem::Box" => {
                let contents = interp.memory.get(&place(&fields[0])).clone();
                release(interp, &contents);
                unbox(interp, value);
            }
            "mem::Shared" | "task::Cown" => {
                let count = place(&fields[COUNT]);
                let left = interp.memory.get(&count).int() - 1;
                interp.memory.set(&count, Val::Int(left));
                if left == 0 {
                    let contents = interp.memory.get(&place(&fields[CONTENTS])).clone();
                    release(interp, &contents);
                    interp.budgets.free(fields[SHARED_ALLOCATION].int() as u32);
                }
            }
            "task::Sender" | "task::Receiver" => super::tasks::release(interp, value),
            "mem::ListCursor"
            | "mem::MapIntoCursor"
            | "mem::HashMapIntoCursor"
            | "mem::SetIntoCursor" => {
                let position = fields[1].int() as usize;
                for element in entries(interp, &fields[0]).iter().skip(position) {
                    release(interp, element);
                }
                release_container(interp, &fields[0]);
            }
            _ => {
                for field in fields.iter() {
                    release(interp, field);
                }
            }
        },
        Val::Variant(_, _, fields) | Val::Array(fields) => {
            for field in fields.iter() {
                release(interp, field);
            }
        }
        _ => {}
    }
}

/// Releases a container's elements, frees its allocation and ends its storage.
fn release_container(interp: &mut Interpreter, container: &Val) {
    for element in entries(interp, container) {
        release(interp, &element);
    }
    let parts = container.fields();
    if parts[ALLOCATION].int() >= 0 {
        interp.budgets.free(parts[ALLOCATION].int() as u32);
    }
    interp.memory.kill(place(&parts[STORAGE]).extent);
}

#[cfg(test)]
mod tests;
