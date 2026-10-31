//! `nan upgrade --language <N> [<scope>]`: moves each package of the scope to language version
//! N. The toolchain implements version 1 alone, which retires no form, so the command reports an
//! unsupported N, an N below a package's version and the errors of the scope, and writes no
//! file.

use std::io;
use std::path::Path;

use super::{Invocation, Trace, record};
use crate::check;
use crate::diagnostic::{Class, Code, Diagnostic};
use crate::output::{Output, Verdict};
use crate::scope::Scope;
use crate::toolchain;
use crate::workspace::Workspace;

pub fn language(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    let Some((scope, written)) = Scope::resolve(workspace, current, invocation.scope()) else {
        return Ok(invocation.tool_error(Code::ToolUnknownScope, invocation.scope().unwrap_or(".")));
    };
    let Some(version) = invocation
        .value("--language")
        .and_then(|version| version.parse::<u64>().ok())
    else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    trace.scope = Some(written.clone());
    let mut diagnostics = Vec::new();
    if !toolchain::LANGUAGES.contains(&version) {
        diagnostics.push(Diagnostic::new(Code::LanguageUnsupported));
    }
    let packages = workspace
        .packages
        .iter()
        .filter(|package| match scope.package(workspace) {
            Some(scoped) => std::ptr::eq(&***package, scoped),
            None => true,
        });
    for package in packages {
        let Some(manifest) = &package.manifest else {
            continue;
        };
        if let Some((stated, line)) = manifest.language
            && u128::from(version) < stated
        {
            let path = format!("{}/package.nan", package.dir);
            diagnostics.push(Diagnostic::at(Code::LanguageDowngrade, &path, line));
        }
    }
    diagnostics.extend(
        check::reported(workspace, &scope)
            .into_iter()
            .filter(|diagnostic| diagnostic.code.class() == Class::Error),
    );
    let errors = diagnostics.len();
    Ok(Output {
        command: invocation.command(&written, true),
        elision: invocation.command(&written, false),
        verdict: if errors > 0 {
            Verdict::Failed
        } else {
            Verdict::Ok
        },
        summary: vec![("files", 0), ("errors", errors), ("incomplete", 0)],
        records: diagnostics
            .iter()
            .map(|diagnostic| record(workspace, &scope, diagnostic))
            .collect(),
    })
}

#[cfg(test)]
mod tests;
