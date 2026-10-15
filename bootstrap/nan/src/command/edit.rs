//! The editing commands: `nan edit replace`, `nan edit rename` and `nan edit move`, which write their
//! change through the journal only when the workspace with it reports no error it did not report
//! before.

use std::fs;
use std::io::{self, Read};
use std::path::Path;

use super::{Invocation, Trace, record, written_for};
use crate::check::{self, Relocated};
use crate::diagnostic::{Code, Diagnostic, Fix, Reason, Replacement, Role};
use crate::fix;
use crate::hash;
use crate::journal;
use crate::output::{self, Output, Record, Verdict};
use crate::scope::Scope;
use crate::syntax::file::{SourceFile, parse};
use crate::syntax::token::TokenKind;
use crate::workspace::Workspace;

/// `nan edit replace <item> --expect <hash> --from <file>`: replaces the item with the item the file
/// holds while its item hash is the one expected, its identity stays and its lines check.
pub fn replace(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    let written = invocation.args.join(" ");
    let ([operand], Some(expected), Some(from)) = (
        &invocation.operands[..],
        invocation.value("--expect").and_then(hash::parse_form),
        invocation.value("--from"),
    ) else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    let Some((scope, written)) = Scope::resolve(workspace, current, Some(operand)) else {
        return Ok(invocation.tool_error(Code::ToolUnknownScope, operand));
    };
    let Scope::Lines(path, _) = &scope else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    trace.scope = Some(written.clone());
    let [shown] = &check::shown(workspace, &scope)[..] else {
        return Ok(invocation.tool_error(Code::ToolUnknownScope, &written));
    };
    let writer = scope.package(workspace).and_then(|package| package.name());
    let item = shown
        .path
        .as_deref()
        .map_or_else(|| "-".to_owned(), |item| written_for(item, writer));
    let location = format!("{path}:{}", shown.line);
    let (_, file) = workspace.file(path).expect("items lie in workspace files");
    let error = |code: Code, because: Vec<Reason>| {
        let diagnostic = Diagnostic {
            because,
            ..Diagnostic::at(code, path, shown.line)
        };
        record(workspace, &scope, &diagnostic)
    };
    if shown.hashes.item.0[..expected.len()] != expected[..] {
        let mut records = vec![error(Code::EditStale, Vec::new())];
        records.push(Record {
            word: "item".to_owned(),
            fields: vec![
                item,
                location,
                shown.hashes.item.short(),
                shown
                    .hashes
                    .interface
                    .map_or_else(|| "-".to_owned(), |hash| hash.short()),
            ],
            lines: shown
                .lines
                .clone()
                .map(|line| output::printable(file.syntax.line_text(line)))
                .collect(),
        });
        return Ok(edited(invocation, &written, records, 0));
    }
    let mut text = Vec::new();
    let read = if from == "-" {
        io::stdin().read_to_end(&mut text).map(|_| ())
    } else {
        fs::read(current.join(from)).map(|read| text = read)
    };
    if read.is_err() {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    }
    if !text.is_empty() && !text.ends_with(b"\n") {
        text.push(b'\n');
    }
    if !one_item(&parse(text.clone())) {
        return Ok(edited(
            invocation,
            &written,
            vec![error(Code::EditNotOneItem, Vec::new())],
            0,
        ));
    }
    let first = *shown.lines.start();
    let fix = Fix(vec![(
        path.clone(),
        Replacement {
            start: file.syntax.lines[first as usize - 1].start,
            end: line_after(&file.syntax, *shown.lines.end()),
            text: String::from_utf8_lossy(&text).into_owned(),
        },
    )]);
    let changed = fix::apply(workspace, &[&fix]);
    let after = workspace.with_sources(&changed);
    if check::identity(workspace, path, shown.line) != check::identity(&after, path, first) {
        let because = vec![Reason {
            role: Role::Command,
            subject: "nan edit rename".to_owned(),
            at: None,
        }];
        return Ok(edited(
            invocation,
            &written,
            vec![error(Code::EditIdentityChanged, because)],
            0,
        ));
    }
    let new_lines = first..=first + text.iter().filter(|byte| **byte == b'\n').count() as u32 - 1;
    let before = check::reported(workspace, &Scope::Workspace);
    let rechecked = check::reported(&after, &Scope::Workspace);
    let (failing, elsewhere): (Vec<Diagnostic>, Vec<Diagnostic>) =
        fix::introduced(&before, &fix, &rechecked)
            .into_iter()
            .partition(|diagnostic| {
                diagnostic.path.as_deref() == Some(path.as_str())
                    && diagnostic
                        .line
                        .is_some_and(|line| new_lines.contains(&line))
            });
    if !failing.is_empty() {
        let mut records = vec![error(Code::EditCheckFailed, Vec::new())];
        records.extend(
            failing
                .iter()
                .map(|diagnostic| record(&after, &scope, diagnostic)),
        );
        return Ok(edited(invocation, &written, records, 0));
    }
    let wrote: Vec<(String, Vec<u8>)> = changed.into_iter().collect();
    journal::write(&workspace.root, &wrote)?;
    let replaced = check::shown(&after, &Scope::Lines(path.clone(), new_lines));
    let mut records: Vec<Record> = replaced
        .iter()
        .map(|shown| Record {
            word: "replaced".to_owned(),
            fields: vec![
                item.clone(),
                format!("{path}:{}", shown.line),
                shown.hashes.item.short(),
            ],
            lines: Vec::new(),
        })
        .collect();
    records.extend(
        elsewhere
            .iter()
            .map(|diagnostic| record(&after, &scope, diagnostic)),
    );
    records.extend(wrote_records(&wrote));
    Ok(edited(invocation, &written, records, wrote.len()))
}

/// `nan edit rename <item> <name>`: renames an item or member, its declaration, labels and every
/// reference of the workspace, writing the rename tombstone a field or variant of a type deriving
/// `Encode` needs.
pub fn rename(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    let written = invocation.args.join(" ");
    let [operand, name] = invocation.operands[..] else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    let Some((scope, written)) = Scope::resolve(workspace, current, Some(operand)) else {
        return Ok(invocation.tool_error(Code::ToolUnknownScope, operand));
    };
    let Scope::Lines(path, lines) = &scope else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    trace.scope = Some(written.clone());
    let Some(fix) = check::renamed(workspace, path, lines, name) else {
        return Ok(invocation.tool_error(Code::ToolUnknownScope, &written));
    };
    write_checked(
        invocation,
        workspace,
        &scope,
        &written,
        (path, *lines.start()),
        &fix,
        &[],
    )
}

/// `nan edit move <item> <module>`: moves an item, with the functions named after it and the impl
/// lines naming it in its module, to another module of its package.
pub fn move_item(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    let written = invocation.args.join(" ");
    let [operand, module] = invocation.operands[..] else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    let Some((scope, written)) = Scope::resolve(workspace, current, Some(operand)) else {
        return Ok(invocation.tool_error(Code::ToolUnknownScope, operand));
    };
    let Scope::Lines(path, lines) = &scope else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    trace.scope = Some(written.clone());
    match check::moved(workspace, path, lines, module) {
        None => Ok(invocation.tool_error(Code::ToolUnknownScope, &written)),
        Some(Err(())) => {
            let diagnostic = Diagnostic::at(Code::EditMoveInvalid, path, *lines.start());
            let records = vec![record(workspace, &scope, &diagnostic)];
            Ok(edited(invocation, &written, records, 0))
        }
        Some(Ok(moved)) => write_checked(
            invocation,
            workspace,
            &scope,
            &written,
            (path, *lines.start()),
            &moved.fix,
            &moved.relocated,
        ),
    }
}

/// Writes an edit's files when the workspace with it reports no error it did not report before,
/// the lines of `relocated` items counting as the lines they left, and otherwise reports
/// `edit.check-failed` at `at`, followed by those errors.
fn write_checked(
    invocation: &Invocation,
    workspace: &Workspace,
    scope: &Scope,
    written: &str,
    (path, line): (&str, u32),
    fix: &Fix,
    relocated: &[Relocated],
) -> io::Result<Output> {
    let changed = fix::apply(workspace, &[fix]);
    let after = workspace.with_sources(&changed);
    let before = check::reported(workspace, &Scope::Workspace);
    let mut introduced = fix::introduced(&before, fix, &check::reported(&after, &Scope::Workspace));
    introduced.retain(|diagnostic| {
        let left = relocated.iter().find_map(|moved| {
            let line = diagnostic.line?;
            (diagnostic.path.as_deref() == Some(moved.to.0.as_str())
                && (moved.to.1..moved.to.1 + moved.lines).contains(&line))
            .then(|| (moved.from.0.as_str(), moved.from.1 + line - moved.to.1))
        });
        left.is_none_or(|(path, line)| {
            !before.iter().any(|earlier| {
                earlier.code == diagnostic.code
                    && earlier.path.as_deref() == Some(path)
                    && earlier.line == Some(line)
            })
        })
    });
    if !introduced.is_empty() {
        let mut records = vec![record(
            workspace,
            scope,
            &Diagnostic::at(Code::EditCheckFailed, path, line),
        )];
        records.extend(
            introduced
                .iter()
                .map(|diagnostic| record(&after, scope, diagnostic)),
        );
        return Ok(edited(invocation, written, records, 0));
    }
    let wrote: Vec<(String, Vec<u8>)> = changed.into_iter().collect();
    journal::write(&workspace.root, &wrote)?;
    Ok(edited(
        invocation,
        written,
        wrote_records(&wrote),
        wrote.len(),
    ))
}

fn wrote_records(wrote: &[(String, Vec<u8>)]) -> Vec<Record> {
    let mut paths: Vec<&String> = wrote.iter().map(|(path, _)| path).collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| Record {
            word: "wrote".to_owned(),
            fields: vec![path.clone()],
            lines: Vec::new(),
        })
        .collect()
}

/// The output of an editing command printing `records`, of which those headed `error` are its
/// errors, having written `files` files.
fn edited(invocation: &Invocation, written: &str, records: Vec<Record>, files: usize) -> Output {
    let errors = records
        .iter()
        .filter(|record| record.word == "error")
        .count();
    let incomplete = records
        .iter()
        .filter(|record| record.word == "incomplete")
        .count();
    Output {
        command: invocation.command(written, true),
        elision: invocation.command(written, false),
        verdict: if errors > 0 {
            Verdict::Failed
        } else {
            Verdict::Ok
        },
        summary: vec![
            ("files", files),
            ("errors", errors),
            ("incomplete", incomplete),
        ],
        records,
    }
}

/// Whether a text is exactly one item starting at column 0.
fn one_item(file: &SourceFile) -> bool {
    let [item] = &file.items[..] else {
        return false;
    };
    item.tree.is_some()
        && file.summary.is_none()
        && file.tokens[item.first].kind == TokenKind::Col0
        && file.tokens[item.end..]
            .iter()
            .all(|token| token.kind.is_virtual())
}

/// The offset of the line after `line`, or the end of the source.
fn line_after(file: &SourceFile, line: u32) -> u32 {
    file.lines
        .get(line as usize)
        .map_or(file.source.len() as u32, |next| next.start)
}

#[cfg(test)]
mod tests;
