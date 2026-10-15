//! The stores the interpreter supplies: allocators and budgets over the run's cells, and the
//! growable containers `mem::List`, `mem::Bytes` and `mem::Pool`, each holding its storage, the
//! cell it draws through, its capacity and its allocation.

use std::rc::Rc;

use super::super::budget::CHUNK;
use super::super::memory::layout;
use super::super::value::{Loc, Val};
use super::super::{Exec, Flow, Interpreter};
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

/// The positions of a pool slot's parts: its generation and its occupant.
const GENERATION: usize = 0;
const OCCUPANT: usize = 1;

/// A member of `mem::Alloc`, `mem::Budget`, `mem::List`, `mem::Bytes` or `mem::Pool`; none for
/// another member.
pub fn member(
    interp: &mut Interpreter,
    owner: &str,
    member: &str,
    type_args: &[Arg],
    args: &[Val],
    at: (ItemId, Span),
) -> Option<Exec<Flow>> {
    let element = type_args
        .first()
        .and_then(Arg::ty)
        .cloned()
        .unwrap_or(Ty::Unit);
    Some(Ok(Flow::Return(match (owner, member) {
        ("mem::Alloc", "split") => {
            let parent = cell_of(interp, &args[0]);
            let cell = interp.budgets.split(parent, args[1].int() as u128);
            let budget = interp.ty_item("mem", "Budget");
            Val::Struct(budget, Rc::new(vec![Val::Int(i128::from(cell))]))
        }
        ("mem::Budget", "alloc") => allocator(interp, cell_of(interp, &args[0])),
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
        ("mem::List" | "mem::Bytes" | "mem::Pool", "empty") => {
            let cell = cell_of(interp, &args[0]);
            container(interp, owner, Vec::new(), cell)
        }
        ("mem::Bytes", "zeroed") => {
            let cell = cell_of(interp, &args[1]);
            let len = args[0].int() as usize;
            let value = container(interp, owner, Vec::new(), cell);
            let value = match grow(
                interp,
                &value,
                len,
                &Ty::Scalar(crate::check::ty::Scalar::U8),
            ) {
                Some(value) => value,
                None => return Some(Err(interp.trap("trap.exhausted", at))),
            };
            let storage = storage(interp, &value);
            interp
                .memory
                .set(&storage, Val::Array(Rc::new(vec![Val::Int(0); len])));
            value
        }
        ("mem::List" | "mem::Bytes", "push" | "try_push") => {
            let Val::Ref(place) = &args[0] else {
                panic!("a container passed by mut");
            };
            let current = interp.memory.get(place).clone();
            let mut elements = interp
                .memory
                .get(&storage(interp, &current))
                .fields()
                .to_vec();
            let needed = elements.len() + 1;
            let grown = match grow(interp, &current, needed, &element) {
                Some(grown) => grown,
                None if member == "try_push" => {
                    let error = interp.ty_item("mem", "OutOfMemory");
                    return Some(Ok(Flow::Fail(Val::Variant(error, 0, Rc::new(Vec::new())))));
                }
                None => return Some(Err(interp.trap("trap.exhausted", at))),
            };
            elements.push(args[1].clone());
            let storage = storage(interp, &grown);
            interp.memory.set(&storage, Val::Array(Rc::new(elements)));
            interp.memory.set(place, grown);
            Val::Unit
        }
        ("mem::Pool", "insert") => {
            let Val::Ref(place) = &args[0] else {
                panic!("a pool passed by mut");
            };
            let current = interp.memory.get(place).clone();
            let mut slots = interp
                .memory
                .get(&storage(interp, &current))
                .fields()
                .to_vec();
            let free = slots
                .iter()
                .position(|slot| slot.fields()[OCCUPANT] == Val::Absent);
            let index = match free {
                Some(index) => index,
                None => {
                    let grown = match grow(interp, &current, slots.len() + 1, &element) {
                        Some(grown) => grown,
                        None => return Some(Err(interp.trap("trap.exhausted", at))),
                    };
                    interp.memory.set(place, grown);
                    slots.push(Val::Array(Rc::new(vec![Val::Int(0), Val::Absent])));
                    slots.len() - 1
                }
            };
            let generation = slots[index].fields()[GENERATION].clone();
            slots[index] = Val::Array(Rc::new(vec![generation.clone(), args[1].clone()]));
            let current = interp.memory.get(place).clone();
            let storage = storage(interp, &current);
            interp.memory.set(&storage, Val::Array(Rc::new(slots)));
            let handle = interp.ty_item("mem", "Handle");
            Val::Struct(handle, Rc::new(vec![Val::Int(index as i128), generation]))
        }
        ("mem::Pool", "remove" | "contains") => {
            let pool = interp.deref(&args[0]);
            let storage = storage(interp, &pool);
            let mut slots = interp.memory.get(&storage).fields().to_vec();
            let handle = interp.deref(&args[1]);
            let (index, generation) = (
                handle.fields()[0].int() as usize,
                handle.fields()[1].clone(),
            );
            let live = slots[index].fields()[GENERATION] == generation
                && slots[index].fields()[OCCUPANT] != Val::Absent;
            if member == "contains" {
                Val::Bool(live)
            } else {
                if !live {
                    return Some(Err(interp.trap("trap.stale-handle", at)));
                }
                let occupant = slots[index].fields()[OCCUPANT].clone();
                let next = Val::Int(slots[index].fields()[GENERATION].int() + 1);
                slots[index] = Val::Array(Rc::new(vec![next, Val::Absent]));
                interp.memory.set(&storage, Val::Array(Rc::new(slots)));
                occupant
            }
        }
        _ => return None,
    })))
}

/// The cell an allocator or budget names.
fn cell_of(interp: &Interpreter, value: &Val) -> u32 {
    interp.deref(value).fields()[0].int() as u32
}

/// An allocator naming a cell.
pub fn allocator(interp: &Interpreter, cell: u32) -> Val {
    let alloc = interp.ty_item("mem", "Alloc");
    Val::Struct(alloc, Rc::new(vec![Val::Int(i128::from(cell))]))
}

/// A new container of the std type at `path`, drawing through a cell, holding elements
/// without memory of its own.
fn container(interp: &mut Interpreter, path: &str, elements: Vec<Val>, cell: u32) -> Val {
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

/// The place of a container's storage.
pub fn storage(interp: &Interpreter, container: &Val) -> Loc {
    match &interp.deref(container).fields()[STORAGE] {
        Val::Ref(loc) => loc.clone(),
        other => panic!("a container's storage, not {other:?}"),
    }
}

/// The container with a capacity for `needed` elements: unchanged when it has one, otherwise
/// grown to the largest of twice its capacity, the needed length and 4 in a new allocation
/// that replaces its old one; none on exhaustion.
fn grow(interp: &mut Interpreter, container: &Val, needed: usize, element: &Ty) -> Option<Val> {
    let Val::Struct(item, parts) = container else {
        panic!("a container value");
    };
    let capacity = parts[CAPACITY].int() as usize;
    if needed <= capacity {
        return Some(container.clone());
    }
    let grown = (2 * capacity).max(needed).max(4);
    let (size, align) = layout(interp.ck, element);
    let cell = parts[CELL].int() as u32;
    let bytes = (size * grown as u64).max(1);
    let allocation = interp
        .budgets
        .allocate(cell, bytes.min(u64::MAX / CHUNK), align)?;
    if parts[ALLOCATION].int() >= 0 {
        interp.budgets.free(parts[ALLOCATION].int() as u32);
    }
    let mut parts = parts.as_ref().clone();
    parts[CAPACITY] = Val::Int(grown as i128);
    parts[ALLOCATION] = Val::Int(i128::from(allocation));
    Some(Val::Struct(*item, Rc::new(parts)))
}

/// Releases a container: its allocation is freed and its storage ends.
pub fn release(interp: &mut Interpreter, container: &Val) {
    let parts = container.fields();
    if parts.len() > ALLOCATION && parts[ALLOCATION].int() >= 0 {
        interp.budgets.free(parts[ALLOCATION].int() as u32);
    }
    if let Some(Val::Ref(loc)) = parts.first() {
        interp.memory.kill(loc.extent);
    }
}
