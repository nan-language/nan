//! Requires-sets: what a package's acceptance relies on in other packages, one entry per foreign
//! item a rule consults while checking it, from the reads its checker recorded.

use std::collections::{BTreeMap, BTreeSet};

use super::Checker;
use super::Member;
use super::db::{ItemId, PackageId};
use super::sig::SigKind;
use super::ty::{Arg, Ty};
use super::units::ItemNames;
use crate::encoding::{Record, bytes, list, uvar};
use crate::hash::Hash;
use crate::nir::encode::Encoder;
use crate::nir::{Callable, Callee, FnRef, Op, Terminator};
use crate::syntax::ast::{FnBody, ItemKind, TypeModifier};

/// A way of using an item, in the role table's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Call,
    Build,
    Implement,
    Dispatch,
    Name,
}

/// An entry of a requires-set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub roles: BTreeSet<Role>,
    pub interface: Hash,
    pub epoch: u64,
    /// The digest facts relied on: each field's key in the digest encoding and the path of a
    /// set's capability or validator, empty for a field that is not a set.
    pub relied: BTreeSet<(u64, String)>,
    /// Each method name resolved on the item as a type, with the function reference found as
    /// the NIR encoding writes it.
    pub methods: BTreeMap<String, Vec<u8>>,
    pub item_hash: Option<Hash>,
    /// For an interface the package implements, the names of the functions named after an impl
    /// line's target in its module that are not its methods.
    pub implementers: BTreeSet<String>,
}

impl Entry {
    /// The entry as a code root's requires-set records it.
    pub fn encoded(&self) -> Vec<u8> {
        let mut record = Record::default();
        let content = |write: &dyn Fn(&mut Vec<u8>)| {
            let mut out = Vec::new();
            write(&mut out);
            out
        };
        record.field(1, content(&|out| bytes(out, self.path.as_bytes())));
        record.field(
            3,
            content(&|out| list(out, self.roles.iter(), |out, role| uvar(out, *role as u128))),
        );
        record.field(4, content(&|out| bytes(out, &self.interface.0)));
        record.field(5, content(&|out| uvar(out, u128::from(self.epoch))));
        if !self.relied.is_empty() {
            record.field(
                6,
                content(&|out| {
                    list(out, self.relied.iter(), |out, (key, path)| {
                        uvar(out, u128::from(*key));
                        bytes(out, path.as_bytes());
                    });
                }),
            );
        }
        if !self.methods.is_empty() {
            record.field(
                7,
                content(&|out| {
                    list(out, self.methods.iter(), |out, (name, reference)| {
                        bytes(out, name.as_bytes());
                        out.extend(reference);
                    });
                }),
            );
        }
        if let Some(hash) = &self.item_hash {
            record.field(8, content(&|out| bytes(out, &hash.0)));
        }
        if !self.implementers.is_empty() {
            record.field(
                9,
                content(&|out| {
                    list(out, self.implementers.iter(), |out, name| {
                        bytes(out, name.as_bytes())
                    });
                }),
            );
        }
        record.finish()
    }
}

/// The digest fields a limit demands the absence of, by their key in the digest encoding:
/// wait 1, alloc 2, device 3, priv 4, sync 5, may-trap 6; terminates, key 7, is relied on present.
const PURE: [u64; 5] = [1, 2, 3, 4, 5];

/// Records the entry of an item in a requires-set, giving its canonical path.
type Recording<'r> = dyn FnMut(ItemId, &mut BTreeMap<String, Entry>) -> Option<String> + 'r;

impl Checker<'_> {
    /// The requires-set of a package from what its checker recorded checking it, each entry's
    /// epoch given by `epoch`, in byte order of the entries' encodings.
    pub fn requires_set(&self, package: PackageId, epoch: &dyn Fn(&str) -> u64) -> Vec<Entry> {
        let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
        let mut foreign: Vec<ItemId> = Vec::new();
        let own: Vec<ItemId> = self
            .db
            .package(package)
            .modules
            .iter()
            .flat_map(|module| self.db.module(*module).items.iter().copied())
            .collect();
        for &item in &own {
            for read in self.db.read_by(item) {
                if self.db.package_of(read) != package && !foreign.contains(&read) {
                    foreign.push(read);
                }
            }
        }
        let mut entry = |item: ItemId, entries: &mut BTreeMap<String, Entry>| -> Option<String> {
            let path = self.canonical_path(item)?;
            if !entries.contains_key(&path) {
                let hashes = self.hashes(item);
                entries.insert(
                    path.clone(),
                    Entry {
                        path: path.clone(),
                        roles: BTreeSet::new(),
                        interface: hashes.interface.unwrap_or(hashes.item),
                        epoch: epoch(&path),
                        relied: BTreeSet::new(),
                        methods: BTreeMap::new(),
                        item_hash: None,
                        implementers: BTreeSet::new(),
                    },
                );
            }
            Some(path)
        };
        for &item in &foreign {
            if let Some(path) = entry(item, &mut entries) {
                entries
                    .get_mut(&path)
                    .expect("an entry")
                    .roles
                    .insert(Role::Name);
            }
        }
        for &item in &own {
            self.record_uses(item, package, &mut |used, role, instance| {
                if let Some(path) = entry(used, &mut entries) {
                    let found = entries.get_mut(&path).expect("an entry");
                    found.roles.insert(role);
                    if instance {
                        found.item_hash = Some(self.hashes(used).item);
                    }
                }
            });
            for (callee, relied) in self.relied_facts(item, package) {
                if let Some(path) = entry(callee, &mut entries) {
                    entries
                        .get_mut(&path)
                        .expect("an entry")
                        .relied
                        .extend(relied);
                }
            }
            self.record_implements(item, package, &mut entries, &mut entry);
        }
        for (named, _, member) in self.members_named.borrow().iter() {
            let (Member::Function(function), true) = (member, own.contains(named)) else {
                continue;
            };
            let Some(owner) = self.db.owners.get(function).copied() else {
                continue;
            };
            let super::db::TypeKey::Item(owner) = owner else {
                continue;
            };
            if self.db.package_of(owner) == package {
                continue;
            }
            if let Some(path) = entry(owner, &mut entries) {
                let name = self
                    .db
                    .canonical(*function)
                    .rsplit_once('.')
                    .map(|(_, name)| name.to_owned())
                    .unwrap_or_default();
                let names = ItemNames::new(self, *named);
                let mut reference = Vec::new();
                Encoder { names: &names }.fn_ref(
                    &mut reference,
                    &FnRef::item(*function, super::ty::no_args()),
                );
                entries
                    .get_mut(&path)
                    .expect("an entry")
                    .methods
                    .insert(name, reference);
            }
        }
        let mut found: Vec<Entry> = entries.into_values().collect();
        found.sort_by_key(Entry::encoded);
        found
    }

    /// Calls `used` with each foreign item an item's unit uses in a role other than `name`, and
    /// whether it instantiates the item or unfolds its body.
    fn record_uses(
        &self,
        item: ItemId,
        package: PackageId,
        used: &mut dyn FnMut(ItemId, Role, bool),
    ) {
        let Some(unit) = self.unit(item) else {
            return;
        };
        let foreign = |target: ItemId| self.db.package_of(target) != package;
        let callable =
            |callable: &Callable, args: &[Arg], used: &mut dyn FnMut(ItemId, Role, bool)| {
                match callable {
                    Callable::Item { item: callee, .. } if foreign(*callee) => {
                        let unfolded = matches!(&self.db.ast(*callee).kind,
                        ItemKind::Fn(function) if matches!(function.body, FnBody::Expr(_)))
                            && self.unfolds(*callee, item);
                        used(*callee, Role::Call, !args.is_empty() || unfolded);
                    }
                    Callable::Method { interface, .. } if foreign(*interface) => {
                        used(*interface, Role::Dispatch, false);
                    }
                    _ => {}
                }
            };
        for function in &unit.functions {
            for block in &function.blocks {
                for inst in &block.insts {
                    match &inst.op {
                        Op::FnRef(reference) => {
                            callable(&reference.callable, &reference.args, used)
                        }
                        Op::Call(call) => match &call.callee {
                            Callee::Fn(reference) => {
                                callable(&reference.callable, &reference.args, used);
                            }
                            Callee::Dyn { interface, .. } if foreign(*interface) => {
                                used(*interface, Role::Dispatch, false);
                            }
                            _ => {}
                        },
                        Op::Construct {
                            ty: Ty::Item(built, args),
                            ..
                        } if foreign(*built) && self.closed(*built) => {
                            used(*built, Role::Build, !args.is_empty());
                        }
                        _ => {}
                    }
                }
                if let Terminator::Call { call, .. } = &block.term
                    && let Callee::Fn(reference) = &call.callee
                {
                    callable(&reference.callable, &reference.args, used);
                }
            }
        }
    }

    /// Whether a struct or enum is closed.
    fn closed(&self, item: ItemId) -> bool {
        self.db.ast(item).has_modifier(TypeModifier::Closed)
    }

    /// The digest facts an item's limits rely on in each foreign function its body calls: the
    /// absence of what `pure`, `noalloc` and `notrap` exclude, and terminates.
    fn relied_facts(&self, item: ItemId, package: PackageId) -> Vec<(ItemId, Vec<(u64, String)>)> {
        let Some(head) = self.fn_sig(item) else {
            return Vec::new();
        };
        let limits = head.limits;
        let mut relied: Vec<(u64, String)> = Vec::new();
        if limits.pure {
            relied.extend(PURE.iter().map(|key| (*key, String::new())));
        } else if limits.noalloc {
            relied.push((2, String::new()));
        }
        if limits.notrap {
            relied.push((6, String::new()));
        }
        if limits.terminates {
            relied.push((7, String::new()));
        }
        if relied.is_empty() {
            return Vec::new();
        }
        let Some(unit) = self.unit(item) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for function in &unit.functions {
            for block in &function.blocks {
                let calls = block.insts.iter().filter_map(|inst| match &inst.op {
                    Op::Call(call) => Some(call),
                    _ => None,
                });
                let term = match &block.term {
                    Terminator::Call { call, .. } => Some(call),
                    _ => None,
                };
                for call in calls.chain(term) {
                    if let Callee::Fn(FnRef {
                        callable: Callable::Item { item: callee, .. },
                        ..
                    }) = &call.callee
                        && self.db.package_of(*callee) != package
                    {
                        found.push((*callee, relied.clone()));
                    }
                }
            }
        }
        found
    }

    /// For an impl line naming a foreign interface: the `implement` role and the names of the
    /// functions named after its target in its module that are not methods of the interface.
    fn record_implements(
        &self,
        item: ItemId,
        package: PackageId,
        entries: &mut BTreeMap<String, Entry>,
        entry: &mut Recording,
    ) {
        let SigKind::Impl {
            target: Some(target),
            interface: Some(bound),
            ..
        } = &self.sig(item).kind
        else {
            return;
        };
        if self.db.package_of(bound.interface) == package {
            return;
        }
        let Some(path) = entry(bound.interface, entries) else {
            return;
        };
        let methods: Vec<String> = match &self.sig(bound.interface).kind {
            SigKind::Interface { methods } => {
                methods.iter().map(|method| method.name.clone()).collect()
            }
            _ => Vec::new(),
        };
        let module = self.db.module_of(item);
        let implementers: Vec<String> = self
            .db
            .members
            .get(target)
            .into_iter()
            .flatten()
            .filter(|(name, functions)| {
                !methods.contains(name)
                    && functions
                        .iter()
                        .any(|function| self.db.module_of(*function) == module)
            })
            .map(|(name, _)| name.clone())
            .collect();
        let found = entries.get_mut(&path).expect("an entry");
        found.roles.insert(Role::Implement);
        found.implementers.extend(implementers);
    }
}
