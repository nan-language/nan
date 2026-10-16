//! The gate codes of the release gate: API snapshots that are not current, pending weakenings,
//! and trust escapes without a current seal.

use super::snapshot::{Items, differing, snapshots};
use super::{Checker, check_targets, unchecked};
use crate::diagnostic::{Code, Diagnostic, Reason};
use crate::workspace::{Package, Target, Workspace};

/// The diagnostics the release gate reports on a workspace package beyond its checks and tests.
pub fn gate(workspace: &Workspace, package: &Package) -> Vec<Diagnostic> {
    let std = crate::stdlib::packages();
    let Some(closure) = workspace.closure_of(package) else {
        return Vec::new();
    };
    let checkers: Vec<(Target, Checker)> = check_targets(package)
        .into_iter()
        .map(|target| (target, unchecked(workspace, closure, std, target)))
        .collect();
    let views: Vec<(Target, &Checker)> =
        checkers.iter().map(|(target, ck)| (*target, ck)).collect();
    let name = package.name().unwrap_or_default();
    let items = Items::of(&views, name);
    let computed = snapshots(&items, package, &[]);
    let mut found = Vec::new();
    let summary = |module: &str| -> (String, u32) {
        package
            .modules
            .iter()
            .find(|candidate| candidate.name == module)
            .and_then(|module| module.dirs.first())
            .map_or_else(
                || (format!("{}/package.nan", package.dir), 1),
                |dir| (dir.first_file.clone(), 1),
            )
    };
    let mut modules: Vec<&str> = computed
        .modules
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    modules.extend(
        package
            .machine
            .snapshots
            .iter()
            .map(|(name, _)| name.as_str()),
    );
    modules.sort();
    modules.dedup();
    for module in modules {
        let current = computed
            .modules
            .iter()
            .find(|(name, _)| name == module)
            .and_then(|(_, snapshot)| snapshot.as_ref());
        let written = package.machine.snapshot(module);
        for path in differing(current, written) {
            let (file, line) = match items.first(&path) {
                Some(variant) => (
                    variant.file.clone(),
                    variant.ck.db.item(variant.item).head_line(),
                ),
                None if items.package_path() == Some(path.as_str()) => (
                    format!("{}/package.nan", package.dir),
                    manifest_line(package),
                ),
                None => summary(module),
            };
            let diagnostic = Diagnostic::at(Code::ApiStale, &file, line);
            if !found.contains(&diagnostic) {
                found.push(diagnostic);
            }
        }
    }
    for pending in computed.recorded {
        found.push(Diagnostic {
            because: pending
                .parts
                .iter()
                .map(|part| Reason::note(part))
                .collect(),
            item: Some(pending.path),
            ..Diagnostic::at(Code::ApiWeakeningPending, &pending.file, pending.line)
        });
    }
    let entries = computed
        .modules
        .iter()
        .filter_map(|(_, snapshot)| snapshot.as_ref())
        .flat_map(|snapshot| &snapshot.entries);
    let sealed = |path: &str| {
        entries
            .clone()
            .find(|entry| entry.path == path)
            .is_some_and(|entry| {
                entry
                    .seals
                    .iter()
                    .any(|seal| items.seal_current(path, seal))
            })
    };
    let mut escaping: Vec<String> = items
        .by_path
        .keys()
        .filter(|path| items.holds_escape(path))
        .cloned()
        .collect();
    escaping.extend(
        items
            .package_path()
            .filter(|path| items.holds_escape(path))
            .map(str::to_owned),
    );
    if let Some(history) = super::author::History::open(workspace) {
        let held: Vec<String> = escaping
            .iter()
            .filter(|path| sealed(path))
            .cloned()
            .collect();
        found.extend(history.same_author(package, &items, &held));
        found.extend(history.acceptances(package, &items));
    }
    for path in escaping {
        if sealed(&path) {
            continue;
        }
        let (file, line, escapes) = match items.first(&path) {
            Some(variant) => {
                let data = variant.ck.db.item(variant.item);
                let escapes: Vec<String> = variant
                    .ck
                    .ledger(variant.item)
                    .into_iter()
                    .filter(|entry| entry.escape)
                    .map(|entry| format!("{} {}:{}", entry.kind.word(), entry.path, entry.line))
                    .collect();
                (data.file.path.clone(), data.head_line(), escapes)
            }
            None => {
                let escapes = items
                    .package
                    .map(|(id, ck)| {
                        ck.grant_ledger(id)
                            .into_iter()
                            .map(|entry| format!("grant {}:{}", entry.path, entry.line))
                            .collect()
                    })
                    .unwrap_or_default();
                (
                    format!("{}/package.nan", package.dir),
                    manifest_line(package),
                    escapes,
                )
            }
        };
        found.push(Diagnostic {
            because: escapes
                .iter()
                .map(|escape: &String| Reason::note(escape))
                .collect(),
            item: Some(path),
            ..Diagnostic::at(Code::TrustUnsealed, &file, line)
        });
    }
    found
}

/// The line of a package's `package` keyword.
fn manifest_line(package: &Package) -> u32 {
    package
        .manifest
        .as_ref()
        .map_or(1, |manifest| manifest.line)
}
