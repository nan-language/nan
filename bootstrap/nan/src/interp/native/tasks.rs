//! The channels interpreter runs make without waiting: a channel's senders and receiver share
//! a buffer drawn for its capacity, counting the handles that keep it; sending and receiving
//! wait, which interpreter runs never do.

use std::rc::Rc;

use super::super::memory::layout;
use super::super::value::{Loc, Val};
use super::super::{Exec, Flow, Interpreter};
use super::stores::{cell_of, exhausted, place, release as release_value};
use crate::check::db::ItemId;
use crate::check::ty::{Arg, Ty};
use crate::nir::Span;

/// The positions of a channel's shared parts: its buffer, its allocation and its handles.
const BUFFER: usize = 0;
const ALLOCATION: usize = 1;
const HANDLES: usize = 2;

/// A member of `task::Channel` or `task::Sender` an interpreter run can call; none for another.
pub fn member(
    interp: &mut Interpreter,
    owner: &str,
    member: &str,
    type_args: &[Arg],
    args: &[Val],
    at: (ItemId, Span),
) -> Option<Exec<Flow>> {
    let value = match (owner, member) {
        ("task::Channel", "bounded" | "try_bounded") => {
            let element = type_args
                .first()
                .and_then(Arg::ty)
                .cloned()
                .unwrap_or(Ty::Unit);
            let (size, align) = layout(interp.ck, &element);
            let cell = cell_of(interp, &args[1]);
            let bytes = size.saturating_mul(args[0].int() as u64).max(1);
            let Some(allocation) = interp.budgets.allocate(cell, bytes, align) else {
                return Some(exhausted(interp, member == "try_bounded", at));
            };
            let state = Val::Array(Rc::new(vec![
                Val::Array(Rc::new(Vec::new())),
                Val::Int(i128::from(allocation)),
                Val::Int(2),
            ]));
            let shared = Val::Ref(Loc {
                extent: interp.memory.alloc(state, true, 0),
                path: Vec::new(),
            });
            let sender = Val::Struct(
                interp.ty_item("task", "Sender"),
                Rc::new(vec![shared.clone()]),
            );
            let receiver = Val::Struct(interp.ty_item("task", "Receiver"), Rc::new(vec![shared]));
            Val::Struct(
                interp.ty_item("task", "Channel"),
                Rc::new(vec![sender, receiver]),
            )
        }
        ("task::Sender", "split") => {
            let sender = interp.deref(&args[0]);
            let handles = place(&sender.fields()[0]).with(HANDLES as u32);
            let count = interp.memory.get(&handles).int();
            interp.memory.set(&handles, Val::Int(count + 1));
            sender
        }
        _ => return None,
    };
    Some(Ok(Flow::Return(value)))
}

/// Releases a sender or receiver: the last handle of a channel releases its buffered values
/// and returns its buffer's memory.
pub fn release(interp: &mut Interpreter, handle: &Val) {
    let state = place(&handle.fields()[0]);
    let handles = state.with(HANDLES as u32);
    let left = interp.memory.get(&handles).int() - 1;
    interp.memory.set(&handles, Val::Int(left));
    if left == 0 {
        let buffered = interp.memory.get(&state.with(BUFFER as u32)).clone();
        release_value(interp, &buffered);
        let allocation = interp.memory.get(&state.with(ALLOCATION as u32)).int();
        interp.budgets.free(allocation as u32);
    }
}
