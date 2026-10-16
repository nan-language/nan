//! `nan seal <item> --as <principal>`: records a seal of the principal on the item in its
//! module's API snapshot, accepting the item's pending weakening.

use std::io;
use std::path::Path;

use super::{Invocation, Trace, record};
use crate::check::{self, Unsealed};
use crate::diagnostic::{Class, Code, Diagnostic, Reason};
use crate::journal;
use crate::output::{Output, Record, Verdict};
use crate::scope::Scope;
use crate::workspace::Workspace;

pub fn run(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    let written = invocation.args.join(" ");
    let ([named], Some(principal)) = (&invocation.operands[..], invocation.value("--as")) else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    let Some((scope, _)) = Scope::resolve(workspace, current, Some(named)) else {
        return Ok(invocation.tool_error(Code::ToolUnknownScope, named));
    };
    trace.scope = Some(written.clone());
    let (index, path, location) = match &scope {
        Scope::Package(index) => {
            let package = &workspace.packages[*index];
            let manifest = package
                .manifest
                .as_ref()
                .expect("a package with a manifest");
            let location = (format!("{}/package.nan", package.dir), manifest.line);
            (*index, manifest.name.clone(), location)
        }
        Scope::Lines(file, lines) => {
            let Some(item) = workspace.item_at(file, *lines.start()) else {
                return Ok(invocation.tool_error(Code::ToolUsage, &written));
            };
            let index = workspace
                .packages
                .iter()
                .position(|package| file.starts_with(&format!("{}/", package.dir)))
                .expect("a file of a package");
            (index, item.canonical(), (file.clone(), *lines.start()))
        }
        _ => return Ok(invocation.tool_error(Code::ToolUsage, &written)),
    };
    let package = &workspace.packages[index];
    let named_well = (1..=64).contains(&principal.len())
        && principal
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-@".contains(&byte));
    let mut diagnostics = Vec::new();
    let mut changes = Vec::new();
    if !named_well {
        diagnostics.push(Diagnostic::new(Code::SealBadPrincipal));
    } else {
        match check::sealed(workspace, package, &path, principal) {
            Ok(sealed) => {
                let file = format!("{}/.nan/api/{}.nan", package.dir, sealed.module);
                changes.push((file, sealed.snapshot.bytes()));
            }
            Err(Unsealed::Unneeded) => {
                diagnostics.push(Diagnostic::at(Code::SealUnneeded, &location.0, location.1));
            }
            Err(Unsealed::Stale) => {
                diagnostics.push(Diagnostic::at(Code::ApiStale, &location.0, location.1));
            }
            Err(Unsealed::SameAuthor(author)) => diagnostics.push(Diagnostic {
                because: vec![Reason::note(&format!("{principal} {author}"))],
                ..Diagnostic::at(Code::SealSameAuthor, &location.0, location.1)
            }),
        }
    }
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
        .map(|diagnostic| record(workspace, &Scope::Workspace, diagnostic))
        .collect();
    records.extend(changes.iter().map(|(path, _)| Record {
        word: "wrote".to_owned(),
        fields: vec![path.clone()],
        lines: Vec::new(),
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
