//! Capability kinds: minters, reaches and derivations. A minter holds its state and the entries
//! it minted. An entry of a kind records, after whether it is live and its source, its minter's
//! state and its record; an entry `sys::revocable` derives from one records neither, and a reach
//! through it finds them at the nearest entry of its sources recording them.

use std::rc::Rc;

use super::super::value::Val;
use super::super::{Exec, Flow, Interpreter, Site};
use super::hardware::{call_value, reference};
use super::sims::{ENTRY, LIVE, SOURCE, revoked, stale};
use super::stores::place;
use crate::check::db::ItemId;
use crate::check::ty::{Arg, Ty};
use crate::nir::Span;

/// The positions of a kind entry's minter and record.
const MINTER: usize = 2;
const RECORD: usize = 3;

/// The positions of a minter's state and of the list of the entries it minted.
const STATE: usize = 0;
const MINTED: usize = 1;

/// An operation of a minter, `sys::reach` or `sys::derive`; none for another.
pub fn member(
    interp: &mut Interpreter,
    owner: &str,
    member: &str,
    type_args: &[Arg],
    args: &[Val],
    at: (ItemId, Span),
    site: Site,
) -> Option<Exec<Flow>> {
    let value = match (owner, member) {
        ("sys::Minter", "of") => {
            let state = interp.memory.alloc(args[0].clone(), true, 0);
            let minted = interp
                .memory
                .alloc(Val::Array(Rc::new(Vec::new())), true, 0);
            let item = interp.ty_item("sys", "Minter");
            Val::Struct(item, Rc::new(vec![reference(state), reference(minted)]))
        }
        ("sys::Minter", "mint") => {
            let minter = interp.deref(&args[0]);
            let entry = kind_entry(
                interp,
                Val::Absent,
                minter.fields()[STATE].clone(),
                args[1].clone(),
            );
            let minted = place(&minter.fields()[MINTED]);
            let mut entries = interp.memory.get(&minted).fields().to_vec();
            entries.push(entry.clone());
            interp.memory.set(&minted, Val::Array(Rc::new(entries)));
            Val::Struct(kind_item(type_args), Rc::new(vec![entry]))
        }
        ("sys::Minter", "revoke") => {
            let minter = place(&interp.deref(&args[0]).fields()[STATE]);
            if !stale(interp, &args[1]) && place(&pinned(interp, &args[1]).0) == minter {
                let entry = place(&interp.deref(&args[1]).fields()[ENTRY]);
                interp
                    .memory
                    .set(&entry.with(LIVE as u32), Val::Bool(false));
            }
            Val::Unit
        }
        ("sys::Minter", "close") => {
            let minter = interp.deref(&args[0]);
            let minted = place(&minter.fields()[MINTED]);
            for entry in interp.memory.get(&minted).fields().to_vec() {
                interp
                    .memory
                    .set(&place(&entry).with(LIVE as u32), Val::Bool(false));
            }
            let state = interp.deref(&minter.fields()[STATE]);
            super::release(interp, &state);
            Val::Unit
        }
        ("sys::reach", "") => {
            if stale(interp, &args[0]) {
                return Some(Ok(Flow::Return(Val::none(
                    interp.ty_item("core", "Option"),
                ))));
            }
            let (state, record) = pinned(interp, &args[0]);
            return Some(
                match call_value(interp, &args[1], vec![state, record], at, site) {
                    Ok(Flow::Return(value)) => Ok(Flow::Return(Val::some(
                        interp.ty_item("core", "Option"),
                        value,
                    ))),
                    other => other,
                },
            );
        }
        ("sys::derive", "") => {
            if stale(interp, &args[0]) {
                return Some(Ok(Flow::Fail(revoked(interp))));
            }
            let (state, record) = pinned(interp, &args[0]);
            let narrowed = match call_value(interp, &args[1], vec![state.clone(), record], at, site)
            {
                Ok(Flow::Return(value)) => value,
                other => return Some(other),
            };
            let source = interp.deref(&args[0]).fields()[ENTRY].clone();
            let entry = kind_entry(interp, source, state, narrowed);
            Val::Struct(kind_item(type_args), Rc::new(vec![entry]))
        }
        _ => return None,
    };
    Some(Ok(Flow::Return(value)))
}

/// The kind a minter's operation or a derivation fixes first.
fn kind_item(type_args: &[Arg]) -> ItemId {
    match type_args.first().and_then(Arg::ty) {
        Some(Ty::Item(item, _)) => *item,
        other => panic!("a capability kind, not {other:?}"),
    }
}

/// A new live entry of a kind deriving from `source`, belonging to the minter whose state is
/// `minter` and recording `record`.
fn kind_entry(interp: &mut Interpreter, source: Val, minter: Val, record: Val) -> Val {
    let parts = Val::Array(Rc::new(vec![Val::Bool(true), source, minter, record]));
    reference(interp.memory.alloc(parts, true, 0))
}

/// The state of the minter a capability's entry belongs to, and the entry's record: those the
/// nearest entry of its sources recording them records.
fn pinned(interp: &Interpreter, capability: &Val) -> (Val, Val) {
    let mut entry = place(&interp.deref(capability).fields()[ENTRY]);
    loop {
        let parts = interp.memory.get(&entry).fields().to_vec();
        if parts.len() > RECORD {
            return (parts[MINTER].clone(), parts[RECORD].clone());
        }
        entry = place(&parts[SOURCE]);
    }
}
