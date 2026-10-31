//! Release checks: each locked release a workspace package reaches, checked whole from its source
//! tree with the schema trees its artefact records, its errors reported on the directive through
//! which the package reaches it.

use std::collections::{BTreeMap, BTreeSet};

use super::db::PackageId;
use super::{Checker, check_targets, checked};
use crate::artefact;
use crate::diagnostic::{Class, Code, Diagnostic, Reason, Role};
use crate::workspace::{Member, Package, Workspace, reaching_directive};

impl Checker<'_> {
    /// The packages a package of the checker's closure reaches through its `dep` aliases, itself
    /// first.
    pub fn reached(&self, root: PackageId) -> Vec<PackageId> {
        let mut found = vec![root];
        let mut index = 0;
        while index < found.len() {
            for dependency in self.db.package(found[index]).deps.values().flatten() {
                if !found.contains(dependency) {
                    found.push(*dependency);
                }
            }
            index += 1;
        }
        found
    }

    /// A `note` reason naming each type whose schema tree the code root of a release's artefact
    /// records differs from the tree the type has in the checker's closure.
    pub fn stale_schemas(&self, release: PackageId) -> Vec<Reason> {
        let Some(Member::Release(index)) = self.db.package(release).member else {
            return Vec::new();
        };
        let artefact = &self.db.workspace.releases[index].entry.artefact;
        let recorded: BTreeMap<String, Vec<u8>> = artefact::decode(artefact)
            .ok()
            .and_then(|decoded| decoded.schema_trees())
            .unwrap_or_default()
            .into_iter()
            .collect();
        let derived: BTreeMap<String, Vec<u8>> = self.schema_trees(release).into_iter().collect();
        let paths: BTreeSet<&String> = recorded.keys().chain(derived.keys()).collect();
        paths
            .into_iter()
            .filter(|path| recorded.get(*path) != derived.get(*path))
            .map(|path| Reason::note(path))
            .collect()
    }
}

/// The release checks of the releases a workspace package of a checker's closure reaches: one
/// `artefact.recheck-failed` per release whose check finds errors, each error a `diagnostic`
/// reason of its because-chain, followed by a `note` reason per stale schema tree.
pub fn release_errors(checker: &Checker, root: PackageId) -> Vec<Diagnostic> {
    let source = checker.db.package(root).source;
    let (Some(manifest), Some(lock)) = (&source.manifest, source.machine.lock()) else {
        return Vec::new();
    };
    let releases: Vec<PackageId> = checker
        .reached(root)
        .into_iter()
        .filter(|id| matches!(checker.db.package(*id).member, Some(Member::Release(_))))
        .collect();
    for id in &releases {
        checker.check_whole(*id);
    }
    let quiet = checker.quiet.borrow();
    releases
        .iter()
        .filter_map(|id| {
            let release = checker.db.package(*id);
            let mut because = errors(release.source, &quiet);
            if because.is_empty() {
                return None;
            }
            because.sort_by_key(|reason| {
                (
                    reason.at.is_none(),
                    reason.at.clone(),
                    reason.subject.clone(),
                )
            });
            because.dedup();
            because.extend(checker.stale_schemas(*id));
            let line = lock
                .deps
                .iter()
                .find(|line| line.package == release.name)
                .map_or(manifest.line, |line| reaching_directive(manifest, line));
            let path = format!("{}/package.nan", source.dir);
            Some(Diagnostic {
                because,
                ..Diagnostic::at(Code::ArtefactRecheckFailed, &path, line)
            })
        })
        .collect()
}

/// The `diagnostic` reasons of the errors on a release's files: those of loading it, of parsing
/// its files and of checking it.
fn errors(release: &Package, checked: &[Diagnostic]) -> Vec<Reason> {
    let prefix = format!("{}/", release.dir);
    let parsed = release.files.iter().flat_map(|file| {
        file.syntax.diagnostics.iter().map(|diagnostic| {
            (
                diagnostic.code,
                Some(file.path.as_str()),
                Some(diagnostic.line),
            )
        })
    });
    let reported = release
        .diagnostics
        .iter()
        .chain(checked.iter().filter(|diagnostic| {
            diagnostic
                .path
                .as_ref()
                .is_some_and(|path| path.starts_with(&prefix))
        }));
    reported
        .map(|diagnostic| (diagnostic.code, diagnostic.path.as_deref(), diagnostic.line))
        .chain(parsed)
        .filter(|(code, _, _)| code.class() == Class::Error)
        .map(|(code, path, line)| Reason {
            role: Role::Diagnostic,
            subject: code.name().to_owned(),
            at: path.zip(line).map(|(path, line)| (path.to_owned(), line)),
        })
        .collect()
}

/// The release checks of the releases a workspace package reaches, under each target it is
/// checked for.
pub fn release_check(workspace: &Workspace, package: &Package) -> Vec<Diagnostic> {
    let Some(closure) = workspace.closure_of(package) else {
        return Vec::new();
    };
    if !closure
        .members
        .values()
        .any(|member| matches!(member, Member::Release(_)))
    {
        return Vec::new();
    }
    let std = crate::stdlib::packages();
    let mut found = Vec::new();
    for target in check_targets(package) {
        let checker = checked(workspace, closure, std, target);
        let root = checker
            .package_named(package.name().unwrap_or_default())
            .expect("a checked package in its checker");
        found.extend(release_errors(&checker, root));
    }
    found
}
