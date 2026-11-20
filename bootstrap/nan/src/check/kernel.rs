//! Kernel rules: the anchor reached only from entry contexts, the forms of interrupt fns and
//! `interrupt fn` types, float use under a kernel target, and the floats a loaded registry's
//! function fields pass.

use super::Checker;
use super::db::ItemId;
use super::digest::Brought;
use super::sig::SigKind;
use super::tir::TBody;
use super::ty::{Arg, FnParam, FnTy, Ty};
use crate::diagnostic::Code;
use crate::syntax::ast::{FnModifier, ItemKind, Mode};

/// The types of the information a trap frame carries.
const FRAME_INFOS: [&str; 3] = ["hw::NoCode", "hw::ErrorCode", "hw::PageFault"];

impl Checker<'_> {
    /// The names of the function fields of a registry's entry type that pass a float: a
    /// parameter, result or failure type of theirs holds one.
    pub fn float_fields(&self, registry: ItemId) -> Vec<String> {
        let SigKind::Registry {
            entry: Ty::Item(structure, _),
        } = &self.sig(registry).kind
        else {
            return Vec::new();
        };
        let SigKind::Struct { fields } = &self.sig(*structure).kind else {
            return Vec::new();
        };
        let float = |ty: &Ty| {
            self.any_part(
                ty,
                &|part| matches!(part, Ty::Scalar(scalar) if scalar.is_float()),
            )
        };
        fields
            .iter()
            .filter(|field| match &field.ty {
                Ty::Fn(fn_ty) => {
                    fn_ty.params.iter().any(|param| float(&param.ty))
                        || float(&fn_ty.result)
                        || fn_ty.fails.as_ref().is_some_and(float)
                }
                _ => false,
            })
            .map(|field| field.name.clone())
            .collect()
    }

    /// Whether an item is an entry context: an interrupt or landing fn.
    pub fn is_entry_context(&self, item: ItemId) -> bool {
        matches!(&self.db.ast(item).kind, ItemKind::Fn(function)
            if matches!(function.modifier, Some((FnModifier::Interrupt | FnModifier::Landing, _))))
    }

    /// Whether the head of an interrupt fn or an `interrupt fn` type has one of the interrupt,
    /// device and exception forms: no `fails` or `uses wait`, an `hw::IrqExit` result, and an
    /// `hw::IrqContext` taken after an optional `mut` device, or a `mut` trap frame alone.
    pub fn is_irq_form(&self, head: &FnTy) -> bool {
        let is = |ty: &Ty, path: &str| ty.has_error() || self.is_std(ty, path);
        let context = |param: &FnParam| param.mode == Mode::Take && is(&param.ty, "hw::IrqContext");
        let frame = |param: &FnParam| {
            param.mode == Mode::Mut
                && match &param.ty {
                    Ty::Item(_, args) if is(&param.ty, "hw::TrapFrame") => matches!(&args[..],
                        [Arg::Ty(info)] if FRAME_INFOS.iter().any(|path| is(info, path))),
                    ty => ty.has_error(),
                }
        };
        head.fails.is_none()
            && !head.wait
            && is(&head.result, "hw::IrqExit")
            && match &head.params[..] {
                [only] => context(only) || frame(only),
                [device, irq] => device.mode == Mode::Mut && context(irq),
                _ => false,
            }
    }

    /// Each float operation of a function's or const's text, closures included, under a machine
    /// target; under a nanos target, an interrupt fn whose digest uses floats.
    pub fn check_float_use(&self, item: ItemId, body: &TBody) {
        if self.db.target.is_nanos() {
            let interrupt = matches!(&self.db.ast(item).kind, ItemKind::Fn(function)
                if matches!(function.modifier, Some((FnModifier::Interrupt, _))));
            if interrupt && self.digest(item).float {
                self.report(item, self.db.ast(item).token, Code::KernelFloatUse);
            }
            return;
        }
        if !self.db.target.is_machine() {
            return;
        }
        for site in self.text_sites(item, body) {
            if site.brought == Brought::Operation && site.facts.float {
                self.report(item, site.token, Code::KernelFloatUse);
            }
        }
    }
}
