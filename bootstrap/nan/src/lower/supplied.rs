//! Supplied operations: the std functions without a body and the builtins, which lowering
//! implements. Each std function is a lowered function of its own, written by the family of its
//! owning type; builtins are lowered where they are called.

pub mod codecs;
pub mod floats;
pub mod generate;
pub mod mem;
pub mod numbers;
pub mod pointers;
pub mod render;
pub mod same;
pub mod sequences;
pub mod sims;
pub mod sys;
pub mod tasks;
pub mod text;

use cranelift_codegen::ir::{InstBuilder, Value};

use super::Lowering;
use super::abi::{Abi, Repr, Returned};
use super::body::{Body, Held};
use super::fx::Fx;
use crate::check::db::ItemId;
use crate::check::ty::{Args, Ty};
use crate::nir::{BlockId, Builtin, NTy, Operand, Value as NirValue};

/// The parameters of a supplied operation being defined, and how it returns.
pub struct Supply<'s> {
    pub abi: &'s Abi,
    pub args: Args,
    pub path: String,
    /// The type of the owner the operation is named after, with its arguments.
    pub owner: Option<Ty>,
    pub sret: Option<Value>,
    pub failure: Option<Value>,
    pub params: Vec<Held>,
}

impl Supply<'_> {
    /// The member name after the owner's path.
    pub fn member(&self) -> &str {
        self.path.rsplit_once('.').map_or("", |(_, member)| member)
    }

    pub fn param(&self, index: usize) -> Held {
        self.params[index]
    }

    pub fn type_arg(&self, index: usize) -> Ty {
        self.args
            .get(index)
            .and_then(crate::check::ty::Arg::ty)
            .cloned()
            .unwrap_or_else(|| panic!("type argument {index} of {}", self.path))
    }

    /// Returns a value held one way or another as the operation's result.
    pub fn ret(&self, lo: &mut Lowering, fx: &mut Fx, held: Held) {
        let mut returns = Vec::new();
        if self.abi.flagged {
            returns.push(fx.int(cranelift_codegen::ir::types::I8, 0));
        }
        match self.abi.returned {
            Returned::Scalar(_) => {
                let nty = NTy::Owned(self.abi.result.clone());
                let repr = lo.owned_repr(&self.abi.result);
                let held = super::call::adapt_value(lo, fx, held, &nty, repr);
                returns.push(held.scalar());
            }
            Returned::Part => returns.push(held.address()),
            Returned::Indirect => {
                let layout = lo.layout(&self.abi.result);
                let sret = self.sret.expect("a result address");
                if let Held::Address(source) = held
                    && source != sret
                {
                    lo.copy(fx, sret, source, layout.size, layout.align);
                }
            }
            Returned::Empty => {}
        }
        fx.b.ins().return_(&returns);
    }

    /// Returns `Unit`.
    pub fn done(&self, lo: &mut Lowering, fx: &mut Fx) {
        self.ret(lo, fx, Held::Empty);
    }

    /// Ends the operation on exhaustion: a `try_` form releases the values it took and fails
    /// with `mem::OutOfMemory.Exhausted`, any other form traps with `trap.exhausted`.
    pub fn exhausted(&self, lo: &mut Lowering, fx: &mut Fx, taken: &[(Held, Ty)]) {
        if !self.member().starts_with("try_") {
            super::runtime::report::raise_kind(lo, fx, "trap.exhausted");
            return;
        }
        for (held, ty) in taken {
            if let Held::Address(address) = held {
                lo.release(fx, ty, *address);
            }
        }
        let fails = self.abi.fails.clone().expect("a try_ form's failure type");
        let out_of_memory = lo.std_ty("mem", "OutOfMemory", Vec::new());
        let exhausted = lo.variant_value(fx, &out_of_memory, "Exhausted", &[], false);
        let value = if fails == out_of_memory {
            exhausted
        } else {
            lo.variant_value(fx, &fails, "OutOfMemory", &[exhausted], false)
        };
        let failure = self.failure.expect("a failure address");
        lo.store_held(fx, failure, value, &fails);
        self.fail(fx);
    }

    /// Fails with the failure value already written at the failure address.
    pub fn fail(&self, fx: &mut Fx) {
        let mut returns = vec![fx.int(cranelift_codegen::ir::types::I8, 1)];
        match self.abi.returned {
            Returned::Scalar(ty) => returns.push(super::body::zero(fx, ty)),
            Returned::Part => returns.push(fx.ptr(0)),
            _ => {}
        }
        fx.b.ins().return_(&returns);
    }
}

/// The address of element `index` of a sequence of elements of a type starting at `start`.
pub fn element_at(
    lo: &mut Lowering,
    fx: &mut Fx,
    element: &Ty,
    start: Value,
    index: Value,
) -> Value {
    let size = lo.layout(element).size;
    let offset = fx.b.ins().imul_imm_s(index, size as i64);
    fx.b.ins().iadd(start, offset)
}

/// Defines the lowered function of a std function without a body.
pub fn lower(lo: &mut Lowering, fx: &mut Fx, abi: &Abi, item: ItemId, args: &Args) {
    let incoming = fx.entry();
    let mut incoming = incoming.into_iter();
    let sret = (abi.returned == Returned::Indirect).then(|| incoming.next().expect("a result"));
    let failure = abi
        .flagged
        .then(|| incoming.next().expect("a failure address"));
    let params: Vec<Held> = abi
        .params
        .iter()
        .map(|(_, repr)| match repr {
            Repr::Scalar(_) => Held::Scalar(incoming.next().expect("a parameter")),
            Repr::Address => Held::Address(incoming.next().expect("a parameter")),
            Repr::Empty => Held::Empty,
        })
        .collect();
    for (position, ((nty, _), held)) in abi.params.iter().zip(&params).enumerate() {
        if let (Ty::Fn(_), Held::Address(address)) = (nty.ty(), held) {
            fx.fn_params.insert(*address, position);
        }
    }
    let path = lo.ck.db.canonical(item);
    let owner = match lo.ck.db.owners.get(&item) {
        Some(crate::check::db::TypeKey::Item(owner)) => {
            let generics = lo.ck.sig(*owner).generics.len();
            Some(Ty::item(
                *owner,
                args.iter().take(generics).cloned().collect(),
            ))
        }
        Some(crate::check::db::TypeKey::Scalar(scalar)) => Some(Ty::Scalar(*scalar)),
        _ => None,
    };
    let supply = Supply {
        abi,
        args: args.clone(),
        path: path.clone(),
        owner,
        sret,
        failure,
        params,
    };
    let lowered = numbers::lower(lo, fx, &supply)
        || generate::lower(lo, fx, &supply)
        || sequences::lower(lo, fx, &supply)
        || pointers::lower(lo, fx, &supply)
        || mem::lower(lo, fx, &supply)
        || render::lower(lo, fx, &supply)
        || (lo.seeded() && sims::lower(lo, fx, &supply))
        || same::lower(lo, fx, &supply)
        || sys::lower(lo, fx, &supply)
        || tasks::lower(lo, fx, &supply)
        || text::lower(lo, fx, &supply)
        || codecs::lower(lo, fx, &supply);
    assert!(lowered, "no lowering of {path}");
}

/// Lowers a builtin where it is called.
pub fn builtin(
    body: &mut Body,
    builtin: Builtin,
    args: &Args,
    operands: &[Operand],
    result: Option<NirValue>,
    block: BlockId,
    position: usize,
) -> Held {
    match builtin {
        Builtin::Conversion(conversion, scalar) => {
            numbers::conversion(body, conversion, scalar, &operands[0], block, position)
        }
        Builtin::Bits(conversion, owner) => {
            numbers::bits(body, conversion, owner, &operands[0], result)
        }
        Builtin::Intrinsic(path) => match path {
            "hw::mint_root" => sys::mint_root(body),
            "text::fmt" => render::template(body, args, operands),
            "Dyn.view" => render::dyn_view(body, args, &operands[0]),
            other => panic!("the intrinsic {other} has no lowering on this target"),
        },
    }
}

/// Whether releasing a std base type does anything, none for a type std declares in full.
pub fn base_needs_release(lo: &mut Lowering, path: &str, ty: &Ty) -> Option<bool> {
    mem::needs_release(lo, path, ty)
        .or_else(|| sys::needs_release(path))
        .or_else(|| tasks::needs_release(path))
        .or_else(|| text::needs_release(path))
}

/// Whether moving a value of a std base type into another domain's reach does anything; none
/// for a type std declares in full.
pub fn base_transfers(lo: &mut Lowering, path: &str, ty: &Ty) -> Option<bool> {
    mem::transfers(lo, path, ty)
        .or_else(|| sys::transfers(path))
        .or_else(|| tasks::transfers(path))
        .or_else(|| text::transfers(path))
}

/// Moves a value of a std base type into another domain's reach; false for a type std declares
/// in full.
pub fn transfer_base(
    lo: &mut Lowering,
    fx: &mut Fx,
    ty: &Ty,
    address: Value,
    lift: Value,
    owner: Value,
) -> bool {
    let Ty::Item(item, _) = ty else {
        return false;
    };
    let path = lo.ck.db.canonical(*item);
    mem::transfer(lo, fx, &path, ty, address, lift, owner)
        || sys::transfer(lo, fx, &path, address, owner)
        || tasks::transfer(lo, fx, &path, ty, address, lift, owner)
        || text::transfer(lo, fx, &path, address, lift)
}

/// Releases a value of a std base type; false for a type std declares in full.
pub fn release_base(lo: &mut Lowering, fx: &mut Fx, ty: &Ty, address: Value) -> bool {
    let Ty::Item(item, _) = ty else {
        return false;
    };
    let path = lo.ck.db.canonical(*item);
    mem::release(lo, fx, &path, ty, address)
        || sys::release(lo, fx, &path, ty, address)
        || tasks::release(lo, fx, &path, ty, address)
        || text::release(lo, fx, &path, address)
}
