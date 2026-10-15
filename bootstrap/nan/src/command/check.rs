//! The commands that check a scope and rewrite its files whole: `nan check`, `nan fix` and
//! `nan fmt`.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use super::read::pack_records;
use super::{Invocation, Name, Trace, item_written, record, written_for};
use crate::check;
use crate::diagnostic::{Class, Code, Diagnostic, Fix};
use crate::fix;
use crate::format;
use crate::journal;
use crate::output::{self, Output, Record, Verdict};
use crate::scope::Scope;
use crate::syntax::grammar::Rule;
use crate::syntax::items::Item;
use crate::workspace::Workspace;

/// The codes whose fixes `nan fix --propagate` keeps applying.
const PROPAGATED: [Code; 6] = [
    Code::ItemWaitMissing,
    Code::ItemFailsMissing,
    Code::ErrorCancelUnmapped,
    Code::ErrorUnconvertible,
    Code::EffectAwaitMissing,
    Code::ErrorUnhandled,
];

pub fn run(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    if let Some(prefix) = invocation.value("--prefix") {
        return Ok(prefixed(invocation, workspace, current, prefix));
    }
    let Some((scope, written)) = Scope::resolve(workspace, current, invocation.scope()) else {
        return Ok(invocation.tool_error(Code::ToolUnknownScope, invocation.scope().unwrap_or(".")));
    };
    trace.scope = Some(written.clone());
    let agent = invocation.value("--agent").unwrap_or("default");
    let agent_named = !agent.is_empty()
        && agent
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    if !agent_named {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    }
    let mut wrote = BTreeMap::new();
    let fixed = match invocation.name {
        Name::Fix => fixed(invocation, workspace, current),
        _ => None,
    };
    if let Some(fixed) = &fixed {
        wrote.extend(changed_files(workspace, fixed));
    }
    if invocation.name == Name::Fmt {
        wrote.extend(format::workspace(workspace, &scope));
    }
    let workspace = fixed.as_ref().unwrap_or(workspace);
    let wrote: Vec<(String, Vec<u8>)> = wrote.into_iter().collect();
    journal::write(&workspace.root, &wrote)?;
    let mut diagnostics = check::reported(workspace, &scope);
    fix::offer(workspace, current, invocation.scope(), &mut diagnostics);
    let count = |class: Class| {
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code.class() == class)
            .count()
    };
    let (errors, incomplete) = (count(Class::Error), count(Class::Incompleteness));
    let mut records: Vec<Record> = diagnostics
        .iter()
        .map(|diagnostic| record(workspace, &scope, diagnostic))
        .collect();
    if invocation.name == Name::Check {
        let errors: Vec<(String, u32)> = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code.class() == Class::Error)
            .filter_map(|diagnostic| Some((diagnostic.path.clone()?, diagnostic.line?)))
            .collect();
        records.extend(last_green(workspace, &scope, agent, &errors)?);
    }
    if invocation.flag("--radius") {
        records.extend(radii(workspace, &scope));
    }
    records.extend(wrote.iter().map(|(path, _)| Record {
        word: "wrote".to_owned(),
        fields: vec![path.clone()],
        lines: Vec::new(),
    }));
    let summary = invocation
        .name
        .counts()
        .iter()
        .map(|count| match *count {
            "files" => (*count, wrote.len()),
            "errors" => (*count, errors),
            _ => (*count, incomplete),
        })
        .collect();
    Ok(Output {
        command: invocation.command(&written, true),
        elision: invocation.command(&written, false),
        verdict: if errors > 0 {
            Verdict::Failed
        } else {
            Verdict::Ok
        },
        summary,
        records,
    })
}

/// An entry of a last-green record: an item's location, interface hash and api lines.
struct Entry {
    location: String,
    interface: String,
    api: Vec<String>,
}

/// Keeps an agent's last-green record: after a check reporting no error, replaces the entries of
/// the items of its scope; after one reporting errors, returns a `changed` record per item of the
/// closure sets of the lines holding one whose interface hash differs from its entry.
fn last_green(
    workspace: &Workspace,
    scope: &Scope,
    agent: &str,
    errors: &[(String, u32)],
) -> io::Result<Vec<Record>> {
    let path = format!(".nan/green/{agent}");
    let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
    if let Ok(text) = std::fs::read_to_string(workspace.root.join(&path)) {
        let mut current: Option<String> = None;
        for line in text.lines() {
            if let Some(api) = line.strip_prefix("    ")
                && let Some(entry) = current.as_ref().and_then(|item| entries.get_mut(item))
            {
                entry.api.push(api.to_owned());
                continue;
            }
            let mut fields = line.strip_prefix("item ").unwrap_or_default().split(' ');
            let (Some(item), Some(location), Some(interface)) =
                (fields.next(), fields.next(), fields.next())
            else {
                current = None;
                continue;
            };
            entries.insert(
                item.to_owned(),
                Entry {
                    location: location.to_owned(),
                    interface: interface.to_owned(),
                    api: Vec::new(),
                },
            );
            current = Some(item.to_owned());
        }
    }
    let interface = |green: &check::Green| {
        green
            .interface
            .map_or_else(|| "-".to_owned(), |hash| hash.full())
    };
    if !errors.is_empty() {
        let writer = scope.package(workspace).and_then(|package| package.name());
        return Ok(check::greens(workspace, None, errors)
            .iter()
            .filter_map(|green| {
                let entry = entries.get(&green.path)?;
                (entry.interface != interface(green)).then(|| Record {
                    word: "changed".to_owned(),
                    fields: vec![
                        written_for(&green.path, writer),
                        format!("{}:{}", green.file, green.line),
                    ],
                    lines: crate::diff::merged(&entry.api, &green.api),
                })
            })
            .collect());
    }
    entries.retain(|_, entry| {
        let (file, line) = entry.location.rsplit_once(':').unwrap_or_default();
        !scope.holds(workspace, file, line.parse().ok())
    });
    for green in check::greens(workspace, Some(scope), &[]) {
        entries.insert(
            green.path.clone(),
            Entry {
                location: format!("{}:{}", green.file, green.line),
                interface: interface(&green),
                api: green.api,
            },
        );
    }
    let mut text = String::new();
    for (item, entry) in &entries {
        text.push_str(&format!(
            "item {item} {} {}\n",
            entry.location, entry.interface
        ));
        for line in &entry.api {
            text.push_str(&format!("    {line}\n"));
        }
    }
    journal::replace_alone(&workspace.root, &path, text.as_bytes())?;
    Ok(Vec::new())
}

/// A `radius` record per item of the scope with its closure radius, largest first, then by
/// path.
fn radii(workspace: &Workspace, scope: &Scope) -> Vec<Record> {
    let writer = scope.package(workspace).and_then(|package| package.name());
    let mut radii: Vec<(usize, String)> = check::contexts(workspace, scope, None)
        .iter()
        .map(|pack| {
            let radius = output::units_of(&pack_records(workspace, pack, writer));
            (radius, written_for(&pack.path, writer))
        })
        .collect();
    radii.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    radii
        .into_iter()
        .map(|(radius, item)| Record {
            word: "radius".to_owned(),
            fields: vec![item, radius.to_string()],
            lines: Vec::new(),
        })
        .collect()
}

/// The codes whose diagnostics with an exact fix prefix checking reports as pending clauses.
const PENDING: [Code; 4] = [
    Code::ItemFailsMissing,
    Code::ItemWaitMissing,
    Code::ErrorCancelUnmapped,
    Code::ErrorUnconvertible,
];

/// The codes not reported when their construct holds a token closing appends.
const OPEN_CONSTRUCTS: [Code; 18] = [
    Code::ClosureCaptureMode,
    Code::ClosureCaptureUnused,
    Code::ExprArgumentCount,
    Code::ExprBranchForm,
    Code::ExprMissingElse,
    Code::ExprNonExhaustive,
    Code::GenericParameterUnused,
    Code::ItemFailsUnneeded,
    Code::ItemWaitUnneeded,
    Code::OwnLinearDropped,
    Code::StmtElseNotDiverging,
    Code::StmtMissingReturn,
    Code::StmtUnusedLabel,
    Code::TextHoleUnbound,
    Code::ThresholdUnusedWaiver,
    Code::TypeMissingField,
    Code::TypePayloadShape,
    Code::UnusedLocal,
];

/// `nan check --prefix`: the error at the prefix's first dead token, the diagnostics of the
/// prefix closed, its pending clauses, and the typed holes and closers closing appends.
fn prefixed(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    written: &str,
) -> Output {
    let (path, line) = match written.rsplit_once(':') {
        Some((path, line)) if path.ends_with(".nan") => (path, line.parse::<u32>().ok()),
        _ => (written, None),
    };
    let Some((_, file)) = workspace
        .file(path)
        .filter(|_| invocation.scope().is_none())
    else {
        return invocation.tool_error(Code::ToolUnknownScope, written);
    };
    let source = &file.syntax.source;
    let end = match line {
        Some(line) => match file.syntax.lines.get(line as usize) {
            Some(next) => next.start as usize,
            None if (line as usize) <= file.syntax.lines.len() => source.len(),
            None => return invocation.tool_error(Code::ToolUnknownScope, written),
        },
        None => source.len(),
    };
    let closed = crate::syntax::closing::close(&source[..end]);
    let fixed = workspace
        .with_sources(&std::iter::once((path.to_owned(), closed.source.clone())).collect());
    let scope = Scope::File(path.to_owned());
    let mut diagnostics = check::reported(&fixed, &scope);
    fix::offer(&fixed, current, Some(path), &mut diagnostics);
    let appended_lines: Vec<u32> = closed.pieces.iter().map(|piece| piece.line).collect();
    let mut records = Vec::new();
    let mut dead = 0;
    if let Some(at) = closed.dead {
        let prefix = workspace
            .with_sources(&std::iter::once((path.to_owned(), source[..end].to_vec())).collect());
        let failing = check::reported(&prefix, &scope)
            .into_iter()
            .find(|diagnostic| diagnostic.failing == Some(at));
        if let Some(failing) = failing {
            records.push(record(&prefix, &scope, &failing));
            dead = 1;
        }
    }
    let mut pending = Vec::new();
    for diagnostic in &diagnostics {
        let held = diagnostic
            .line
            .is_some_and(|line| appended_lines.contains(&line))
            || OPEN_CONSTRUCTS.contains(&diagnostic.code)
                && diagnostic
                    .judged
                    .or(diagnostic.span)
                    .is_some_and(|(start, end)| {
                        closed
                            .pieces
                            .iter()
                            .any(|piece| piece.span.0 < end && piece.span.1 > start)
                    });
        if held {
            continue;
        }
        if PENDING.contains(&diagnostic.code) && !diagnostic.fixes.is_empty() {
            pending.extend(pending_clauses(&fixed, &scope, diagnostic));
            continue;
        }
        records.push(record(&fixed, &scope, diagnostic));
    }
    let errors = records.len()
        - dead
        - records
            .iter()
            .filter(|record| record.word != "error")
            .count();
    let pending_count = pending.len();
    records.extend(pending);
    let holes = check::holes(&fixed);
    let mut hole_count = 0;
    for piece in &closed.pieces {
        if piece.hole {
            hole_count += 1;
            let expected = holes
                .iter()
                .find(|hole| hole.path == path && hole.line == piece.line)
                .map_or_else(|| "-".to_owned(), |hole| hole.expected.clone());
            let writer = scope.package(&fixed).and_then(|package| package.name());
            let item = item_written(&fixed, writer, path, piece.line, None);
            records.push(Record {
                word: "hole".to_owned(),
                fields: vec![format!("{path}:{}", piece.line), item, expected],
                lines: Vec::new(),
            });
        } else {
            records.push(Record {
                word: "close".to_owned(),
                fields: piece.text.split(' ').map(str::to_owned).collect(),
                lines: Vec::new(),
            });
        }
    }
    Output {
        command: invocation.command(written, true),
        elision: invocation.command(written, false),
        verdict: if dead > 0 || errors > 0 {
            Verdict::Failed
        } else {
            Verdict::Ok
        },
        summary: vec![
            ("dead", dead),
            ("pending", pending_count),
            ("holes", hole_count),
        ],
        records,
    }
}

/// The `pending` records of a diagnostic whose fix adds clauses: one per clause, at its line.
fn pending_clauses(workspace: &Workspace, scope: &Scope, diagnostic: &Diagnostic) -> Vec<Record> {
    let (Some(path), Some(line)) = (&diagnostic.path, diagnostic.line) else {
        return Vec::new();
    };
    let writer = scope.package(workspace).and_then(|package| package.name());
    let item = item_written(workspace, writer, path, line, None);
    let added = diagnostic.fixes[0]
        .0
        .iter()
        .map(|(_, replacement)| replacement.text.trim().to_owned())
        .collect::<Vec<_>>()
        .join(" ");
    let clauses = match added.split_once(" uses wait") {
        Some((fails, _)) if !fails.is_empty() => vec![fails.to_owned(), "uses wait".to_owned()],
        _ => vec![added],
    };
    clauses
        .into_iter()
        .map(|clause| {
            let mut fields = vec![format!("{path}:{line}"), item.clone()];
            fields.extend(clause.split(' ').map(str::to_owned));
            Record {
                word: "pending".to_owned(),
                fields,
                lines: Vec::new(),
            }
        })
        .collect()
}

/// The workspace once `nan fix` has made its fixes, none when it makes none: one pass applying
/// the fix of each diagnostic offering exactly one, then with `--propagate`, passes applying
/// those of the propagated codes until one applies none.
fn fixed(invocation: &Invocation, workspace: &Workspace, current: &Path) -> Option<Workspace> {
    let scope = invocation.scope();
    let mut fixed = pass(workspace, current, scope, |_, _| true);
    while invocation.flag("--propagate") {
        let base = fixed.as_ref().unwrap_or(workspace);
        let Some(next) = pass(base, current, scope, |diagnostic, fix| {
            PROPAGATED.contains(&diagnostic.code) && !edits_interface(base, fix)
        }) else {
            break;
        };
        fixed = Some(next);
    }
    fixed
}

/// One pass of `nan fix`: the workspace once the fixes `keep` accepts are made, among the one
/// fix of each diagnostic offering exactly one, in path and line order, a fix sharing a line
/// with an earlier one skipped; none when it makes none.
fn pass(
    workspace: &Workspace,
    current: &Path,
    scope: Option<&str>,
    keep: impl Fn(&Diagnostic, &Fix) -> bool,
) -> Option<Workspace> {
    let (resolved, _) = Scope::resolve(workspace, current, scope)?;
    let mut diagnostics = check::reported(workspace, &resolved);
    fix::offer(workspace, current, scope, &mut diagnostics);
    let mut taken: Vec<(String, u32)> = Vec::new();
    let mut chosen: Vec<&Fix> = Vec::new();
    for diagnostic in &diagnostics {
        let [fix] = &diagnostic.fixes[..] else {
            continue;
        };
        if !keep(diagnostic, fix) {
            continue;
        }
        let lines = fix::lines(workspace, fix);
        if lines.iter().any(|line| taken.contains(line)) {
            continue;
        }
        taken.extend(lines);
        chosen.push(fix);
    }
    (!chosen.is_empty()).then(|| workspace.with_sources(&fix::apply(workspace, &chosen)))
}

/// Whether a fix edits the head of a `pkg` or `pub` function or a `pkg` or `pub` enum.
fn edits_interface(workspace: &Workspace, fix: &Fix) -> bool {
    fix.0.iter().any(|(path, replacement)| {
        let Some((_, file)) = workspace.file(path) else {
            return false;
        };
        let syntax = &file.syntax;
        let Some(item) = syntax.items.iter().find(|item| {
            syntax.tokens[item.first].start <= replacement.start
                && replacement.end <= syntax.tokens[item.end - 1].end
        }) else {
            return false;
        };
        let Some(tree) = &item.tree else {
            return false;
        };
        let item = Item { file: syntax, tree };
        if item.visibility().is_none() {
            return false;
        }
        match item.kind() {
            Some((Rule::EnumItem, _)) => true,
            Some((Rule::FnItem, _)) => item
                .head_end()
                .is_none_or(|end| replacement.start <= syntax.tokens[end as usize].end),
            _ => false,
        }
    })
}

/// The files whose sources differ between a workspace and the same workspace fixed, with their
/// new sources.
fn changed_files(workspace: &Workspace, fixed: &Workspace) -> Vec<(String, Vec<u8>)> {
    fixed
        .files()
        .filter(|(_, file)| {
            workspace
                .file(&file.path)
                .is_some_and(|(_, old)| old.syntax.source != file.syntax.source)
        })
        .map(|(_, file)| (file.path.clone(), file.syntax.source.clone()))
        .collect()
}

#[cfg(test)]
mod tests;
