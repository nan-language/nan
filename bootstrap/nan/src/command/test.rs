//! `nan test`: checks its scope as `nan check` does and, when no diagnostic is an error, runs the
//! tests the scope selects, the affected ones or the run a replay token names, printing a `fail`
//! record per test failing on a run target; with `--as`, records each test's passes in its
//! package's passes file, signed when the package's principals file gives the principal a key.

use std::io;
use std::path::Path;

use super::{Invocation, Trace, apply, key, record, written_for};
use crate::cache::Cache;
use crate::check;
use crate::diagnostic::{Class, Code, Diagnostic, sort_diagnostics};
use crate::fix;
use crate::machine::passes::Pass;
use crate::machine::principals::is_principal;
use crate::output::{Output, Record, Verdict};
use crate::scope::Scope;
use crate::sign;
use crate::test_runs::{self, Failure, Passed, Report, Runs, Token};
use crate::workspace::Workspace;

pub fn run(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    let (located, runs) = match invocation.value("--replay") {
        Some(text) => {
            let named =
                Token::read(text).and_then(|token| Some((token.location(workspace)?, token)));
            let Some((location, token)) = named else {
                let diagnostic = Diagnostic::new(Code::TestReplayToken);
                let records = vec![record(workspace, &Scope::Workspace, &diagnostic)];
                return Ok(output(
                    invocation,
                    "",
                    records,
                    (0, 1, 0),
                    &Report::default(),
                ));
            };
            (Some(location), Runs::Replay(token))
        }
        None if invocation.flag("--affected") => (None, Runs::Affected),
        None => (None, Runs::All),
    };
    let principal = invocation.value("--as");
    let scoped = located.as_deref().or(invocation.scope());
    let Some((scope, written)) = Scope::resolve(workspace, current, scoped) else {
        return Ok(invocation.tool_error(Code::ToolUnknownScope, scoped.unwrap_or(".")));
    };
    trace.scope = Some(written.clone());
    let mut diagnostics = check::reported(workspace, &scope);
    fix::offer(workspace, current, scoped, &mut diagnostics);
    if principal.is_some_and(|principal| !is_principal(principal)) {
        diagnostics.push(Diagnostic::new(Code::SealBadPrincipal));
    }
    let mut report = Report::default();
    if !diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code.class() == Class::Error)
    {
        let ran = match Cache::open()? {
            Ok(cache) => test_runs::run(workspace, &scope, &runs, &cache, None)?,
            Err(exposed) => Err(exposed),
        };
        match ran {
            Ok(ran) => report = ran,
            Err(exposed) => diagnostics.push(Diagnostic {
                path: Some(exposed.0),
                ..Diagnostic::new(Code::BuildCacheExposed)
            }),
        }
    }
    diagnostics.append(&mut report.diagnostics);
    let mut changes = Vec::new();
    if let Some(principal) = principal {
        record_passes(
            workspace,
            principal,
            &report.passed,
            &mut changes,
            &mut diagnostics,
        )?;
    }
    diagnostics.extend(apply(workspace, &mut changes)?);
    sort_diagnostics(&mut diagnostics);
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
    records.extend(fail_records(workspace, &scope, &report.failures));
    records.extend(changes.iter().map(|(path, _)| Record {
        word: "wrote".to_owned(),
        fields: vec![path.clone()],
        lines: Vec::new(),
    }));
    Ok(output(
        invocation,
        &written,
        records,
        (changes.len(), errors, incomplete),
        &report,
    ))
}

/// Adds to `changes` the passes file of each package holding a test that passed, recording its
/// passes by `principal`, signed by the principal's secret key when the package's principals file
/// gives the principal a key, leaving out a file left unchanged and adding to `diagnostics` the
/// diagnostic of a key the user directory lacks in place of the file.
fn record_passes(
    workspace: &Workspace,
    principal: &str,
    passed: &[Passed],
    changes: &mut Vec<(String, Option<Vec<u8>>)>,
    diagnostics: &mut Vec<Diagnostic>,
) -> io::Result<()> {
    for package in &workspace.packages {
        let held: Vec<&Passed> = passed
            .iter()
            .filter(|passed| passed.dir == package.dir)
            .collect();
        if held.is_empty() {
            continue;
        }
        let path = format!("{}/.nan/passes.nan", package.dir);
        let seed = match key::signer(package.machine.principals(), principal)? {
            Ok(seed) => seed,
            Err(diagnostic) => {
                diagnostics.push(diagnostic);
                continue;
            }
        };
        let mut passes = package.machine.passes().clone();
        for passed in held {
            let message = sign::pass_message(
                principal,
                &passed.test_path,
                passed.target.name(),
                passed.reach,
            );
            passes = passes.recording(Pass {
                test_path: passed.test_path.clone(),
                target: passed.target.name().to_owned(),
                principal: principal.to_owned(),
                reach: passed.reach,
                signature: seed.map(|seed| sign::sign(&seed, &message)),
            });
        }
        let bytes = passes.bytes();
        if bytes.as_deref() != package.machine.bytes(&path) {
            changes.push((path, bytes));
        }
    }
    Ok(())
}

/// A `fail` record per failing test.
pub fn fail_records(workspace: &Workspace, scope: &Scope, failures: &[Failure]) -> Vec<Record> {
    let writer = scope.package(workspace).and_then(|package| package.name());
    failures
        .iter()
        .map(|failure| Record {
            word: "fail".to_owned(),
            fields: vec![
                written_for(&failure.item, writer),
                format!("{}:{}", failure.path, failure.line),
                failure.target.name().to_owned(),
                failure.cause.clone(),
            ],
            lines: failure.lines.clone(),
        })
        .collect()
}

/// The output of a test command: its records, its counts of files written, errors and
/// incompleteness, and what its runs found.
fn output(
    invocation: &Invocation,
    written: &str,
    records: Vec<Record>,
    (files, errors, incomplete): (usize, usize, usize),
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
            ("files", files),
            ("errors", errors),
            ("incomplete", incomplete),
            ("tests", tests),
            ("failed", failed),
        ],
        records,
    }
}
