//! Types as the checker computes them, their generic arguments and substitution.

use std::collections::HashMap;
use std::rc::Rc;

use super::db::ItemId;
use super::reversibility::Class;
use crate::syntax::ast::{FnKind, Mode};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scalar {
    U8,
    U16,
    U32,
    U64,
    Usize,
    I8,
    I16,
    I32,
    I64,
    Isize,
    F32,
    F64,
    Bool,
    Char,
}

impl Scalar {
    pub const ALL: [Scalar; 14] = [
        Scalar::U8,
        Scalar::U16,
        Scalar::U32,
        Scalar::U64,
        Scalar::Usize,
        Scalar::I8,
        Scalar::I16,
        Scalar::I32,
        Scalar::I64,
        Scalar::Isize,
        Scalar::F32,
        Scalar::F64,
        Scalar::Bool,
        Scalar::Char,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Scalar::U8 => "u8",
            Scalar::U16 => "u16",
            Scalar::U32 => "u32",
            Scalar::U64 => "u64",
            Scalar::Usize => "usize",
            Scalar::I8 => "i8",
            Scalar::I16 => "i16",
            Scalar::I32 => "i32",
            Scalar::I64 => "i64",
            Scalar::Isize => "isize",
            Scalar::F32 => "f32",
            Scalar::F64 => "f64",
            Scalar::Bool => "bool",
            Scalar::Char => "char",
        }
    }

    pub fn from_name(name: &str) -> Option<Scalar> {
        Scalar::ALL.into_iter().find(|scalar| scalar.name() == name)
    }

    pub fn is_integer(self) -> bool {
        !matches!(
            self,
            Scalar::F32 | Scalar::F64 | Scalar::Bool | Scalar::Char
        )
    }

    pub fn is_signed(self) -> bool {
        matches!(
            self,
            Scalar::I8 | Scalar::I16 | Scalar::I32 | Scalar::I64 | Scalar::Isize
        )
    }

    pub fn is_float(self) -> bool {
        matches!(self, Scalar::F32 | Scalar::F64)
    }

    pub fn is_number(self) -> bool {
        self.is_integer() || self.is_float()
    }

    /// The width of its bit pattern.
    pub fn bits(self) -> u32 {
        match self {
            Scalar::U8 | Scalar::I8 | Scalar::Bool => 8,
            Scalar::U16 | Scalar::I16 => 16,
            Scalar::U32 | Scalar::I32 | Scalar::F32 | Scalar::Char => 32,
            _ => 64,
        }
    }

    /// The least and greatest values of an integer type.
    pub fn range(self) -> (i128, i128) {
        let bits = self.bits();
        if self.is_signed() {
            (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
        } else {
            (0, (1i128 << bits) - 1)
        }
    }

    /// The type `to_bits` returns.
    /// Whether the scalar is an unsigned integer that is its own bits type.
    pub fn is_unsigned_bits(self) -> bool {
        matches!(self, Scalar::U8 | Scalar::U16 | Scalar::U32 | Scalar::U64)
    }

    pub fn bits_type(self) -> Scalar {
        match self {
            Scalar::U8 | Scalar::I8 | Scalar::Bool => Scalar::U8,
            Scalar::U16 | Scalar::I16 => Scalar::U16,
            Scalar::U32 | Scalar::I32 | Scalar::F32 | Scalar::Char => Scalar::U32,
            Scalar::U64 | Scalar::I64 | Scalar::F64 | Scalar::Usize | Scalar::Isize => Scalar::U64,
        }
    }
}

/// A generic parameter: the item declaring it and its index, `SELF` for an interface's `Self`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Param {
    pub owner: ItemId,
    pub index: u16,
}

impl Param {
    pub const SELF: u16 = u16::MAX;

    pub fn is_self(self) -> bool {
        self.index == Param::SELF
    }
}

/// A const generic argument.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Const {
    Int(i128),
    Bool(bool),
    Param(Param),
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Arg {
    Ty(Ty),
    Const(Const),
}

impl Arg {
    pub fn ty(&self) -> Option<&Ty> {
        match self {
            Arg::Ty(ty) => Some(ty),
            Arg::Const(_) => None,
        }
    }
}

pub type Args = Rc<[Arg]>;

/// The demands of a `limits` clause.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct LimitSet {
    pub pure: bool,
    pub notrap: bool,
    pub terminates: bool,
    pub noalloc: bool,
    pub class: Option<Class>,
    pub stack: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FnParam {
    pub mode: Mode,
    pub name: String,
    pub ty: Ty,
}

/// A function type: a full head without a name or generic parameters.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FnTy {
    pub kind: FnKind,
    pub params: Vec<FnParam>,
    pub result: Ty,
    pub fails: Option<Ty>,
    pub wait: bool,
    pub limits: LimitSet,
    pub convention: Option<String>,
}

/// An interface with its arguments.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Bound {
    pub interface: ItemId,
    pub args: Args,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    Scalar(Scalar),
    Unit,
    Never,
    /// A struct, enum or layout, std's core types included, with its arguments.
    Item(ItemId, Args),
    Param(Param),
    Fn(Rc<FnTy>),
    Dyn(Rc<Bound>),
    /// A registers block, which is a type only as the argument of a register window.
    Registers(ItemId),
    /// The type of a failed construct, which satisfies every rule checking it.
    Error,
}

pub fn no_args() -> Args {
    Rc::from(Vec::new())
}

impl Ty {
    pub fn item(item: ItemId, args: Vec<Arg>) -> Ty {
        Ty::Item(item, Rc::from(args))
    }

    pub fn is_never(&self) -> bool {
        matches!(self, Ty::Never)
    }

    pub fn scalar(&self) -> Option<Scalar> {
        match self {
            Ty::Scalar(scalar) => Some(*scalar),
            _ => None,
        }
    }

    /// The type arguments of an item type.
    pub fn args(&self) -> &[Arg] {
        match self {
            Ty::Item(_, args) => args,
            _ => &[],
        }
    }

    pub fn type_arg(&self, index: usize) -> Option<&Ty> {
        self.args().get(index).and_then(Arg::ty)
    }

    /// Whether the type mentions `Ty::Error` anywhere.
    pub fn has_error(&self) -> bool {
        match self {
            Ty::Error => true,
            Ty::Item(_, args) => args.iter().any(|arg| match arg {
                Arg::Ty(ty) => ty.has_error(),
                Arg::Const(constant) => *constant == Const::Error,
            }),
            Ty::Fn(fn_ty) => {
                fn_ty.params.iter().any(|param| param.ty.has_error())
                    || fn_ty.result.has_error()
                    || fn_ty.fails.as_ref().is_some_and(Ty::has_error)
            }
            Ty::Dyn(bound) => bound
                .args
                .iter()
                .any(|arg| arg.ty().is_some_and(Ty::has_error)),
            _ => false,
        }
    }

    /// Visits each function type the type holds, at any depth.
    pub fn visit_fns(&self, visit: &mut dyn FnMut(&FnTy)) {
        match self {
            Ty::Item(_, args) => {
                for arg in args.iter() {
                    if let Arg::Ty(ty) = arg {
                        ty.visit_fns(visit);
                    }
                }
            }
            Ty::Fn(fn_ty) => {
                visit(fn_ty);
                for param in &fn_ty.params {
                    param.ty.visit_fns(visit);
                }
                fn_ty.result.visit_fns(visit);
                if let Some(fails) = &fn_ty.fails {
                    fails.visit_fns(visit);
                }
            }
            Ty::Dyn(bound) => {
                for arg in bound.args.iter() {
                    if let Arg::Ty(ty) = arg {
                        ty.visit_fns(visit);
                    }
                }
            }
            _ => {}
        }
    }

    /// Whether the type mentions one of the parameters `params` is given for.
    pub fn mentions(&self, params: &dyn Fn(Param) -> bool) -> bool {
        match self {
            Ty::Param(param) => params(*param),
            Ty::Item(_, args) => args.iter().any(|arg| match arg {
                Arg::Ty(ty) => ty.mentions(params),
                Arg::Const(Const::Param(param)) => params(*param),
                Arg::Const(_) => false,
            }),
            Ty::Fn(fn_ty) => {
                fn_ty.params.iter().any(|param| param.ty.mentions(params))
                    || fn_ty.result.mentions(params)
                    || fn_ty.fails.as_ref().is_some_and(|ty| ty.mentions(params))
            }
            Ty::Dyn(bound) => bound.args.iter().any(|arg| match arg {
                Arg::Ty(ty) => ty.mentions(params),
                Arg::Const(Const::Param(param)) => params(*param),
                Arg::Const(_) => false,
            }),
            _ => false,
        }
    }
}

/// A substitution of generic parameters by arguments.
#[derive(Clone, Debug, Default)]
pub struct Subst(pub HashMap<Param, Arg>);

impl Subst {
    pub fn of(params: impl IntoIterator<Item = Param>, args: &[Arg]) -> Subst {
        Subst(params.into_iter().zip(args.iter().cloned()).collect())
    }

    pub fn insert(&mut self, param: Param, arg: Arg) {
        self.0.insert(param, arg);
    }

    pub fn get(&self, param: Param) -> Option<&Arg> {
        self.0.get(&param)
    }

    pub fn ty(&self, ty: &Ty) -> Ty {
        if self.0.is_empty() {
            return ty.clone();
        }
        match ty {
            Ty::Param(param) => match self.0.get(param) {
                Some(Arg::Ty(ty)) => ty.clone(),
                _ => ty.clone(),
            },
            Ty::Item(item, args) => Ty::Item(*item, self.args(args)),
            Ty::Fn(fn_ty) => Ty::Fn(Rc::new(self.fn_ty(fn_ty))),
            Ty::Dyn(bound) => Ty::Dyn(Rc::new(self.bound(bound))),
            _ => ty.clone(),
        }
    }

    pub fn args(&self, args: &[Arg]) -> Args {
        args.iter().map(|arg| self.arg(arg)).collect()
    }

    pub fn arg(&self, arg: &Arg) -> Arg {
        match arg {
            Arg::Ty(ty) => Arg::Ty(self.ty(ty)),
            Arg::Const(Const::Param(param)) => match self.0.get(param) {
                Some(Arg::Const(constant)) => Arg::Const(constant.clone()),
                _ => arg.clone(),
            },
            Arg::Const(_) => arg.clone(),
        }
    }

    pub fn fn_ty(&self, fn_ty: &FnTy) -> FnTy {
        FnTy {
            kind: fn_ty.kind,
            params: fn_ty
                .params
                .iter()
                .map(|param| FnParam {
                    mode: param.mode,
                    name: param.name.clone(),
                    ty: self.ty(&param.ty),
                })
                .collect(),
            result: self.ty(&fn_ty.result),
            fails: fn_ty.fails.as_ref().map(|ty| self.ty(ty)),
            wait: fn_ty.wait,
            limits: fn_ty.limits,
            convention: fn_ty.convention.clone(),
        }
    }

    pub fn bound(&self, bound: &Bound) -> Bound {
        Bound {
            interface: bound.interface,
            args: self.args(&bound.args),
        }
    }
}
