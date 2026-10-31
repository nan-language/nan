//! Capability kinds: the structs an impl line naming `sys::Kind` declares kinds, the form of
//! those lines, and the uses of minters, reaches and derivations, which only the module
//! declaring a kind makes.

use super::Checker;
use super::db::{ItemId, TypeKey};
use super::sig::SigKind;
use super::ty::{Arg, Ty};
use crate::diagnostic::Code;
use crate::syntax::ast::{ItemKind, Tok, TypeModifier};
use crate::workspace::Authority;

/// The largest record a kind's entries keep, in bytes.
const RECORD_BYTES: u64 = 16;

/// The std functions only the module declaring their kind uses.
const KIND_USES: [&str; 6] = [
    "sys::Minter.of",
    "sys::Minter.mint",
    "sys::Minter.revoke",
    "sys::Minter.close",
    "sys::reach",
    "sys::derive",
];

impl Checker<'_> {
    /// The `sys::Kind` impl line of a type item, when it has one.
    fn kind_line(&self, item: ItemId) -> Option<ItemId> {
        self.impl_lines(TypeKey::Item(item))
            .into_iter()
            .find(|line| {
                matches!(&self.sig(*line).kind, SigKind::Impl { interface: Some(bound), .. }
                if self.db.canonical(bound.interface) == "sys::Kind")
            })
    }

    /// Whether a type item is a capability kind: a struct of the form kinds take that a
    /// `sys::Kind` impl line names.
    pub fn is_kind(&self, item: ItemId) -> bool {
        self.kind_shaped(item) && self.kind_line(item).is_some()
    }

    /// Whether a type item has the form of a kind: a struct without members, generic
    /// parameters, derives or a modifier other than `open`.
    fn kind_shaped(&self, item: ItemId) -> bool {
        let ast = self.db.ast(item);
        let ItemKind::Struct(structure) = &ast.kind else {
            return false;
        };
        structure.members.is_empty()
            && structure.generics.is_empty()
            && structure.derives.is_empty()
            && ast
                .type_modifiers
                .iter()
                .all(|(modifier, _)| *modifier == TypeModifier::Open)
    }

    /// The kind whose function an item is, when its head does not demand `pure`.
    pub fn kind_member(&self, function: ItemId) -> Option<ItemId> {
        match self.db.owners.get(&function) {
            Some(TypeKey::Item(owner))
                if self.is_kind(*owner)
                    && !self.fn_sig(function).is_some_and(|sig| sig.limits.pure) =>
            {
                Some(*owner)
            }
            _ => None,
        }
    }

    /// The rules of an impl line naming `sys::Kind`: its module performs `capability`, and it
    /// names a struct of a kind's form and a record of at most 16 bytes.
    pub fn check_kind_impl(&self, line: ItemId) {
        let SigKind::Impl {
            target: Some(key),
            interface: Some(bound),
            ..
        } = &self.sig(line).kind
        else {
            return;
        };
        if self.db.canonical(bound.interface) != "sys::Kind" {
            return;
        }
        let token = self.db.ast(line).token;
        self.require(line, token, Authority::Capability);
        let shaped = matches!(key, TypeKey::Item(item) if self.kind_shaped(*item));
        let record = bound.args.get(1).and_then(Arg::ty);
        let sized = record.is_none_or(|record| {
            record.has_error()
                || self
                    .fixed_size(record)
                    .is_some_and(|size| size <= RECORD_BYTES)
        });
        if !shaped || !sized {
            self.report(line, token, Code::AuthorityKindForm);
        }
    }

    /// A use of a minter's operation, `sys::reach` or `sys::derive` by an item, `kind` the kind
    /// it fixes: one that is not a kind the item's module declares is reported.
    pub fn check_kind_use(&self, user: ItemId, token: Tok, function: ItemId, kind: Option<&Arg>) {
        if !self.db.is_std(function) || !KIND_USES.contains(&self.db.canonical(function).as_str()) {
            return;
        }
        let declared = match kind {
            Some(Arg::Ty(Ty::Item(item, _))) => {
                self.is_kind(*item) && self.db.module_of(*item) == self.db.module_of(user)
            }
            Some(Arg::Ty(ty)) => ty.has_error(),
            _ => true,
        };
        if !declared {
            self.report(user, token, Code::AuthorityKindForeign);
        }
    }
}
