//! Functions a package's modules gain only to be checked, each judged by whether it checks without
//! error and by the statuses of its obligation sites.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use super::db::ItemId;
use super::{Checker, check_targets, checked};
use crate::diagnostic::Class;
use crate::prover::Obligated;
use crate::workspace::Workspace;

/// A function to judge: the directory of the module it joins, its canonical path and its text.
#[derive(Clone)]
pub struct Synthetic {
    pub dir: String,
    pub path: String,
    pub text: String,
}

impl Synthetic {
    /// A function of an item's module, named `name` there.
    pub fn beside(ck: &Checker, item: ItemId, name: &str, text: String) -> Synthetic {
        let data = ck.db.item(item);
        let module = ck.db.module(data.module);
        let package = &ck.db.package(module.package).name;
        Synthetic {
            dir: data
                .file
                .path
                .rsplit_once('/')
                .map_or("", |(dir, _)| dir)
                .to_owned(),
            path: match module.root {
                true => format!("{package}::{name}"),
                false => format!("{package}::{}::{name}", module.name),
            },
            text,
        }
    }
}

/// What checking a synthetic function gives: whether an error lies on its lines, and its
/// obligation sites with their statuses.
pub struct Judged {
    pub failed: bool,
    pub sites: Rc<Vec<Obligated>>,
}

/// Judges functions added to the modules of the workspace package at `index`, under the first
/// target it is checked for: none for a function the check does not find.
pub fn judge(workspace: &Workspace, index: usize, functions: &[Synthetic]) -> Vec<Option<Judged>> {
    let package = &workspace.packages[index];
    let Some(&target) = check_targets(package).first() else {
        return functions.iter().map(|_| None).collect();
    };
    let mut sources: BTreeMap<String, String> = BTreeMap::new();
    for function in functions {
        sources
            .entry(format!("{}/.judged.nan", function.dir))
            .or_insert_with(|| "//! Judged functions.\n".to_owned())
            .push_str(&function.text);
    }
    let changed: HashMap<String, Vec<u8>> = sources
        .into_iter()
        .map(|(path, text)| (path, text.into_bytes()))
        .collect();
    let judged = workspace.with_sources(&changed);
    let Some(closure) = judged.closure_of(&judged.packages[index]) else {
        return functions.iter().map(|_| None).collect();
    };
    let checker = checked(&judged, closure, crate::stdlib::packages(), target);
    let found = checker.found.borrow().clone();
    let items: HashMap<String, ItemId> = (0..checker.db.items.len())
        .map(|index| ItemId(index as u32))
        .filter(|item| checker.db.item(*item).file.path.ends_with("/.judged.nan"))
        .filter_map(|item| Some((checker.canonical_path(item)?, item)))
        .collect();
    functions
        .iter()
        .map(|function| {
            let item = *items.get(&function.path)?;
            let data = checker.db.item(item);
            let lines = data.syntax().item_lines(&data.syntax().items[data.index]);
            let failed = found.iter().any(|diagnostic| {
                diagnostic.code.class() == Class::Error
                    && diagnostic.path.as_deref() == Some(data.file.path.as_str())
                    && diagnostic.line.is_some_and(|line| lines.contains(&line))
            });
            Some(Judged {
                failed,
                sites: checker.obligations(item),
            })
        })
        .collect()
}
