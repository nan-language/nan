//! `nan dep sync`, `nan dep add`, `nan dep approve` and `nan upgrade` without `--language`:
//! resolving a package's dependency closure into its lock, after adding a `dep` directive to its
//! manifest for `nan dep add`, forwarding its uses of retired forms for `nan upgrade`.

use std::collections::HashMap;
use std::io;
use std::path::Path;

use super::{Invocation, Trace, record};
use crate::cache::Cache;
use crate::diagnostic::{Class, Code, Diagnostic};
use crate::format;
use crate::journal;
use crate::output::{Output, Record, Verdict};
use crate::resolution::{self, Outcome};
use crate::scope::Scope;
use crate::store::Store;
use crate::syntax::file::parse;
use crate::workspace::Workspace;

/// `nan dep sync [<package>]`: resolves the package's dependency closure and writes its lock.
pub fn sync(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    let Some((Scope::Package(index), written)) =
        Scope::resolve(workspace, current, invocation.scope())
    else {
        return Ok(invocation.tool_error(Code::ToolUsage, invocation.scope().unwrap_or(".")));
    };
    trace.scope = Some(written.clone());
    let mut records = Vec::new();
    let mut changes = Vec::new();
    synced(workspace, index, &mut records, &mut changes, false)?;
    Ok(written_output(invocation, &written, records, &changes))
}

/// `nan upgrade [<package>]`: resolves the package's dependency closure as `nan dep sync` does,
/// writing its lock and the forwarding fixes of its uses of retired forms, and reports each use
/// still moved.
pub fn upgrade(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    let Some((Scope::Package(index), written)) =
        Scope::resolve(workspace, current, invocation.scope())
    else {
        return Ok(invocation.tool_error(Code::ToolUsage, invocation.scope().unwrap_or(".")));
    };
    trace.scope = Some(written.clone());
    let mut records = Vec::new();
    let mut changes = Vec::new();
    synced(workspace, index, &mut records, &mut changes, true)?;
    Ok(written_output(invocation, &written, records, &changes))
}

/// `nan dep add <alias> <source>`: writes the `dep` directive into the manifest of the current
/// directory's package, then resolves its closure as `nan dep sync` does.
pub fn add(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    let written = invocation.args.join(" ");
    let (Some(index), [alias, source]) = (
        workspace
            .packages
            .iter()
            .position(|package| current.starts_with(workspace.root.join(&package.dir))),
        &invocation.operands[..],
    ) else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    trace.scope = Some(written.clone());
    let package = &workspace.packages[index];
    let Some(manifest) = package.files.first() else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    let text = String::from_utf8_lossy(&manifest.syntax.source).into_owned();
    let Some(closing) = text.rfind("\n}") else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    let line = format!("\n    dep {alias} {}", crate::machine::escaped(source));
    let mut added = text.clone();
    added.insert_str(closing, &line);
    let added = format::source_file(&parse(added.into_bytes()), None);
    let changed = HashMap::from([(manifest.path.clone(), added.clone())]);
    let with_dep = workspace.with_sources(&changed);
    let mut records = Vec::new();
    let mut changes = vec![(manifest.path.clone(), Some(added))];
    synced(&with_dep, index, &mut records, &mut changes, false)?;
    Ok(written_output(invocation, &written, records, &changes))
}

/// `nan dep approve [<package>]`: writes the package's lock with every approval its final closure
/// needs.
pub fn approve(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    let Some((Scope::Package(index), written)) =
        Scope::resolve(workspace, current, invocation.scope())
    else {
        return Ok(invocation.tool_error(Code::ToolUsage, invocation.scope().unwrap_or(".")));
    };
    trace.scope = Some(written.clone());
    let package = &workspace.packages[index];
    let lock = package.machine.lock().cloned().unwrap_or_default();
    let approved = crate::check::approvals(workspace, package, lock, true);
    let path = format!("{}/.nan/lock.nan", package.dir);
    let bytes = approved.bytes();
    let mut changes = Vec::new();
    if bytes.as_deref() != package.machine.bytes(&path) {
        changes.push((path, bytes));
    }
    journal::apply(&workspace.root, &changes)?;
    Ok(written_output(invocation, &written, Vec::new(), &changes))
}

/// Resolves a package's closure, forwarding its uses of retired forms when `forwarding`, writing
/// its lock, the files forwarding rewrites and those of `changes` when an assignment is
/// acceptable, and records what it prints.
fn synced(
    workspace: &Workspace,
    index: usize,
    records: &mut Vec<Record>,
    changes: &mut Vec<(String, Option<Vec<u8>>)>,
    forwarding: bool,
) -> io::Result<()> {
    let package = &workspace.packages[index];
    let cache = match Cache::open()? {
        Ok(cache) => cache,
        Err(exposed) => {
            records.push(exposed_record(workspace, exposed.0));
            changes.clear();
            return Ok(());
        }
    };
    let store = Store::new(cache.store());
    let outcome = match resolution::resolve(workspace, index, &store, &cache, forwarding)? {
        Ok(outcome) => outcome,
        Err(exposed) => {
            records.push(exposed_record(workspace, exposed.0));
            changes.clear();
            return Ok(());
        }
    };
    match outcome {
        Outcome::Resolved {
            lock,
            changes: released,
            sources,
            moved,
        } => {
            for change in released {
                let hash = |hash: Option<crate::hash::Hash>| {
                    hash.map_or_else(|| "-".to_owned(), |hash| hash.short())
                };
                records.push(Record {
                    word: "release".to_owned(),
                    fields: vec![
                        change.package,
                        hash(change.old),
                        hash(change.new),
                        change.class.word().to_owned(),
                    ],
                    lines: Vec::new(),
                });
            }
            records.extend(
                moved
                    .iter()
                    .map(|diagnostic| record(workspace, &Scope::Workspace, diagnostic)),
            );
            let path = format!("{}/.nan/lock.nan", package.dir);
            let bytes = lock.bytes();
            if bytes.as_deref() != package.machine.bytes(&path) {
                changes.push((path, bytes));
            }
            changes.extend(
                sources
                    .into_iter()
                    .map(|(path, source)| (path, Some(source))),
            );
            changes.sort();
            journal::apply(&workspace.root, changes)?;
        }
        Outcome::Unresolved(diagnostics) => {
            changes.clear();
            records.extend(
                diagnostics
                    .iter()
                    .map(|diagnostic| record(workspace, &Scope::Workspace, diagnostic)),
            );
        }
    }
    Ok(())
}

/// The record of an exposed part of the user directory.
fn exposed_record(workspace: &Workspace, path: String) -> Record {
    let diagnostic = Diagnostic {
        path: Some(path),
        ..Diagnostic::new(Code::BuildCacheExposed)
    };
    record(workspace, &Scope::Workspace, &diagnostic)
}

/// The output of a command writing files: its records, then a `wrote` or `deleted` record per
/// file, counting errors and incompleteness among its records.
fn written_output(
    invocation: &Invocation,
    written: &str,
    mut records: Vec<Record>,
    changes: &[(String, Option<Vec<u8>>)],
) -> Output {
    let count = |class: Class| {
        records
            .iter()
            .filter(|record| record.word == class.word())
            .count()
    };
    let (errors, incomplete) = (count(Class::Error), count(Class::Incompleteness));
    records.extend(changes.iter().map(|(path, content)| {
        Record {
            word: match content {
                Some(_) => "wrote",
                None => "deleted",
            }
            .to_owned(),
            fields: vec![path.clone()],
            lines: Vec::new(),
        }
    }));
    Output {
        command: invocation.command(written, true),
        elision: invocation.command(written, false),
        verdict: if errors > 0 {
            Verdict::Failed
        } else {
            Verdict::Ok
        },
        summary: vec![
            ("files", changes.len()),
            ("errors", errors),
            ("incomplete", incomplete),
        ],
        records,
    }
}
