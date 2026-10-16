//! Calling forms: how each lowered function receives its parameters, returns its result and
//! fails. A value of a scalar type passes in a register, any other value as the address of its
//! bytes, and a reference as the address of its place; a function that may fail writes its
//! failure where its caller says and returns whether it failed.

use cranelift_codegen::ir::{AbiParam, Signature, Type, types};

use super::layout::Shape;
use super::{CALL_CONV, Instance, Lowering};
use crate::check::ty::{FnTy, Subst, Ty};
use crate::nir::{NTy, Role};
use crate::syntax::ast::Mode;

/// How a value is held: a scalar in a register, an address, or nothing for an empty type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repr {
    Scalar(Type),
    Address,
    Empty,
}

/// How a function gives its result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Returned {
    /// In a register.
    Scalar(Type),
    /// Written at the address the caller passes.
    Indirect,
    /// The address of a part, which a projection returns.
    Part,
    Empty,
}

#[derive(Clone, Debug)]
pub struct Abi {
    /// Whether the first parameter is the environment of a function value.
    pub env: bool,
    pub params: Vec<(NTy, Repr)>,
    pub result: Ty,
    pub returned: Returned,
    /// The failure type, when the function may fail.
    pub fails: Option<Ty>,
    /// Whether the function receives a failure address and returns whether it failed: those
    /// that may fail, and every function value, whichever its type, as a function that cannot
    /// fail fits a type that may.
    pub flagged: bool,
}

impl Abi {
    pub fn signature(&self) -> Signature {
        let mut signature = Signature::new(CALL_CONV);
        let pointer = AbiParam::new(types::I64);
        if self.env {
            signature.params.push(pointer);
        }
        if self.returned == Returned::Indirect {
            signature.params.push(pointer);
        }
        if self.flagged {
            signature.params.push(pointer);
            signature.returns.push(AbiParam::new(types::I8));
        }
        for (_, repr) in &self.params {
            match repr {
                Repr::Scalar(ty) => signature.params.push(AbiParam::new(*ty)),
                Repr::Address => signature.params.push(pointer),
                Repr::Empty => {}
            }
        }
        match self.returned {
            Returned::Scalar(ty) => signature.returns.push(AbiParam::new(ty)),
            Returned::Part => signature.returns.push(pointer),
            Returned::Indirect | Returned::Empty => {}
        }
        signature
    }
}

impl Lowering<'_, '_> {
    /// How a value of a NIR type is held.
    pub fn repr(&mut self, nty: &NTy) -> Repr {
        match nty {
            NTy::Ref(_) | NTy::RefMut(_) => Repr::Address,
            NTy::Owned(ty) => self.owned_repr(ty),
        }
    }

    pub fn owned_repr(&mut self, ty: &Ty) -> Repr {
        match &self.layout(ty).shape {
            Shape::Scalar { ty, .. } => Repr::Scalar(*ty),
            Shape::Empty => Repr::Empty,
            _ => Repr::Address,
        }
    }

    /// The NIR type of a parameter of a function type or head, its type concrete: a read
    /// parameter is its value when Copy or a view and a reference otherwise.
    pub fn param_nty(&self, mode: Mode, ty: Ty) -> NTy {
        match mode {
            Mode::Mut => NTy::RefMut(ty),
            Mode::Take => NTy::Owned(ty),
            Mode::Read => {
                if self.ck.nir_copy(&ty) || self.ck.nir_view(&ty) {
                    NTy::Owned(ty)
                } else {
                    NTy::Ref(ty)
                }
            }
        }
    }

    /// The type a function returning a value of `ty` gives and how it gives it: a projection
    /// gives a part, or an `Option` or `Result` wrapping its parts, unless the result is a view
    /// value.
    pub fn result_form(&mut self, ty: &Ty, projection: bool) -> (Ty, Returned) {
        let given = if projection && self.wraps(ty) {
            self.wrapped_type(ty)
        } else {
            ty.clone()
        };
        if projection && !self.ck.nir_view(ty) && !self.wraps(ty) {
            return (given, Returned::Part);
        }
        let returned = match self.owned_repr(&given) {
            Repr::Scalar(scalar) => Returned::Scalar(scalar),
            Repr::Address => Returned::Indirect,
            Repr::Empty => Returned::Empty,
        };
        (given, returned)
    }

    /// Whether a type is an `Option` or a `Result`, which a projection returns as a wrapped part.
    pub fn wraps(&self, ty: &Ty) -> bool {
        self.ck.is_std(ty, "core::Option") || self.ck.is_std(ty, "core::Result")
    }

    /// The calling form of a function type's values: the environment, then the parameters as
    /// its head gives them.
    pub fn fn_abi(&mut self, fn_ty: &FnTy) -> Abi {
        let params = fn_ty
            .params
            .iter()
            .map(|param| {
                let nty = self.param_nty(param.mode, param.ty.clone());
                let repr = self.repr(&nty);
                (nty, repr)
            })
            .collect();
        let (result, returned) = self.result_form(&fn_ty.result, false);
        Abi {
            env: true,
            params,
            result,
            returned,
            fails: failure(&fn_ty.fails),
            flagged: true,
        }
    }

    /// The calling form of an instance.
    pub fn abi(&mut self, instance: &Instance) -> Abi {
        match instance {
            Instance::Nir {
                item,
                role,
                index,
                args,
            } => {
                let unit = self.ck.unit(*item).expect("a unit lowered");
                let function = unit
                    .function(*role, *index)
                    .expect("a function of the unit");
                let subst = self.subst(*item, args);
                if *role == Role::Closure {
                    let captures = self.closure_captures(&unit, *index);
                    let fn_ty = self.closure_type(function, captures, &subst);
                    return self.fn_abi(&fn_ty);
                }
                let params = function
                    .params
                    .iter()
                    .map(|param| {
                        let nty = substituted(&subst, function.ty(param.value));
                        let repr = self.repr(&nty);
                        (nty, repr)
                    })
                    .collect();
                let projection = matches!(role, Role::Body | Role::Entry | Role::Default)
                    && self.is_projection(*item, *role, *index);
                let (result, returned) = self.result_form(&subst.ty(&function.result), projection);
                let fails = failure(&function.fails.as_ref().map(|ty| subst.ty(ty)));
                Abi {
                    env: false,
                    params,
                    result,
                    returned,
                    flagged: fails.is_some(),
                    fails,
                }
            }
            Instance::Supplied { item, args } => {
                let sig = self.ck.fn_sig(*item).expect("a supplied function's head");
                let subst = Subst::of(sig.generics.iter().map(|def| def.param), args);
                self.head_abi(&sig.fn_ty(), &subst, sig.is_proj())
            }
            Instance::Derived {
                owner,
                method,
                args,
            } => {
                let (fn_ty, projection) = self.derived_head(*owner, method, args);
                self.head_abi(&fn_ty, &Subst::default(), projection)
            }
            Instance::Conformance { ty, method, args } => {
                let fn_ty = self.conformance_head(ty, method, args);
                self.head_abi(&fn_ty, &Subst::default(), false)
            }
            Instance::Release(ty) => Abi {
                env: false,
                params: vec![(NTy::RefMut(ty.clone()), Repr::Address)],
                result: Ty::Unit,
                returned: Returned::Empty,
                fails: None,
                flagged: false,
            },
            Instance::Value(inner) => {
                let fn_ty = self.instance_fn_ty(inner);
                self.fn_abi(&fn_ty)
            }
            Instance::Dyn {
                ty,
                interface,
                args,
                method,
            } => {
                let (fn_ty, projection) = self.method_head(ty, *interface, args, *method);
                let mut abi = self.head_abi(&fn_ty, &Subst::default(), projection);
                abi.params[0] = (NTy::Ref(ty.clone()), Repr::Address);
                abi
            }
            Instance::Sides(inner) => Abi {
                result: Ty::Unit,
                returned: Returned::Indirect,
                ..self.abi(inner)
            },
        }
    }

    /// The calling form of a function head whose types `subst` makes concrete.
    fn head_abi(&mut self, fn_ty: &FnTy, subst: &Subst, projection: bool) -> Abi {
        let concrete = subst.fn_ty(fn_ty);
        let params = concrete
            .params
            .iter()
            .map(|param| {
                let nty = self.param_nty(param.mode, param.ty.clone());
                let repr = self.repr(&nty);
                (nty, repr)
            })
            .collect();
        let (result, returned) = self.result_form(&concrete.result, projection);
        let fails = failure(&concrete.fails);
        Abi {
            env: false,
            params,
            result,
            returned,
            flagged: fails.is_some(),
            fails,
        }
    }

    /// The substitution an item's generic parameters receive from an instance's arguments.
    pub fn subst(&self, item: crate::check::db::ItemId, args: &[crate::check::ty::Arg]) -> Subst {
        Subst::of(self.ck.sig(item).generics.iter().map(|def| def.param), args)
    }

    /// Whether a function of an item's unit returns as a projection does.
    fn is_projection(&self, item: crate::check::db::ItemId, role: Role, index: u32) -> bool {
        match role {
            Role::Default => self.interface_method_is_proj(item, index),
            _ => self.ck.fn_sig(item).is_some_and(|sig| sig.is_proj()),
        }
    }
}

/// The failure type of a head, none for one that cannot fail.
pub fn failure(fails: &Option<Ty>) -> Option<Ty> {
    fails.clone().filter(|ty| *ty != Ty::Never)
}

/// A NIR type with its type substituted.
pub fn substituted(subst: &Subst, nty: &NTy) -> NTy {
    match nty {
        NTy::Owned(ty) => NTy::Owned(subst.ty(ty)),
        NTy::Ref(ty) => NTy::Ref(subst.ty(ty)),
        NTy::RefMut(ty) => NTy::RefMut(subst.ty(ty)),
    }
}
