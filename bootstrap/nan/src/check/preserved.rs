//! Whether the `preserves` tombstones a release adds to its parent preserve the exported items
//! they retire, as publishing checks them.

use std::collections::BTreeMap;

use super::db::ItemId;
use super::synthetic::{self, Judged, Synthetic};
use super::{Checker, check_targets, checked};
use crate::diagnostic::{Code, Diagnostic};
use crate::machine::api::Entry;
use crate::prover::Status;
use crate::release::{Forward, replaces};
use crate::syntax::ast::{ExprKind, ItemKind, RetiredNew};
use crate::workspace::Workspace;

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
    let mut forwardings: Vec<(Diagnostic, Synthetic)> = Vec::new();
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
            match forwarding_function(&checker, item, old, forwardings.len()) {
                Some(function) => forwardings.push((at(Code::RetiredNotPreserved), function)),
                None => errors.push(at(Code::RetiredNotPreserved)),
            }
        }
    }
    let functions: Vec<Synthetic> = forwardings
        .iter()
        .map(|(_, function)| Synthetic {
            dir: function.dir.clone(),
            path: function.path.clone(),
            text: function.text.clone(),
        })
        .collect();
    let judged = synthetic::judge(workspace, index, &functions);
    for ((error, function), judged) in forwardings.into_iter().zip(judged) {
        if !judged.is_some_and(|judged| preserves(&judged, &function.text)) {
            errors.push(error);
        }
    }
    errors
}

/// The forwarding function of a head tombstone: a function of its module with its old head under
/// a new name, the old item's requires, ensures and example clauses in the parent, and a body
/// returning the rewrite; none when its line does not read as a head tombstone.
fn forwarding_function(
    checker: &Checker,
    item: ItemId,
    old: &Entry,
    number: usize,
) -> Option<Synthetic> {
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
    Some(Synthetic::beside(checker, item, &renamed, text))
}

/// Whether a forwarding function preserves its old item: it checks without error, and the prover
/// proves every obligation or the old item's examples, which its text holds, all hold.
fn preserves(judged: &Judged, text: &str) -> bool {
    let proved = judged
        .sites
        .iter()
        .all(|site| matches!(site.status, Status::Proven | Status::Assumed));
    let examples = text.lines().any(|line| line.starts_with("    example "));
    !judged.failed && (proved || examples)
}
