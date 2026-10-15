//! Hashing in interpreter runs: the bytes core hashing and derived hashes write to a hasher,
//! each write a call of the hasher's `write`.

use std::rc::Rc;

use super::super::value::{Loc, Val, View};
use super::super::{Exec, Flow, Interpreter, Site};
use super::stores::entries;
use crate::check::db::ItemId;
use crate::check::sig::{Payload, SigKind};
use crate::check::ty::{Arg, Scalar, Subst, Ty};
use crate::nir::{Callable, FnRef, Span};

/// What one hashing writes to: the hasher and its type.
struct Hashing<'h> {
    hasher: &'h Val,
    ty: Ty,
    at: (ItemId, Span),
    site: Site,
}

/// The hash core writes for a value of a type, `args` holding the value and the hasher, whose
/// type is `hasher` or, when not given, the type of its value.
pub fn builtin(
    interp: &mut Interpreter,
    ty: &Ty,
    hasher: Option<&Ty>,
    args: &[Val],
    at: (ItemId, Span),
    site: Site,
) -> Exec<()> {
    let hashing = Hashing {
        hasher: &args[1],
        ty: hasher.cloned().unwrap_or_else(|| type_of(interp, &args[1])),
        at,
        site,
    };
    let value = interp.deref(&args[0]);
    core(interp, &value, ty, &hashing)
}

/// The hash a derive writes: a struct's fields in byte order of their names, or an enum's
/// variant name with its length, then its payload values, labelled ones in byte order of
/// labels.
pub fn derived(
    interp: &mut Interpreter,
    owner: ItemId,
    type_args: &[Arg],
    args: &[Val],
    at: (ItemId, Span),
    site: Site,
) -> Exec<()> {
    let hashing = Hashing {
        hasher: &args[1],
        ty: type_of(interp, &args[1]),
        at,
        site,
    };
    let value = interp.deref(&args[0]);
    let sig = interp.ck.sig(owner);
    let subst = Subst::of(sig.generics.iter().map(|def| def.param), type_args);
    let mut parts: Vec<(String, Ty, Val)> = match (&sig.kind, &value) {
        (SigKind::Struct { fields }, _) => fields
            .iter()
            .zip(value.fields())
            .map(|(field, part)| (field.name.clone(), subst.ty(&field.ty), part.clone()))
            .collect(),
        (SigKind::Enum { variants }, Val::Variant(_, index, payload)) => {
            let variant = &variants[*index as usize];
            let mut name = (variant.name.len() as u64).to_le_bytes().to_vec();
            name.extend_from_slice(variant.name.as_bytes());
            write(interp, &hashing, &name)?;
            match &variant.payload {
                Payload::None => Vec::new(),
                Payload::Single(ty) => vec![(String::new(), subst.ty(ty), payload[0].clone())],
                Payload::Labelled(labels) => labels
                    .iter()
                    .zip(payload.iter())
                    .map(|((label, ty), part)| (label.clone(), subst.ty(ty), part.clone()))
                    .collect(),
            }
        }
        (other, _) => panic!("a derived Hash of {other:?}"),
    };
    parts.sort_by(|left, right| left.0.cmp(&right.0));
    for (_, ty, part) in parts {
        hash(interp, &part, &ty, &hashing)?;
    }
    Ok(())
}

/// The hash core writes for a value of a type whose `Hash` the toolchain supplies.
fn core(interp: &mut Interpreter, value: &Val, ty: &Ty, hashing: &Hashing) -> Exec<()> {
    let length = |len: usize| (len as u64).to_le_bytes().to_vec();
    match ty {
        Ty::Scalar(scalar) => {
            let bytes = match scalar {
                Scalar::Bool => vec![u8::from(value.bool())],
                Scalar::Char => (value.int() as u32).to_le_bytes().to_vec(),
                _ => {
                    let width = (scalar.bits() / 8) as usize;
                    (value.int() as u64).to_le_bytes()[..width].to_vec()
                }
            };
            write(interp, hashing, &bytes)
        }
        Ty::Unit => Ok(()),
        Ty::Item(item, args) => {
            let element = args.first().and_then(Arg::ty).cloned().unwrap_or(Ty::Unit);
            match interp.ck.db.canonical(*item).as_str() {
                "core::Str" | "text::String" | "text::Fixed" => {
                    let bytes: Vec<u8> = match interp.ck.db.canonical(*item).as_str() {
                        "text::Fixed" => value.fields()[0].fields()
                            [..value.fields()[1].int() as usize]
                            .iter()
                            .map(|byte| byte.int() as u8)
                            .collect(),
                        "text::String" => entries(interp, value)
                            .iter()
                            .map(|byte| byte.int() as u8)
                            .collect(),
                        _ => interp.bytes(value),
                    };
                    write(interp, hashing, &length(bytes.len()))?;
                    write(interp, hashing, &bytes)
                }
                "core::Array" => {
                    for part in value.fields().to_vec() {
                        hash(interp, &part, &element, hashing)?;
                    }
                    Ok(())
                }
                "core::Slice" | "core::SliceMut" | "mem::List" | "mem::Bytes" | "mem::Set"
                | "mem::Bounded" => {
                    let elements = match interp.ck.db.canonical(*item).as_str() {
                        "core::Slice" | "core::SliceMut" => {
                            let view = interp.view(value);
                            interp.view_values(&view)
                        }
                        "mem::Bounded" => value.fields()[0].fields().to_vec(),
                        _ => entries(interp, value),
                    };
                    let element = match interp.ck.db.canonical(*item).as_str() {
                        "mem::Bytes" => Ty::Scalar(Scalar::U8),
                        _ => element,
                    };
                    write(interp, hashing, &length(elements.len()))?;
                    for part in elements {
                        hash(interp, &part, &element, hashing)?;
                    }
                    Ok(())
                }
                "mem::Map" | "mem::HashMap" => {
                    let value_ty = args.get(1).and_then(Arg::ty).cloned().unwrap_or(Ty::Unit);
                    let held = entries(interp, value);
                    write(interp, hashing, &length(held.len()))?;
                    for entry in held {
                        hash(interp, &entry.fields()[0], &element, hashing)?;
                        hash(interp, &entry.fields()[1], &value_ty, hashing)?;
                    }
                    Ok(())
                }
                "mem::Id" => write(
                    interp,
                    hashing,
                    &(value.fields()[0].int() as u32).to_le_bytes(),
                ),
                "mem::Handle" => {
                    write(
                        interp,
                        hashing,
                        &(value.fields()[0].int() as u32).to_le_bytes(),
                    )?;
                    write(
                        interp,
                        hashing,
                        &(value.fields()[1].int() as u32).to_le_bytes(),
                    )
                }
                _ => Err(super::super::End::Missing),
            }
        }
        _ => Err(super::super::End::Missing),
    }
}

/// Hashes a value of a type through its `Hash`.
fn hash(interp: &mut Interpreter, value: &Val, ty: &Ty, hashing: &Hashing) -> Exec<()> {
    let interface = interp.ty_item("core", "Hash");
    let fn_ref = FnRef {
        callable: Callable::Method {
            interface,
            method: 0,
        },
        args: Rc::from(vec![Arg::Ty(ty.clone()), Arg::Ty(hashing.ty.clone())]),
    };
    let args = vec![value.clone(), hashing.hasher.clone()];
    interp.call_function(&fn_ref, args, hashing.at, hashing.site)?;
    Ok(())
}

/// Writes bytes to the hasher through its `write`.
fn write(interp: &mut Interpreter, hashing: &Hashing, bytes: &[u8]) -> Exec<()> {
    let values = bytes
        .iter()
        .map(|byte| Val::Int(i128::from(*byte)))
        .collect();
    let extent = interp
        .memory
        .alloc(Val::Array(Rc::new(values)), false, bytes.len() as u64);
    let view = Val::View(View {
        array: Loc {
            extent,
            path: Vec::new(),
        },
        start: 0,
        len: bytes.len(),
    });
    let interface = interp.ty_item("core", "Hasher");
    let fn_ref = FnRef {
        callable: Callable::Method {
            interface,
            method: 0,
        },
        args: Rc::from(vec![Arg::Ty(hashing.ty.clone())]),
    };
    match interp.call_function(
        &fn_ref,
        vec![hashing.hasher.clone(), view],
        hashing.at,
        hashing.site,
    )? {
        Flow::Return(_) => Ok(()),
        _ => panic!("a hasher's write ending otherwise than by returning"),
    }
}

/// The type of a hasher value: its struct or enum without type arguments.
fn type_of(interp: &Interpreter, hasher: &Val) -> Ty {
    match interp.deref(hasher) {
        Val::Struct(item, _) | Val::Variant(item, _, _) => Ty::item(item, Vec::new()),
        other => panic!("a hasher value, not {other:?}"),
    }
}

#[cfg(test)]
mod tests;
