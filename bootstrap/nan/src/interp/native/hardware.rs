//! The hardware operations interpreter runs reach on their one CPU, which has no maskable
//! interrupts and no devices: user addresses, interrupt masking, signals, spin locks, shared
//! devices bound to no vector, per-CPU values, and the simulated register blocks of `hw::sim`
//! with the windows reaching them.
//!
//! A simulated block holds, for each register, a cell per array element or one cell, each
//! holding its value's bits, its queue and its last written bits, which its windows and
//! operations share.

use std::rc::Rc;

use super::super::value::{Loc, Val};
use super::super::{Exec, Flow, Interpreter, Site};
use super::numbers::{from_bits, layout_bits, layout_value, scalar_bits};
use super::stores::{cell_of, place};
use crate::check::db::ItemId;
use crate::check::sig::SigKind;
use crate::check::ty::{Arg, Const, Ty};
use crate::nir::Span;
use crate::syntax::ast::Access;

/// The positions of a simulated block's parts: whether a window reaches it, its registers and
/// the cell its queues draw through.
const TAKEN: usize = 0;
const REGISTERS: usize = 1;
const SIM_CELL: usize = 2;

/// The positions of a register cell's parts: its bits, its queue and its last written bits.
const HELD: usize = 0;
const QUEUE: usize = 1;
const LAST: usize = 2;

/// A hardware operation an interpreter run can make; none for another.
pub fn member(
    interp: &mut Interpreter,
    owner: &str,
    member: &str,
    type_args: &[Arg],
    args: &[Val],
    at: (ItemId, Span),
    site: Site,
) -> Option<Exec<Flow>> {
    let option = interp.ty_item("core", "Option");
    let value = match (owner, member) {
        ("hw::UserAddr", "of") => {
            let item = interp.ty_item("hw", "UserAddr");
            Val::Struct(item, Rc::new(vec![args[0].clone()]))
        }
        ("hw::UserAddr", "value") => interp.deref(&args[0]).fields()[0].clone(),
        ("hw::cpu_count", "") => Val::Int(1),
        ("hw::cpu_hardware_id", "") => match args[0].int() {
            0 => Val::some(option, Val::Int(0)),
            _ => Val::none(option),
        },
        ("hw::fence", "") => Val::Unit,
        ("hw::without_irqs", "") => {
            let token = Val::Struct(interp.ty_item("hw", "IrqsDisabled"), Rc::new(Vec::new()));
            return Some(call_value(interp, &args[0], vec![token], at, site));
        }
        ("hw::Signal", "empty") => {
            let held = interp.memory.alloc(Val::Absent, true, 0);
            Val::Struct(
                interp.ty_item("hw", "Signal"),
                Rc::new(vec![reference(held)]),
            )
        }
        ("hw::Signal", "post") => {
            let slot = place(&interp.deref(&args[0]).fields()[0]);
            let stored = interp.memory.get(&slot).clone();
            super::release(interp, &stored);
            interp.memory.set(&slot, args[2].clone());
            Val::Unit
        }
        ("hw::SpinLock" | "hw::IrqShared", "of" | "install") => {
            let held = interp.memory.alloc(args[0].clone(), true, 0);
            let (module, name) = owner.split_once("::").expect("a std path");
            Val::Struct(interp.ty_item(module, name), Rc::new(vec![reference(held)]))
        }
        ("hw::IrqShared", "install_on") => {
            if args[1].int() != 0 {
                return Some(Err(interp.trap("trap.bounds", at)));
            }
            let held = interp.memory.alloc(args[0].clone(), true, 0);
            Val::Struct(
                interp.ty_item("hw", "IrqShared"),
                Rc::new(vec![reference(held)]),
            )
        }
        ("hw::IrqShared", "install_msi") => {
            if args[1].int() != 0 {
                return Some(Err(interp.trap("trap.bounds", at)));
            }
            let unsupported =
                Val::Variant(interp.ty_item("hw", "MsiError"), 2, Rc::new(Vec::new()));
            let refusal = Val::Struct(
                interp.ty_item("hw", "MsiRefusal"),
                Rc::new(vec![args[0].clone(), unsupported]),
            );
            Val::Variant(interp.ty_item("core", "Result"), 1, Rc::new(vec![refusal]))
        }
        ("hw::IrqShared", "message") => Val::none(option),
        ("hw::SpinLock" | "hw::IrqShared", "with") => {
            let contents = interp.deref(&args[0]).fields()[0].clone();
            return Some(call_value(interp, &args[2], vec![contents], at, site));
        }
        ("hw::IrqShared", "uninstall") => {
            let contents = interp.deref(&args[0]).fields()[0].clone();
            interp.deref(&contents)
        }
        ("hw::PerCpu", "of") => {
            let initial = match call_value(interp, &args[0], vec![Val::Int(0)], at, site) {
                Ok(Flow::Return(value)) => value,
                other => return Some(other),
            };
            let held = interp.memory.alloc(initial, true, 0);
            Val::Struct(
                interp.ty_item("hw", "PerCpu"),
                Rc::new(vec![reference(held)]),
            )
        }
        ("hw::PerCpu", "with") => {
            let contents = interp.deref(&args[0]).fields()[0].clone();
            return Some(call_value(interp, &args[2], vec![contents], at, site));
        }
        ("hw::sim::Registers", "of") => {
            let block = block_of(type_args);
            let registers = registers(interp, block)
                .iter()
                .map(|(ty, _)| {
                    let count = match ty {
                        Ty::Item(_, args) => match args.get(1) {
                            Some(Arg::Const(Const::Int(count))) => *count as usize,
                            _ => 1,
                        },
                        _ => 1,
                    };
                    let cell = Val::Array(Rc::new(vec![
                        Val::Int(0),
                        Val::Array(Rc::new(Vec::new())),
                        Val::Absent,
                    ]));
                    Val::Array(Rc::new(vec![cell; count]))
                })
                .collect();
            let cell = cell_of(interp, &args[0]);
            let state = Val::Array(Rc::new(vec![
                Val::Bool(false),
                Val::Array(Rc::new(registers)),
                Val::Int(i128::from(cell)),
            ]));
            let held = interp.memory.alloc(state, true, 0);
            let item = interp.ty_item("hw::sim", "Registers");
            Val::Struct(item, Rc::new(vec![reference(held)]))
        }
        ("hw::sim::Registers", "mmio" | "ports") => {
            let state = sim_state(interp, &args[0]);
            let taken = state.with(TAKEN as u32);
            if interp.memory.get(&taken).bool() {
                return Some(Err(interp.trap("trap.duplicate-access", at)));
            }
            interp.memory.set(&taken, Val::Bool(true));
            let name = match member {
                "mmio" => "Mmio",
                _ => "Ports",
            };
            Val::Struct(interp.ty_item("hw", name), Rc::new(vec![Val::Ref(state)]))
        }
        ("hw::sim::Registers", operation) => {
            let state = sim_state(interp, &args[0]);
            let block = block_of(type_args);
            let register = args[1].int() as usize;
            let indexed = operation.ends_with("_at");
            let index = match indexed {
                true => args[2].int() as usize,
                false => 0,
            };
            let Some(cell) = cell(interp, &state, register, index) else {
                return Some(Err(interp.trap("trap.bounds", at)));
            };
            let (ty, _) = registers(interp, block)[register].clone();
            let element = element_ty(&ty, indexed);
            let given = args.last().map(|value| interp.deref(value));
            match operation.trim_end_matches("_at") {
                "set" => {
                    let bits = bits_of(interp, given.as_ref().expect("a value"));
                    interp.memory.set(&cell.with(HELD as u32), Val::Int(bits));
                    Val::Unit
                }
                "queue" => {
                    let sim_cell = interp.memory.get(&state.with(SIM_CELL as u32)).int() as u32;
                    if interp.budgets.allocate(sim_cell, 8, 8).is_none() {
                        return Some(Err(interp.trap("trap.exhausted", at)));
                    }
                    let bits = bits_of(interp, given.as_ref().expect("a value"));
                    let queue = cell.with(QUEUE as u32);
                    let mut queued = interp.memory.get(&queue).fields().to_vec();
                    queued.push(Val::Int(bits));
                    interp.memory.set(&queue, Val::Array(Rc::new(queued)));
                    Val::Unit
                }
                "last_written" => match interp.memory.get(&cell.with(LAST as u32)).clone() {
                    Val::Absent => Val::none(option),
                    bits => Val::some(option, value_of(interp, &element, bits.int() as u64)),
                },
                _ => return None,
            }
        }
        ("hw::Mmio" | "hw::Ports", "read" | "write" | "read_at" | "write_at") => {
            let window = interp.deref(&args[0]);
            let Some(Val::Ref(state)) = window.fields().first().cloned() else {
                return None;
            };
            let block = block_of(type_args);
            let register = args[1].int() as usize;
            let indexed = member.ends_with("_at");
            let index = match indexed {
                true => args[2].int() as usize,
                false => 0,
            };
            let Some(cell) = cell(interp, &state, register, index) else {
                return Some(Err(interp.trap("trap.bounds", at)));
            };
            let (ty, access) = registers(interp, block)[register].clone();
            let element = element_ty(&ty, indexed);
            match member.trim_end_matches("_at") {
                "read" => {
                    let queue = cell.with(QUEUE as u32);
                    let mut queued = interp.memory.get(&queue).fields().to_vec();
                    let bits = match queued.is_empty() {
                        true => interp.memory.get(&cell.with(HELD as u32)).int(),
                        false => {
                            let oldest = queued.remove(0).int();
                            interp.memory.set(&queue, Val::Array(Rc::new(queued)));
                            oldest
                        }
                    };
                    value_of(interp, &element, bits as u64)
                }
                _ => {
                    let written = interp.deref(args.last().expect("a value"));
                    let bits = bits_of(interp, &written);
                    let held = cell.with(HELD as u32);
                    let kept = match access {
                        Access::WriteOneClear => interp.memory.get(&held).int() & !bits,
                        _ => bits,
                    };
                    interp.memory.set(&held, Val::Int(kept));
                    interp.memory.set(&cell.with(LAST as u32), Val::Int(bits));
                    Val::Unit
                }
            }
        }
        ("hw::Mmio" | "hw::Ports", "unmap") => match interp.deref(&args[0]).fields().first() {
            Some(Val::Ref(_)) => Val::Unit,
            _ => return None,
        },
        _ => return None,
    };
    Some(Ok(Flow::Return(value)))
}

pub(super) fn reference(extent: u32) -> Val {
    Val::Ref(Loc {
        extent,
        path: Vec::new(),
    })
}

/// Calls a function value with arguments.
pub(super) fn call_value(
    interp: &mut Interpreter,
    function: &Val,
    args: Vec<Val>,
    at: (ItemId, Span),
    site: Site,
) -> Exec<Flow> {
    let Val::Fn(function) = interp.deref(function) else {
        panic!("a function value");
    };
    interp.invoke(&function, args, at, site)
}

/// The registers block a sim or window's first type argument names.
fn block_of(type_args: &[Arg]) -> ItemId {
    match type_args.first().and_then(Arg::ty) {
        Some(Ty::Registers(block)) => *block,
        other => panic!("a registers block, not {other:?}"),
    }
}

/// The type and access kinds of each register of a block, in declaration order.
fn registers(interp: &Interpreter, block: ItemId) -> Vec<(Ty, Access)> {
    match &interp.ck.sig(block).kind {
        SigKind::Registers { registers } => registers
            .iter()
            .map(|register| (register.ty.clone(), register.access))
            .collect(),
        _ => panic!("a registers block"),
    }
}

/// The type of a register's values, of its elements for an indexed access.
fn element_ty(ty: &Ty, indexed: bool) -> Ty {
    match (ty, indexed) {
        (Ty::Item(_, args), true) => args.first().and_then(Arg::ty).cloned().unwrap_or(Ty::Unit),
        _ => ty.clone(),
    }
}

/// The place of a simulated block's state.
fn sim_state(interp: &Interpreter, sim: &Val) -> Loc {
    place(&interp.deref(sim).fields()[0])
}

/// The place of a register's cell, none past its elements.
fn cell(interp: &Interpreter, state: &Loc, register: usize, index: usize) -> Option<Loc> {
    let cells = state.with(REGISTERS as u32).with(register as u32);
    (index < interp.memory.get(&cells).fields().len()).then(|| cells.with(index as u32))
}

/// The bits of a value of an `AnyBits` type.
fn bits_of(interp: &Interpreter, value: &Val) -> i128 {
    match value {
        Val::Struct(layout, _) | Val::Variant(layout, _, _) => {
            i128::from(layout_bits(interp, *layout, value))
        }
        Val::F32(_) => i128::from(scalar_bits(value, crate::check::ty::Scalar::F32)),
        Val::F64(_) => i128::from(scalar_bits(value, crate::check::ty::Scalar::F64)),
        other => other.int(),
    }
}

/// The value of an `AnyBits` type with bits.
fn value_of(interp: &Interpreter, ty: &Ty, bits: u64) -> Val {
    match ty {
        Ty::Scalar(scalar) => from_bits(bits, *scalar).expect("an AnyBits scalar"),
        Ty::Item(layout, _) => layout_value(interp, *layout, bits).expect("a layout value"),
        other => panic!("a register value of {other:?}"),
    }
}
