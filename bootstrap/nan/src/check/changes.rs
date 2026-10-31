//! The change classes of the items changed between two states of a workspace, as `nan diff`
//! reports them.

use std::collections::{BTreeMap, BTreeSet};

use super::db::ItemId;
use super::entail::{self, Weakened};
use super::{Checker, checked_closures};
use crate::diagnostic::{Code, Diagnostic, Reason};
use crate::hash::Hash;
use crate::machine::api::{Accepted, Seal};
use crate::machine::escaped;
use crate::syntax::ast::{ExprKind, ItemKind, RetiredNew};
use crate::workspace::{Package, Workspace};

/// What `nan diff` compares of an item in one state.
struct Facts {
    file: String,
    line: u32,
    /// Its lines as written, doc comments included.
    text: Vec<String>,
    item: Hash,
    normalised: String,
    interface: Option<String>,
    spec: Accepted,
    trust: Trust,
    /// Its schema hash, when it derives `Encode`.
    encode: Option<Hash>,
    /// For a `package` item: its `dep`, `allow` and `deny` directives, its lock's `approved` and
    /// `epoch` records, and its authority summary.
    directives: Vec<String>,
    approvals: Vec<String>,
    summary: Vec<String>,
}

/// The trust records of an item: its ledger entries, and its accepted spec, pending weakening and
/// seals as its module's API snapshot records them.
#[derive(PartialEq)]
struct Trust {
    ledger: Vec<String>,
    accepted: Accepted,
    pending: Vec<(Hash, String)>,
    seals: Vec<Seal>,
}

/// An item changed between two states: its canonical path, its location in the new state or,
/// when removed, the old, its change classes in order, whether it is a weakening or a widening,
/// and its old and new texts.
pub struct Change {
    pub path: String,
    pub file: String,
    pub line: u32,
    pub classes: Vec<&'static str>,
    pub weakening: bool,
    pub widening: bool,
    pub old: Vec<String>,
    pub new: Vec<String>,
}

impl Facts {
    /// The lines a `changed` record merges: its text, then its lock's approvals.
    fn shown(&self) -> Vec<String> {
        self.text.iter().chain(&self.approvals).cloned().collect()
    }
}

/// An item of the old state, and of the new one with the index of its checker.
type Matched<'s> = (Option<&'s Facts>, Option<(usize, ItemId, &'s Facts)>);

/// One state's items by canonical path, with the checker holding each; with `file`, only the
/// items of that file.
struct State<'w> {
    checkers: Vec<Checker<'w>>,
    items: BTreeMap<String, (usize, ItemId, Facts)>,
    /// The old canonical paths its rename tombstones retire, with the paths their rewrites name.
    renames: BTreeMap<String, String>,
}

impl<'w> State<'w> {
    fn of(workspace: &'w Workspace, file: Option<&str>) -> State<'w> {
        let std = crate::stdlib::packages();
        let checkers: Vec<Checker<'w>> = checked_closures(workspace, std)
            .into_iter()
            .map(|(_, checker)| checker)
            .collect();
        let mut items = BTreeMap::new();
        let mut renames = BTreeMap::new();
        for (index, checker) in checkers.iter().enumerate() {
            for id in 0..checker.db.items.len() {
                let item = ItemId(id as u32);
                if !checker.db.is_reported(item) {
                    continue;
                }
                if let Some((old, new)) = checker.rename(item) {
                    renames.insert(old, new);
                }
                if checker.db.in_history(item) {
                    continue;
                }
                let data = checker.db.item(item);
                if file.is_some_and(|file| file != data.file.path) {
                    continue;
                }
                let Some(path) = checker.canonical_path(item) else {
                    continue;
                };
                items
                    .entry(path)
                    .or_insert_with(|| (index, item, checker.facts(item)));
            }
        }
        State {
            checkers,
            items,
            renames,
        }
    }
}

impl Checker<'_> {
    /// The old canonical path a rename tombstone retires and the path its rewrite names.
    fn rename(&self, item: ItemId) -> Option<(String, String)> {
        let ItemKind::Retired(retired) = &self.db.ast(item).kind else {
            return None;
        };
        let RetiredNew::Rewrite {
            expr,
            preserves: true,
        } = &retired.new
        else {
            return None;
        };
        if retired.head.is_some() {
            return None;
        }
        let module = self.db.module_of(item);
        let old = self.db.old_form(module, retired)?;
        let new = match &expr.kind {
            ExprKind::Path { path, args: None } => self.db.spelled(module, path)?,
            ExprKind::Field { base, name } => match &base.kind {
                ExprKind::Path { path, args: None } => {
                    let owner = match self.db.owner_key(module, path) {
                        Some(key) => self.db.type_path(key),
                        None => self.db.spelled(module, path)?,
                    };
                    format!("{owner}.{}", name.text)
                }
                _ => return None,
            },
            _ => return None,
        };
        Some((old, new))
    }

    fn facts(&self, item: ItemId) -> Facts {
        let data = self.db.item(item);
        let syntax = data.syntax();
        let lines = syntax.item_lines(&syntax.items[data.index]);
        let text = lines
            .clone()
            .map(|line| String::from_utf8_lossy(syntax.line_text(line)).into_owned())
            .collect();
        let forms = self.forms(item);
        let package = self.db.package(self.db.package_of(item)).source;
        let module = &self.db.module(data.module).name;
        let path = self.canonical_path(item).unwrap_or_default();
        let entry = package
            .machine
            .snapshot(module)
            .and_then(|snapshot| snapshot.entries.iter().find(|entry| entry.path == path))
            .cloned()
            .unwrap_or_default();
        let ledger = self
            .ledger(item)
            .iter()
            .map(|entry| format!("{} {}", entry.kind.word(), entry.texts.join(" ")))
            .collect();
        let derives_encode = match &self.db.ast(item).kind {
            ItemKind::Struct(structure) => {
                structure.derives.iter().any(|name| name.text == "Encode")
            }
            ItemKind::Enum(enumeration) => {
                enumeration.derives.iter().any(|name| name.text == "Encode")
            }
            _ => false,
        };
        let encode =
            derives_encode.then(|| self.schema(&super::ty::Ty::item(item, Vec::new())).hash());
        let (directives, approvals, summary) = match &self.db.ast(item).kind {
            ItemKind::Package => self.package_authority(package),
            _ => Default::default(),
        };
        Facts {
            file: data.file.path.clone(),
            line: *lines.start(),
            text,
            item: self.hashes(item).item,
            normalised: forms.normalised,
            interface: forms.interface,
            spec: self.spec(item),
            trust: Trust {
                ledger,
                accepted: entry.accepted,
                pending: entry.pending,
                seals: entry.seals,
            },
            encode,
            directives,
            approvals,
            summary,
        }
    }

    /// A package item's `dep`, `allow` and `deny` directives, its lock's `approved` and `epoch`
    /// records, and its package's authority summary.
    fn package_authority(&self, package: &Package) -> (Vec<String>, Vec<String>, Vec<String>) {
        let mut directives = Vec::new();
        if let Some(manifest) = &package.manifest {
            let file = &package.files[0].syntax;
            let lines = manifest
                .deps
                .iter()
                .map(|dep| dep.line)
                .chain(manifest.allows.iter().map(|allow| allow.line))
                .chain(manifest.denies.iter().map(|(_, _, line)| *line));
            for line in lines {
                directives.push(
                    String::from_utf8_lossy(file.line_text(line))
                        .trim()
                        .to_owned(),
                );
            }
        }
        let mut approvals = Vec::new();
        if let Some(lock) = package.machine.lock() {
            approvals.extend(
                lock.approved
                    .iter()
                    .map(|(package, entry)| format!("approved {package} {}", entry.written())),
            );
            approvals.extend(
                lock.epochs
                    .iter()
                    .map(|(path, epoch)| format!("epoch {} {epoch}", escaped(path))),
            );
        }
        let summary = package
            .name()
            .and_then(|name| self.package_named(name))
            .map(|id| {
                self.authority_summary(id)
                    .iter()
                    .map(|entry| entry.written())
                    .collect()
            })
            .unwrap_or_default();
        (directives, approvals, summary)
    }
}

/// A form with each canonical path a rename retires replaced by the path its rewrite names.
fn rewritten(form: &str, renames: &BTreeMap<String, String>) -> String {
    let mut text = form.to_owned();
    for (old, new) in renames {
        let mut out = String::with_capacity(text.len());
        let mut rest = text.as_str();
        while let Some(at) = rest.find(old.as_str()) {
            let after = &rest[at + old.len()..];
            let whole = !after.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
                && !after.starts_with("::");
            out.push_str(&rest[..at]);
            out.push_str(if whole { new } else { old });
            rest = after;
        }
        out.push_str(rest);
        text = out;
    }
    text
}

/// The items changed between two states of a workspace, with `file` only those of that file,
/// in byte order of canonical path.
pub fn changes(old: &Workspace, new_workspace: &Workspace, file: Option<&str>) -> Vec<Change> {
    let before = State::of(old, file);
    let after = State::of(new_workspace, file);
    let renames: BTreeMap<String, String> = after
        .renames
        .iter()
        .filter(|(path, _)| !before.renames.contains_key(*path))
        .map(|(old, new)| (old.clone(), new.clone()))
        .collect();
    let mut matched: BTreeMap<String, Matched> = BTreeMap::new();
    for (path, (_, _, facts)) in &before.items {
        let target = renames.get(path).unwrap_or(path);
        matched.entry(target.clone()).or_default().0 = Some(facts);
    }
    for (path, (index, item, facts)) in &after.items {
        matched.entry(path.clone()).or_default().1 = Some((*index, *item, facts));
    }
    let mut changes = Vec::new();
    for (path, (old, new)) in matched {
        let change = match (old, new) {
            (Some(old), Some((index, item, new))) => {
                let renamed = !before.items.contains_key(&path);
                let form = rewritten(&old.normalised, &renames);
                let interface = old
                    .interface
                    .as_deref()
                    .map(|form| rewritten(form, &renames));
                let mut classes = Vec::new();
                let text_changed = old.text != new.text || old.file != new.file;
                if text_changed && !renamed && old.item == new.item {
                    classes.push("format");
                }
                if renamed || (old.item != new.item && form == new.normalised) {
                    classes.push("move");
                }
                if form != new.normalised && interface == new.interface {
                    classes.push("body");
                }
                if interface != new.interface {
                    classes.push("interface");
                }
                if !old.spec.same(&new.spec) {
                    classes.push("spec");
                }
                if (&old.directives, &old.approvals, &old.summary)
                    != (&new.directives, &new.approvals, &new.summary)
                {
                    classes.push("authority");
                }
                if old.encode != new.encode {
                    classes.push("encode");
                }
                if old.trust != new.trust {
                    classes.push("trust");
                }
                if classes.is_empty() {
                    continue;
                }
                let checker = &after.checkers[index];
                let source = checker.db.package(checker.db.package_of(item)).source;
                let package = new_workspace
                    .packages
                    .iter()
                    .position(|package| std::ptr::eq(&**package, source));
                let weakened = Weakened {
                    ck: checker,
                    item,
                    accepted: old.spec.clone(),
                    later: None,
                };
                let weakening = package.is_some_and(|package| {
                    !entail::left(new_workspace, package, &[weakened])
                        .remove(0)
                        .is_empty()
                });
                let widening = new.summary.iter().any(|entry| !old.summary.contains(entry));
                Change {
                    path,
                    file: new.file.clone(),
                    line: new.line,
                    classes,
                    weakening,
                    widening,
                    old: old.shown(),
                    new: new.shown(),
                }
            }
            (Some(old), None) => Change {
                path,
                file: old.file.clone(),
                line: old.line,
                classes: vec!["interface"],
                weakening: false,
                widening: false,
                old: old.shown(),
                new: Vec::new(),
            },
            (None, Some((_, _, new))) => Change {
                path,
                file: new.file.clone(),
                line: new.line,
                classes: vec!["interface"],
                weakening: false,
                widening: !new.summary.is_empty(),
                old: Vec::new(),
                new: new.shown(),
            },
            (None, None) => continue,
        };
        changes.push(change);
    }
    changes
}

/// What a semantic conflict reads of an item in one version: its interface hash, the canonical
/// paths of the items it names, and its interface text.
struct Naming {
    interface: Option<Hash>,
    names: BTreeSet<String>,
    api: Vec<String>,
}

/// The items of a version by canonical path, with `file` only those of that file.
fn namings(workspace: &Workspace, file: Option<&str>) -> BTreeMap<String, Naming> {
    let std = crate::stdlib::packages();
    let mut found = BTreeMap::new();
    for (_, checker) in checked_closures(workspace, std) {
        for id in 0..checker.db.items.len() {
            let item = ItemId(id as u32);
            if !checker.db.is_reported(item) || checker.db.in_history(item) {
                continue;
            }
            if file.is_some_and(|file| file != checker.db.item(item).file.path) {
                continue;
            }
            let Some(path) = checker.canonical_path(item) else {
                continue;
            };
            found.entry(path).or_insert_with(|| Naming {
                interface: checker.hashes(item).interface,
                names: checker
                    .closure_entries(item)
                    .into_iter()
                    .map(|entry| entry.path)
                    .collect(),
                api: checker.interface_text(item, None),
            });
        }
    }
    found
}

/// The semantic conflicts of merging two versions of a workspace, or with `file` of one file, into
/// `merged`: one side changed the interface hash of an item I, and the other added an item J
/// naming I or changed J so that it names I where its base did not. Each is reported on J where
/// `merged` holds it, quoting I's base and changed interface text.
pub fn semantic_conflicts(
    base: &Workspace,
    ours: &Workspace,
    theirs: &Workspace,
    merged: &Workspace,
    file: Option<&str>,
) -> Vec<Diagnostic> {
    let (base, ours, theirs) = (
        namings(base, file),
        namings(ours, file),
        namings(theirs, file),
    );
    let mut found: BTreeMap<(String, String), Diagnostic> = BTreeMap::new();
    for (changer, other) in [(&ours, &theirs), (&theirs, &ours)] {
        for (named, old) in &base {
            let Some(changed) = changer
                .get(named)
                .filter(|new| new.interface != old.interface)
            else {
                continue;
            };
            for (path, naming) in other {
                let newly = base
                    .get(path)
                    .is_none_or(|before| !before.names.contains(named));
                if !naming.names.contains(named) || !newly {
                    continue;
                }
                let Some((at, line)) = merged.item_location(path) else {
                    continue;
                };
                let because = old
                    .api
                    .iter()
                    .map(|line| Reason::note(&format!("- {line}")))
                    .chain(
                        changed
                            .api
                            .iter()
                            .map(|line| Reason::note(&format!("+ {line}"))),
                    )
                    .collect();
                found.insert(
                    (path.clone(), named.clone()),
                    Diagnostic {
                        because,
                        item: Some(path.clone()),
                        ..Diagnostic::at(Code::MergeSemanticConflict, &at, line)
                    },
                );
            }
        }
    }
    found.into_values().collect()
}
