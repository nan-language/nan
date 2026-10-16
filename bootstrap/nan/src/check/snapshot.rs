//! API snapshots as `nan api update` writes them: per module of a package, an entry for each
//! item consumers see with its interface text, promises, interface hash, digests and public
//! docs, each item's accepted spec and pending weakening, and the seals of its items.

use std::collections::{BTreeMap, BTreeSet};

use super::Checker;
use super::db::{ItemId, PackageId};
use super::props::Property;
use super::seal::closure_hash;
use crate::hash::Hash;
use crate::machine::api::{Accepted, ClosureEntry, Doc, Entry, Seal, Snapshot};
use crate::syntax::ast::ItemKind;
use crate::workspace::{Package, Target};

/// An item as one checker holds it: the target the checker checks under, the item, and the path
/// of the file holding it.
pub struct Variant<'c, 'a> {
    pub target: Target,
    pub ck: &'c Checker<'a>,
    pub item: ItemId,
    pub file: String,
}

/// The items of a package across the checkers of its targets, by canonical path, each with the
/// variants holding it under each target, in byte order of file path, then of target.
pub struct Items<'c, 'a> {
    pub by_path: BTreeMap<String, Vec<Variant<'c, 'a>>>,
    /// The package item's hashes and ledger under the first checker.
    pub package: Option<(PackageId, &'c Checker<'a>)>,
}

impl<'c, 'a> Items<'c, 'a> {
    pub fn of(checkers: &[(Target, &'c Checker<'a>)], package: &str) -> Items<'c, 'a> {
        let mut by_path: BTreeMap<String, Vec<Variant>> = BTreeMap::new();
        let mut first = None;
        for (target, ck) in checkers {
            let Some(id) = ck.package_named(package) else {
                continue;
            };
            first.get_or_insert((id, *ck));
            for &module in &ck.db.package(id).modules {
                for &item in &ck.db.module(module).items {
                    let Some(path) = ck.canonical_path(item) else {
                        continue;
                    };
                    by_path.entry(path).or_default().push(Variant {
                        target: *target,
                        ck,
                        item,
                        file: ck.db.item(item).file.path.clone(),
                    });
                }
            }
        }
        for variants in by_path.values_mut() {
            variants.sort_by(|left, right| {
                (&left.file, left.target.name()).cmp(&(&right.file, right.target.name()))
            });
        }
        Items {
            by_path,
            package: first,
        }
    }

    /// The variant of an item whose text and docs its entry holds: the first by file path.
    pub fn first(&self, path: &str) -> Option<&Variant<'c, 'a>> {
        self.by_path.get(path)?.first()
    }

    /// The sealed item hash of an item: its item hash, or for an item that two or more target
    /// variants hold, the hash of `variants`, LF and each variant's item hash in full form and LF,
    /// in byte order of its directory's path.
    pub fn sealed_hash(&self, path: &str) -> Option<Hash> {
        if self.package_path() == Some(path) {
            let (package, ck) = self.package?;
            return ck.package_hashes(package).map(|hashes| hashes.item);
        }
        let variants = self.by_path.get(path)?;
        let mut held: Vec<(&str, Hash)> = Vec::new();
        for variant in variants {
            let dir = variant.file.rsplit_once('/').map_or("", |(dir, _)| dir);
            if !held.iter().any(|(known, _)| *known == dir) {
                held.push((dir, variant.ck.hashes(variant.item).item));
            }
        }
        if held.len() < 2 {
            return held.first().map(|(_, hash)| *hash);
        }
        let mut text = String::from("variants\n");
        for (_, hash) in held {
            text.push_str(&hash.full());
            text.push('\n');
        }
        Some(Hash::of(text.as_bytes()))
    }

    /// The closure entries of an item: the union of its variants'.
    pub fn closure_entries(&self, path: &str) -> BTreeSet<ClosureEntry> {
        self.by_path
            .get(path)
            .into_iter()
            .flatten()
            .flat_map(|variant| variant.ck.closure_entries(variant.item))
            .collect()
    }

    /// Whether a seal of an item is current: it holds no signature and the item's sealed item
    /// hash and closure hash are those it records.
    pub fn seal_current(&self, path: &str, seal: &Seal) -> bool {
        seal.signature.is_none()
            && self.sealed_hash(path) == Some(seal.item)
            && closure_hash(&self.closure_entries(path)) == seal.closure
    }

    /// Whether an item holds a trust escape: the package item through its grants.
    pub fn holds_escape(&self, path: &str) -> bool {
        if self.package_path() == Some(path) {
            return self
                .package
                .is_some_and(|(package, ck)| !ck.grant_ledger(package).is_empty());
        }
        self.first(path)
            .is_some_and(|variant| variant.ck.holds_escape(variant.item))
    }

    /// The canonical path of the package item: the package name.
    pub fn package_path(&self) -> Option<&str> {
        let (package, ck) = self.package?;
        Some(ck.db.package(package).name.as_str())
    }
}

/// A pending weakening `nan api update` records: the item's canonical path, file and first
/// line, and its weakened parts.
pub struct Recorded {
    pub path: String,
    pub file: String,
    pub line: u32,
    pub parts: Vec<String>,
}

/// The snapshots of a package's modules, by module name, none for a module whose snapshot holds
/// no entry, and the pending weakenings recorded.
pub struct Snapshots {
    pub modules: Vec<(String, Option<Snapshot>)>,
    pub recorded: Vec<Recorded>,
}

/// The snapshots `nan api update` computes for a package from its source, its snapshots and,
/// for each item of `confirmed`, the bound hashes of its public docs set to its interface hash.
pub fn snapshots(items: &Items, source: &Package, confirmed: &[String]) -> Snapshots {
    let mut modules: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
    let mut recorded = Vec::new();
    let root = items
        .package
        .and_then(|(package, ck)| ck.db.package(package).root)
        .map(|root| {
            items
                .package
                .expect("a package")
                .1
                .db
                .module(root)
                .name
                .clone()
        });
    for module in source.modules.iter() {
        modules.entry(module.name.clone()).or_default();
    }
    for (path, variants) in &items.by_path {
        let first = &variants[0];
        let ck = first.ck;
        if matches!(
            ck.db.ast(first.item).kind,
            ItemKind::Retired(_) | ItemKind::Package
        ) {
            continue;
        }
        let module = ck.db.module(ck.db.module_of(first.item)).name.clone();
        let old = source
            .machine
            .snapshot(&module)
            .and_then(|snapshot| snapshot.entry(path));
        let (entry, pending) = entry(items, path, old, confirmed.contains(path));
        if let Some(parts) = pending {
            let data = ck.db.item(first.item);
            recorded.push(Recorded {
                path: path.clone(),
                file: data.file.path.clone(),
                line: data.head_line(),
                parts,
            });
        }
        if let Some(entry) = entry {
            modules.entry(module).or_default().push(entry);
        }
    }
    if let (Some(root), Some(path)) = (root, items.package_path()) {
        let old = source
            .machine
            .snapshot(&root)
            .and_then(|snapshot| snapshot.entry(path));
        let seals = kept_seals(items, path, old, false);
        if !seals.is_empty() {
            let entries = modules.entry(root).or_default();
            entries.push(Entry {
                path: path.to_owned(),
                seals,
                ..Entry::default()
            });
            entries.sort_by(|left, right| left.path.cmp(&right.path));
        }
    }
    Snapshots {
        modules: modules
            .into_iter()
            .map(|(name, entries)| {
                let snapshot = (!entries.is_empty()).then_some(Snapshot { entries });
                (name, snapshot)
            })
            .collect(),
        recorded,
    }
}

/// Why `nan seal` writes nothing: the item needs no seal, its module's snapshot is not current,
/// or the seal would break the author rule, sharing this author.
pub enum Unsealed {
    Unneeded,
    Stale,
    SameAuthor(String),
}

/// A seal `nan seal` writes: the name of the module whose snapshot records it, that snapshot, the
/// seal, and the spec it accepts when the item has a pending weakening.
pub struct Sealed {
    pub module: String,
    pub snapshot: Snapshot,
    pub seal: Seal,
    pub accepted: Option<Accepted>,
}

/// The snapshot of the module recording the item at `path` once `principal` seals it: the seal
/// replaces an earlier one of the principal and accepts the item's pending weakening.
pub fn sealed(
    items: &Items,
    source: &Package,
    path: &str,
    principal: &str,
) -> Result<Sealed, Unsealed> {
    let module = match items.first(path) {
        _ if items.package_path() == Some(path) => {
            let (package, ck) = items.package.ok_or(Unsealed::Unneeded)?;
            let root = ck.db.package(package).root.ok_or(Unsealed::Unneeded)?;
            ck.db.module(root).name.clone()
        }
        Some(first) => first
            .ck
            .db
            .module(first.ck.db.module_of(first.item))
            .name
            .clone(),
        None => return Err(Unsealed::Unneeded),
    };
    let computed = snapshots(items, source, &[]);
    let current = computed
        .modules
        .into_iter()
        .find(|(name, _)| *name == module)
        .and_then(|(_, snapshot)| snapshot);
    if current.as_ref() != source.machine.snapshot(&module) {
        return Err(Unsealed::Stale);
    }
    let mut snapshot = current.unwrap_or_default();
    let at = match snapshot.entries.iter().position(|entry| entry.path == path) {
        Some(at) => at,
        None => {
            let at = snapshot
                .entries
                .partition_point(|entry| entry.path.as_str() < path);
            snapshot.entries.insert(
                at,
                Entry {
                    path: path.to_owned(),
                    ..Entry::default()
                },
            );
            at
        }
    };
    let entry = &mut snapshot.entries[at];
    let pending = !entry.pending.is_empty()
        || entry
            .accepted
            .methods
            .iter()
            .any(|method| !method.pending.is_empty());
    if !pending && !items.holds_escape(path) {
        return Err(Unsealed::Unneeded);
    }
    let mut accepted = None;
    if pending && let Some(first) = items.first(path) {
        entry.accepted = first.ck.spec(first.item);
        entry.pending.clear();
        accepted = Some(entry.accepted.clone());
    }
    let entries = items.closure_entries(path);
    let seal = Seal {
        principal: principal.to_owned(),
        item: items.sealed_hash(path).ok_or(Unsealed::Unneeded)?,
        closure: closure_hash(&entries),
        entries: entries.into_iter().collect(),
        signature: None,
    };
    entry.seals.retain(|kept| kept.principal != principal);
    let at = entry
        .seals
        .partition_point(|kept| kept.principal.as_str() < principal);
    entry.seals.insert(at, seal.clone());
    Ok(Sealed {
        module,
        snapshot,
        seal,
        accepted,
    })
}

/// The entry of an item and the weakened parts it records as pending, if any.
fn entry(
    items: &Items,
    path: &str,
    old: Option<&Entry>,
    confirm: bool,
) -> (Option<Entry>, Option<Vec<String>>) {
    let first = items.first(path).expect("an item with a variant");
    let (ck, item) = (first.ck, first.item);
    let mut entry = Entry {
        path: path.to_owned(),
        ..Entry::default()
    };
    if ck.consumers_see(item) {
        let hashes = ck.hashes(item);
        entry.text = ck.interface_text(item, None);
        entry.promises = promises(ck, item);
        entry.interface = hashes.interface;
        if matches!(ck.db.ast(item).kind, ItemKind::Fn(_)) {
            let mut digests: Vec<(String, String)> = Vec::new();
            for variant in &items.by_path[path] {
                let target = variant.target.name().to_owned();
                if !digests.iter().any(|(known, _)| *known == target) {
                    digests.push((target, variant.ck.digest_text(variant.item)));
                }
            }
            digests.sort();
            entry.digests = digests;
        }
        let interface = hashes.interface.unwrap_or(hashes.item);
        entry.docs = ck
            .public_docs(item)
            .into_iter()
            .map(|(key, text, _)| {
                let kept = old
                    .and_then(|old| {
                        old.docs
                            .iter()
                            .find(|doc| doc.key == key && doc.text == text)
                    })
                    .map(|doc| doc.bound)
                    .filter(|_| !confirm);
                Doc {
                    key,
                    bound: kept.unwrap_or(interface),
                    text,
                }
            })
            .collect();
    }
    let current = ck.spec(item);
    let accepted = old.map(|old| old.accepted.clone()).unwrap_or_default();
    let mut pending = None;
    if !current.is_empty() || !accepted.is_empty() {
        let item_hash = ck.hashes(item).item;
        if matches!(ck.db.ast(item).kind, ItemKind::Interface(_)) {
            let weakened = ck.weakened_methods(&accepted, &current);
            if weakened.is_empty() {
                entry.accepted = current;
            } else {
                entry.accepted = accepted;
                let mut all = Vec::new();
                for method in &mut entry.accepted.methods {
                    method.pending = weakened
                        .get(&method.name)
                        .into_iter()
                        .flatten()
                        .map(|part| (item_hash, part.clone()))
                        .collect();
                    all.extend(weakened.get(&method.name).into_iter().flatten().cloned());
                }
                pending = Some(all);
            }
        } else {
            let weakened = ck.weakened(item, &accepted, &current);
            if weakened.is_empty() {
                entry.accepted = current;
            } else {
                entry.accepted = accepted;
                entry.pending = weakened
                    .iter()
                    .map(|part| (item_hash, part.clone()))
                    .collect();
                pending = Some(weakened);
            }
        }
    }
    entry.seals = kept_seals(items, path, old, pending.is_some());
    let kept = ck.consumers_see(item) || !entry.accepted.is_empty() || !entry.seals.is_empty();
    (kept.then_some(entry), pending)
}

/// The seals of an item's entry that `nan api update` keeps: all, but the lapsed ones of an item
/// that needs no seal.
fn kept_seals(items: &Items, path: &str, old: Option<&Entry>, pending: bool) -> Vec<Seal> {
    let needs = pending || items.holds_escape(path);
    old.map(|old| old.seals.clone())
        .unwrap_or_default()
        .into_iter()
        .filter(|seal| needs || items.seal_current(path, seal))
        .collect()
}

/// The promises a `pub` struct or enum states: those of `Copy`, `Send`, `Inert` and `AnyBits`
/// consumers see, each with the generic parameters it rests on.
fn promises(ck: &Checker, item: ItemId) -> Vec<(String, Vec<String>)> {
    if !matches!(
        ck.db.ast(item).kind,
        ItemKind::Struct(_) | ItemKind::Enum(_)
    ) {
        return Vec::new();
    }
    let seen = ck.consumer_props(item);
    let generics = &ck.sig(item).generics;
    [
        ("Copy", seen.copy, Property::Copy),
        ("Send", seen.send, Property::Send),
        ("Inert", seen.inert, Property::Inert),
        ("AnyBits", seen.any_bits, Property::AnyBits),
    ]
    .into_iter()
    .filter(|(_, holds, _)| *holds)
    .map(|(name, _, property)| {
        let resting = ck
            .dependent_params(item, property)
            .into_iter()
            .filter_map(|index| generics.get(index).map(|def| def.name.clone()))
            .collect();
        (name.to_owned(), resting)
    })
    .collect()
}

impl Accepted {
    /// Whether two accepted specs hold equal clauses and tests.
    pub fn same(&self, other: &Accepted) -> bool {
        self.clauses == other.clauses && self.tests == other.tests && self.methods == other.methods
    }
}

/// The entries of a snapshot that differ from another's, absence included, by canonical path.
pub fn differing(current: Option<&Snapshot>, written: Option<&Snapshot>) -> Vec<String> {
    let entries = |snapshot: Option<&Snapshot>| -> BTreeMap<String, Entry> {
        snapshot
            .map(|snapshot| {
                snapshot
                    .entries
                    .iter()
                    .map(|entry| (entry.path.clone(), entry.clone()))
                    .collect()
            })
            .unwrap_or_default()
    };
    let (current, written) = (entries(current), entries(written));
    let paths: BTreeSet<&String> = current.keys().chain(written.keys()).collect();
    paths
        .into_iter()
        .filter(|path| current.get(*path) != written.get(*path))
        .cloned()
        .collect()
}
