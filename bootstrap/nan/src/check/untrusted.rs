//! Untrusted data: what `Untrusted.of` may wrap, where `inspect` appears, the rules of validators,
//! parsing types, and the validator and decoder operations that need a `validate` grant.

use super::Checker;
use super::db::ItemId;
use super::props::Property;
use super::sig::{FnSig, SigKind};
use super::ty::{Arg, Ty};
use crate::diagnostic::Code;
use crate::syntax::ast::{FnModifier, ItemKind, Tok};
use crate::workspace::Authority;

impl Checker<'_> {
    /// Whether a type is a parsing type: a scalar, `Unit`, `Never`, an Untrusted instantiation, a
    /// layout or a struct or enum other than a base type built only from parsing types.
    pub fn is_parsing(&self, ty: &Ty) -> bool {
        self.parsing(ty, None, &mut Vec::new())
    }

    /// Whether a type is a parsing type, the type parameters of `counted` counted as parsing and
    /// the items of `seen` taken as parsing while their definitions are decided.
    fn parsing(&self, ty: &Ty, counted: Option<ItemId>, seen: &mut Vec<ItemId>) -> bool {
        match ty {
            Ty::Scalar(_) | Ty::Unit | Ty::Never | Ty::Error => true,
            Ty::Param(param) => Some(param.owner) == counted,
            Ty::Item(item, args) => {
                if self.is_std(ty, "core::Untrusted") {
                    return true;
                }
                let core_enum = self.is_std(ty, "core::Option") || self.is_std(ty, "core::Result");
                if self.is_base_type(*item) && !core_enum {
                    return false;
                }
                let kind_parses = matches!(
                    self.sig(*item).kind,
                    SigKind::Struct { .. } | SigKind::Enum { .. } | SigKind::Layout { .. }
                );
                kind_parses
                    && args.iter().all(|arg| match arg {
                        Arg::Ty(ty) => self.parsing(ty, counted, seen),
                        Arg::Const(_) => true,
                    })
                    && self.parsing_definition(*item, seen)
            }
            _ => false,
        }
    }

    /// Whether a struct, enum or layout is a parsing type with its type parameters counted as
    /// parsing types.
    fn parsing_definition(&self, item: ItemId, seen: &mut Vec<ItemId>) -> bool {
        if seen.contains(&item) {
            return true;
        }
        seen.push(item);
        let parses = self
            .constituents(item)
            .iter()
            .all(|constituent| self.parsing(constituent, Some(item), seen));
        seen.pop();
        parses
    }

    /// Whether a type item's decoder is declassifying: that of a generic item, or of one that is
    /// no parsing type.
    pub fn declassifying_decoder(&self, item: ItemId) -> bool {
        !self.sig(item).generics.is_empty() || !self.is_parsing(&Ty::item(item, Vec::new()))
    }

    /// The rules of a function's `validator` modifier: an Untrusted input, no interface method,
    /// and the `validate` grant its declaration needs.
    pub fn check_validator(&self, item: ItemId, sig: &FnSig) {
        let token = self.db.ast(item).token;
        let inputs = sig
            .params
            .iter()
            .any(|param| self.is_std(&param.ty, "core::Untrusted"));
        let broken = if !self.implemented_lines(item).is_empty() {
            Some(Code::UntrustedValidatorInterface)
        } else {
            (!inputs).then_some(Code::UntrustedValidatorNoInput)
        };
        match broken {
            Some(code) => {
                self.report(item, token, code);
                self.perform(item, Authority::Validate);
            }
            None => self.require(item, token, Authority::Validate),
        }
    }

    /// The `validate` grant a type item deriving `Encode` with a declassifying decoder needs, on
    /// the line of its `derives` clause.
    pub fn check_decoder_derive(&self, item: ItemId) {
        let (derives, token) = match &self.db.ast(item).kind {
            ItemKind::Struct(structure) => (&structure.derives, structure.derives_token),
            ItemKind::Enum(enumeration) => (&enumeration.derives, enumeration.derives_token),
            _ => return,
        };
        if let Some(token) = token
            && derives.iter().any(|name| name.text == "Encode")
            && self.derive_problem(item, "Encode").is_none()
            && self.declassifying_decoder(item)
        {
            self.require(item, token, Authority::Validate);
        }
    }

    /// The `validate` grant an impl line naming `core::Decoder` needs.
    pub fn check_decoder_impl(&self, item: ItemId) {
        if let SigKind::Impl {
            interface: Some(bound),
            ..
        } = &self.sig(item).kind
            && self.db.canonical(bound.interface) == "core::Decoder"
        {
            self.require(item, self.db.ast(item).token, Authority::Validate);
        }
    }

    /// Whether a function is a validator: declared `validator`, or implementing a method of
    /// `core::Decoder`.
    pub fn is_validator(&self, function: ItemId) -> bool {
        let declared = matches!(&self.db.ast(function).kind,
            ItemKind::Fn(head) if head.modifier.is_some_and(|(modifier, _)| modifier == FnModifier::Validator));
        declared
            || self.implemented_lines(function).iter().any(|&line| {
                matches!(&self.sig(line).kind, SigKind::Impl { interface: Some(bound), .. }
                    if self.db.canonical(bound.interface) == "core::Decoder")
            })
    }

    /// Reports the `validate` grant a use of a validator, or of the decoder of an instantiation
    /// that is no parsing type, needs.
    pub fn check_validator_use(&self, item: ItemId, token: Tok, function: ItemId) {
        if self.is_validator(function) {
            self.require(item, token, Authority::Validate);
        }
    }

    /// Reports the `validate` grant a use of the decoder of `decoded` needs.
    pub fn check_decoder_use(&self, item: ItemId, token: Tok, decoded: &Ty) {
        if !decoded.has_error() && !self.is_parsing(decoded) {
            self.require(item, token, Authority::Validate);
        }
    }

    /// Whether `Untrusted.of` may wrap a type: not one that is or contains `SliceMut`, `Dyn`, a
    /// type with interior mutability, `task::Sender` or `sys::Outbox`, nor holding a type parameter
    /// that is a view or has no bound naming `Inert`.
    pub fn wraps_untrusted(&self, ty: &Ty) -> bool {
        let aliasing = |ty: &Ty| {
            matches!(ty, Ty::Dyn(_))
                || ["core::SliceMut", "task::Sender", "sys::Outbox"]
                    .iter()
                    .any(|path| self.is_std(ty, path))
        };
        let open_param = |ty: &Ty| match ty {
            Ty::Param(param) => {
                self.has(ty, Property::View)
                    || !self
                        .param_bounds(*param)
                        .iter()
                        .any(|bound| self.db.canonical(bound.interface) == "core::Inert")
            }
            _ => false,
        };
        !self.any_part(ty, &|part| aliasing(part) || open_param(part)) && !self.has_interior(ty)
    }

    /// Whether a type, one of its type arguments at any depth, or a constituent of a struct or
    /// enum it instantiates satisfies `test`.
    fn any_part(&self, ty: &Ty, test: &dyn Fn(&Ty) -> bool) -> bool {
        let mut seen = Vec::new();
        self.any_part_from(ty, test, &mut seen)
    }

    fn any_part_from(&self, ty: &Ty, test: &dyn Fn(&Ty) -> bool, seen: &mut Vec<ItemId>) -> bool {
        if test(ty) {
            return true;
        }
        match ty {
            Ty::Item(item, args) => {
                if args
                    .iter()
                    .filter_map(Arg::ty)
                    .any(|arg| self.any_part_from(arg, test, seen))
                {
                    return true;
                }
                if seen.contains(item) || self.is_base_type(*item) {
                    return false;
                }
                seen.push(*item);
                let subst = super::ty::Subst::of(
                    self.sig(*item).generics.iter().map(|def| def.param),
                    args,
                );
                let found = self
                    .constituents(*item)
                    .iter()
                    .any(|constituent| self.any_part_from(&subst.ty(constituent), test, seen));
                seen.pop();
                found
            }
            _ => false,
        }
    }
}
