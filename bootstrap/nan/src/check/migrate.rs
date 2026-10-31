//! Migrations: the functions declared `migrate` that convert a type's values from or to a frozen
//! shape, a type of one of its earlier schema trees, which its package's history records.

use super::Checker;
use super::db::{ItemId, TypeKey};
use super::reversibility::Class;
use super::sig::FnSig;
use super::spec::head_last;
use super::ty::Ty;
use crate::diagnostic::{Code, Offer};
use crate::hash::Hash;
use crate::syntax::ast::{self, FnModifier, ItemKind, LimitKind, Mode};

/// A migration of a type: its function, its frozen shape and whether it converts from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Conversion {
    pub function: ItemId,
    pub shape: ItemId,
    pub forward: bool,
}

impl Checker<'_> {
    /// The migrations of a type of the forms their rule states, by file path and line.
    pub fn migrations(&self, item: ItemId) -> Vec<Conversion> {
        let Some(members) = self.db.members.get(&TypeKey::Item(item)) else {
            return Vec::new();
        };
        let mut found: Vec<Conversion> = members
            .values()
            .flatten()
            .filter_map(|function| self.migration(*function))
            .collect();
        found.sort_by_key(|migration| {
            let data = self.db.item(migration.function);
            (data.file.path.clone(), data.head_line())
        });
        found
    }

    /// A function declared `migrate` of one of the forms its rule states.
    fn migration(&self, function: ItemId) -> Option<Conversion> {
        let ast = self.db.ast(function);
        if !ast.has_fn_modifier(FnModifier::Migrate) {
            return None;
        }
        let Some(TypeKey::Item(owner)) = self.db.owners.get(&function).copied() else {
            return None;
        };
        let sig = self.fn_sig(function)?;
        let package = self.db.package_of(function);
        let encoded = |ty: &Ty| match ty {
            Ty::Item(item, args) if args.is_empty() => {
                let typed = matches!(
                    self.db.ast(*item).kind,
                    ItemKind::Struct(_) | ItemKind::Enum(_)
                );
                let derived = self
                    .db
                    .core_item("Encode")
                    .is_some_and(|encode| self.derives(*item, encode));
                (typed && derived && self.db.package_of(*item) == package).then_some(*item)
            }
            _ => None,
        };
        let own = Ty::item(owner, Vec::new());
        encoded(&own)?;
        let alloc = match &sig.params[..] {
            [_] => true,
            [_, param] => param.mode == Mode::Read && self.is_std(&param.ty, "mem::Alloc"),
            _ => false,
        };
        let fails = sig
            .fails
            .as_ref()
            .is_none_or(|fails| self.is_std(fails, "mem::OutOfMemory"));
        let plain = alloc && fails && !sig.wait && sig.own_generics().is_empty();
        let first = sig.params.first()?;
        let (shape, forward) = match (first.is_self, first.mode) {
            (true, Mode::Read) => (&sig.result, false),
            (false, Mode::Take) if sig.result == own => (&first.ty, true),
            _ => return None,
        };
        let shape = encoded(shape).filter(|shape| plain && *shape != owner)?;
        Some(Conversion {
            function,
            shape,
            forward,
        })
    }

    /// The rules of a function declared `migrate`.
    pub fn check_migration(&self, item: ItemId, function: &ast::FnItem, sig: &FnSig) {
        let token = self.db.ast(item).token;
        let Some(migration) = self.migration(item) else {
            self.report(item, token, Code::EncodeMigrateForm);
            return;
        };
        if !sig.limits.notrap || !sig.limits.terminates {
            let fix = self.limits_fix(item, &function.head, sig);
            self.report_fixed(
                item,
                (token, token),
                Code::EncodeMigrateLimits,
                vec![fix],
                Offer::Each,
            );
        }
        let digest = self.digest(item);
        let effects = digest.device
            || digest.privileged
            || digest.sync
            || digest.wait
            || digest
                .capabilities
                .as_ref()
                .is_none_or(|set| !set.is_empty())
            || digest.validators.as_ref().is_none_or(|set| !set.is_empty())
            || digest.class != Class::Traceless;
        if effects {
            self.report(item, token, Code::EncodeMigrateEffect);
        }
        let Some(TypeKey::Item(owner)) = self.db.owners.get(&item).copied() else {
            return;
        };
        let hash = self.shape_hash(migration.shape);
        let own = self.shape_hash(owner);
        if hash == own || !self.recorded_hashes(owner).contains(&hash) {
            self.report(item, token, Code::EncodeMigrateSchema);
            return;
        }
        let others: Vec<Conversion> = self
            .migrations(owner)
            .into_iter()
            .filter(|other| self.shape_hash(other.shape) == hash)
            .collect();
        let first = others
            .iter()
            .find(|other| other.forward == migration.forward)
            .map(|other| other.function);
        if first != Some(item) {
            self.report(item, token, Code::EncodeMigrateDuplicate);
        } else if !migration.forward && !others.iter().any(|other| other.forward) {
            self.report(item, token, Code::EncodeMigrateInverseAlone);
        }
    }

    /// The schema hash of a type without type parameters.
    pub fn shape_hash(&self, item: ItemId) -> Hash {
        self.schema(&Ty::item(item, Vec::new())).hash()
    }

    /// The schema hashes the history of a type's package records for it.
    fn recorded_hashes(&self, item: ItemId) -> Vec<Hash> {
        let package = self.db.package(self.db.package_of(item));
        let module = &self.db.module(self.db.module_of(item)).name;
        let name = self.db.ast(item).name().unwrap_or_default();
        package
            .source
            .machine
            .history()
            .map(|history| history.schemas(module, &name))
            .unwrap_or_default()
    }

    /// The fix adding `notrap` and `terminates` to a head in the order `nan fmt` writes limits.
    fn limits_fix(&self, item: ItemId, head: &ast::Head, sig: &FnSig) -> crate::diagnostic::Fix {
        let missing: Vec<&str> = [
            (!sig.limits.notrap).then_some("notrap"),
            (!sig.limits.terminates).then_some("terminates"),
        ]
        .into_iter()
        .flatten()
        .collect();
        let words = missing.join(", ");
        let Some(limits) = head
            .limits
            .as_ref()
            .filter(|limits| !limits.limits.is_empty())
        else {
            return self.fix_appending(item, head_last(head), &format!(" limits {words}"));
        };
        let earlier = |kinds: &[LimitKind]| {
            limits
                .limits
                .iter()
                .filter(|limit| kinds.contains(&limit.kind))
                .map(|limit| limit.token)
                .max()
        };
        let after = match sig.limits.notrap {
            true => earlier(&[LimitKind::Pure, LimitKind::Notrap]),
            false => earlier(&[LimitKind::Pure]),
        };
        match after {
            Some(token) => self.fix_appending(item, token, &format!(", {words}")),
            None => self.fix_inserting(item, limits.limits[0].token, &format!("{words}, ")),
        }
    }
}
