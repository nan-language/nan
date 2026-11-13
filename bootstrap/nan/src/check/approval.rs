//! Approvals: the authority summary of a package, the closure summary and epoch pairs a final-link
//! root's final closure needs, and the rules that the root's lock approves them.

use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
mod tests;

use super::Checker;
use super::db::PackageId;
use super::ledger::Kind;
use crate::diagnostic::{Code, Diagnostic, Reason, Role};
use crate::machine::lock::{Entry, Lock};
use crate::workspace::{Member, Workspace};

/// What a final closure needs its root's lock to approve: pairs of a package and an entry of its
/// authority summary, and pairs of a canonical path and a behaviour epoch.
pub struct Needed {
    pub pairs: BTreeSet<(String, Entry)>,
    pub epochs: BTreeSet<(String, u64)>,
}

impl Checker<'_> {
    /// The authority summary of a package: the capabilities and validators its functions' digests,
    /// and the digests their code gives, hold, `alloc`, `device` and `priv` when one holds them,
    /// and each trust escape kind of each item holding one.
    pub fn authority_summary(&self, package: PackageId) -> BTreeSet<Entry> {
        let mut summary = BTreeSet::new();
        for entry in self.package_ledger(package) {
            if entry.escape {
                let kind = crate::machine::lock::ESCAPE_KINDS
                    .into_iter()
                    .find(|kind| *kind == entry.kind.word())
                    .unwrap_or(Kind::Trusted.word());
                summary.insert(Entry::Escape(kind, entry.item));
            }
        }
        for &module in &self.db.package(package).modules {
            for &item in &self.db.module(module).items {
                if !matches!(self.db.ast(item).kind, crate::syntax::ast::ItemKind::Fn(_)) {
                    continue;
                }
                for digest in [self.digest(item), self.code_digest(item)] {
                    for (holds, entry) in [
                        (digest.alloc, Entry::Alloc),
                        (digest.device, Entry::Device),
                        (digest.privileged, Entry::Priv),
                    ] {
                        if holds {
                            summary.insert(entry);
                        }
                    }
                    for capability in digest.capabilities.iter().flatten() {
                        summary.insert(Entry::Capability(self.db.canonical(*capability)));
                    }
                    for validator in digest.validators.iter().flatten() {
                        summary.insert(Entry::Validator(self.db.canonical(*validator)));
                    }
                }
            }
        }
        summary
    }

    /// What a final-link root's final closure needs its lock to approve; nothing for a package
    /// that is no program.
    pub fn needed(&self, root: PackageId) -> Needed {
        let mut needed = Needed {
            pairs: BTreeSet::new(),
            epochs: BTreeSet::new(),
        };
        let program = self
            .db
            .package(root)
            .source
            .manifest
            .as_ref()
            .is_some_and(|manifest| manifest.entry.is_some());
        if !program {
            return needed;
        }
        let closure = self.dependency_closure(root);
        for package in closure
            .into_iter()
            .filter(|package| !self.db.package(*package).std)
        {
            let data = self.db.package(package);
            for entry in self.authority_summary(package) {
                needed.pairs.insert((data.name.clone(), entry));
            }
            if let Some(Member::Release(index)) = data.member
                && let Some(summary) = &self.db.workspace.releases[index].entry.summary
            {
                for (path, _, epoch) in &summary.items {
                    if *epoch > 0 {
                        needed.epochs.insert((path.clone(), *epoch));
                    }
                }
            }
        }
        needed
    }

    /// The rules of a final-link root's approvals: each needed pair of its closure summary and
    /// each needed epoch pair its lock does not approve, listed on its `package` line.
    pub fn check_approvals(&self, root: PackageId) {
        let source = self.db.package(root).source;
        if let Some(lock) = source.machine.lock() {
            let needed = self.needed(root);
            self.found
                .borrow_mut()
                .extend(unapproved(source, &needed, lock));
        }
    }
}

/// Whether a code reports what a final-link root's lock leaves unapproved.
pub fn is_unapproved(code: Code) -> bool {
    matches!(
        code,
        Code::AuthorityWideningUnapproved | Code::DepEpochUnapproved
    )
}

/// The diagnostics of what a final-link root's final closure needs and its lock does not approve:
/// on its `package` line, naming `nan dep approve` of the root, then listing each pair.
fn unapproved(
    package: &crate::workspace::Package,
    needed: &Needed,
    lock: &Lock,
) -> Vec<Diagnostic> {
    let (Some(manifest), Some(name)) = (package.manifest.as_ref(), package.name()) else {
        return Vec::new();
    };
    let path = format!("{}/package.nan", package.dir);
    let pairs: Vec<String> = needed
        .pairs
        .iter()
        .filter(|(package, entry)| {
            !lock
                .approved
                .iter()
                .any(|(known, approved)| known == package && approved == entry)
        })
        .map(|(package, entry)| format!("{package} {}", entry.written()))
        .collect();
    let epochs: Vec<String> = needed
        .epochs
        .iter()
        .filter(|pair| !lock.epochs.contains(pair))
        .map(|(item, epoch)| format!("{item} {epoch}"))
        .collect();
    [
        (Code::AuthorityWideningUnapproved, pairs),
        (Code::DepEpochUnapproved, epochs),
    ]
    .into_iter()
    .filter(|(_, listed)| !listed.is_empty())
    .map(|(code, listed)| Diagnostic {
        because: std::iter::once(Reason {
            role: Role::Command,
            subject: format!("nan dep approve {name}"),
            at: None,
        })
        .chain(listed.iter().map(|pair| Reason::note(pair)))
        .collect(),
        ..Diagnostic::at(code, &path, manifest.line)
    })
    .collect()
}

/// The authority summary of each package named in `names` that a workspace package's closure
/// holds, over the targets the package is checked for.
pub fn authority_summaries(
    workspace: &Workspace,
    package: &crate::workspace::Package,
    names: &BTreeSet<&str>,
) -> BTreeMap<String, BTreeSet<Entry>> {
    let mut summaries: BTreeMap<String, BTreeSet<Entry>> = BTreeMap::new();
    let Some(closure) = workspace.closure_of(package) else {
        return summaries;
    };
    for target in super::check_targets(package) {
        let checker = super::unchecked(workspace, closure, crate::stdlib::packages(), target);
        for name in names {
            if let Some(id) = checker.package_named(name) {
                summaries
                    .entry((*name).to_owned())
                    .or_default()
                    .extend(checker.authority_summary(id));
            }
        }
    }
    summaries
}

/// A lock with the approvals its package's final closure needs: every one when `all`, else those
/// it holds that are still needed; with the diagnostics of what that lock leaves unapproved.
pub fn approvals(
    workspace: &Workspace,
    package: &crate::workspace::Package,
    mut lock: Lock,
    all: bool,
) -> (Lock, Vec<Diagnostic>) {
    let std = crate::stdlib::packages();
    let Some(closure) = workspace.closure_of(package) else {
        return (lock, Vec::new());
    };
    let mut pairs = BTreeSet::new();
    let mut epochs = BTreeSet::new();
    for target in super::check_targets(package) {
        let checker = super::unchecked(workspace, closure, std, target);
        let Some(root) = checker
            .db
            .packages
            .iter()
            .position(|data| std::ptr::eq(data.source, package))
            .map(|index| PackageId(index as u32))
        else {
            continue;
        };
        let needed = checker.needed(root);
        pairs.extend(needed.pairs);
        epochs.extend(needed.epochs);
    }
    if all {
        lock.approved = pairs.iter().cloned().collect();
        lock.epochs = epochs.iter().cloned().collect();
    } else {
        lock.approved.retain(|pair| pairs.contains(pair));
        lock.epochs.retain(|pair| epochs.contains(pair));
    }
    let left = unapproved(package, &Needed { pairs, epochs }, &lock);
    (lock, left)
}
