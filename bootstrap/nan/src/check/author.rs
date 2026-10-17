//! The author rule of a VCS workspace, read from its git history: a seal meets it when each change
//! writing it is by another author than each change introducing its sealed item hash or a hash
//! its closure entries record; and a change whose accepted spec of an item keeps none of its
//! parents' writes such a seal of the item, by another author than the changes introducing that
//! spec as the item's spec.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::entail::{self, Weakened};
use super::snapshot::Items;
use super::spec::keeps;
use super::{Checker, package_checkers};
use crate::diagnostic::{Code, Diagnostic, Reason};
use crate::hash::Hash;
use crate::machine::api::{Accepted, ClosureEntry, Entry, Seal, Snapshot};
use crate::machine::lock::Lock;
use crate::vcs::{Change, Vcs};
use crate::workspace::{Package, Target, Workspace};

/// The machine records of the workspace in one change: each item's API snapshot entry by
/// canonical path, and the lock lines of each package by the package each locks.
#[derive(Default)]
struct Recorded {
    entries: HashMap<String, Entry>,
    locks: HashMap<String, HashMap<String, String>>,
}

/// What checking the workspace in one change gives, by canonical path: each item's item hash and
/// interface hash, sealed item hash and current spec.
#[derive(Default)]
struct Checked {
    hashes: HashMap<String, (Hash, Option<Hash>)>,
    sealed: HashMap<String, Hash>,
    specs: HashMap<String, Accepted>,
}

/// The states of a workspace across the changes of its git history.
pub struct History<'w> {
    vcs: Vcs,
    current: &'w Workspace,
    /// Each commit changing the workspace, with its nearest ancestors changing it.
    parents: HashMap<String, Vec<String>>,
    /// The most recent commit changing the workspace.
    top: Option<String>,
    recorded: RefCell<HashMap<Change, Rc<Recorded>>>,
    checked: RefCell<HashMap<Change, Rc<Checked>>>,
}

impl<'w> History<'w> {
    /// The history of a workspace inside a git working tree; none outside every one.
    pub fn open(current: &'w Workspace) -> Option<History<'w>> {
        let vcs = Vcs::open(&current.root)?;
        let listed = vcs.history();
        let top = listed.first().map(|(commit, _)| commit.clone());
        Some(History {
            parents: listed.into_iter().collect(),
            vcs,
            current,
            top,
            recorded: RefCell::new(HashMap::new()),
            checked: RefCell::new(HashMap::new()),
        })
    }

    fn parents(&self, change: &Change) -> Vec<Change> {
        let names = match change {
            Change::Uncommitted => self.top.iter().cloned().collect(),
            Change::Commit(name) => self.parents.get(name).cloned().unwrap_or_default(),
        };
        names.into_iter().map(Change::Commit).collect()
    }

    /// The workspace in a commit's tree, with only the files `wanted` keeps.
    fn workspace(&self, commit: &str, wanted: &dyn Fn(&str) -> bool) -> Option<Workspace> {
        let files = self.vcs.files(commit, wanted);
        let held: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(path, content)| (path.as_str(), content.as_slice()))
            .collect();
        Workspace::of_stored_files(self.current.root.clone(), &held).ok()
    }

    fn recorded(&self, change: &Change) -> Rc<Recorded> {
        if let Some(known) = self.recorded.borrow().get(change) {
            return known.clone();
        }
        let machine = |path: &str| path.ends_with("/package.nan") || path.contains("/.nan/");
        let recorded = Rc::new(match change {
            Change::Uncommitted => Recorded::of(self.current),
            Change::Commit(name) => self
                .workspace(name, &machine)
                .map(|workspace| Recorded::of(&workspace))
                .unwrap_or_default(),
        });
        self.recorded
            .borrow_mut()
            .insert(change.clone(), recorded.clone());
        recorded
    }

    fn checked(&self, change: &Change) -> Rc<Checked> {
        if let Some(known) = self.checked.borrow().get(change) {
            return known.clone();
        }
        let checked = Rc::new(match change {
            Change::Uncommitted => Checked::of(self.current),
            Change::Commit(name) => self
                .workspace(name, &|_| true)
                .map(|workspace| Checked::of(&workspace))
                .unwrap_or_default(),
        });
        self.checked
            .borrow_mut()
            .insert(change.clone(), checked.clone());
        checked
    }

    /// The changes, at or before `from`, introducing a fact that `from` holds: holding it while
    /// none of their parents does.
    fn introducers(&self, from: &Change, holds: &dyn Fn(&Change) -> bool) -> Vec<Change> {
        let mut found = Vec::new();
        let mut seen = HashSet::new();
        let mut pending = vec![from.clone()];
        while let Some(change) = pending.pop() {
            if !seen.insert(change.clone()) {
                continue;
            }
            let holding: Vec<Change> = self
                .parents(&change)
                .into_iter()
                .filter(|parent| holds(parent))
                .collect();
            match holding.is_empty() {
                true => found.push(change),
                false => pending.extend(holding),
            }
        }
        found
    }

    fn authors(&self, changes: &[Change]) -> HashSet<String> {
        changes
            .iter()
            .map(|change| self.vcs.author(change))
            .collect()
    }

    /// The author that a change of `writers`, writing a seal of the item at `path` in the package
    /// `package` as of `from`, shares with a change introducing its sealed item hash or a hash its
    /// closure entries record; the empty text for a seal not current as of `from`.
    fn shared(
        &self,
        from: &Change,
        writers: &[Change],
        package: &str,
        path: &str,
        seal: &Seal,
    ) -> Option<String> {
        let sealed = |change: &Change| self.checked(change).sealed.get(path) == Some(&seal.item);
        if !sealed(from) {
            return Some(String::new());
        }
        let mut introducers = self.introducers(from, &sealed);
        for entry in &seal.entries {
            if self.checked(from).hashes.contains_key(&entry.path) {
                let held = |change: &Change| {
                    self.checked(change)
                        .hashes
                        .get(&entry.path)
                        .is_some_and(|(item, interface)| held_hash(entry, *item, *interface))
                };
                if held(from) {
                    introducers.extend(self.introducers(from, &held));
                }
                continue;
            }
            let dep = entry.path.split("::").next().unwrap_or_default().to_owned();
            let line = |change: &Change| {
                self.recorded(change)
                    .locks
                    .get(package)
                    .and_then(|lines| lines.get(&dep))
                    .cloned()
            };
            if let Some(current) = line(from) {
                let held = |change: &Change| line(change).as_ref() == Some(&current);
                introducers.extend(self.introducers(from, &held));
            }
        }
        let written = self.authors(writers);
        introducers
            .iter()
            .map(|change| self.vcs.author(change))
            .find(|author| written.contains(author))
    }

    /// The author the seal `nan seal` would write on the item at `path` of `package` shares
    /// with a change introducing a hash it records or, when it accepts a pending weakening,
    /// with a change introducing the item's current spec.
    pub fn would_share(
        &self,
        package: &str,
        path: &str,
        seal: &Seal,
        accepting: Option<&Accepted>,
    ) -> Option<String> {
        let writers = [Change::Uncommitted];
        if let Some(author) = self.shared(&Change::Uncommitted, &writers, package, path, seal) {
            return Some(author);
        }
        let spec = accepting?;
        let introduced = |change: &Change| {
            self.checked(change)
                .specs
                .get(path)
                .is_some_and(|held| held.same(spec))
        };
        let introducers = self.introducers(&Change::Uncommitted, &introduced);
        let author = self.vcs.author(&Change::Uncommitted);
        self.authors(&introducers)
            .contains(&author)
            .then_some(author)
    }

    /// The `seal.same-author` incompleteness of each item of `escaping`, which hold trust
    /// escapes, whose current seals all break the author rule.
    pub fn same_author(
        &self,
        package: &Package,
        items: &Items,
        escaping: &[String],
    ) -> Vec<Diagnostic> {
        let name = package.name().unwrap_or_default();
        let recorded = self.recorded(&Change::Uncommitted);
        let mut found = Vec::new();
        for path in escaping {
            let seals: Vec<&Seal> = recorded
                .entries
                .get(path)
                .map(|entry| {
                    entry
                        .seals
                        .iter()
                        .filter(|seal| items.seal_current(path, seal))
                        .collect()
                })
                .unwrap_or_default();
            if seals.is_empty() {
                continue;
            }
            let mut shared = Vec::new();
            for seal in &seals {
                let written = |change: &Change| {
                    self.recorded(change)
                        .entries
                        .get(path)
                        .is_some_and(|entry| entry.seals.contains(seal))
                };
                let writers = self.introducers(&Change::Uncommitted, &written);
                match self.shared(&Change::Uncommitted, &writers, name, path, seal) {
                    Some(author) => shared.push(format!("{} {author}", seal.principal)),
                    None => break,
                }
            }
            if shared.len() < seals.len() {
                continue;
            }
            if let Some((file, line)) = location(items, package, path) {
                found.push(Diagnostic {
                    because: shared.iter().map(|text| Reason::note(text)).collect(),
                    item: Some(path.clone()),
                    ..Diagnostic::at(Code::SealSameAuthor, &file, line)
                });
            }
        }
        found
    }

    /// The `api.weakening-pending` incompleteness of each item of `package` and each change whose
    /// accepted spec of the item keeps none of its parents' and which writes no seal of the item
    /// that meets the author rule by another author than the changes introducing that spec as
    /// the item's spec.
    pub fn acceptances(&self, package: &Package, items: &Items) -> Vec<Diagnostic> {
        let name = package.name().unwrap_or_default();
        let mut changes = vec![Change::Uncommitted];
        changes.extend(
            self.vcs
                .changing("**/.nan/api/*.nan")
                .into_iter()
                .filter(|commit| self.parents.contains_key(commit))
                .map(Change::Commit),
        );
        let accepted_at = |change: &Change, path: &str| {
            self.recorded(change)
                .entries
                .get(path)
                .map(|entry| entry.accepted.clone())
                .unwrap_or_default()
        };
        let mut judged: Vec<(Change, String, Accepted, Vec<Accepted>)> = Vec::new();
        for path in items.by_path.keys() {
            for change in &changes {
                let accepted = accepted_at(change, path);
                let parents = self.parents(change);
                let earlier: Vec<Accepted> = parents
                    .iter()
                    .map(|parent| accepted_at(parent, path))
                    .collect();
                let kept = accepted.is_empty()
                    || parents.is_empty()
                    || earlier.iter().any(|before| keeps(&accepted, before));
                if !kept {
                    judged.push((change.clone(), path.clone(), accepted, earlier));
                }
            }
        }
        let weakened: Vec<Weakened> = judged
            .iter()
            .flat_map(|(_, path, accepted, earlier)| {
                let first = items.first(path);
                earlier.iter().filter_map(move |before| {
                    let first = first?;
                    Some(Weakened {
                        ck: first.ck,
                        item: first.item,
                        accepted: before.clone(),
                        later: Some(accepted.clone()),
                    })
                })
            })
            .collect();
        let index = self
            .current
            .packages
            .iter()
            .position(|held| std::ptr::eq(held, package));
        let mut left = match index {
            Some(index) if !weakened.is_empty() => entail::left(self.current, index, &weakened),
            _ => Vec::new(),
        }
        .into_iter();
        let mut found = Vec::new();
        for (change, path, accepted, earlier) in judged {
            let entailed = match items.first(&path) {
                Some(_) => {
                    let left: Vec<Vec<String>> = left.by_ref().take(earlier.len()).collect();
                    left.iter().any(Vec::is_empty)
                }
                None => false,
            };
            if entailed || self.accepted_by_seal(&change, name, &path, &accepted) {
                continue;
            }
            let Some((file, line)) = location(items, package, &path) else {
                continue;
            };
            let named = match &change {
                Change::Uncommitted => "uncommitted".to_owned(),
                Change::Commit(name) => name.clone(),
            };
            found.push(Diagnostic {
                because: vec![Reason::note(&format!("change {named}"))],
                item: Some(path.clone()),
                ..Diagnostic::at(Code::ApiWeakeningPending, &file, line)
            });
        }
        found
    }

    /// Whether a change writes a seal of the item at `path` that meets the author rule as of the
    /// change, by another author than the changes introducing `accepted` as the item's spec.
    fn accepted_by_seal(
        &self,
        change: &Change,
        package: &str,
        path: &str,
        accepted: &Accepted,
    ) -> bool {
        let held = |at: &Change| {
            self.recorded(at)
                .entries
                .get(path)
                .map(|entry| entry.seals.clone())
                .unwrap_or_default()
        };
        let earlier: Vec<Seal> = self.parents(change).iter().flat_map(held).collect();
        let introduced = |at: &Change| {
            self.checked(at)
                .specs
                .get(path)
                .is_some_and(|spec| spec.same(accepted))
        };
        let author = self.vcs.author(change);
        let introducers = match introduced(change) {
            true => self.introducers(change, &introduced),
            false => Vec::new(),
        };
        if self.authors(&introducers).contains(&author) {
            return false;
        }
        held(change)
            .iter()
            .filter(|seal| !earlier.contains(seal))
            .any(|seal| {
                self.shared(change, std::slice::from_ref(change), package, path, seal)
                    .is_none()
            })
    }
}

impl Recorded {
    fn of(workspace: &Workspace) -> Recorded {
        let mut recorded = Recorded::default();
        for package in &workspace.packages {
            for (_, loaded) in &package.machine.snapshots {
                let snapshot: Option<&Snapshot> = loaded.valid();
                for entry in snapshot.into_iter().flat_map(|snapshot| &snapshot.entries) {
                    recorded.entries.insert(entry.path.clone(), entry.clone());
                }
            }
            if let (Some(name), Some(lock)) = (package.name(), package.machine.lock()) {
                recorded.locks.insert(name.to_owned(), lock_lines(lock));
            }
        }
        recorded
    }
}

impl Checked {
    fn of(workspace: &Workspace) -> Checked {
        let std = crate::stdlib::packages();
        let mut checked = Checked::default();
        for package in &workspace.packages {
            let Some(name) = package.name() else {
                continue;
            };
            let checkers = package_checkers(workspace, std, package);
            let views: Vec<(Target, &Checker)> =
                checkers.iter().map(|(target, ck)| (*target, ck)).collect();
            let items = Items::of(&views, name);
            for (path, variants) in &items.by_path {
                let first = &variants[0];
                let hashes = first.ck.hashes(first.item);
                checked
                    .hashes
                    .insert(path.clone(), (hashes.item, hashes.interface));
                checked
                    .specs
                    .insert(path.clone(), first.ck.spec(first.item));
                if let Some(sealed) = items.sealed_hash(path) {
                    checked.sealed.insert(path.clone(), sealed);
                }
            }
            if let (Some(path), Some(sealed)) = (items.package_path(), items.sealed_hash(name)) {
                checked.sealed.insert(path.to_owned(), sealed);
            }
        }
        checked
    }
}

/// Each line of a lock by the package it locks, as the lock writes it.
fn lock_lines(lock: &Lock) -> HashMap<String, String> {
    lock.deps
        .iter()
        .map(|line| (line.package.clone(), line.record()))
        .collect()
}

/// Whether a closure entry records the item hash or interface hash an item has.
fn held_hash(entry: &ClosureEntry, item: Hash, interface: Option<Hash>) -> bool {
    match entry.body {
        true => entry.hash == item,
        false => interface == Some(entry.hash),
    }
}

/// The file and first line of an item of a package: the package item's on its `package` line.
fn location(items: &Items, package: &Package, path: &str) -> Option<(String, u32)> {
    if items.package_path() == Some(path) {
        let line = package.manifest.as_ref()?.line;
        return Some((format!("{}/package.nan", package.dir), line));
    }
    let first = items.first(path)?;
    Some((first.file.clone(), first.ck.db.item(first.item).head_line()))
}
