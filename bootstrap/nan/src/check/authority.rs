//! Authority: the manifest grants a module needs for the device, priv, trusted and validate
//! operations it performs directly and the capability kinds it declares, the grants its
//! dependencies need, the names grants and denials list, and denied dependencies.

use super::Checker;
use super::db::{ItemId, ModuleId, PackageId};
use crate::diagnostic::{Code, Fix, Offer};
use crate::syntax::ast::Tok;
use crate::workspace::Authority;

/// The authorities a manifest grant names that a dependency's grant must be relayed for.
const RELAYED: [Authority; 4] = [
    Authority::Device,
    Authority::Priv,
    Authority::Trusted,
    Authority::Validate,
];

impl Checker<'_> {
    /// Whether a grant of a module's package names the module for an authority; std modules
    /// need none.
    pub fn granted(&self, module: ModuleId, authority: &Authority) -> bool {
        let package = self.db.module(module).package;
        let data = self.db.package(package);
        data.std
            || data
                .source
                .manifest
                .iter()
                .flat_map(|manifest| &manifest.allows)
                .any(|allow| {
                    allow.authority == *authority
                        && allow
                            .names
                            .iter()
                            .any(|name| self.db.module_named(package, name) == Some(module))
                })
    }

    /// Records an operation of an authority that an item's module performs directly, reporting
    /// it when no grant names the module for that authority.
    pub fn require(&self, item: ItemId, token: Tok, authority: Authority) {
        let module = self.db.module_of(item);
        if !self.granted(module, &authority) {
            let name = &self.db.module(module).name;
            let fixes = self
                .fix_granting(self.db.package_of(item), &authority, name)
                .into_iter()
                .collect();
            self.report_fixed(
                item,
                (token, token),
                Code::AuthorityUngranted,
                fixes,
                Offer::Each,
            );
        }
        self.perform(item, authority);
    }

    /// Records an operation of an authority that an item's module performs directly.
    pub fn perform(&self, item: ItemId, authority: Authority) {
        self.performed
            .borrow_mut()
            .insert((self.db.module_of(item), authority));
    }

    /// The names a package's manifest grants list that nothing uses under the target checked,
    /// with the lines of their directives: a module performing none of the authority's
    /// operations directly and holding no failed construct, the alias of a dependency not
    /// granted the authority, and in a `contribute` grant, the alias of a package declaring the
    /// registry or contributing nothing to it.
    pub fn unused_grants(&self, package: PackageId) -> Vec<(u32, String)> {
        let data = self.db.package(package);
        let Some(manifest) = &data.source.manifest else {
            return Vec::new();
        };
        let mut unused = Vec::new();
        for allow in &manifest.allows {
            let registry = match &allow.authority {
                Authority::Contribute(written) => match self.granted_registry(package, written) {
                    Ok(registry) => Some(registry),
                    Err(_) => continue,
                },
                _ => None,
            };
            for name in &allow.names {
                let dep = data.deps.get(name);
                let used = match (registry, self.db.module_named(package, name), dep) {
                    (Some(registry), _, Some(&Some(dep))) => self.contributes(dep, registry),
                    (None, Some(module), _) => {
                        self.performed
                            .borrow()
                            .contains(&(module, allow.authority.clone()))
                            || self.module_holds_failure(module)
                    }
                    (None, None, Some(&Some(dep))) => {
                        !RELAYED.contains(&allow.authority)
                            || self.grants_any(dep, &allow.authority)
                    }
                    _ => true,
                };
                if !used {
                    unused.push((allow.line, name.clone()));
                }
            }
        }
        unused
    }

    /// The authority a call of a std function performs: the device operations of register
    /// windows and DMA, and the privileged intrinsics.
    pub fn call_authority(&self, function: ItemId) -> Option<Authority> {
        if !self.db.is_std(function) {
            return None;
        }
        let path = self.db.canonical(function);
        let (owner, member) = path.rsplit_once('.').unwrap_or((path.as_str(), ""));
        match owner {
            "hw::Mmio" | "hw::Ports" | "hw::Dma" | "hw::DmaShared" => Some(Authority::Device),
            "hw::IrqShared"
                if matches!(
                    member,
                    "install" | "install_on" | "install_msi" | "uninstall"
                ) =>
            {
                Some(Authority::Device)
            }
            "hw::dma_fence" => Some(Authority::Device),
            "hw::Ring0" => Some(Authority::Priv),
            _ => None,
        }
    }

    /// The rules of a package's manifest directives that read its modules and dependencies.
    pub fn check_manifest(&self, package: PackageId) {
        let data = self.db.package(package);
        let Some(manifest) = &data.source.manifest else {
            return;
        };
        let path = &data.source.files[0].path;
        let module = |name: &str| self.db.module_named(package, name).is_some();
        let alias = |name: &str| data.deps.contains_key(name);
        for allow in &manifest.allows {
            let named = |name: &str| match allow.authority {
                Authority::Capability => module(name),
                _ => module(name) || alias(name),
            };
            if let Authority::Contribute(_) = allow.authority {
                continue;
            }
            if allow.names.iter().any(|name| !named(name)) {
                self.report_file(path, allow.line, Code::AuthorityUnknownName);
            }
        }
        for (from, to, line) in &manifest.denies {
            let known_to = module(to) || alias(to) || self.db.std.contains_key(to.as_str());
            if !module(from) || !known_to {
                self.report_file(path, *line, Code::AuthorityUnknownName);
            }
        }
        for dep in &manifest.deps {
            let missing = self.ungranted(package, &dep.alias);
            if !missing.is_empty() {
                let fix = missing
                    .iter()
                    .filter_map(|authority| self.fix_granting(package, authority, &dep.alias))
                    .reduce(Fix::and);
                let code = Code::AuthorityDependencyUngranted;
                self.report_file_fixed(path, dep.line, code, fix.into_iter().collect());
            }
        }
        for (from, to, _) in &manifest.denies {
            if let Some(module) = self.db.module_named(package, from) {
                self.check_denied(module, to);
            }
        }
    }

    /// The authorities granted to the package a `dep` alias of a package names that the
    /// package's manifest does not grant the alias.
    pub fn ungranted(&self, package: PackageId, alias: &str) -> Vec<&'static Authority> {
        let Some(Some(target)) = self.db.package(package).deps.get(alias) else {
            return Vec::new();
        };
        let relayed = |authority: &Authority| {
            self.db
                .package(package)
                .source
                .manifest
                .iter()
                .flat_map(|manifest| &manifest.allows)
                .any(|allow| {
                    allow.authority == *authority && allow.names.iter().any(|name| name == alias)
                })
        };
        RELAYED
            .iter()
            .filter(|authority| self.grants_any(*target, authority) && !relayed(authority))
            .collect()
    }

    /// Whether a package's manifest grants an authority to any name.
    fn grants_any(&self, package: PackageId, authority: &Authority) -> bool {
        let data = self.db.package(package);
        !data.std
            && data
                .source
                .manifest
                .iter()
                .flat_map(|manifest| &manifest.allows)
                .any(|allow| allow.authority == *authority)
    }

    /// Reports each line of a module's items holding a path whose first segment is `denied`.
    fn check_denied(&self, module: ModuleId, denied: &str) {
        for &item in &self.db.module(module).items {
            let data = self.db.item(item);
            let syntax = &data.file.syntax;
            let span = &syntax.items[data.index];
            let mut reported = None;
            for index in syntax.path_starts(span.first..span.end) {
                let line = syntax.tokens[index].line;
                if syntax.text(index) == denied && reported != Some(line) {
                    self.report_line(item, line, Code::AuthorityDeniedDependency);
                    reported = Some(line);
                }
            }
        }
    }

    /// An impl line naming `sys::Capability`, which only std writes.
    pub fn check_capability_impl(&self, item: ItemId) {
        if !self.db.is_std(item)
            && let super::sig::SigKind::Impl {
                interface: Some(bound),
                ..
            } = &self.sig(item).kind
            && self.db.canonical(bound.interface) == "sys::Capability"
        {
            self.report(item, self.db.ast(item).token, Code::AuthorityCapabilityImpl);
        }
    }
}
