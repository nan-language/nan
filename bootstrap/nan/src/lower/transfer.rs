//! Transfer: what moving a value into another domain's reach does, where a value leaves a
//! domain. The chunks holding the value's memory that domains nested in a target domain own
//! pass to it, and the holdings the value owns pass to their new owner.
//!
//! A walk gives the target of its chunks, zero for none, and the new owner of holdings: a
//! domain, zero for none, `KEEP`, or a domain with `RAISE` set, which passes only holdings that
//! domains nested in it own. Storage that several handles reach, a cown's value, a shared
//! value's contents and a channel's buffer, records its reach, the innermost domain enclosing
//! every domain that reaches it: a walk enters it only when its reach widens, and the holdings
//! inside it are raised to that reach.

use cranelift_codegen::ir::{
    AbiParam, InstBuilder, MemFlagsData, Signature, Value, condcodes::IntCC, types,
};
use cranelift_module::FuncId;

use super::fx::{Fx, PTR};
use super::layout::Shape;
use super::runtime::Routine;
use super::{CALL_CONV, Glue, Instance, Lowering, supplied};
use crate::check::props::Property;
use crate::check::sig::SigKind;
use crate::check::ty::Ty;

/// The owner of holdings that leaves every holding as it is.
pub const KEEP: i64 = -1;

/// The bit of an owner that passes only holdings domains nested in it own.
pub const RAISE: i64 = 1;

/// The bytes of a task closure's environment before its captures: the address of the walk
/// moving them, zero when moving them does nothing.
pub const ENV_HEADER: u64 = 8;

impl Lowering<'_, '_> {
    /// Whether moving a value of a type into another domain's reach does anything.
    pub fn transfers(&mut self, ty: &Ty) -> bool {
        if let Some(transfers) = self.transfers.get(ty) {
            return *transfers;
        }
        self.transfers.insert(ty.clone(), false);
        let transfers = self.compute_transfers(ty);
        self.transfers.insert(ty.clone(), transfers);
        transfers
    }

    fn compute_transfers(&mut self, ty: &Ty) -> bool {
        match ty {
            Ty::Item(item, _) => {
                if !self.ck.has(ty, Property::Send) || self.ck.has(ty, Property::Copy) {
                    return false;
                }
                if self.ck.db.is_std(*item) {
                    let path = self.ck.db.canonical(*item);
                    if let Some(transfers) = supplied::base_transfers(self, &path, ty) {
                        return transfers;
                    }
                    let base = matches!(&self.ck.sig(*item).kind, SigKind::Struct { fields } if fields.is_empty());
                    let structural = matches!(
                        path.as_str(),
                        "core::Array" | "core::Untrusted" | "core::ArrayCursor"
                    );
                    if base && !structural && self.layout(ty).size > 0 {
                        panic!("no transfer of {path}");
                    }
                }
                let layout = self.layout(ty);
                let parts: Vec<Ty> = match &layout.shape {
                    Shape::Record(fields) | Shape::Bytes { fields, .. } => {
                        fields.iter().map(|field| field.ty.clone()).collect()
                    }
                    Shape::Enum(shape) => shape
                        .variants
                        .iter()
                        .flat_map(|variant| variant.fields.iter().map(|field| field.ty.clone()))
                        .collect(),
                    Shape::Array { element, .. } => vec![element.clone()],
                    _ => Vec::new(),
                };
                parts.iter().any(|part| self.transfers(part))
            }
            Ty::Fn(fn_ty) => fn_ty.kind == crate::syntax::ast::FnKind::Task,
            _ => false,
        }
    }

    /// Moves the value of a type at an address into another domain's reach: the chunks of its
    /// memory pass to `lift` unless it is zero, and its holdings to `owner`.
    pub fn transfer(&mut self, fx: &mut Fx, ty: &Ty, address: Value, lift: Value, owner: Value) {
        if !self.transfers(ty) {
            return;
        }
        let glue = walker(self, ty);
        let reference = self.func_ref(fx, glue);
        fx.b.ins().call(reference, &[address, lift, owner]);
    }

    /// The address of the glue moving a value of a type, zero when moving one does nothing.
    pub fn walker_address(&mut self, fx: &mut Fx, ty: &Ty) -> Value {
        if !self.transfers(ty) {
            return fx.ptr(0);
        }
        let glue = walker(self, ty);
        let reference = self.func_ref(fx, glue);
        fx.b.ins().func_addr(PTR, reference)
    }
}

/// The signature of a walk: the value's address, the target of its chunks and the new owner of
/// its holdings.
pub fn signature() -> Signature {
    let mut signature = Signature::new(CALL_CONV);
    for _ in 0..3 {
        signature.params.push(AbiParam::new(PTR));
    }
    signature
}

/// Calls a walk through its address, which is not zero.
pub fn call_walker(fx: &mut Fx, walker: Value, args: &[Value]) {
    let signature = fx.b.import_signature(signature());
    fx.b.ins().call_indirect(signature, walker, args);
}

/// Passes the chunks an allocation lies in to `lift` unless it is zero; gives whether they
/// passed.
pub fn lift(lo: &mut Lowering, fx: &mut Fx, address: Value, lift: Value) -> Value {
    let lifting = fx.b.ins().icmp_imm_s(IntCC::NotEqual, lift, 0);
    fx.choose_in(
        lo,
        lifting,
        &[types::I8],
        |lo, fx| lo.call_routine(fx, Routine::Lift, &[address, lift]),
        |_, fx| vec![fx.int(types::I8, 0)],
    )[0]
}

/// Passes the holding a word at an address names to `owner` unless it is `KEEP`.
pub fn rehome(lo: &mut Lowering, fx: &mut Fx, address: Value, owner: Value) {
    let moves = fx.b.ins().icmp_imm_s(IntCC::NotEqual, owner, KEEP);
    fx.when(moves, |fx| {
        let holding = fx.load(PTR, address, 0);
        lo.call_routine(fx, Routine::Rehome, &[holding, owner]);
    });
}

/// The owner raising the holdings in storage whose reach is a domain.
pub fn raised(fx: &mut Fx, reach: Value) -> Value {
    fx.b.ins().bor_imm_s(reach, RAISE)
}

/// Widens the reach a word holds to a domain enclosing it, atomically; gives whether this call
/// widened it.
pub fn widen(fx: &mut Fx, word: Value, to: Value) -> Value {
    let flags = MemFlagsData::trusted();
    let none = fx.int(types::I8, 0);
    fx.repeat(&[none], |fx, _| {
        let reach = fx.b.ins().atomic_load(PTR, flags, word);
        let inside = super::runtime::tasks::nested_in(fx, reach, to);
        let outcome = fx.choose(
            inside,
            &[types::I8, types::I8],
            |fx| {
                let seen = fx.b.ins().atomic_cas(flags, word, reach, to);
                let swapped = fx.b.ins().icmp(IntCC::Equal, seen, reach);
                vec![fx.b.ins().bxor_imm_s(swapped, 1), swapped]
            },
            |fx| vec![fx.int(types::I8, 0), fx.int(types::I8, 0)],
        );
        (outcome[0], vec![outcome[1]])
    })[0]
}

/// The address of the walk moving the owned captures of a task closure, each of a type at an
/// offset of its environment; zero when moving them does nothing.
pub fn captures(
    lo: &mut Lowering,
    fx: &mut Fx,
    closure: Instance,
    captures: Vec<(Ty, u64)>,
) -> Value {
    let captures: Vec<(Ty, u64)> = captures
        .into_iter()
        .filter(|(ty, _)| lo.transfers(ty))
        .collect();
    if captures.is_empty() {
        return fx.ptr(0);
    }
    let glue = lo.glue_function(Glue::Captures(closure), signature(), move |lo, fx| {
        let params = fx.entry();
        for (ty, offset) in &captures {
            let at = fx.offset(params[0], *offset);
            lo.transfer(fx, ty, at, params[1], params[2]);
        }
        fx.b.ins().return_(&[]);
    });
    let reference = lo.func_ref(fx, glue);
    fx.b.ins().func_addr(PTR, reference)
}

fn walker(lo: &mut Lowering, ty: &Ty) -> FuncId {
    let ty = ty.clone();
    lo.glue_function(Glue::Transfer(ty.clone()), signature(), move |lo, fx| {
        let params = fx.entry();
        define(lo, fx, &ty, params[0], params[1], params[2]);
        fx.b.ins().return_(&[]);
    })
}

fn define(lo: &mut Lowering, fx: &mut Fx, ty: &Ty, address: Value, lift: Value, owner: Value) {
    if let Ty::Item(item, _) = ty
        && lo.ck.db.is_std(*item)
        && supplied::transfer_base(lo, fx, ty, address, lift, owner)
    {
        return;
    }
    if let Ty::Fn(_) = ty {
        let env = fx.load(PTR, address, 8);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, env, 0);
        fx.when(present, |fx| {
            let walker = fx.load(PTR, env, 0);
            let walks = fx.b.ins().icmp_imm_s(IntCC::NotEqual, walker, 0);
            fx.when(walks, |fx| call_walker(fx, walker, &[env, lift, owner]));
        });
        return;
    }
    let layout = lo.layout(ty);
    match &layout.shape {
        Shape::Record(fields) | Shape::Bytes { fields, .. } => {
            for field in fields.clone() {
                let at = fx.offset(address, field.offset);
                lo.transfer(fx, &field.ty, at, lift, owner);
            }
        }
        Shape::Array { element, count } => {
            let element = element.clone();
            if lo.transfers(&element) {
                let size = lo.layout(&element).size;
                let start = fx.ptr(0);
                let end = fx.ptr(*count);
                fx.each(start, end, |fx, index| {
                    let offset = fx.b.ins().imul_imm_s(index, size as i64);
                    let at = fx.b.ins().iadd(address, offset);
                    lo.transfer(fx, &element, at, lift, owner);
                });
            }
        }
        Shape::Enum(shape) => {
            for variant in &shape.variants {
                let fields: Vec<_> = variant
                    .fields
                    .iter()
                    .filter(|field| lo.transfers(&field.ty))
                    .cloned()
                    .collect();
                if fields.is_empty() {
                    continue;
                }
                let is = lo.variant_is(fx, address, ty, &variant.name);
                fx.when(is, |fx| {
                    for field in &fields {
                        let at = fx.offset(address, field.offset);
                        lo.transfer(fx, &field.ty, at, lift, owner);
                    }
                });
            }
        }
        _ => {}
    }
}
