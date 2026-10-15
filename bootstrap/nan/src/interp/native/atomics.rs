//! The atomics the interpreter supplies: each holds a reference to an extent of its own, which
//! its operations read and replace through a read receiver.

use std::rc::Rc;

use super::super::value::{Loc, Val, fits};
use super::super::{Exec, Flow, Interpreter};
use crate::check::ty::Scalar;

/// A member of a `core::atomic` type, by the type's name; none for another member.
pub fn member(
    interp: &mut Interpreter,
    name: &str,
    member: &str,
    args: &[Val],
) -> Option<Exec<Flow>> {
    let scalar = match name {
        "AtomicU8" => Scalar::U8,
        "AtomicU16" => Scalar::U16,
        "AtomicU32" => Scalar::U32,
        "AtomicU64" => Scalar::U64,
        "AtomicUsize" => Scalar::Usize,
        "AtomicBool" => Scalar::Bool,
        _ => return None,
    };
    if member == "of" {
        let extent = interp.memory.alloc(args[0].clone(), true, 0);
        let atomic = interp.ty_item("core::atomic", name);
        let cell = Val::Ref(Loc {
            extent,
            path: Vec::new(),
        });
        return Some(Ok(Flow::Return(Val::Struct(atomic, Rc::new(vec![cell])))));
    }
    let cell = match &interp.deref(&args[0]).fields()[0] {
        Val::Ref(loc) => loc.clone(),
        other => panic!("an atomic's cell, not {other:?}"),
    };
    let previous = interp.memory.get(&cell).clone();
    let option = interp.ty_item("core", "Option");
    let (stored, result) = match member {
        "load" => (None, previous),
        "store" => (Some(args[1].clone()), Val::Unit),
        "swap" => (Some(args[1].clone()), previous),
        "compare_swap" => {
            let result = interp.ty_item("core", "Result");
            match interp.equal(&previous, &args[1]) {
                true => (
                    Some(args[2].clone()),
                    Val::Variant(result, 0, Rc::new(vec![previous])),
                ),
                false => (None, Val::Variant(result, 1, Rc::new(vec![previous]))),
            }
        }
        "fetch_add" | "fetch_sub" => {
            let amount = args[1].int();
            let exact = match member {
                "fetch_add" => previous.int() + amount,
                _ => previous.int() - amount,
            };
            match fits(exact, scalar) {
                true => (Some(Val::Int(exact)), Val::some(option, previous)),
                false => (None, Val::none(option)),
            }
        }
        "fetch_and" | "fetch_or" | "fetch_xor" => {
            let (a, b) = (previous.int(), args[1].int());
            let bits = match member {
                "fetch_and" => a & b,
                "fetch_or" => a | b,
                _ => a ^ b,
            };
            (Some(Val::Int(bits)), previous)
        }
        _ => return None,
    };
    if let Some(stored) = stored {
        interp.memory.set(&cell, stored);
    }
    Some(Ok(Flow::Return(result)))
}
