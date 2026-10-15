//! `hw::Ptr` in interpreter runs: pointers with provenance into the run's extents, each access
//! checked against the Ptr rules in their order.

use super::super::memory::layout;
use std::rc::Rc;

use super::super::value::{Loc, Provenance, Ptr, Val, wrap};
use super::super::{End, Exec, Flow, Interpreter};
use crate::check::db::ItemId;
use crate::check::ty::{Arg, Ty};
use crate::nir::Span;

/// A member of `hw::Ptr[T]`; none for another member.
pub fn member(
    interp: &mut Interpreter,
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
    let (size, align) = layout(interp.ck, &element);
    let value = match member {
        "of_addr" => Val::Ptr(Ptr {
            address: args[0].int() as u64,
            provenance: None,
        }),
        "of_view" | "of_view_mut" => {
            let view = interp.view(&args[0]);
            let extent = interp.memory.extent(view.array.extent);
            Val::Ptr(Ptr {
                address: extent.address + view.start as u64 * size,
                provenance: Some(Provenance {
                    extent: view.array.extent,
                    read_only: member == "of_view",
                }),
            })
        }
        "of" | "of_mut" => {
            let extent = match &args[0] {
                Val::Ref(loc) if loc.path.is_empty() => loc.extent,
                other => {
                    let value = interp.deref(other);
                    interp.memory.alloc(value, false, size)
                }
            };
            Val::Ptr(Ptr {
                address: interp.memory.extent(extent).address,
                provenance: Some(Provenance {
                    extent,
                    read_only: member == "of",
                }),
            })
        }
        "addr" => Val::Int(i128::from(pointer(interp, &args[0]).address)),
        "is_aligned" => Val::Bool(
            pointer(interp, &args[0])
                .address
                .is_multiple_of(align.max(1)),
        ),
        "offset" => {
            let ptr = pointer(interp, &args[0]);
            let address = (i128::from(ptr.address) + args[1].int() * i128::from(size))
                .rem_euclid(1 << 64) as u64;
            if let Some(provenance) = &ptr.provenance {
                let extent = interp.memory.extent(provenance.extent);
                if address < extent.address || address > extent.address + extent.size {
                    return Some(Err(violation(interp, "bounds", at)));
                }
            }
            Val::Ptr(Ptr { address, ..ptr })
        }
        "cast" => Val::Ptr(pointer(interp, &args[0])),
        "read" | "atomic_load" => match load(interp, &args[0], size, align, at) {
            Ok(value) => value,
            Err(end) => return Some(Err(end)),
        },
        "write" | "atomic_store" => match store(interp, &args[0], args[1].clone(), size, align, at)
        {
            Ok(()) => Val::Unit,
            Err(end) => return Some(Err(end)),
        },
        "atomic_swap" | "atomic_compare_swap" | "atomic_wrapping_add" => {
            let previous = match load(interp, &args[0], size, align, at) {
                Ok(value) => value,
                Err(end) => return Some(Err(end)),
            };
            let (stored, result) = match member {
                "atomic_swap" => (Some(args[1].clone()), previous),
                "atomic_wrapping_add" => {
                    let scalar = element.scalar().expect("an integer pointee");
                    let sum = wrap(previous.int() + interp.deref(&args[1]).int(), scalar);
                    (Some(Val::Int(sum)), previous)
                }
                _ => {
                    let result = interp.ty_item("core", "Result");
                    match interp.equal(&previous, &args[1]) {
                        true => (
                            Some(args[2].clone()),
                            Val::Variant(result, 0, Rc::new(vec![previous])),
                        ),
                        false => (None, Val::Variant(result, 1, Rc::new(vec![previous]))),
                    }
                }
            };
            if let Some(stored) = stored
                && let Err(end) = store(interp, &args[0], stored, size, align, at)
            {
                return Some(Err(end));
            }
            result
        }
        "copy" => {
            let (from, to) = (pointer(interp, &args[0]), pointer(interp, &args[1]));
            let at_element = |ptr: &Ptr, index: u64| {
                Val::Ptr(Ptr {
                    address: ptr.address + index * size,
                    ..ptr.clone()
                })
            };
            let mut copied = Vec::new();
            for index in 0..args[2].int() as u64 {
                match load(interp, &at_element(&from, index), size, align, at) {
                    Ok(value) => copied.push(value),
                    Err(end) => return Some(Err(end)),
                }
            }
            for (index, value) in copied.into_iter().enumerate() {
                let target = at_element(&to, index as u64);
                if let Err(end) = store(interp, &target, value, size, align, at) {
                    return Some(Err(end));
                }
            }
            Val::Unit
        }
        _ => return None,
    };
    Some(Ok(Flow::Return(value)))
}

/// The value a pointer reads, after the Ptr checks and the initialisation rule.
fn load(
    interp: &Interpreter,
    pointer_value: &Val,
    size: u64,
    align: u64,
    at: (ItemId, Span),
) -> Exec<Val> {
    let ptr = pointer(interp, pointer_value);
    let loc = access(interp, &ptr, size, align, false, at)?;
    let value = interp.memory.get(&loc).clone();
    if value == Val::Absent {
        return Err(violation(interp, "initialisation", at));
    }
    Ok(value)
}

/// Writes a value through a pointer, after the Ptr checks.
fn store(
    interp: &mut Interpreter,
    pointer_value: &Val,
    value: Val,
    size: u64,
    align: u64,
    at: (ItemId, Span),
) -> Exec<()> {
    let ptr = pointer(interp, pointer_value);
    let loc = access(interp, &ptr, size, align, true, at)?;
    interp.memory.set(&loc, value);
    Ok(())
}

fn pointer(interp: &Interpreter, value: &Val) -> Ptr {
    match interp.deref(value) {
        Val::Ptr(ptr) => ptr,
        other => panic!("a pointer, not {other:?}"),
    }
}

fn violation(interp: &Interpreter, rule: &str, at: (ItemId, Span)) -> End {
    End::Violation(interp.report(&format!("{rule} violation"), at))
}

/// The place an access of `size` bytes through a pointer reaches, after the provenance, bounds,
/// alignment, read-only and layout rules in their order.
fn access(
    interp: &Interpreter,
    ptr: &Ptr,
    size: u64,
    align: u64,
    write: bool,
    at: (ItemId, Span),
) -> Exec<Loc> {
    let Some(provenance) = &ptr.provenance else {
        return Err(End::Uninterpretable(interp.report("uninterpretable", at)));
    };
    let extent = interp.memory.extent(provenance.extent);
    if !extent.live {
        return Err(violation(interp, "provenance", at));
    }
    if ptr.address < extent.address || ptr.address + size > extent.address + extent.size {
        return Err(violation(interp, "bounds", at));
    }
    if !ptr.address.is_multiple_of(align.max(1)) {
        return Err(violation(interp, "alignment", at));
    }
    if write && (!extent.writable || provenance.read_only) {
        return Err(violation(interp, "read-only", at));
    }
    let offset = ptr.address - extent.address;
    match &extent.value {
        Val::Array(elements) if size > 0 && offset.is_multiple_of(size) => {
            let index = (offset / size) as usize;
            if index >= elements.len() {
                return Err(violation(interp, "layout", at));
            }
            Ok(Loc {
                extent: provenance.extent,
                path: vec![index as u32],
            })
        }
        _ if offset == 0 => Ok(Loc {
            extent: provenance.extent,
            path: Vec::new(),
        }),
        _ => Err(violation(interp, "layout", at)),
    }
}
