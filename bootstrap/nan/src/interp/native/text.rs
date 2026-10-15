//! The texts the interpreter supplies: `text::String`, a growable container of bytes,
//! `text::Fixed[N]`, which holds its N bytes and its length inline, both writers of
//! renderings, and `text::Interner`, whose entries hold each interned text's bytes with their
//! allocation.

use std::rc::Rc;

use super::super::value::{Loc, Val, View};
use super::super::{Exec, Flow, Interpreter, Site};
use super::stores::{
    append, cell_of, container, entries, exhausted, held, holder, interior, place, store,
};
use crate::check::db::ItemId;
use crate::check::ty::{Arg, Const, Scalar, Ty};
use crate::nir::Span;

/// The positions of a fixed text's parts: its bytes and its length.
const BYTES: usize = 0;
const LEN: usize = 1;

/// A member of `text::String`, `text::Fixed` or `text::Interner`; none for another member.
pub fn member(
    interp: &mut Interpreter,
    owner: &str,
    member: &str,
    type_args: &[Arg],
    args: &[Val],
    at: (ItemId, Span),
    site: Site,
) -> Option<Exec<Flow>> {
    if let ("text::String" | "text::Fixed", "write" | "try_write") = (owner, member) {
        return Some(write(interp, owner, member, type_args, args, at, site));
    }
    match owner {
        "text::String" => string(interp, member, args, at),
        "text::Fixed" => fixed(interp, member, type_args, args, at),
        "text::Interner" => interner(interp, member, args, at),
        _ => None,
    }
}

/// `write` and `try_write`: appends the output of a rendering, its budget for a fixed text the
/// smaller of the one given and its room.
fn write(
    interp: &mut Interpreter,
    owner: &str,
    member: &str,
    type_args: &[Arg],
    args: &[Val],
    at: (ItemId, Span),
    site: Site,
) -> Exec<Flow> {
    let mut budget = args[2].int() as usize;
    if owner == "text::Fixed" {
        let bound = match type_args.first() {
            Some(Arg::Const(Const::Int(bound))) => *bound as usize,
            _ => panic!("a fixed text's capacity"),
        };
        let len = interp.deref(&args[0]).fields()[LEN].int() as usize;
        budget = budget.min(bound - len);
    }
    let rendered = super::render::output(interp, &args[1], budget, at, site)?;
    let receiver = place(&args[0]);
    if owner == "text::Fixed" {
        let len_place = receiver.with(LEN as u32);
        let len = interp.memory.get(&len_place).int() as usize;
        for (offset, byte) in rendered.iter().enumerate() {
            let at_byte = receiver.with(BYTES as u32).with((len + offset) as u32);
            interp.memory.set(&at_byte, Val::Int(i128::from(*byte)));
        }
        let total = (len + rendered.len()) as i128;
        interp.memory.set(&len_place, Val::Int(total));
        return Ok(Flow::Return(Val::Unit));
    }
    let bytes: Vec<Val> = rendered
        .into_iter()
        .map(|byte| Val::Int(i128::from(byte)))
        .collect();
    match append(interp, &receiver, &bytes, &Ty::Scalar(Scalar::U8)) {
        Some(()) => Ok(Flow::Return(Val::Unit)),
        None => exhausted(interp, member == "try_write", at),
    }
}

fn string(
    interp: &mut Interpreter,
    member: &str,
    args: &[Val],
    at: (ItemId, Span),
) -> Option<Exec<Flow>> {
    let byte = Ty::Scalar(Scalar::U8);
    Some(Ok(Flow::Return(match member {
        "empty" => {
            let cell = cell_of(interp, &args[0]);
            container(interp, "text::String", Vec::new(), cell)
        }
        "as_str" => {
            let storage = super::stores::storage(interp, &args[0]);
            let len = interp.memory.get(&storage).fields().len();
            Val::View(View {
                array: storage,
                start: 0,
                len,
            })
        }
        "push" | "try_push" | "append" | "try_append" => {
            let bytes = added(interp, member, &args[1]);
            if append(interp, &place(&args[0]), &bytes, &byte).is_none() {
                return Some(exhausted(interp, member.starts_with("try_"), at));
            }
            Val::Unit
        }
        "clear" => {
            store(interp, &place(&args[0]), Vec::new());
            Val::Unit
        }
        "into_bytes" => {
            let Val::Struct(_, parts) = interp.deref(&args[0]) else {
                panic!("a string value");
            };
            Val::Struct(interp.ty_item("mem", "Bytes"), parts)
        }
        _ => return None,
    })))
}

fn fixed(
    interp: &mut Interpreter,
    member: &str,
    type_args: &[Arg],
    args: &[Val],
    at: (ItemId, Span),
) -> Option<Exec<Flow>> {
    let bound = match type_args.first() {
        Some(Arg::Const(Const::Int(bound))) => *bound as usize,
        _ => panic!("a fixed text's capacity"),
    };
    Some(Ok(Flow::Return(match member {
        "empty" => {
            let item = interp.ty_item("text", "Fixed");
            let bytes = Val::Array(Rc::new(vec![Val::Int(0); bound]));
            Val::Struct(item, Rc::new(vec![bytes, Val::Int(0)]))
        }
        "as_str" => Val::View(fixed_view(interp, &args[0])),
        "push" | "append" => {
            let receiver = place(&args[0]);
            let added = added(interp, member, &args[1]);
            let len_place = receiver.with(LEN as u32);
            let len = interp.memory.get(&len_place).int() as usize;
            if len + added.len() > bound {
                return Some(Err(interp.trap("trap.requires", at)));
            }
            let count = added.len();
            for (offset, value) in added.into_iter().enumerate() {
                let byte = receiver.with(BYTES as u32).with((len + offset) as u32);
                interp.memory.set(&byte, value);
            }
            interp
                .memory
                .set(&len_place, Val::Int((len + count) as i128));
            Val::Unit
        }
        "clear" => {
            interp
                .memory
                .set(&place(&args[0]).with(LEN as u32), Val::Int(0));
            Val::Unit
        }
        _ => return None,
    })))
}

fn interner(
    interp: &mut Interpreter,
    member: &str,
    args: &[Val],
    at: (ItemId, Span),
) -> Option<Exec<Flow>> {
    let option = interp.ty_item("core", "Option");
    let symbol = |interp: &Interpreter, index: usize| {
        let item = interp.ty_item("text", "Symbol");
        Val::Struct(item, Rc::new(vec![Val::Int(index as i128)]))
    };
    Some(Ok(Flow::Return(match member {
        "empty" => {
            let cell = cell_of(interp, &args[0]);
            let inner = container(interp, "text::Interner", Vec::new(), cell);
            interior(interp, "text::Interner", inner)
        }
        "len" => Val::Int(entries(interp, &held(interp, &args[0])).len() as i128),
        "intern" | "try_intern" | "lookup" => {
            let text = interp.bytes(&args[1]);
            let stored = entries(interp, &held(interp, &args[0]));
            let present = stored.iter().position(|entry| {
                let bytes = entry.fields()[0].fields();
                bytes.len() == text.len()
                    && bytes
                        .iter()
                        .zip(&text)
                        .all(|(a, b)| a.int() == i128::from(*b))
            });
            match (member, present) {
                ("lookup", found) => match found {
                    Some(index) => Val::some(option, symbol(interp, index)),
                    None => Val::none(option),
                },
                (_, Some(index)) => symbol(interp, index),
                (_, None) => {
                    let fallible = member == "try_intern";
                    let inner = held(interp, &args[0]);
                    let cell = inner.fields()[1].int() as u32;
                    if stored.len() > u32::MAX as usize {
                        return Some(exhausted(interp, fallible, at));
                    }
                    let Some(allocation) =
                        interp.budgets.allocate(cell, text.len().max(1) as u64, 1)
                    else {
                        return Some(exhausted(interp, fallible, at));
                    };
                    let bytes = text
                        .iter()
                        .map(|byte| Val::Int(i128::from(*byte)))
                        .collect();
                    let entry = Val::Array(Rc::new(vec![
                        Val::Array(Rc::new(bytes)),
                        Val::Int(i128::from(allocation)),
                    ]));
                    let str_ty = Ty::item(interp.ty_item("core", "Str"), Vec::new());
                    if append(interp, &holder(interp, &args[0]), &[entry], &str_ty).is_none() {
                        interp.budgets.free(allocation);
                        return Some(exhausted(interp, fallible, at));
                    }
                    symbol(interp, stored.len())
                }
            }
        }
        "resolve" => {
            let inner = held(interp, &args[0]);
            let index = interp.deref(&args[1]).fields()[0].int() as usize;
            let storage = super::stores::storage(interp, &inner);
            let Some(entry) = interp.memory.get(&storage).fields().get(index).cloned() else {
                return Some(Err(interp.trap("trap.bounds", at)));
            };
            Val::View(View {
                array: storage.with(index as u32).with(0),
                start: 0,
                len: entry.fields()[0].fields().len(),
            })
        }
        _ => return None,
    })))
}

/// The bytes a `push` or `append` adds: a char's encoding or a text's bytes.
fn added(interp: &mut Interpreter, member: &str, value: &Val) -> Vec<Val> {
    let bytes: Vec<u8> = match member.trim_start_matches("try_") {
        "push" => {
            let Val::Char(c) = interp.deref(value) else {
                panic!("a char value");
            };
            c.to_string().into_bytes()
        }
        _ => interp.bytes(value),
    };
    bytes
        .into_iter()
        .map(|byte| Val::Int(i128::from(byte)))
        .collect()
}

/// The view of a fixed text's bytes up to its length: in its place for a reference, else in a
/// new read-only extent.
fn fixed_view(interp: &mut Interpreter, receiver: &Val) -> View {
    let value = interp.deref(receiver);
    let len = value.fields()[LEN].int() as usize;
    let array = match receiver {
        Val::Ref(loc) => loc.with(BYTES as u32),
        _ => Loc {
            extent: interp.memory.alloc(value.fields()[BYTES].clone(), false, 0),
            path: Vec::new(),
        },
    };
    View {
        array,
        start: 0,
        len,
    }
}
