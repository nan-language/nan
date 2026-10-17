//! Newer releases: the release of a `dep` directive's source, newer than the locked one, that
//! names what a path through the directive's alias names nothing by.

use super::db::{ItemId, PackageId};
use super::{Checker, check_targets, unchecked};
use crate::diagnostic::{Code, Diagnostic, Reason, Role};
use crate::release;
use crate::syntax::ast::{ItemKind, Tok, Visibility};
use crate::workspace::{Member, Package, Workspace};

impl Checker<'_> {
    /// Reports a path of an item, written `alias::rest` on its tokens `first..=last`, that names
    /// nothing while a newer release of the source of the `dep` directive declaring `alias`
    /// names it; whether it does.
    pub fn report_newer_release(
        &self,
        item: ItemId,
        (first, last): (Tok, Tok),
        written: &str,
    ) -> bool {
        let Some(release) = written
            .split_once("::")
            .and_then(|(alias, rest)| self.newer_release(item, alias, rest))
        else {
            return false;
        };
        let because = vec![
            Reason::note(&release),
            Reason {
                role: Role::Command,
                subject: "nan dep sync".to_owned(),
                at: None,
            },
        ];
        let diagnostic = Diagnostic {
            because,
            ..self.on(item, (first, last), Code::NameNewerRelease)
        };
        self.report_diagnostic(item, diagnostic);
        true
    }

    /// The newer release of the source of the `dep` directive of an item's package declaring
    /// `alias` that names `rest` as a `pub` item or member, as a diagnostic names it: the current
    /// state of a path source whose code root differs from the locked release's, by its source,
    /// or else the earliest named release under the store name with a greater publication index
    /// than the locked one, by its store name and code root.
    fn newer_release(&self, item: ItemId, alias: &str, rest: &str) -> Option<String> {
        let package = self.db.package_of(item);
        let source = self.db.package(package).source;
        let dep = source
            .manifest
            .as_ref()?
            .deps
            .iter()
            .find(|dep| dep.alias == alias)?;
        let locked = self.db.package(self.db.alias(package, alias)?);
        let path = format!("{}::{rest}", locked.name);
        let named = dep.source.as_deref()?;
        if dep.path {
            let workspace = self.db.workspace;
            let current = workspace.packages.iter().find(|held| held.dir == named)?;
            let differs = matches!(locked.member, Some(Member::Release(_)));
            return (differs && names_now(workspace, current, &path)).then(|| dep.written.clone());
        }
        let root = source
            .machine
            .lock()?
            .deps
            .iter()
            .find(|line| line.alias.as_deref() == Some(alias))?
            .root;
        let entries = self.store_entries()?;
        let index = entries.iter().find(|entry| entry.root() == root)?.index;
        let mut newer: Vec<_> = entries
            .iter()
            .filter(|entry| entry.name.as_deref() == Some(named) && entry.index > index)
            .collect();
        newer.sort_by_key(|entry| entry.index);
        let release = newer.into_iter().find(|entry| {
            let exported = release::exported(&entry.artefact);
            exported.contains_key(&path)
                || path.rsplit_once('.').is_some_and(|(owner, member)| {
                    exported
                        .get(owner)
                        .is_some_and(|entry| entry.declares(member))
                })
        })?;
        Some(format!("{named} {}", release.root().short()))
    }

    /// Whether a package of the checker names a canonical path as a `pub` item, or as a variant
    /// of a `pub` enum.
    fn names_pub(&self, package: PackageId, path: &str) -> bool {
        let modules = &self.db.package(package).modules;
        let items = modules
            .iter()
            .flat_map(|&module| self.db.module(module).items.iter().copied());
        let owner = path.rsplit_once('.');
        items
            .filter(|&item| self.db.ast(item).visibility == Visibility::Pub)
            .any(|item| match self.canonical_path(item) {
                Some(found) if found == path => true,
                Some(found) => owner.is_some_and(|(owner, member)| {
                    found == owner
                        && matches!(&self.db.ast(item).kind, ItemKind::Enum(declared)
                            if declared.variants.iter().any(|variant| variant.name.text == member))
                }),
                None => false,
            })
    }
}

/// Whether the current state of a workspace package names a canonical path as a `pub` item or
/// member.
fn names_now(workspace: &Workspace, package: &Package, path: &str) -> bool {
    let (Some(closure), Some(&target)) = (
        workspace.closure_of(package),
        check_targets(package).first(),
    ) else {
        return false;
    };
    let checker = unchecked(workspace, closure, crate::stdlib::packages(), target);
    package
        .name()
        .and_then(|name| checker.package_named(name))
        .is_some_and(|id| checker.names_pub(id, path))
}
