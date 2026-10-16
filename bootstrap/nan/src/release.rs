//! Releases compared: the exported items of a release from its code root's interface, and each
//! item's class relative to the release's parent.

use std::collections::BTreeMap;

use crate::artefact::{self, read_bytes, read_uvar};
use crate::hash::Hash;
use crate::machine::api::{Entry, Snapshot};
use crate::store::{Class, Summary};

/// The exported items of a release, by canonical path: the entries its interface records with an
/// interface hash.
pub fn exported(artefact: &[u8]) -> BTreeMap<String, Entry> {
    let Ok(decoded) = artefact::decode(artefact) else {
        return BTreeMap::new();
    };
    let mut found = BTreeMap::new();
    for (_, text) in interface(&decoded.code_root) {
        if text.is_empty() {
            continue;
        }
        let check = Hash::of(text.as_bytes()).full();
        let file = format!("{text}check \"{check}\"\n");
        if let Ok(snapshot) = Snapshot::read(file.as_bytes()) {
            for entry in snapshot.entries {
                if entry.interface.is_some() {
                    found.insert(entry.path.clone(), entry);
                }
            }
        }
    }
    found
}

/// How a `preserves` tombstone of a release forwards its old form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Forward {
    /// Through its forwarding function, which has the old head.
    Head,
    /// To what this canonical path names.
    Path(String),
}

/// The old forms that the `preserves` tombstones of a release's code root forward, from their
/// normalised forms, by canonical path.
pub fn forwards(artefact: &[u8]) -> BTreeMap<String, Forward> {
    let Ok(decoded) = artefact::decode(artefact) else {
        return BTreeMap::new();
    };
    let mut forwards = BTreeMap::new();
    for key in [6, 7] {
        for form in part(&decoded.code_root, key)
            .iter()
            .flat_map(|part| texts(part))
        {
            let Some(rest) = form.trim_end().strip_prefix("retired ") else {
                continue;
            };
            let Some(rewrite) = rest.strip_suffix(" preserves") else {
                continue;
            };
            let Some((old, new)) = rewrite.split_once(" => ") else {
                continue;
            };
            let forward = match old.split_once('(') {
                Some(_) => Forward::Head,
                None => Forward::Path(new.to_owned()),
            };
            let path = old.split_once('(').map_or(old, |(path, _)| path);
            forwards.insert(path.to_owned(), forward);
        }
    }
    forwards
}

/// The content of a code root's part by its key.
fn part(code_root: &[u8], wanted: u128) -> Option<&[u8]> {
    let (count, mut rest) = read_uvar(code_root)?;
    for _ in 0..count {
        let (key, after) = read_uvar(rest)?;
        let (content, after) = read_bytes(after)?;
        if key == wanted {
            return Some(content);
        }
        rest = after;
    }
    None
}

/// The elements of a list of texts.
fn texts(list: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    let Some((count, mut rest)) = read_uvar(list) else {
        return found;
    };
    for _ in 0..count {
        let Some((text, after)) = read_bytes(rest) else {
            return found;
        };
        rest = after;
        found.push(String::from_utf8_lossy(text).into_owned());
    }
    found
}

/// The module names and snapshot texts of a code root's interface part.
fn interface(code_root: &[u8]) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let Some((pairs, mut held)) = part(code_root, 3).and_then(read_uvar) else {
        return found;
    };
    for _ in 0..pairs {
        let Some((module, after)) = read_bytes(held) else {
            return found;
        };
        let Some((text, after)) = read_bytes(after) else {
            return found;
        };
        held = after;
        found.push((
            String::from_utf8_lossy(module).into_owned(),
            String::from_utf8_lossy(text).into_owned(),
        ));
    }
    found
}

/// The facts a digest line states: its effects and authority entries, may-trap and terminates.
struct Facts<'a> {
    entries: Vec<&'a str>,
    may_trap: bool,
    terminates: bool,
}

/// The words of a digest line's part after its name, `none` left out.
fn words<'a>(part: Option<&&'a str>, prefix: &str) -> Vec<&'a str> {
    part.and_then(|part| part.strip_prefix(prefix))
        .map(|rest| rest.split(' ').filter(|word| *word != "none").collect())
        .unwrap_or_default()
}

fn facts(line: &str) -> Facts<'_> {
    let parts: Vec<&str> = line.split(" | ").collect();
    let mut entries = words(parts.first(), "effects ");
    entries.extend(words(parts.get(1), "authority "));
    Facts {
        entries,
        may_trap: parts.get(2) == Some(&"may-trap yes"),
        terminates: parts.get(3) == Some(&"terminates"),
    }
}

/// Whether a digest gains a fact or set element over another, or loses terminates.
fn widens(old: &str, new: &str) -> bool {
    let (old, new) = (facts(old), facts(new));
    let unbounded = new.entries.contains(&"any") && !old.entries.contains(&"any");
    unbounded
        || new.entries.iter().any(|entry| !old.entries.contains(entry))
        || (new.may_trap && !old.may_trap)
        || (old.terminates && !new.terminates)
}

/// Whether an item's replacement, read as if it had the item's name, keeps its interface text and
/// promises and gains no digest fact.
pub fn replaces(old: &Entry, new: &Entry, old_path: &str, new_path: &str) -> bool {
    let name = |path: &str| path.rsplit(['.', ':']).next().unwrap_or(path).to_owned();
    let (old_name, new_name) = (name(old_path), name(new_path));
    let renamed: Vec<String> = new
        .text
        .iter()
        .map(|line| {
            line.split_inclusive(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .map(|word| match word.strip_prefix(new_name.as_str()) {
                    Some(rest) if rest.chars().all(|c| !c.is_ascii_alphanumeric() && c != '_') => {
                        format!("{old_name}{rest}")
                    }
                    _ => word.to_owned(),
                })
                .collect()
        })
        .collect();
    renamed == old.text
        && new.promises == old.promises
        && new.digests.iter().all(|(target, digest)| {
            old.digests
                .iter()
                .find(|(known, _)| known == target)
                .is_some_and(|(_, before)| !widens(before, digest))
        })
}

/// The class of each exported item of a release relative to its parent's, by canonical path,
/// removed items included: identical when its interface hash, epoch and digests are unchanged,
/// additive when it is new, when only its digests change without gaining a fact, or when it is
/// removed while a `preserves` tombstone forwards it to a replacement that keeps its interface,
/// and breaking otherwise.
pub fn classes(
    parent: &BTreeMap<String, Entry>,
    release: &BTreeMap<String, Entry>,
    epochs: &dyn Fn(&str) -> (u64, u64),
    forwards: &BTreeMap<String, Forward>,
) -> BTreeMap<String, Class> {
    let mut classes = BTreeMap::new();
    for (path, entry) in release {
        let class = match parent.get(path) {
            None => Class::Additive,
            Some(old) => {
                let (old_epoch, new_epoch) = epochs(path);
                if old.interface != entry.interface || old_epoch != new_epoch {
                    Class::Breaking
                } else if old.digests == entry.digests {
                    Class::Identical
                } else {
                    let widened = entry.digests.iter().any(|(target, digest)| {
                        old.digests
                            .iter()
                            .find(|(known, _)| known == target)
                            .is_none_or(|(_, before)| widens(before, digest))
                    });
                    if widened {
                        Class::Breaking
                    } else {
                        Class::Additive
                    }
                }
            }
        };
        classes.insert(path.clone(), class);
    }
    for (path, old) in parent {
        if release.contains_key(path) {
            continue;
        }
        let kept = match forwards.get(path) {
            Some(Forward::Head) => true,
            Some(Forward::Path(new)) => release
                .get(new)
                .is_some_and(|entry| replaces(old, entry, path, new)),
            None => false,
        };
        let class = if kept {
            Class::Additive
        } else {
            Class::Breaking
        };
        classes.insert(path.clone(), class);
    }
    classes
}

/// The class of a release: the worst of its items' classes, additive without a parent.
pub fn release_class(classes: &BTreeMap<String, Class>, parented: bool) -> Class {
    if !parented {
        return Class::Additive;
    }
    classes.values().copied().max().unwrap_or(Class::Identical)
}

/// The epoch an item has in a release: its summary's, 0 without one.
pub fn epoch(summary: Option<&Summary>, path: &str) -> u64 {
    summary.map_or(0, |summary| summary.epoch(path))
}

#[cfg(test)]
mod tests;
