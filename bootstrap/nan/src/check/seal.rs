//! Seals: the closure entries an item's seal covers, the closure hash, and whether an item needs
//! a seal.

use std::collections::BTreeSet;

use super::Checker;
use super::db::ItemId;
use super::ty::{Arg, Ty};
use crate::hash::Hash;
use crate::machine::api::ClosureEntry;
use crate::nir::{Callable, Callee, Constant, Op, Terminator, Unit};
use crate::syntax::ast::{FnBody, ItemKind};

/// The closure hash of closure entries: the hash of `closure`, LF and one line per entry, in
/// their order: the kind, the path and the full form of the hash.
pub fn closure_hash(entries: &BTreeSet<ClosureEntry>) -> Hash {
    let mut text = String::from("closure\n");
    for entry in entries {
        text.push_str(&format!(
            "{} {} {}\n",
            entry.kind(),
            entry.path,
            entry.hash.full()
        ));
    }
    Hash::of(text.as_bytes())
}

impl Checker<'_> {
    /// The closure entries of an item: an `interface` entry for each other item its unit names
    /// or a body it unfolds names, a `body` entry for each transparent function whose calls it
    /// unfolds, and, for an item holding a declassifying validator or decoder, a `body` entry for
    /// each function of its package its unit reaches through functions of its package.
    pub fn closure_entries(&self, item: ItemId) -> BTreeSet<ClosureEntry> {
        let mut entries = BTreeSet::new();
        let Some(unit) = self.unit(item) else {
            return entries;
        };
        let mut unfolded = Vec::new();
        let mut pending: Vec<ItemId> = named_items(&unit);
        let mut seen: Vec<ItemId> = vec![item];
        while let Some(named) = pending.pop() {
            if seen.contains(&named) {
                continue;
            }
            seen.push(named);
            if let Some(path) = self.canonical_path(named)
                && let Some(hash) = self.hashes(named).interface
            {
                entries.insert(ClosureEntry {
                    path: path.clone(),
                    body: false,
                    hash,
                });
            }
            if self.transparent_function(named) && self.unfolds(named, item) {
                unfolded.push(named);
                if let Some(body) = self.unit(named) {
                    pending.extend(named_items(&body));
                }
            }
        }
        for function in unfolded {
            if let Some(path) = self.canonical_path(function) {
                entries.insert(ClosureEntry {
                    path,
                    body: true,
                    hash: self.hashes(function).item,
                });
            }
        }
        if self.declassifies(item) {
            let package = self.db.package_of(item);
            let mut reached: Vec<ItemId> = Vec::new();
            let mut pending = named_items(&unit);
            while let Some(named) = pending.pop() {
                let own = self.db.package_of(named) == package
                    && matches!(self.db.ast(named).kind, ItemKind::Fn(_))
                    && !self.transparent_function(named);
                if !own || reached.contains(&named) || named == item {
                    continue;
                }
                reached.push(named);
                if let Some(path) = self.canonical_path(named) {
                    entries.insert(ClosureEntry {
                        path,
                        body: true,
                        hash: self.hashes(named).item,
                    });
                }
                if let Some(unit) = self.unit(named) {
                    pending.extend(named_items(&unit));
                }
            }
        }
        entries
    }

    /// Whether an item holds a declassifying validator or the declassifying decoder of a type.
    fn declassifies(&self, item: ItemId) -> bool {
        self.ledger(item).iter().any(|entry| {
            entry.escape
                && matches!(
                    entry.kind,
                    super::ledger::Kind::Validator | super::ledger::Kind::Decoder
                )
        })
    }

    /// Whether a function's body is transparent: written after `=`.
    fn transparent_function(&self, item: ItemId) -> bool {
        matches!(&self.db.ast(item).kind, ItemKind::Fn(function) if matches!(function.body, FnBody::Expr(_)))
    }

    /// Whether an item holds a trust escape.
    pub fn holds_escape(&self, item: ItemId) -> bool {
        self.ledger(item).iter().any(|entry| entry.escape)
    }
}

/// The items a unit names: its types, function references, constants, registers blocks and
/// `Dyn` interfaces, each once.
fn named_items(unit: &Unit) -> Vec<ItemId> {
    let mut found: Vec<ItemId> = Vec::new();
    let mut add = |item: ItemId| {
        if !found.contains(&item) {
            found.push(item);
        }
    };
    let ty = |ty: &Ty, add: &mut dyn FnMut(ItemId)| visit_ty(ty, add);
    for function in &unit.functions {
        for value in &function.types {
            ty(value.ty(), &mut add);
        }
        ty(&function.result, &mut add);
        if let Some(fails) = &function.fails {
            ty(fails, &mut add);
        }
        for block in &function.blocks {
            for inst in &block.insts {
                match &inst.op {
                    Op::FnRef(callee) => callable(&callee.callable, &callee.args, &mut add),
                    Op::Call(call) => call_items(call, &mut add),
                    Op::Constant(Constant::Item(item) | Constant::Register(item, _)) => add(*item),
                    Op::Slot { ty: slot, .. } => ty(slot, &mut add),
                    Op::Construct { ty: built, .. }
                    | Op::Variant { ty: built, .. }
                    | Op::Is { ty: built, .. } => ty(built, &mut add),
                    Op::Convert { to, .. } => ty(to, &mut add),
                    _ => {}
                }
            }
            if let Terminator::Call { call, .. } = &block.term {
                call_items(call, &mut add);
            }
        }
    }
    found
}

fn call_items(call: &crate::nir::Call, add: &mut dyn FnMut(ItemId)) {
    match &call.callee {
        Callee::Fn(callee) => callable(&callee.callable, &callee.args, add),
        Callee::Dyn { interface, .. } => add(*interface),
        Callee::Value(_) => {}
    }
}

fn callable(callable: &Callable, args: &[Arg], add: &mut dyn FnMut(ItemId)) {
    match callable {
        Callable::Item { item, .. } => add(*item),
        Callable::Method { interface, .. } => add(*interface),
        Callable::Derived { owner, .. } => add(*owner),
        Callable::Builtin(crate::nir::Builtin::Bits(_, crate::nir::BitsOwner::Layout(item))) => {
            add(*item);
        }
        Callable::Builtin(_) => {}
    }
    for arg in args {
        if let Arg::Ty(ty) = arg {
            visit_ty(ty, add);
        }
    }
}

fn visit_ty(ty: &Ty, add: &mut dyn FnMut(ItemId)) {
    match ty {
        Ty::Item(item, args) => {
            add(*item);
            for arg in args.iter() {
                if let Arg::Ty(ty) = arg {
                    visit_ty(ty, add);
                }
            }
        }
        Ty::Fn(fn_ty) => {
            for param in &fn_ty.params {
                visit_ty(&param.ty, add);
            }
            visit_ty(&fn_ty.result, add);
            if let Some(fails) = &fn_ty.fails {
                visit_ty(fails, add);
            }
        }
        Ty::Dyn(bound) => {
            add(bound.interface);
            for arg in bound.args.iter() {
                if let Arg::Ty(ty) = arg {
                    visit_ty(ty, add);
                }
            }
        }
        Ty::Registers(block) => add(*block),
        _ => {}
    }
}
