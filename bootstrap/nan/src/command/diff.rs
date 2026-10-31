//! `nan diff <old> <new> [--path <path>]`: the items changed between two versions of the
//! workspace, or with `--path` of one file, with their change classes; and `nan diff --green
//! [<scope>] [--agent <name>]`: the items of the scope whose interface hash differs from the
//! agent's last-green entry.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use super::check::{agent, changed, green_entries};
use super::{Invocation, Trace};
use crate::check;
use crate::diagnostic::Code;
use crate::output::{Output, Record, Verdict};
use crate::scope::Scope;
use crate::workspace::{self, Workspace};

/// The workspace form, comparing two versions of a workspace, each relative to the current
/// directory.
pub fn workspaces(
    invocation: &Invocation,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    let written = invocation.args.join(" ");
    let [old, new] = invocation.operands[..] else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    trace.scope = Some(written.clone());
    let (old, new) = (current.join(old), current.join(new));
    for root in [&old, &new] {
        if !workspace::holds_package(root)? {
            return Ok(invocation.tool_error(Code::ToolNoWorkspace, &written));
        }
    }
    let old = Workspace::load(old, &mut Vec::new())?;
    let new = Workspace::load(new, &mut Vec::new())?;
    Ok(output(
        invocation,
        &written,
        check::changes(&old, &new, None),
    ))
}

/// The file form, comparing two versions, relative to the current directory, of the workspace
/// file at `--path`; and the `--green` form.
pub fn run(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    if invocation.flag("--green") {
        return green(invocation, workspace, current, trace);
    }
    let written = invocation.args.join(" ");
    let ([old, new], Some(path)) = (&invocation.operands[..], invocation.value("--path")) else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    trace.scope = Some(written.clone());
    let version = |file: &str| {
        let source = fs::read(current.join(file)).unwrap_or_default();
        workspace.with_sources(&HashMap::from([(path.to_owned(), source)]))
    };
    let (old, new) = (version(old), version(new));
    Ok(output(
        invocation,
        &written,
        check::changes(&old, &new, Some(path)),
    ))
}

fn green(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    let scope = invocation.operands.first().copied();
    let Some((scope, written)) = Scope::resolve(workspace, current, scope) else {
        return Ok(invocation.tool_error(Code::ToolUnknownScope, scope.unwrap_or(".")));
    };
    trace.scope = Some(written.clone());
    let Some(agent) = agent(invocation) else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    let writer = scope.package(workspace).and_then(|package| package.name());
    let greens = check::greens(workspace, &scope, &[]);
    let records = changed(&green_entries(workspace, agent), &greens, writer);
    Ok(Output {
        command: invocation.command(&written, true),
        elision: invocation.command(&written, false),
        verdict: Verdict::Ok,
        summary: vec![("records", records.len())],
        records,
    })
}

/// A `changed` record per changed item: its location, its change classes and whether it is a
/// weakening or a widening, holding its old and new texts merged.
fn output(invocation: &Invocation, written: &str, changes: Vec<check::Change>) -> Output {
    let records: Vec<Record> = changes
        .into_iter()
        .map(|change| {
            let mut fields = vec![change.path, format!("{}:{}", change.file, change.line)];
            fields.extend(change.classes.iter().map(|class| (*class).to_owned()));
            if change.weakening {
                fields.push("weakening".to_owned());
            }
            if change.widening {
                fields.push("widening".to_owned());
            }
            Record {
                word: "changed".to_owned(),
                fields,
                lines: crate::diff::merged(&change.old, &change.new),
            }
        })
        .collect();
    Output {
        command: invocation.command(written, true),
        elision: invocation.command(written, false),
        verdict: Verdict::Ok,
        summary: vec![("records", records.len())],
        records,
    }
}

#[cfg(test)]
mod tests;
