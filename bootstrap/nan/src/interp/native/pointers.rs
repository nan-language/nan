//! `hw::Ptr` in interpreter runs: pointers with provenance into the run's extents, each access
//! checked against the Ptr rules in their order.

use super::super::memory::layout;
use super::super::value::{Loc, Provenance, Ptr, Val};
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
        "read" => {
            let ptr = pointer(interp, &args[0]);
            let loc = match access(interp, &ptr, size, align, false, at) {
                Ok(loc) => loc,
                Err(end) => return Some(Err(end)),
            };
            let value = interp.memory.get(&loc).clone();
            if value == Val::Absent {
                return Some(Err(violation(interp, "initialisation", at)));
            }
            value
        }
        "write" => {
            let ptr = pointer(interp, &args[0]);
            let loc = match access(interp, &ptr, size, align, true, at) {
                Ok(loc) => loc,
                Err(end) => return Some(Err(end)),
            };
            interp.memory.set(&loc, args[1].clone());
            Val::Unit
        }
        _ => return None,
    };
    Some(Ok(Flow::Return(value)))
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
