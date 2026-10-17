//! Weakening judged by entailment. Of the parts of an accepted spec that a later spec's texts
//! weaken, the prover clears `requires` when the accepted requires entail the later requires, and
//! an accepted ensures when the accepted requires and the later ensures entail it, over a function
//! of the item's module with the accepted head, requires and ensures whose body calls a function
//! holding the later spec: the item itself for its current spec, or else a function stating it.

use super::Checker;
use super::db::ItemId;
use super::props::Property;
use super::snapshot::{Items, Snapshots};
use super::synthetic::{self, Judged, Synthetic};
use crate::machine::api::Accepted;
use crate::nir::{CheckKind, Role};
use crate::prover::{Obligated, Obligation, Status};
use crate::syntax::ast::{ItemKind, Mode};
use crate::workspace::Workspace;

/// A function of a checker whose later spec weakens an accepted spec by its texts: the later spec
/// is the function's current one when `later` is none.
#[derive(Clone)]
pub struct Weakened<'c, 'a> {
    pub ck: &'c Checker<'a>,
    pub item: ItemId,
    pub accepted: Accepted,
    pub later: Option<Accepted>,
}

/// The parts of each weakening that entailment leaves, judging those of the functions of the
/// workspace package at `index` together.
pub fn left(workspace: &Workspace, index: usize, weakened: &[Weakened]) -> Vec<Vec<String>> {
    let mut functions = Vec::new();
    let mut judging: Vec<(Vec<String>, Option<usize>)> = Vec::new();
    for weakening in weakened {
        let later = weakening
            .later
            .clone()
            .unwrap_or_else(|| weakening.ck.spec(weakening.item));
        let parts = weakening
            .ck
            .weakened(weakening.item, &weakening.accepted, &later);
        let premised = weakening
            .accepted
            .clauses
            .iter()
            .any(|clause| clause.starts_with("requires "));
        let judged = parts
            .iter()
            .any(|part| (part == "requires" && premised) || part.starts_with("ensures "));
        let made = judged
            .then(|| judging_functions(weakening, functions.len()))
            .flatten();
        let at = made.map(|made| {
            functions.extend(made);
            functions.len() - 1
        });
        judging.push((parts, at));
    }
    let judged = synthetic::judge(workspace, index, &functions);
    judging
        .into_iter()
        .zip(weakened)
        .map(|((parts, at), weakening)| match at {
            Some(at) => remaining(judged[at].as_ref(), &weakening.accepted, &parts),
            None => parts,
        })
        .collect()
}

/// Re-judges by entailment the pending weakenings of the snapshots computed for the workspace
/// package at `index`: an item left with no weakened part takes its current spec as accepted.
pub fn refine(workspace: &Workspace, index: usize, items: &Items, snapshots: &mut Snapshots) {
    let weakened: Vec<Weakened> = snapshots
        .recorded
        .iter()
        .filter_map(|recorded| {
            let first = items.first(&recorded.path)?;
            Some(Weakened {
                ck: first.ck,
                item: first.item,
                accepted: snapshots.entry(&recorded.path)?.accepted.clone(),
                later: None,
            })
        })
        .collect();
    if weakened.is_empty() {
        return;
    }
    let left = left(workspace, index, &weakened);
    let mut recorded = std::mem::take(&mut snapshots.recorded);
    for (weakening, parts) in weakened.iter().zip(left) {
        let path = weakening
            .ck
            .canonical_path(weakening.item)
            .unwrap_or_default();
        let hash = weakening.ck.hashes(weakening.item).item;
        let Some(entry) = snapshots.entry_mut(&path) else {
            continue;
        };
        if parts.is_empty() {
            entry.accepted = weakening.ck.spec(weakening.item);
            entry.pending.clear();
            if !items.holds_escape(&path) {
                entry.seals.retain(|seal| items.seal_current(&path, seal));
            }
            recorded.retain(|held| held.path != path);
        } else {
            entry.pending = parts.iter().map(|part| (hash, part.clone())).collect();
            if let Some(held) = recorded.iter_mut().find(|held| held.path == path) {
                held.parts = parts;
            }
        }
    }
    snapshots.recorded = recorded;
}

/// The parts the prover leaves of a weakening once its judging function is checked.
fn remaining(judged: Option<&Judged>, accepted: &Accepted, parts: &[String]) -> Vec<String> {
    let Some(judged) = judged.filter(|judged| !judged.failed) else {
        return parts.to_vec();
    };
    let proven = |site: &Obligated| matches!(site.status, Status::Proven | Status::Assumed);
    let body: Vec<&Obligated> = judged
        .sites
        .iter()
        .filter(|site| site.role == Role::Body)
        .collect();
    let requires = body
        .iter()
        .filter(|site| site.obligation == Obligation::Check(CheckKind::Requires))
        .all(|site| proven(site));
    let ensures: Vec<&String> = accepted
        .clauses
        .iter()
        .filter(|clause| clause.starts_with("ensures "))
        .collect();
    parts
        .iter()
        .filter(|part| {
            if *part == "requires" {
                return !requires;
            }
            let Some(clause) = ensures.iter().position(|ensures| *ensures == *part) else {
                return true;
            };
            !body.iter().any(|site| {
                site.obligation == Obligation::Check(CheckKind::Ensures)
                    && site.clause == Some(clause as u32)
                    && proven(site)
            })
        })
        .cloned()
        .collect()
}

/// A head read from its text: its name, the text after the name, each parameter's mode and name,
/// and whether it states a result, a failure type and `uses wait`.
struct Head<'t> {
    name: &'t str,
    rest: &'t str,
    params: Vec<(Mode, String)>,
    result: bool,
    fails: bool,
    wait: bool,
}

impl<'t> Head<'t> {
    fn read(text: &'t str) -> Option<Head<'t>> {
        let head = &text[text.find("fn ")? + 3..];
        let (name, rest) = head.split_at(head.find(['(', '['])?);
        let (params, after) = parameters(rest)?;
        Some(Head {
            name,
            rest,
            params,
            result: after.contains("->"),
            fails: after.contains(" fails "),
            wait: after.contains(" uses wait"),
        })
    }
}

/// The functions judging a weakening, the last of them the judging function: of the item's
/// module, with the accepted head under a new name, the accepted requires and ensures, and a body
/// calling a function holding the later spec with its parameters matched by position. A later
/// spec other than the item's current one is stated by a function before the judging function.
/// None when a head does not read so or the heads' parameters are not as many.
fn judging_functions(weakening: &Weakened, number: usize) -> Option<Vec<Synthetic>> {
    let ck = weakening.ck;
    let accepted = Head::read(weakening.accepted.head.as_deref()?)?;
    let mut functions = Vec::new();
    let (call, unit) = match &weakening.later {
        None => item_call(ck, weakening.item, &accepted)?,
        Some(later) => {
            let stated = Head::read(later.head.as_deref()?)?;
            if stated.params.len() != accepted.params.len() {
                return None;
            }
            let renamed = format!("{}_stated_{number}", stated.name);
            let body = match stated.result {
                true => "return todo \"stated\"",
                false => "todo \"stated\"",
            };
            functions.push(synthetic(
                ck,
                weakening.item,
                &renamed,
                stated.rest,
                later,
                body,
            ));
            (stated_call(&stated, &renamed, &accepted), !stated.result)
        }
    };
    let body = match unit {
        true => call,
        false => format!("return {call}"),
    };
    let renamed = format!("{}_judged_{number}", accepted.name);
    functions.push(synthetic(
        ck,
        weakening.item,
        &renamed,
        accepted.rest,
        &weakening.accepted,
        &body,
    ));
    Some(functions)
}

/// The call of an item by a function with the accepted head, and whether its result is `Unit`.
fn item_call(ck: &Checker, item: ItemId, accepted: &Head) -> Option<(String, bool)> {
    let ItemKind::Fn(function) = &ck.db.ast(item).kind else {
        return None;
    };
    let sig = ck.fn_sig(item)?;
    if sig.params.len() != accepted.params.len() {
        return None;
    }
    let marked = |mode: Mode, ty| match mode {
        Mode::Mut => "mut ",
        Mode::Take if !ck.has(ty, Property::Copy) => "take ",
        _ => "",
    };
    let mut args = Vec::new();
    for (index, (param, (_, given))) in sig.params.iter().zip(&accepted.params).enumerate() {
        if param.is_self {
            continue;
        }
        let value = format!("{}{given}", marked(param.mode, &param.ty));
        args.push(labelled(
            index == usize::from(sig.has_self()),
            &param.name,
            value,
            given,
        ));
    }
    let callee = match sig.params.first().filter(|param| param.is_self) {
        Some(receiver) => format!(
            "{}{}.{}",
            marked(receiver.mode, &receiver.ty),
            accepted.params[0].1,
            function.label.name.text
        ),
        None => function.label.written(),
    };
    let call = format!(
        "{}{callee}({})",
        markers(sig.fails.is_some(), sig.wait),
        args.join(", ")
    );
    Some((call, sig.result == super::ty::Ty::Unit))
}

/// The call of the function stating a later spec, named `renamed`, by a function with the
/// accepted head.
fn stated_call(stated: &Head, renamed: &str, accepted: &Head) -> String {
    let receiving = stated
        .params
        .first()
        .is_some_and(|(_, name)| name == "self");
    let marked = |mode: Mode| match mode {
        Mode::Mut => "mut ",
        Mode::Take => "take ",
        Mode::Read => "",
    };
    let mut args = Vec::new();
    let pairs = stated.params.iter().zip(&accepted.params).enumerate();
    for (index, ((mode, name), (_, given))) in pairs {
        if index == 0 && receiving {
            continue;
        }
        let value = format!("{}{given}", marked(*mode));
        args.push(labelled(
            index == usize::from(receiving),
            name,
            value,
            given,
        ));
    }
    let callee = match (receiving, renamed.rsplit_once('.')) {
        (true, Some((_, method))) => format!(
            "{}{}.{method}",
            marked(stated.params[0].0),
            accepted.params[0].1
        ),
        _ => renamed.to_owned(),
    };
    format!(
        "{}{callee}({})",
        markers(stated.fails, stated.wait),
        args.join(", ")
    )
}

/// An argument as a call writes it: unlabelled first, or a binding named like its parameter;
/// otherwise labelled.
fn labelled(first: bool, label: &str, value: String, given: &str) -> String {
    match first || label == given {
        true => value,
        false => format!("{label}: {value}"),
    }
}

fn markers(fails: bool, wait: bool) -> &'static str {
    match (fails, wait) {
        (true, true) => "try await ",
        (true, false) => "try ",
        (false, true) => "await ",
        (false, false) => "",
    }
}

/// A function of an item's module named `renamed`, with the head text `rest` after its name, the
/// requires and ensures of a spec, and a body.
fn synthetic(
    ck: &Checker,
    item: ItemId,
    renamed: &str,
    rest: &str,
    spec: &Accepted,
    body: &str,
) -> Synthetic {
    let clauses: Vec<&String> = spec
        .clauses
        .iter()
        .filter(|clause| clause.starts_with("requires ") || clause.starts_with("ensures "))
        .collect();
    let mut text = format!("\nfn {renamed}{rest}");
    match clauses.is_empty() {
        true => text.push_str(" {\n"),
        false => {
            text.push('\n');
            for clause in clauses {
                text.push_str(&format!("    {clause}\n"));
            }
            text.push_str("{\n");
        }
    }
    text.push_str(&format!("    {body}\n}} {renamed}\n"));
    Synthetic::beside(ck, item, renamed, text)
}

/// The modes and names of the parameters a head writes after its name, generic parameters first
/// when it has them, and the text after them.
fn parameters(rest: &str) -> Option<(Vec<(Mode, String)>, &str)> {
    let mut depth = 0;
    let mut open = None;
    for (at, c) in rest.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            '(' if depth == 0 => {
                open = Some(at);
                break;
            }
            _ => {}
        }
    }
    let open = open?;
    let mut params = Vec::new();
    let mut depth = 0;
    let mut start = open + 1;
    for (at, c) in rest[open..].char_indices().skip(1) {
        let at = open + at;
        match c {
            '(' | '[' => depth += 1,
            ')' if depth == 0 => {
                params.extend(parameter(&rest[start..at]));
                return Some((params, &rest[at + 1..]));
            }
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                params.extend(parameter(&rest[start..at]));
                start = at + 1;
            }
            _ => {}
        }
    }
    None
}

/// A parameter's mode and name from its text, `mode name: Type` or `mode self`.
fn parameter(param: &str) -> Option<(Mode, String)> {
    let named = param.split(':').next()?.trim();
    let mut words = named.split(' ');
    let first = words.next()?;
    let (mode, name) = match (first, words.next()) {
        ("mut", Some(name)) => (Mode::Mut, name),
        ("take", Some(name)) => (Mode::Take, name),
        (name, _) => (Mode::Read, name),
    };
    (!name.is_empty()).then(|| (mode, name.to_owned()))
}
