//! The rules of function heads, closures and function types that read their types and modifier:
//! the forms of interrupt fns, a view result outside a projection, a state token beside
//! `uses wait`, the receiver a projection takes, and a foreign function that waits.

use super::Checker;
use super::props::Property;
use super::sig::FnSig;
use super::ty::{FnTy, Ty};
use crate::diagnostic::Code;
use crate::syntax::ast::{FnKind, FnModifier, Mode};

/// The core types whose type parameters accept views and hold their arguments as they are.
const VIEW_CONTAINERS: [&str; 7] = [
    "core::Array",
    "core::ArrayCursor",
    "core::Option",
    "core::Result",
    "core::Untrusted",
    "core::Slice",
    "core::SliceMut",
];

impl Checker<'_> {
    /// The rule a head breaks by its types: a view result outside a projection, or a parameter
    /// holding a state token beside `uses wait`.
    pub fn head_problem<'t>(
        &self,
        params: impl IntoIterator<Item = &'t Ty>,
        result: &Ty,
        wait: bool,
        proj: bool,
    ) -> Option<Code> {
        if !proj && self.counts_as_view(result) {
            return Some(Code::PropViewResult);
        }
        if wait && params.into_iter().any(|ty| self.holds_token(ty)) {
            return Some(Code::PropTokenWait);
        }
        None
    }

    /// The rule a function or method head breaks, a projection's receiver included.
    pub fn fn_head_problem(&self, sig: &FnSig) -> Option<Code> {
        if sig.modifier == Some(FnModifier::Interrupt)
            && (!sig.generics.is_empty() || !self.is_irq_form(&sig.fn_ty()))
        {
            return Some(Code::KernelIrqHead);
        }
        let typed = self.head_problem(
            sig.params.iter().map(|param| &param.ty),
            &sig.result,
            sig.wait,
            sig.is_proj(),
        );
        if typed.is_some() {
            return typed;
        }
        let receiver = sig
            .params
            .first()
            .is_some_and(|param| param.is_self && param.mode != Mode::Take);
        if sig.is_proj() && !receiver {
            return Some(Code::OwnProjReceiver);
        }
        (sig.modifier == Some(FnModifier::Foreign) && sig.wait).then_some(Code::EffectForeignWait)
    }

    /// The rule a function type breaks.
    pub fn fn_type_problem(&self, fn_ty: &FnTy) -> Option<Code> {
        if fn_ty.kind == FnKind::Interrupt && !self.is_irq_form(fn_ty) {
            return Some(Code::KernelIrqHead);
        }
        let typed = self.head_problem(
            fn_ty.params.iter().map(|param| &param.ty),
            &fn_ty.result,
            fn_ty.wait,
            false,
        );
        typed.or_else(|| {
            (fn_ty.convention.is_some() && fn_ty.wait).then_some(Code::EffectForeignWait)
        })
    }

    /// Whether a type is a view with each type parameter counted as none.
    pub fn counts_as_view(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Param(_) => false,
            Ty::Dyn(_) => true,
            Ty::Item(_, args)
                if [
                    "core::Option",
                    "core::Result",
                    "core::Array",
                    "core::ArrayCursor",
                    "core::Untrusted",
                ]
                .iter()
                .any(|path| self.is_std(ty, path)) =>
            {
                args.iter()
                    .filter_map(super::ty::Arg::ty)
                    .any(|arg| self.counts_as_view(arg))
            }
            _ => self.has(ty, Property::View),
        }
    }

    /// Whether a type is a core type whose type parameters accept views.
    pub fn is_view_container(&self, ty: &Ty) -> bool {
        VIEW_CONTAINERS.iter().any(|path| self.is_std(ty, path))
    }
}
