//! Code roots: the artefact of a package's current state in a closure, from its checkers under
//! each target it declares, with the files its attestation hashes.

use std::collections::{BTreeMap, BTreeSet};

use super::db::{ItemId, PackageId};
use super::requires::Entry as Required;
use super::sig::SigKind;
use super::snapshot::Items;
use super::{Checker, check_targets, unchecked};
use crate::artefact::{self, AttestedSeal, Parts};
use crate::cache::Cache;
use crate::encoding::{Record, bytes, list, uvar};
use crate::hash::Hash;
use crate::syntax::ast::ItemKind;
use crate::workspace::{Closure, Member, Package, Target, Workspace};

/// The artefact of a package's current state and the source and machine files its attestation
/// hashes, by path within the package.
pub struct Built {
    pub artefact: Vec<u8>,
    pub sources: Vec<(String, Vec<u8>)>,
}

/// What checking a package gives its artefact: its code root, and the seals its attestation
/// records as `artefact::seal_list` encodes them.
struct Coded {
    code_root: Vec<u8>,
    seals: Vec<u8>,
}

/// The artefact of the current state of a closure's member. Its code root and seals are kept in
/// the build cache, under the toolchain, the member's name and the states of the closure's
/// members.
pub fn build(workspace: &Workspace, closure: &Closure, member: Member) -> Built {
    let name = workspace.member(member).name().unwrap_or_default();
    let mut key = format!("coded\n{}\n{name}\n", crate::toolchain::hash(1).full());
    for (name, held) in &closure.members {
        let state = match held {
            Member::Source(at) => coded_state(&workspace.packages[*at]),
            Member::Release(at) => workspace.releases[*at].root,
        };
        key.push_str(&format!("{name} {}\n", state.full()));
    }
    let key = Hash::of(key.as_bytes());
    let cache = Cache::open().ok().and_then(Result::ok);
    let held = cache
        .as_ref()
        .and_then(|cache| cache.entry(&key).ok()?.ok()?)
        .and_then(|held| decoded(&held));
    let coded = match held {
        Some(coded) => coded,
        None => {
            let coded = coded(workspace, closure, member);
            if let Some(cache) = &cache {
                drop(cache.put(&key, &encoded(&coded)));
            }
            coded
        }
    };
    assemble(workspace.member(member), &coded)
}

/// The state of a workspace package its code root and seals depend on: the hash of its files but
/// its lock, which its closure's members stand for.
fn coded_state(package: &Package) -> Hash {
    let files: Vec<(String, Vec<u8>)> = package_files(package)
        .into_iter()
        .filter(|(path, _)| path != ".nan/lock.nan")
        .collect();
    artefact::source_tree_hash(&files)
}

fn encoded(coded: &Coded) -> Vec<u8> {
    let mut out = Vec::new();
    bytes(&mut out, &coded.code_root);
    bytes(&mut out, &coded.seals);
    out
}

fn decoded(held: &[u8]) -> Option<Coded> {
    let (code_root, rest) = artefact::read_bytes(held)?;
    let (seals, rest) = artefact::read_bytes(rest)?;
    rest.is_empty().then(|| Coded {
        code_root: code_root.to_vec(),
        seals: seals.to_vec(),
    })
}

/// The artefact of a package's current state from its code root and seals.
fn assemble(package: &Package, coded: &Coded) -> Built {
    let sources = package_files(package);
    let attestation = artefact::attestation(
        crate::toolchain::hash(1),
        artefact::source_tree_hash(&sources),
        &coded.seals,
    );
    Built {
        artefact: artefact::artefact(&coded.code_root, &attestation),
        sources,
    }
}

/// The canonical NIR stream of each workspace package's current state, by package name, as its
/// code root holds it.
pub fn nir_streams(workspace: &Workspace) -> Vec<(String, Vec<u8>)> {
    let std = crate::stdlib::packages();
    let mut streams = Vec::new();
    for package in &workspace.packages {
        let (Some(closure), Some(name)) = (workspace.closure_of(package), package.name()) else {
            continue;
        };
        let checkers: Vec<(Target, Checker)> = check_targets(package)
            .into_iter()
            .map(|target| {
                let checker = unchecked(workspace, closure, std, target);
                if let Some(id) = checker.package_named(name) {
                    checker.check_whole(id);
                }
                (target, checker)
            })
            .collect();
        let views: Vec<(Target, &Checker)> =
            checkers.iter().map(|(target, ck)| (*target, ck)).collect();
        streams.push((name.to_owned(), super::units::stream(&views, name).bytes));
    }
    streams
}

/// The code root and seals of the current state of a closure's member.
fn coded(workspace: &Workspace, closure: &Closure, member: Member) -> Coded {
    let std = crate::stdlib::packages();
    let package = workspace.member(member);
    let name = package.name().unwrap_or_default().to_owned();
    let checkers: Vec<(Target, Checker)> = check_targets(package)
        .into_iter()
        .map(|target| {
            let checker = unchecked(workspace, closure, std, target);
            checker.db.record();
            if let Some(id) = checker.package_named(&name) {
                checker.check_whole(id);
            }
            (target, checker)
        })
        .collect();
    let views: Vec<(Target, &Checker)> =
        checkers.iter().map(|(target, ck)| (*target, ck)).collect();
    let items = Items::of(&views, package);
    let computed = super::computed_snapshots(workspace, package, &items, &[]);
    let interface: Vec<(String, String)> = computed
        .modules
        .iter()
        .map(|(module, snapshot)| {
            let text = snapshot.as_ref().map_or_else(String::new, |snapshot| {
                let mut text = String::from("nan api 1\n");
                for entry in &snapshot.entries {
                    let lines: Vec<String> = entry
                        .lines()
                        .into_iter()
                        .filter(|line| {
                            !line.starts_with("seal ")
                                && !line.starts_with("closure ")
                                && !line.starts_with("signature ")
                        })
                        .collect();
                    if lines.len() > 1 {
                        for line in lines {
                            text.push_str(&line);
                            text.push('\n');
                        }
                    }
                }
                text
            });
            (module.clone(), text)
        })
        .collect();
    let stream = super::units::stream(&views, &name);
    let epoch = |path: &str| epoch_of(workspace, closure, path);
    let mut requires: BTreeMap<String, Required> = BTreeMap::new();
    for (_, checker) in &checkers {
        let Some(id) = checker.package_named(&name) else {
            continue;
        };
        for entry in checker.requires_set(id, &epoch) {
            match requires.get_mut(&entry.path) {
                Some(known) => {
                    known.roles.extend(entry.roles);
                    known.relied.extend(entry.relied);
                    known.methods.extend(entry.methods);
                    known.implementers.extend(entry.implementers);
                    known.item_hash = known.item_hash.or(entry.item_hash);
                }
                None => {
                    requires.insert(entry.path.clone(), entry);
                }
            }
        }
    }
    let first = &checkers[0].1;
    let id = first.package_named(&name).expect("the built package");
    let source_tombstones = first.tombstone_forms(id, false);
    let history_tombstones = first.tombstone_forms(id, true);
    let ledger = first
        .package_ledger(id)
        .iter()
        .map(|entry| {
            let text = |value: &str| {
                let mut out = Vec::new();
                bytes(&mut out, value.as_bytes());
                out
            };
            let number = |value: u64| {
                let mut out = Vec::new();
                uvar(&mut out, u128::from(value));
                out
            };
            let mut texts = Vec::new();
            list(&mut texts, entry.texts.iter(), |out, text| {
                bytes(out, text.as_bytes())
            });
            let mut record = Record::default();
            record.field(1, text(&entry.item));
            record.field(2, number(entry.kind.position()));
            record.field(3, text(&span_path(package, &name, &entry.path)));
            record.field(4, number(u64::from(entry.line)));
            record.field(5, texts);
            record.finish()
        })
        .collect();
    let lines = stream
        .lines
        .iter()
        .map(|(path, held)| {
            let within = path.split_once('/').map_or("", |(_, within)| within);
            let file = package
                .files
                .iter()
                .find(|file| file.path == format!("{}/{within}", package.dir));
            let texts = held
                .iter()
                .map(|line| {
                    let text = file.map_or_else(String::new, |file| {
                        String::from_utf8_lossy(file.syntax.line_text(*line)).into_owned()
                    });
                    (*line, text)
                })
                .collect();
            (path.clone(), texts)
        })
        .collect();
    let parts = Parts {
        language: 1,
        manifest: package.files.first().map_or_else(String::new, |file| {
            String::from_utf8_lossy(&crate::format::source_file(&file.syntax, None)).into_owned()
        }),
        interface,
        nir: stream.bytes,
        requires: requires.values().map(Required::encoded).collect(),
        source_tombstones,
        history_tombstones,
        ledger,
        lines,
        schemas: first.state_schemas(id),
    };
    let code_root = artefact::code_root(&parts);
    let seals: Vec<AttestedSeal> = computed
        .modules
        .iter()
        .filter_map(|(_, snapshot)| snapshot.as_ref())
        .flat_map(|snapshot| &snapshot.entries)
        .flat_map(|entry| {
            entry.seals.iter().map(|seal| AttestedSeal {
                principal: seal.principal.clone(),
                item: entry.path.clone(),
                sealed: seal.item,
                closure: seal.closure,
                entries: seal
                    .entries
                    .iter()
                    .map(|closure| (closure.body, closure.path.clone(), closure.hash))
                    .collect(),
                signed: seal
                    .signature
                    .zip(items.principals.key(&seal.principal).copied()),
            })
        })
        .collect();
    Coded {
        code_root,
        seals: artefact::seal_list(&seals),
    }
}

/// The behaviour epoch of a foreign item in the release a closure reads: from the publish
/// summary of a release of the store, 0 otherwise.
fn epoch_of(workspace: &Workspace, closure: &Closure, path: &str) -> u64 {
    let package = path.split("::").next().unwrap_or_default();
    match closure.members.get(package) {
        Some(Member::Release(index)) => workspace.releases[*index]
            .entry
            .summary
            .as_ref()
            .map_or(0, |summary| summary.epoch(path)),
        _ => 0,
    }
}

/// A file's path as spans write it: the package name, then its path within the package.
fn span_path(package: &Package, name: &str, path: &str) -> String {
    let within = path
        .strip_prefix(&format!("{}/", package.dir))
        .unwrap_or(path);
    format!("{name}/{within}")
}

/// A package's source and machine files, by path within the package.
pub fn package_files(package: &Package) -> Vec<(String, Vec<u8>)> {
    let prefix = format!("{}/", package.dir);
    let mut files: Vec<(String, Vec<u8>)> = package
        .files
        .iter()
        .filter_map(|file| {
            let within = file.path.strip_prefix(&prefix)?;
            Some((within.to_owned(), file.syntax.source.clone()))
        })
        .collect();
    for (path, content) in package.machine.files() {
        if let Some(within) = path.strip_prefix(&prefix) {
            files.push((within.to_owned(), content.clone()));
        }
    }
    files.sort();
    files
}

impl Checker<'_> {
    /// Checks a package of the checker's closure, quiet or not, and the rules relating its items.
    pub(super) fn check_whole(&self, package: PackageId) {
        self.package.set(Some(package));
        self.check_package(package);
        self.fix_misnamed();
        self.reject_items();
        for &module in &self.db.package(package).modules {
            for &item in &self.db.module(module).items {
                self.check_elaborated(item);
            }
        }
        self.check_tables(package);
        self.check_wheres();
        self.check_const_cycles();
    }

    /// The normalised forms of the tombstones in a package's history, or else in its source.
    fn tombstone_forms(&self, package: PackageId, history: bool) -> Vec<String> {
        self.db
            .package(package)
            .modules
            .iter()
            .flat_map(|module| self.db.module(*module).items.clone())
            .filter(|item| matches!(self.db.ast(*item).kind, ItemKind::Retired(_)))
            .filter(|item| self.db.in_history(*item) == history)
            .map(|item| self.forms(item).normalised)
            .collect()
    }

    /// Each component state a package declares, the target of an impl line naming
    /// `sys::Component`, with its canonical path and schema hash.
    fn state_schemas(&self, package: PackageId) -> Vec<(String, Hash)> {
        let mut found: BTreeSet<(String, ItemId)> = BTreeSet::new();
        for module in &self.db.package(package).modules {
            for item in &self.db.module(*module).items {
                if let SigKind::Impl {
                    target: Some(super::db::TypeKey::Item(target)),
                    interface: Some(bound),
                    ..
                } = &self.sig(*item).kind
                    && self.db.canonical(bound.interface) == "sys::Component"
                {
                    found.insert((self.db.canonical(*target), *target));
                }
            }
        }
        found
            .into_iter()
            .map(|(path, target)| {
                (
                    path,
                    self.schema(&super::ty::Ty::item(target, Vec::new())).hash(),
                )
            })
            .collect()
    }
}
