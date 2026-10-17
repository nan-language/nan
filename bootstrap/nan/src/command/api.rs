//! `nan api update`: computes the API snapshots of the packages holding its scope and rewrites
//! those of the modules in its scope, confirming the public docs of the items `--confirm-docs`
//! names.

use std::io;
use std::path::Path;

use super::{Invocation, Trace, record};
use crate::check::{self, Snapshots};
use crate::diagnostic::{Class, Code, Diagnostic, Reason};
use crate::journal;
use crate::output::{Output, Record, Verdict};
use crate::scope::Scope;
use crate::workspace::Workspace;

pub fn update(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    let Some((scope, written)) = Scope::resolve(workspace, current, invocation.scope()) else {
        return Ok(invocation.tool_error(Code::ToolUnknownScope, invocation.scope().unwrap_or(".")));
    };
    trace.scope = Some(written.clone());
    let mut confirmed = Vec::new();
    for named in invocation.listed("--confirm-docs") {
        let Some((item_scope, _)) = Scope::resolve(workspace, current, Some(named)) else {
            return Ok(invocation.tool_error(Code::ToolUnknownScope, &written));
        };
        let Scope::Lines(path, lines) = &item_scope else {
            return Ok(invocation.tool_error(Code::ToolUsage, &written));
        };
        let item = workspace
            .item_at(path, *lines.start())
            .map(|item| item.canonical());
        let documented = item
            .as_deref()
            .is_some_and(|item| check::has_public_docs(workspace, item));
        match item {
            Some(item) if documented && scope.holds(workspace, path, Some(*lines.start())) => {
                confirmed.push(item);
            }
            _ => return Ok(invocation.tool_error(Code::ToolUsage, &written)),
        }
    }
    let mut changes: Vec<(String, Option<Vec<u8>>)> = Vec::new();
    let mut diagnostics = check::reported(workspace, &scope);
    for (index, package) in workspace.packages.iter().enumerate() {
        let in_scope = |module: usize| match &scope {
            Scope::Workspace => true,
            Scope::Package(held) => *held == index,
            Scope::Module(held, held_module) => *held == index && *held_module == module,
            Scope::File(path) | Scope::Lines(path, _) => {
                workspace.file(path).is_some_and(|(holder, file)| {
                    std::ptr::eq(holder, package) && file.module == Some(module)
                })
            }
        };
        if !(0..package.modules.len()).any(in_scope) {
            continue;
        }
        let Snapshots { modules, recorded } =
            check::package_snapshots(workspace, package, &confirmed);
        for (name, snapshot) in modules {
            let Some(module) = package
                .modules
                .iter()
                .position(|module| module.name == name)
            else {
                continue;
            };
            if !in_scope(module) {
                continue;
            }
            let path = format!("{}/.nan/api/{name}.nan", package.dir);
            let bytes = snapshot.and_then(|snapshot| snapshot.bytes());
            let old = package.machine.bytes(&path);
            if bytes.as_deref() != old {
                changes.push((path, bytes));
            }
        }
        for pending in recorded {
            if scope.holds(workspace, &pending.file, Some(pending.line)) {
                diagnostics.push(Diagnostic {
                    because: pending
                        .parts
                        .iter()
                        .map(|part| Reason::note(part))
                        .collect(),
                    item: Some(pending.path),
                    ..Diagnostic::at(Code::ApiWeakeningPending, &pending.file, pending.line)
                });
            }
        }
    }
    changes.sort();
    journal::apply(&workspace.root, &changes)?;
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
    Ok(Output {
        command: invocation.command(&written, true),
        elision: invocation.command(&written, false),
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
    })
}

#[cfg(test)]
mod tests;
