//! Registries: their entry types and key and unique fields, the registries contributions and
//! `allow contribute` grants name, the contributions a judging root admits, and the platform
//! registries' contributors and targets.

use std::collections::BTreeSet;

use super::Checker;
use super::db::{ItemId, ModuleId, PackageId};
use super::props::Property;
use super::resolve::Named;
use super::sig::SigKind;
use super::ty::{Subst, Ty};
use crate::diagnostic::{Code, Offer};
use crate::syntax::ast::{self, ItemKind};
use crate::workspace::{Authority, Target};

/// A contribution as a judging root sees it.
pub struct Contribution {
    pub item: ItemId,
    pub registry: ItemId,
    pub contributor: PackageId,
    pub admitted: bool,
}

impl Checker<'_> {
    /// The rules of a registry item: a struct entry type that a const may have, and key and
    /// unique fields naming distinct fields of it, visible here, whose types implement `Ord` and
    /// `Eq`.
    pub fn check_registry(&self, item: ItemId, registry: &ast::RegistryItem) {
        let SigKind::Registry { entry } = &self.sig(item).kind else {
            return;
        };
        if entry.has_error() {
            return;
        }
        let structure = match entry {
            Ty::Item(target, _) if matches!(self.db.ast(*target).kind, ItemKind::Struct(_)) => {
                Some(*target)
            }
            _ => None,
        };
        let Some(structure) = structure
            .filter(|_| self.has(entry, Property::Inert) && !self.has(entry, Property::Linear))
        else {
            self.report(item, registry.entry.token, Code::RegistryEntryType);
            return;
        };
        let sig = self.sig(structure);
        let SigKind::Struct { fields } = &sig.kind else {
            return;
        };
        let subst = Subst::of(sig.generics.iter().map(|def| def.param), entry.args());
        let module = self.db.module_of(item);
        let mut named: Vec<&str> = Vec::new();
        for (index, name) in std::iter::once(&registry.key)
            .chain(&registry.unique)
            .enumerate()
        {
            if named.contains(&name.text.as_str()) {
                let fix = self
                    .fix_element_deleted(item, name.token, name.token)
                    .unwrap_or_else(|| {
                        let syntax = &self.db.item(item).file.syntax;
                        let keyword = syntax
                            .previous(name.token as usize)
                            .unwrap_or(name.token as usize);
                        self.fix_deleting(item, keyword as ast::Tok, name.token)
                    });
                let construct = (name.token, name.token);
                self.report_fixed(
                    item,
                    construct,
                    Code::RegistryUniqueRepeated,
                    vec![fix],
                    Offer::Each,
                );
                continue;
            }
            named.push(&name.text);
            let Some(field) = fields.iter().find(|field| field.name == name.text) else {
                self.report(item, name.token, Code::RegistryUnknownField);
                continue;
            };
            if !self.db.field_visible(module, structure, field.visibility) {
                self.report(item, name.token, Code::NamePrivate);
                continue;
            }
            let (interface, code) = match index {
                0 => ("Ord", Code::RegistryKeyType),
                _ => ("Eq", Code::RegistryUniqueType),
            };
            let ty = subst.ty(&field.ty);
            let implemented = self
                .db
                .core_item(interface)
                .is_none_or(|interface| ty.has_error() || self.implements(&ty, interface));
            if !implemented {
                self.report(item, name.token, code);
            }
        }
    }

    /// The registry a path written in a module names, or the code of the rule it breaks.
    fn registry_named(&self, module: ModuleId, path: &ast::Path) -> Result<ItemId, Option<Code>> {
        match self.db.resolve(module, path) {
            Ok(Named::Item(registry))
                if matches!(self.db.ast(registry).kind, ItemKind::Registry(_)) =>
            {
                if self.db.visible(module, registry) {
                    Ok(registry)
                } else {
                    Err(Some(Code::NamePrivate))
                }
            }
            Ok(_) => Err(Some(Code::RegistryNotRegistry)),
            Err(error) => Err(error.code),
        }
    }

    /// The registry a contribution adds to, when its path names one.
    pub fn contribution_registry(&self, item: ItemId) -> Option<ItemId> {
        let ItemKind::Contribute(contribution) = &self.db.ast(item).kind else {
            return None;
        };
        self.registry_named(self.db.module_of(item), &contribution.registry)
            .ok()
    }

    /// Whether an item is a contribution to a platform registry.
    pub fn is_platform_contribution(&self, item: ItemId) -> bool {
        self.contribution_registry(item).is_some_and(|registry| {
            self.db.is_std(registry)
                && matches!(
                    self.db.canonical(registry).as_str(),
                    "hw::vectors" | "hw::syscall_entry" | "hw::preempt"
                )
        })
    }

    /// The rules of a contribution's path: a visible registry, and for a platform registry, a
    /// target with the seams it binds and, for `hw::preempt`, a platform package.
    pub fn check_contribution(&self, item: ItemId, contribution: &ast::ContributeItem) {
        let registry = match self.registry_named(self.db.module_of(item), &contribution.registry) {
            Ok(registry) => registry,
            Err(code) => {
                if let Some(code) = code {
                    self.report(item, contribution.registry.token(), code);
                }
                return;
            }
        };
        if !self.db.is_std(registry) {
            return;
        }
        let vectors = matches!(self.db.target, Target::Uefi | Target::Bare);
        let seams = match self.db.canonical(registry).as_str() {
            "hw::vectors" => vectors,
            "hw::preempt" => {
                self.report(
                    item,
                    contribution.registry.token(),
                    Code::RegistryPlatformEntry,
                );
                vectors
            }
            "hw::syscall_entry" => self.db.target == Target::Bare,
            _ => true,
        };
        if !seams {
            self.report(
                item,
                contribution.registry.token(),
                Code::RegistryPlatformTarget,
            );
        }
    }

    /// The rules of a package as a judging root: the registries and aliases its `allow
    /// contribute` grants name, and each contribution of its dependency closure it does not
    /// admit, reported once per registry and contributing package.
    pub fn check_contributors(&self, package: PackageId) {
        let data = self.db.package(package);
        let Some(manifest) = &data.source.manifest else {
            return;
        };
        let path = &data.source.files[0].path;
        let mut grants: Vec<(ItemId, &[String])> = Vec::new();
        for allow in &manifest.allows {
            let Authority::Contribute(registry) = &allow.authority else {
                continue;
            };
            match self.granted_registry(package, registry) {
                Ok(registry) => grants.push((registry, &allow.names)),
                Err(Some(code)) => self.report_file(path, allow.line, code),
                Err(None) => {}
            }
            if allow.names.iter().any(|name| !data.deps.contains_key(name)) {
                self.report_file(path, allow.line, Code::RegistryGrantNotAlias);
            }
        }
        let mut reported = BTreeSet::new();
        for contribution in self.contributions(package) {
            let (registry, contributor) = (contribution.registry, contribution.contributor);
            if contribution.admitted || !reported.insert((registry, contributor)) {
                continue;
            }
            let dep = manifest
                .deps
                .iter()
                .find(|dep| data.deps.get(&dep.alias) == Some(&Some(contributor)));
            let line = dep.map_or(manifest.line, |dep| dep.line);
            let fix = dep.zip(data.root).and_then(|(dep, root)| {
                let granted = Authority::Contribute(self.item_written(registry, root));
                self.fix_granting(package, &granted, &dep.alias)
            });
            let code = Code::RegistryUngranted;
            self.report_file_fixed(path, line, code, fix.into_iter().collect());
        }
    }

    /// The contributions of a judging root's dependency closure, each with whether the root
    /// admits it: its package declares its registry, or an `allow contribute` grant of the root
    /// names it by an alias of the root's.
    pub fn contributions(&self, root: PackageId) -> Vec<Contribution> {
        let data = self.db.package(root);
        let grants: Vec<(ItemId, &[String])> = data
            .source
            .manifest
            .iter()
            .flat_map(|manifest| &manifest.allows)
            .filter_map(|allow| match &allow.authority {
                Authority::Contribute(registry) => self
                    .granted_registry(root, registry)
                    .ok()
                    .map(|registry| (registry, &allow.names[..])),
                _ => None,
            })
            .collect();
        let named = |contributor: PackageId, aliases: &[String]| {
            aliases
                .iter()
                .any(|alias| data.deps.get(alias) == Some(&Some(contributor)))
        };
        let mut found = Vec::new();
        for contributor in std::iter::once(root).chain(self.dependency_closure(root)) {
            for &module in &self.db.package(contributor).modules {
                for &item in &self.db.module(module).items {
                    let Some(registry) = self.contribution_registry(item) else {
                        continue;
                    };
                    let admitted = contributor == root
                        || self.db.package_of(registry) == contributor
                        || grants.iter().any(|(granted, aliases)| {
                            *granted == registry && named(contributor, aliases)
                        });
                    found.push(Contribution {
                        item,
                        registry,
                        contributor,
                        admitted,
                    });
                }
            }
        }
        found
    }

    /// The registry the path of a package's `allow contribute` grant names, resolved in its root
    /// module, or the code of the rule it breaks.
    pub fn granted_registry(
        &self,
        package: PackageId,
        written: &str,
    ) -> Result<ItemId, Option<Code>> {
        let Some(root) = self.db.package(package).root else {
            return Err(None);
        };
        let path = ast::Path {
            segments: written
                .split("::")
                .map(|segment| ast::Name {
                    text: segment.to_owned(),
                    token: 0,
                })
                .collect(),
        };
        self.registry_named(root, &path)
    }

    /// Whether a package contributes to a registry it does not declare, under the target
    /// checked.
    pub fn contributes(&self, contributor: PackageId, registry: ItemId) -> bool {
        self.db.package_of(registry) != contributor
            && self.db.package(contributor).modules.iter().any(|&module| {
                self.db
                    .module(module)
                    .items
                    .iter()
                    .any(|&item| self.contribution_registry(item) == Some(registry))
            })
    }

    /// The workspace packages a package depends on, directly or through others, itself excluded.
    fn dependency_closure(&self, package: PackageId) -> Vec<PackageId> {
        let mut closure = vec![package];
        let mut next = 0;
        while let Some(&current) = closure.get(next) {
            next += 1;
            for dep in self.db.package(current).deps.values().flatten() {
                if !closure.contains(dep) {
                    closure.push(*dep);
                }
            }
        }
        closure.remove(0);
        closure
    }
}
