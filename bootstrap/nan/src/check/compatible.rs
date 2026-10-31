//! Releases compared at publishing: whether each change a release makes to the interface form of
//! an exported item of its parent is one the compatible changes list for every role the item has,
//! and the encode class, from the schema trees of the exported types implementing `Encode`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::db::ItemId;
use super::entail::{self, Weakened};
use super::reversibility;
use super::schema::Node;
use super::sig::SigKind;
use super::synthetic::{self, Synthetic};
use super::{Checker, check_targets, checked};
use crate::machine::api::{Accepted, Entry};
use crate::store::{self, Class};
use crate::workspace::Workspace;

/// What comparing an item's interface texts leaves to judge: nothing for a breaking change, or
/// the contract and the examples of a function whose head is compatible.
enum Judging {
    Breaking,
    Compatible,
    Function {
        contract: Option<Accepted>,
        examples: Vec<String>,
    },
}

/// The canonical paths of the exported items of the release that the workspace package at
/// `index` is, whose interface form differs from their parent's only by compatible changes,
/// `parent` and `release` giving the two releases' exported entries.
pub fn compatible(
    workspace: &Workspace,
    index: usize,
    parent: &BTreeMap<String, Entry>,
    release: &BTreeMap<String, Entry>,
) -> BTreeSet<String> {
    let package = &workspace.packages[index];
    let (Some(closure), Some(&target)) = (
        workspace.closure_of(package),
        check_targets(package).first(),
    ) else {
        return BTreeSet::new();
    };
    let checker = checked(workspace, closure, crate::stdlib::packages(), target);
    let items: HashMap<String, ItemId> = (0..checker.db.items.len())
        .map(|index| ItemId(index as u32))
        .filter(|item| checker.db.is_reported(*item) && !checker.db.in_history(*item))
        .filter_map(|item| Some((checker.canonical_path(item)?, item)))
        .collect();
    let mut compatible = BTreeSet::new();
    let mut contracts: Vec<(String, Weakened)> = Vec::new();
    let mut examples: Vec<(String, Synthetic)> = Vec::new();
    for (path, new) in release {
        let (Some(old), Some(&item)) = (parent.get(path), items.get(path)) else {
            continue;
        };
        if old.interface == new.interface {
            continue;
        }
        match judging(&checker, item, old, new) {
            Judging::Breaking => continue,
            Judging::Compatible => {
                compatible.insert(path.clone());
            }
            Judging::Function {
                contract,
                examples: old_examples,
            } => {
                if let Some(accepted) = contract {
                    let weakened = Weakened {
                        ck: &checker,
                        item,
                        accepted,
                        later: None,
                    };
                    contracts.push((path.clone(), weakened));
                }
                if !old_examples.is_empty() {
                    let number = examples.len();
                    examples.push((
                        path.clone(),
                        examples_function(&checker, item, &old_examples, number),
                    ));
                }
                compatible.insert(path.clone());
            }
        }
    }
    let weakened: Vec<Weakened> = contracts
        .iter()
        .map(|(_, weakened)| weakened.clone())
        .collect();
    for ((path, _), left) in contracts
        .iter()
        .zip(entail::left(workspace, index, &weakened))
    {
        if !left.is_empty() {
            compatible.remove(path);
        }
    }
    let functions: Vec<Synthetic> = examples
        .iter()
        .map(|(_, function)| function.clone())
        .collect();
    for ((path, _), judged) in examples
        .iter()
        .zip(synthetic::judge(workspace, index, &functions))
    {
        if judged.is_none_or(|judged| judged.failed) {
            compatible.remove(path);
        }
    }
    compatible
}

/// What an item's old and new interface texts leave to judge.
fn judging(ck: &Checker, item: ItemId, old: &Entry, new: &Entry) -> Judging {
    let (Some(old_head), Some(new_head)) = (old.text.first(), new.text.first()) else {
        return Judging::Breaking;
    };
    let promised = old
        .promises
        .iter()
        .all(|promise| new.promises.contains(promise));
    if !promised {
        return Judging::Breaking;
    }
    if old.text == new.text {
        return Judging::Compatible;
    }
    let held = |lines: &[String]| lines.iter().all(|line| new.text.contains(line));
    match &ck.sig(item).kind {
        SigKind::Fn(_) => {
            let (old_core, old_limits, old_class) = limits(old_head);
            let (new_core, new_limits, new_class) = limits(new_head);
            let added: Vec<&&str> = new_limits
                .iter()
                .filter(|limit| !old_limits.contains(limit))
                .collect();
            let kept = old_limits.iter().all(|limit| new_limits.contains(limit));
            let class_kept = match (old_class, new_class) {
                (Some(old), Some(new)) => new.at_most(old),
                (Some(_), None) => false,
                (None, _) => true,
            };
            if old_core != new_core
                || !kept
                || !class_kept
                || added.iter().any(|limit| limit.starts_with("stack "))
            {
                return Judging::Breaking;
            }
            let clauses = |entry: &Entry, keyword: &str| -> Vec<String> {
                entry.text[1..]
                    .iter()
                    .map(|line| line.trim().to_owned())
                    .filter(|line| line.starts_with(keyword))
                    .collect()
            };
            let mut old_contract = clauses(old, "requires ");
            old_contract.extend(clauses(old, "ensures "));
            let mut new_contract = clauses(new, "requires ");
            new_contract.extend(clauses(new, "ensures "));
            let contract = (old_contract != new_contract).then(|| Accepted {
                head: Some(old_core.to_owned()),
                clauses: old_contract,
                ..Accepted::default()
            });
            let new_examples = clauses(new, "example ");
            let examples = clauses(old, "example ")
                .into_iter()
                .filter(|example| !new_examples.contains(example))
                .collect();
            Judging::Function { contract, examples }
        }
        SigKind::Enum { .. } if opened(old_head, new_head) && held(&old.text) => {
            Judging::Compatible
        }
        SigKind::Struct { fields } if opened(old_head, new_head) && held(&old.text) => {
            let encoded = new_head.contains("Encode");
            let default = ck.db.core_item("Default");
            let defaulted = new
                .text
                .iter()
                .filter(|line| !old.text.contains(line))
                .all(|line| {
                    let name = line
                        .trim()
                        .trim_start_matches("pub ")
                        .split(':')
                        .next()
                        .unwrap_or_default();
                    !encoded
                        || fields.iter().any(|field| {
                            field.name == name.trim()
                                && default.is_some_and(|default| ck.implements(&field.ty, default))
                        })
                });
            match defaulted {
                true => Judging::Compatible,
                false => Judging::Breaking,
            }
        }
        SigKind::Interface { .. } if old_head == new_head && held(&old.text) => {
            let defaulted = new.text[1..new.text.len() - 1]
                .iter()
                .filter(|line| !old.text.contains(line))
                .all(|line| line.ends_with(" default"));
            match defaulted && ck.dyn_usable(item) {
                true => Judging::Compatible,
                false => Judging::Breaking,
            }
        }
        _ => Judging::Breaking,
    }
}

/// Whether a struct or enum keeps its declaration line and is open.
fn opened(old: &str, new: &str) -> bool {
    old == new && new.contains(" open ")
}

/// A function's head line without its limits, its limits other than a class, and its class.
fn limits(head: &str) -> (&str, Vec<&str>, Option<reversibility::Class>) {
    let Some((core, mut rest)) = head.split_once(" limits ") else {
        return (head, Vec::new(), None);
    };
    let (mut limits, mut class) = (Vec::new(), None);
    while !rest.is_empty() {
        let end = match rest.starts_with("irreversible(") {
            true => rest.find(')').map_or(rest.len(), |close| close + 1),
            false => rest.find(", ").unwrap_or(rest.len()),
        };
        match reversibility::Class::parse(&rest[..end]) {
            Some(named) => class = Some(named),
            None => limits.push(&rest[..end]),
        }
        rest = rest[end..].strip_prefix(", ").unwrap_or(&rest[end..]);
    }
    (core, limits, class)
}

/// A function of an item's module holding an item's old examples, whose check runs them against
/// the release.
fn examples_function(ck: &Checker, item: ItemId, examples: &[String], number: usize) -> Synthetic {
    let name = format!("judged_examples_{number}");
    let mut text = format!("\nfn {name}() -> bool\n");
    for example in examples {
        text.push_str(&format!("    {example}\n"));
    }
    text.push_str(&format!("{{\n    return true\n}} {name}\n"));
    Synthetic::beside(ck, item, &name, text)
}

/// The encode class of the release the workspace package at `index` is against the parent its
/// store entry holds: the worst class of the exported types implementing `Encode` in either,
/// identical for an unchanged schema hash, additive for a new type or a new tree accepting the
/// old one, breaking otherwise.
pub fn encode_class(workspace: &Workspace, index: usize, parent: Option<&store::Entry>) -> Class {
    let Some(parent) = parent else {
        return Class::Additive;
    };
    let dir = &workspace.packages[index].dir;
    let files: Vec<(String, &[u8])> = parent
        .sources
        .iter()
        .map(|(path, content)| (format!("{dir}/{path}"), content.as_slice()))
        .collect();
    let held: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(path, content)| (path.as_str(), *content))
        .collect();
    let Ok(published) = Workspace::of_stored_files(workspace.root.clone(), &held) else {
        return Class::Breaking;
    };
    let (new, old) = (encoded(workspace, index), encoded(&published, 0));
    let mut class = Class::Identical;
    for (path, tree) in &new {
        class = class.max(match old.get(path) {
            None => Class::Additive,
            Some(old) if old.hash() == tree.hash() => Class::Identical,
            Some(old) if tree.accepts(old) => Class::Additive,
            Some(_) => Class::Breaking,
        });
    }
    if old.keys().any(|path| !new.contains_key(path)) {
        class = Class::Breaking;
    }
    class
}

/// The schema tree of each exported type implementing `Encode` of the workspace package at
/// `index`, by canonical path.
fn encoded(workspace: &Workspace, index: usize) -> BTreeMap<String, Node> {
    let package = &workspace.packages[index];
    let (Some(closure), Some(&target)) = (
        workspace.closure_of(package),
        check_targets(package).first(),
    ) else {
        return BTreeMap::new();
    };
    let checker = checked(workspace, closure, crate::stdlib::packages(), target);
    let Some(id) = package.name().and_then(|name| checker.package_named(name)) else {
        return BTreeMap::new();
    };
    checker
        .exported_encoded(id)
        .into_iter()
        .map(|(path, ty)| (path, checker.schema(&ty)))
        .collect()
}
