//! Failure types: which types a failure may have, which are cancellable, the bounds a generic
//! parameter's use as a failure type implies, and failure conversion.

use super::Checker;
use super::db::{ItemId, TypeKey};
use super::props::Property;
use super::sig::{FnSig, Payload, SigKind};
use super::ty::{FnTy, Param, Subst, Ty};
use crate::diagnostic::{Code, Fix};
use crate::syntax::ast::ItemKind;

/// The bounds a generic parameter carries from its uses as a failure type in its item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Implied {
    /// Its argument is a failure type or `Never`.
    pub failure: bool,
    /// Its argument is a cancellable type.
    pub cancellable: bool,
}

impl Checker<'_> {
    /// Whether a type is a failure type: an enum that is neither a view nor linear, a generic
    /// parameter not bounded by `Any`, or `task::Cancelled`.
    pub fn is_failure_type(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Item(item, _) => {
                matches!(self.sig(*item).kind, SigKind::Enum { .. })
                    && !self.has(ty, Property::View)
                    && !self.has(ty, Property::Linear)
                    || self.is_std(ty, "task::Cancelled")
            }
            Ty::Param(param) => !self
                .param_bounds(*param)
                .iter()
                .any(|bound| self.db.canonical(bound.interface) == "core::Any"),
            Ty::Error => true,
            _ => false,
        }
    }

    /// Whether a type is cancellable: `task::Cancelled`, an enum with a cancellation variant, or a
    /// generic parameter carrying the implied cancellable bound.
    pub fn is_cancellable(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Item(item, _) => {
                self.is_std(ty, "task::Cancelled") || self.cancellation_variant(*item).is_some()
            }
            Ty::Param(param) => self.implied(*param).cancellable,
            Ty::Error => true,
            _ => false,
        }
    }

    /// The from variant of an enum whose declared payload type is `task::Cancelled`.
    pub fn cancellation_variant(&self, item: ItemId) -> Option<usize> {
        let SigKind::Enum { variants } = &self.sig(item).kind else {
            return None;
        };
        variants.iter().position(|variant| {
            variant.from
                && matches!(&variant.payload, Payload::Single(payload)
                    if self.is_std(payload, "task::Cancelled"))
        })
    }

    /// The bounds a generic parameter's uses as a failure type in its item, or in the heads of the
    /// functions named after its type in the type's module, imply: a failure type in a `fails`
    /// clause or as the payload of a from variant, and a cancellable one as the failure type of a
    /// head or function type stating `uses wait`, or of a head with a parameter whose function
    /// type does.
    pub fn implied(&self, param: Param) -> Implied {
        let named = |ty: &Option<Ty>| *ty == Some(Ty::Param(param));
        let sig = self.sig(param.owner);
        let head = match &sig.kind {
            SigKind::Fn(head) => Some(head),
            SigKind::Interface { methods } => methods
                .iter()
                .map(|method| &method.sig)
                .find(|head| head.generics.iter().any(|def| def.param == param)),
            _ => None,
        };
        let types: Vec<&Ty> = match (head, &sig.kind) {
            (Some(head), _) => head_types(head).collect(),
            (None, SigKind::Struct { fields }) => fields.iter().map(|field| &field.ty).collect(),
            (None, SigKind::Enum { variants }) => variants
                .iter()
                .flat_map(|variant| match &variant.payload {
                    Payload::None => Vec::new(),
                    Payload::Single(ty) => vec![ty],
                    Payload::Labelled(fields) => fields.iter().map(|(_, ty)| ty).collect(),
                })
                .collect(),
            _ => Vec::new(),
        };
        let mut implied = Implied::default();
        for ty in types {
            ty.visit_fns(&mut |fn_ty| {
                if named(&fn_ty.fails) {
                    implied.failure = true;
                    implied.cancellable |= fn_ty.wait;
                }
            });
        }
        let members: Vec<ItemId> = match &sig.kind {
            SigKind::Struct { .. } | SigKind::Enum { .. } => self
                .db
                .members
                .get(&TypeKey::Item(param.owner))
                .into_iter()
                .flat_map(|members| members.values().flatten().copied())
                .filter(|function| self.db.module_of(*function) == self.db.module_of(param.owner))
                .collect(),
            _ => Vec::new(),
        };
        let member_heads: Vec<FnSig> = members
            .iter()
            .filter_map(|function| self.fn_sig(*function))
            .collect();
        for head in head.into_iter().chain(&member_heads) {
            if named(&head.fails) {
                implied.failure = true;
                implied.cancellable |= head.wait
                    || head
                        .params
                        .iter()
                        .any(|candidate| matches!(&candidate.ty, Ty::Fn(fn_ty) if fn_ty.wait));
            }
        }
        if let SigKind::Enum { variants } = &sig.kind && variants.iter().any(|variant| {
            variant.from
                && matches!(&variant.payload, Payload::Single(Ty::Param(found)) if *found == param)
        }) {
            implied.failure = true;
        }
        implied
    }

    /// Whether a head stating `uses wait`, or taking a function type that does, breaks the rule
    /// that its failure type is cancellable: one that is not, or a missing `fails` clause where
    /// `may_fail` does not already make the head miss it.
    pub fn cancel_unmapped(&self, head: &FnTy, may_fail: bool) -> bool {
        let waits = head.wait
            || head
                .params
                .iter()
                .any(|param| matches!(&param.ty, Ty::Fn(fn_ty) if fn_ty.wait));
        if !waits {
            return false;
        }
        match &head.fails {
            Some(fails) => !self.is_cancellable(fails),
            None => head.wait && !may_fail,
        }
    }

    /// The rules of a `fails` clause's type and the cancellation rule of the head holding it,
    /// for a head without a body.
    pub fn fails_problem(&self, head: &FnTy) -> Option<Code> {
        if let Some(fails) = &head.fails
            && !self.is_failure_type(fails)
        {
            return Some(Code::ErrorFailureType);
        }
        self.cancel_unmapped(head, false)
            .then_some(Code::ErrorCancelUnmapped)
    }

    /// The from variants of an enum: each with one unlabelled payload of a failure type, no two
    /// with equal declared payload types.
    pub fn check_from_variants(&self, item: ItemId) {
        let SigKind::Enum { variants } = &self.sig(item).kind else {
            return;
        };
        let mut seen: Vec<&Ty> = Vec::new();
        for variant in variants.iter().filter(|variant| variant.from) {
            let Payload::Single(payload) = &variant.payload else {
                self.report(item, variant.token, Code::ErrorFromShape);
                continue;
            };
            if !self.is_failure_type(payload) {
                self.report(item, variant.token, Code::ErrorFromShape);
                continue;
            }
            if seen.contains(&payload) {
                self.report(item, variant.token, Code::ErrorFromDuplicate);
            }
            seen.push(payload);
        }
    }

    /// Whether an enum instantiation is ambiguous: its arguments make the payload types of two
    /// generic from variants equal while no concrete from variant has that payload type.
    pub fn ambiguous(&self, ty: &Ty) -> bool {
        let Ty::Item(item, args) = ty else {
            return false;
        };
        let sig = self.sig(*item);
        let SigKind::Enum { variants } = &sig.kind else {
            return false;
        };
        let subst = Subst::of(sig.generics.iter().map(|def| def.param), args);
        let generic = |payload: &Ty| payload.mentions(&|param| param.owner == *item);
        let payloads: Vec<(Ty, bool)> = variants
            .iter()
            .filter(|variant| variant.from)
            .filter_map(|variant| match &variant.payload {
                Payload::Single(payload) => Some((subst.ty(payload), generic(payload))),
                _ => None,
            })
            .collect();
        payloads
            .iter()
            .enumerate()
            .any(|(index, (payload, is_generic))| {
                *is_generic
                    && payloads[index + 1..]
                        .iter()
                        .any(|(other, other_generic)| *other_generic && other == payload)
                    && !payloads
                        .iter()
                        .any(|(other, other_generic)| !*other_generic && other == payload)
            })
    }

    /// Whether a failure of type `from` converts to the failure type `to`: equal, through the
    /// from variant from resolution chooses, or a cancellation into a cancellable parameter.
    pub fn converts(&self, from: &Ty, to: &Ty) -> bool {
        if from == to || from.has_error() || to.has_error() || from.is_never() {
            return true;
        }
        if let Ty::Item(item, _) = to
            && self.chosen_from_variant(*item, to, from).is_some()
        {
            return true;
        }
        matches!(to, Ty::Param(_))
            && self.is_cancellable(to)
            && self.is_std(from, "task::Cancelled")
    }

    /// The name of the from variant failure conversion chooses for a failure of type `from` into
    /// the failure type `to`.
    pub fn conversion_variant(&self, from: &Ty, to: &Ty) -> Option<String> {
        let Ty::Item(item, _) = to else {
            return None;
        };
        let index = self.chosen_from_variant(*item, to, from)?;
        match &self.sig(*item).kind {
            SigKind::Enum { variants } => Some(variants[index].name.clone()),
            _ => None,
        }
    }

    /// The from variant of the enum `to` that from resolution chooses for a failure of type
    /// `from`: the one concrete candidate, else the one candidate.
    pub fn chosen_from_variant(&self, item: ItemId, to: &Ty, from: &Ty) -> Option<usize> {
        let sig = self.sig(item);
        let SigKind::Enum { variants } = &sig.kind else {
            return None;
        };
        let subst = Subst::of(sig.generics.iter().map(|def| def.param), to.args());
        let candidates: Vec<(usize, bool)> = variants
            .iter()
            .enumerate()
            .filter(|(_, variant)| variant.from)
            .filter_map(|(index, variant)| match &variant.payload {
                Payload::Single(payload) if subst.ty(payload) == *from => {
                    Some((index, !payload.mentions(&|param| param.owner == item)))
                }
                _ => None,
            })
            .collect();
        let concrete: Vec<usize> = candidates
            .iter()
            .filter(|(_, concrete)| *concrete)
            .map(|(index, _)| *index)
            .collect();
        match (&concrete[..], &candidates[..]) {
            ([index], _) | ([], [(index, _)]) => Some(*index),
            _ => None,
        }
    }
}

/// The types a head states: its parameters', result and failure type.
fn head_types(head: &FnSig) -> impl Iterator<Item = &Ty> {
    head.params
        .iter()
        .map(|param| &param.ty)
        .chain(std::iter::once(&head.result))
        .chain(head.fails.iter())
}

impl Checker<'_> {
    /// The fix adding `Cancelled(task::Cancelled) from` to a failure type that is an enum of the
    /// package being checked without a variant named `Cancelled`.
    pub fn cancelled_variant_fix(&self, failure: &Ty) -> Option<Fix> {
        let cancelled = Ty::item(self.db.std_item("task", "Cancelled")?, Vec::new());
        self.variant_added_fix(&cancelled, failure)
    }

    /// The exact fix of `error.unconvertible` from `from` to `to`: the variant `Name(C) from`
    /// added to `to`, an enum of the package being checked, when `from` mentions no generic
    /// parameter, `Name` being the last segment of its path with a final `Error` removed, unless
    /// that leaves no name or names a variant of `to`.
    pub fn variant_added_fix(&self, from: &Ty, to: &Ty) -> Option<Fix> {
        let (Ty::Item(source, _), Ty::Item(target, _)) = (from, to) else {
            return None;
        };
        let ItemKind::Enum(enumeration) = &self.db.ast(*target).kind else {
            return None;
        };
        if Some(self.db.package_of(*target)) != self.current_package() || from.mentions(&|_| true) {
            return None;
        }
        let path = self.db.canonical(*source);
        let last = path.rsplit([':', '.']).next()?;
        let name = last.strip_suffix("Error").unwrap_or(last);
        if name.is_empty()
            || enumeration
                .variants
                .iter()
                .any(|variant| variant.name.text == name)
        {
            return None;
        }
        let written = self.show(from, self.db.module_of(*target));
        self.member_added(*target, &format!("{name}({written}) from"))
    }
}
