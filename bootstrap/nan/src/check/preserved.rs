//! Whether the `preserves` tombstones a release adds to its parent preserve the exported items
//! they retire, as publishing checks them.

use std::collections::{BTreeMap, HashMap};

use super::db::ItemId;
use super::{Checker, check_targets, checked};
use crate::diagnostic::{Class, Code, Diagnostic};
use crate::machine::api::Entry;
use crate::prover::Status;
use crate::release::{Forward, replaces};
use crate::syntax::ast::{ClauseKind, ExprKind, ItemKind, RetiredNew};
use crate::workspace::Workspace;

/// A head tombstone checked through a forwarding function: the tombstone's file and line, and the
/// function's name.
struct Forwarding {
    file: String,
    line: u32,
    name: String,
}

/// The `retired.not-preserved` errors of the `preserves` tombstones of the workspace package at
/// `index` that its parent's code root does not hold, `held` giving the old forms it does, and
/// whose old forms name exported items of the parent, `parent` and `release` giving the exported
/// entries of the parent and of the release.
pub fn not_preserved(
    workspace: &Workspace,
    index: usize,
    parent: &BTreeMap<String, Entry>,
    held: &BTreeMap<String, Forward>,
    release: &BTreeMap<String, Entry>,
) -> Vec<Diagnostic> {
    let package = &workspace.packages[index];
    let (Some(closure), Some(&target)) = (
        workspace.closure_of(package),
        check_targets(package).first(),
    ) else {
        return Vec::new();
    };
    let std = crate::stdlib::packages();
    let checker = checked(workspace, closure, std, target);
    let Some(id) = package.name().and_then(|name| checker.package_named(name)) else {
        return Vec::new();
    };
    let mut errors = Vec::new();
    let mut sources: BTreeMap<String, String> = BTreeMap::new();
    let mut forwardings = Vec::new();
    for &module in &checker.db.package(id).modules {
        for &item in &checker.db.module(module).items {
            let ItemKind::Retired(retired) = &checker.db.ast(item).kind else {
                continue;
            };
            let RetiredNew::Rewrite {
                expr,
                preserves: true,
            } = &retired.new
            else {
                continue;
            };
            let Some(form) = checker.db.old_form(module, retired) else {
                continue;
            };
            let Some(old) = parent.get(&form).filter(|_| !held.contains_key(&form)) else {
                continue;
            };
            let data = checker.db.item(item);
            let line = data.head_line();
            let at = |code| Diagnostic::at(code, &data.file.path, line);
            if retired.head.is_none() {
                let kept = match &expr.kind {
                    ExprKind::Path { path, args: None } => checker
                        .db
                        .spelled(module, path)
                        .and_then(|new| Some((release.get(&new)?, new)))
                        .is_some_and(|(entry, new)| replaces(old, entry, &form, &new)),
                    _ => true,
                };
                if !kept {
                    errors.push(at(Code::RetiredNotPreserved));
                }
                continue;
            }
            let Some(function) = forwarding_function(&checker, item, old, forwardings.len()) else {
                errors.push(at(Code::RetiredNotPreserved));
                continue;
            };
            let dir = data.file.path.rsplit_once('/').map_or("", |(dir, _)| dir);
            let file = format!("{dir}/forwarding-checks.nan");
            sources
                .entry(file)
                .or_insert_with(|| "//! Forwarding checks.\n".to_owned())
                .push_str(&function.1);
            forwardings.push(Forwarding {
                file: data.file.path.clone(),
                line,
                name: function.0,
            });
        }
    }
    if forwardings.is_empty() {
        return errors;
    }
    let changed: HashMap<String, Vec<u8>> = sources
        .into_iter()
        .map(|(path, text)| (path, text.into_bytes()))
        .collect();
    let checked_workspace = workspace.with_sources(&changed);
    let Some(closure) = checked_workspace.closure_of(&checked_workspace.packages[index]) else {
        return errors;
    };
    let checker = checked(&checked_workspace, closure, std, target);
    let found = checker.found.borrow().clone();
    for forwarding in forwardings {
        let item = (0..checker.db.items.len())
            .map(|index| ItemId(index as u32))
            .find(|item| checker.canonical_path(*item).as_deref() == Some(&forwarding.name));
        if !item.is_some_and(|item| preserves(&checker, item, &found)) {
            errors.push(Diagnostic::at(
                Code::RetiredNotPreserved,
                &forwarding.file,
                forwarding.line,
            ));
        }
    }
    errors
}

/// The canonical path and text of the forwarding function of a head tombstone: a function of its
/// module with its old head under a new name, the old item's requires, ensures and example
/// clauses in the parent, and a body returning the rewrite; none when its line does not read as
/// a head tombstone.
fn forwarding_function(
    checker: &Checker,
    item: ItemId,
    old: &Entry,
    number: usize,
) -> Option<(String, String)> {
    let data = checker.db.item(item);
    let line = String::from_utf8_lossy(data.syntax().line_text(data.head_line())).into_owned();
    let (head, rest) = line.strip_prefix("retired ")?.split_once(" => ")?;
    let rewrite = rest.strip_suffix(" preserves")?;
    let (name, signature) = head.split_at(head.find('(')?);
    let renamed = format!("{name}_forwarding_{number}");
    let marked = match (
        signature.contains(" fails "),
        signature.contains(" uses wait"),
    ) {
        (true, true) => format!("try await {rewrite}"),
        (true, false) => format!("try {rewrite}"),
        (false, true) => format!("await {rewrite}"),
        (false, false) => rewrite.to_owned(),
    };
    let body = match signature.contains(" -> ") {
        true => format!("    return {marked}"),
        false => format!("    {marked}"),
    };
    let mut text = format!("\nfn {renamed}{signature}");
    match old.accepted.clauses.is_empty() {
        true => text.push_str(" {\n"),
        false => {
            text.push('\n');
            for clause in &old.accepted.clauses {
                text.push_str(&format!("    {clause}\n"));
            }
            text.push_str("{\n");
        }
    }
    text.push_str(&format!("{body}\n}} {renamed}\n"));
    let module = &checker.db.module(data.module);
    let package = &checker.db.package(module.package).name;
    let path = match module.root {
        true => format!("{package}::{renamed}"),
        false => format!("{package}::{}::{renamed}", module.name),
    };
    Some((path, text))
}

/// Whether a forwarding function preserves its old item: it checks without error, and the prover
/// proves every obligation or the old item's examples, which it holds, all hold.
fn preserves(checker: &Checker, item: ItemId, found: &[Diagnostic]) -> bool {
    let data = checker.db.item(item);
    let lines = data.syntax().item_lines(&data.syntax().items[data.index]);
    let failed = found.iter().any(|diagnostic| {
        diagnostic.code.class() == Class::Error
            && diagnostic.path.as_deref() == Some(data.file.path.as_str())
            && diagnostic.line.is_some_and(|line| lines.contains(&line))
    });
    if failed {
        return false;
    }
    let proved = checker
        .obligations(item)
        .iter()
        .all(|site| matches!(site.status, Status::Proven | Status::Assumed));
    let examples = match &checker.db.ast(item).kind {
        ItemKind::Fn(function) => function
            .clauses
            .iter()
            .any(|clause| matches!(clause.kind, ClauseKind::Example(_))),
        _ => false,
    };
    proved || examples
}
