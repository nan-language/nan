//! `nan test`: checks its scope as `nan check` does and, when no diagnostic is an error, runs the
//! tests the scope selects, the affected ones or the run a replay token names, printing a `fail`
//! record per test failing on a run target.

use std::io;
use std::path::Path;

use super::{Invocation, Trace, record, written_for};
use crate::cache::Cache;
use crate::check;
use crate::diagnostic::{Class, Code, Diagnostic};
use crate::fix;
use crate::output::{Output, Record, Verdict};
use crate::scope::Scope;
use crate::test_runs::{self, Report, Runs, Token};
use crate::workspace::Workspace;

pub fn run(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    let (located, runs) = match invocation.value("--replay") {
        Some(_) if invocation.scope().is_some() || invocation.flag("--affected") => {
            return Ok(invocation.tool_error(Code::ToolUsage, "."));
        }
        Some(text) => {
            let named =
                Token::read(text).and_then(|token| Some((token.location(workspace)?, token)));
            let Some((location, token)) = named else {
                let diagnostic = Diagnostic::new(Code::TestReplayToken);
                let records = vec![record(workspace, &Scope::Workspace, &diagnostic)];
                return Ok(output(invocation, "", records, (1, 0), &Report::default()));
            };
            (Some(location), Runs::Replay(token))
        }
        None if invocation.flag("--affected") => (None, Runs::Affected),
        None => (None, Runs::All),
    };
    let scoped = located.as_deref().or(invocation.scope());
    let Some((scope, written)) = Scope::resolve(workspace, current, scoped) else {
        return Ok(invocation.tool_error(Code::ToolUnknownScope, scoped.unwrap_or(".")));
    };
    trace.scope = Some(written.clone());
    let mut diagnostics = check::reported(workspace, &scope);
    fix::offer(workspace, current, scoped, &mut diagnostics);
    let count = |class: Class| {
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code.class() == class)
            .count()
    };
    let (mut errors, incomplete) = (count(Class::Error), count(Class::Incompleteness));
    let mut records: Vec<Record> = diagnostics
        .iter()
        .map(|diagnostic| record(workspace, &scope, diagnostic))
        .collect();
    let mut report = Report::default();
    if errors == 0 {
        let ran = match Cache::open()? {
            Ok(cache) => test_runs::run(workspace, &scope, &runs, &cache)?,
            Err(exposed) => Err(exposed),
        };
        match ran {
            Ok(ran) => report = ran,
            Err(exposed) => {
                let diagnostic = Diagnostic {
                    path: Some(exposed.0),
                    ..Diagnostic::new(Code::BuildCacheExposed)
                };
                records.push(record(workspace, &scope, &diagnostic));
                errors += 1;
            }
        }
    }
    let writer = scope.package(workspace).and_then(|package| package.name());
    records.extend(report.failures.iter().map(|failure| Record {
        word: "fail".to_owned(),
        fields: vec![
            written_for(&failure.item, writer),
            format!("{}:{}", failure.path, failure.line),
            failure.target.name().to_owned(),
            failure.cause.clone(),
        ],
        lines: failure.lines.clone(),
    }));
    Ok(output(
        invocation,
        &written,
        records,
        (errors, incomplete),
        &report,
    ))
}

/// The output of a test command: its records, its counts of errors and incompleteness, and what
/// its runs found.
fn output(
    invocation: &Invocation,
    written: &str,
    records: Vec<Record>,
    (errors, incomplete): (usize, usize),
    report: &Report,
) -> Output {
    let (tests, failed) = (report.tests, report.failures.len());
    Output {
        command: invocation.command(written, true),
        elision: invocation.command(written, false),
        verdict: if errors > 0 || failed > 0 {
            Verdict::Failed
        } else {
            Verdict::Ok
        },
        summary: vec![
            ("errors", errors),
            ("incomplete", incomplete),
            ("tests", tests),
            ("failed", failed),
        ],
        records,
    }
}
