//! `nan verify`: the release gate over the packages holding its scope and those they reach
//! through chains of relative-path sources: their checks, their tests, their API snapshots,
//! pending weakenings and seals.

use std::io;
use std::path::Path;

use super::test::fail_records;
use super::{Invocation, Trace, record};
use crate::cache::Cache;
use crate::check;
use crate::diagnostic::{Class, Code, Diagnostic};
use crate::output::{Output, Verdict};
use crate::scope::Scope;
use crate::test_runs::{self, Report, Runs};
use crate::workspace::Workspace;

pub fn run(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    let Some((scope, written)) = Scope::resolve(workspace, current, invocation.scope()) else {
        return Ok(invocation.tool_error(Code::ToolUnknownScope, invocation.scope().unwrap_or(".")));
    };
    trace.scope = Some(written.clone());
    let packages = gated(workspace, &scope);
    let (diagnostics, report) = judged(workspace, &packages, &[])?;
    let count = |class: Class| {
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code.class() == class)
            .count()
    };
    let (errors, incomplete) = (count(Class::Error), count(Class::Incompleteness));
    let mut records: Vec<_> = diagnostics
        .iter()
        .map(|diagnostic| record(workspace, &scope, diagnostic))
        .collect();
    records.extend(fail_records(workspace, &scope, &report));
    let failed = report.failures.len();
    Ok(Output {
        command: invocation.command(&written, true),
        elision: invocation.command(&written, false),
        verdict: if diagnostics.is_empty() && failed == 0 {
            Verdict::Ok
        } else {
            Verdict::Failed
        },
        summary: vec![
            ("errors", errors),
            ("incomplete", incomplete),
            ("tests", report.tests),
            ("failed", failed),
        ],
        records,
    })
}

/// What the release gate reports on packages: their check diagnostics, then, when none is an
/// error, their tests' report, and their gate diagnostics, `authors` being the author principals
/// a publication names; by path, line and code.
pub fn judged(
    workspace: &Workspace,
    packages: &[usize],
    authors: &[String],
) -> io::Result<(Vec<Diagnostic>, Report)> {
    let mut diagnostics: Vec<Diagnostic> = check::reported(workspace, &Scope::Workspace)
        .into_iter()
        .filter(|diagnostic| within(workspace, packages, diagnostic))
        .collect();
    let mut report = Report::default();
    let errors = diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code.class() == Class::Error);
    if !errors {
        match Cache::open()? {
            Ok(cache) => {
                for index in packages {
                    match test_runs::run(workspace, &Scope::Package(*index), &Runs::All, &cache)? {
                        Ok(ran) => {
                            report.tests += ran.tests;
                            report.failures.extend(ran.failures);
                            diagnostics.extend(ran.diagnostics);
                        }
                        Err(exposed) => diagnostics.push(Diagnostic {
                            path: Some(exposed.0),
                            ..Diagnostic::new(Code::BuildCacheExposed)
                        }),
                    }
                }
            }
            Err(exposed) => diagnostics.push(Diagnostic {
                path: Some(exposed.0),
                ..Diagnostic::new(Code::BuildCacheExposed)
            }),
        }
    }
    for index in packages {
        diagnostics.extend(check::gate(workspace, &workspace.packages[*index], authors));
    }
    diagnostics.sort_by(|left, right| {
        (&left.path, left.line, left.code.name()).cmp(&(&right.path, right.line, right.code.name()))
    });
    diagnostics.dedup();
    Ok((diagnostics, report))
}

/// The packages the gate judges: each holding part of the scope, and each those reach through a
/// chain of `dep` directives with relative-path sources, by index.
pub fn gated(workspace: &Workspace, scope: &Scope) -> Vec<usize> {
    let mut found: Vec<usize> = (0..workspace.packages.len())
        .filter(|index| {
            let package = &workspace.packages[*index];
            package.files.iter().any(|file| {
                scope.holds(workspace, &file.path, None) || holds_lines(scope, &file.path)
            })
        })
        .collect();
    let mut next = 0;
    while let Some(&index) = found.get(next) {
        next += 1;
        let package = &workspace.packages[index];
        for dep in package.manifest.iter().flat_map(|manifest| &manifest.deps) {
            let Some(dir) = dep.source.as_deref().filter(|_| dep.path) else {
                continue;
            };
            if let Some(reached) = workspace
                .packages
                .iter()
                .position(|package| package.dir == dir)
                && !found.contains(&reached)
            {
                found.push(reached);
            }
        }
    }
    found.sort();
    found
}

/// Whether a scope of lines lies in a file.
fn holds_lines(scope: &Scope, path: &str) -> bool {
    matches!(scope, Scope::Lines(scoped, _) if scoped == path)
}

/// Whether a diagnostic lies in one of the packages, or in no file.
fn within(workspace: &Workspace, packages: &[usize], diagnostic: &Diagnostic) -> bool {
    let Some(path) = diagnostic.path.as_deref() else {
        return true;
    };
    packages
        .iter()
        .any(|index| path.starts_with(&format!("{}/", workspace.packages[*index].dir)))
}
