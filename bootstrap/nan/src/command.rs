//! The `nan` command line: reading it, running the command it names, and printing the result.

use std::fs;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use crate::diagnostic::{Class, Code, Diagnostic};
use crate::format;
use crate::journal;
use crate::output::{self, Format, Output, Record, Verdict};
use crate::scope::Scope;
use crate::toolchain;
use crate::workspace::{self, Workspace};

/// What a run printed and the status it ends with.
pub struct Run {
    pub output: String,
    pub status: i32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Name {
    Check,
    Fmt,
}

impl Name {
    fn word(self) -> &'static str {
        match self {
            Name::Check => "check",
            Name::Fmt => "fmt",
        }
    }
}

/// A command line this implementation runs.
struct Invocation<'a> {
    name: Name,
    scope: Option<&'a str>,
    args: &'a [String],
    format: Format,
}

impl Invocation<'_> {
    /// The command line without `nan`, an omitted scope written after the name, without `--skip`
    /// unless `skip` keeps it.
    fn command(&self, scope: &str, skip: bool) -> String {
        let mut words = vec![self.name.word().to_owned()];
        if self.scope.is_none() {
            words.push(output::argument(scope));
        }
        let mut args = self.args.iter();
        while let Some(arg) = args.next() {
            if arg == "--skip" && !skip {
                args.next();
                continue;
            }
            words.push(output::argument(arg));
        }
        words.join(" ")
    }
}

/// What a command has learnt by the time it may fail: its workspace root, its scope as the
/// verdict line writes it, and the workspace files it read.
#[derive(Default)]
struct Trace {
    root: Option<PathBuf>,
    scope: Option<String>,
    read: Vec<String>,
}

pub fn run(args: &[String], current: &Path) -> Run {
    let Some(invocation) = read(args) else {
        return usage(args);
    };
    let mut trace = Trace::default();
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        execute(&invocation, current, &mut trace)
    }));
    let output = match outcome {
        Ok(Ok(output)) => output,
        Ok(Err(_)) | Err(_) => internal_error(&invocation, args, &trace),
    };
    Run {
        output: output.render(&invocation.format),
        status: output.verdict.status(),
    }
}

/// The output of a command whose implementation failed, after writing its reproducer.
fn internal_error(invocation: &Invocation, args: &[String], trace: &Trace) -> Output {
    let scope = trace.scope.as_deref().or(invocation.scope).unwrap_or(".");
    let reproducer = trace
        .root
        .as_deref()
        .and_then(|root| reproducer(root, args, &trace.read).ok());
    let lines = reproducer
        .into_iter()
        .map(|path| format!("reproducer {path}"))
        .collect();
    Output {
        command: invocation.command(scope, true),
        elision: invocation.command(scope, false),
        verdict: Verdict::InternalError,
        summary: vec![("records", 1)],
        records: vec![unlocated(Code::InternalInvariant, lines)],
    }
}

/// Writes `.nan/repro/<n>` for the least positive `n` naming no directory: the command line, the
/// toolchain hashes and every workspace file the command read. Returns its path.
fn reproducer(root: &Path, args: &[String], read: &[String]) -> io::Result<String> {
    let repro = root.join(".nan").join("repro");
    fs::create_dir_all(&repro)?;
    let number = (1..)
        .find(|number: &u32| !repro.join(number.to_string()).exists())
        .expect("a free number");
    let dir = repro.join(number.to_string());
    fs::create_dir(&dir)?;
    fs::write(
        dir.join("command"),
        args.iter()
            .map(|arg| format!("{arg}\n"))
            .collect::<String>(),
    )?;
    let hashes: String = toolchain::LANGUAGES
        .iter()
        .map(|language| format!("{}\n", toolchain::hash(*language).full()))
        .collect();
    fs::write(dir.join("toolchain"), hashes)?;
    for path in read {
        let target = dir.join(path);
        fs::create_dir_all(target.parent().expect("a file lies in a directory"))?;
        fs::copy(root.join(path), target)?;
    }
    Ok(format!(".nan/repro/{number}"))
}

/// Reads the command line, or `None` when it is outside every synopsis this implementation runs.
fn read(args: &[String]) -> Option<Invocation<'_>> {
    let (name, rest) = args.split_first()?;
    let name = match name.as_str() {
        "check" => Name::Check,
        "fmt" => Name::Fmt,
        _ => return None,
    };
    let mut format = Format {
        json: false,
        budget: None,
        skip: 0,
    };
    let mut scope = None;
    let mut index = 0;
    while index < rest.len() {
        let arg = rest[index].as_str();
        match arg {
            "--json" => format.json = true,
            "--budget" => {
                index += 1;
                format.budget = Some(number(rest.get(index)?).filter(|budget| *budget >= 100)?);
            }
            "--skip" => {
                index += 1;
                format.skip = number(rest.get(index)?)?;
            }
            _ if arg.starts_with("--") || scope.is_some() => return None,
            _ => scope = Some(arg),
        }
        index += 1;
    }
    Some(Invocation {
        name,
        scope,
        args: rest,
        format,
    })
}

fn number(arg: &str) -> Option<usize> {
    arg.bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| arg.parse().ok())
        .flatten()
}

/// The output of a command line outside every synopsis: `tool.usage`.
fn usage(args: &[String]) -> Run {
    let command: Vec<String> = args.iter().map(|arg| output::argument(arg)).collect();
    let command = command.join(" ");
    let summary = match args.first().map(String::as_str) {
        Some("check") => vec![("errors", 1), ("incomplete", 0)],
        Some("fmt") => vec![("files", 0), ("errors", 1), ("incomplete", 0)],
        _ => vec![("records", 1)],
    };
    let output = Output {
        command: command.clone(),
        elision: command,
        verdict: Verdict::Invalid,
        summary,
        records: vec![unlocated(Code::ToolUsage, Vec::new())],
    };
    Run {
        output: output.render(&Format {
            json: args.iter().any(|arg| arg == "--json"),
            budget: None,
            skip: 0,
        }),
        status: Verdict::Invalid.status(),
    }
}

fn execute(invocation: &Invocation, current: &Path, trace: &mut Trace) -> io::Result<Output> {
    let tool_error = |code: Code, scope: &str| Output {
        command: invocation.command(scope, true),
        elision: invocation.command(scope, false),
        verdict: Verdict::Invalid,
        summary: summary(invocation.name, 0, 1, 0),
        records: vec![unlocated(code, Vec::new())],
    };
    let root = workspace::root(current);
    if !workspace::holds_package(&root)? {
        return Ok(tool_error(Code::ToolNoWorkspace, "."));
    }
    trace.root = Some(root.clone());
    let _lock = journal::Lock::acquire(&root, invocation.name == Name::Fmt)?;
    let workspace = Workspace::load(root, &mut trace.read)?;
    let Some((scope, written)) = Scope::resolve(&workspace, current, invocation.scope) else {
        return Ok(tool_error(
            Code::ToolUnknownScope,
            invocation.scope.unwrap_or("."),
        ));
    };
    trace.scope = Some(written.clone());
    let diagnostics = diagnostics(&workspace, &scope);
    let mut records: Vec<Record> = diagnostics
        .iter()
        .map(|diagnostic| record(&workspace, &scope, diagnostic))
        .collect();
    let errors = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code.class() == Class::Error)
        .count();
    let incomplete = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code.class() == Class::Incompleteness)
        .count();
    let mut files = 0;
    if invocation.name == Name::Fmt {
        let written = format::workspace(&workspace, &scope);
        journal::write(&workspace.root, &written)?;
        files = written.len();
        records.extend(written.iter().map(|(path, _)| Record {
            word: "wrote".to_owned(),
            fields: vec![path.clone()],
            lines: Vec::new(),
        }));
    }
    Ok(Output {
        command: invocation.command(&written, true),
        elision: invocation.command(&written, false),
        verdict: if errors > 0 {
            Verdict::Failed
        } else {
            Verdict::Ok
        },
        summary: summary(invocation.name, files, errors, incomplete),
        records,
    })
}

fn summary(
    name: Name,
    files: usize,
    errors: usize,
    incomplete: usize,
) -> Vec<(&'static str, usize)> {
    match name {
        Name::Check => vec![("errors", errors), ("incomplete", incomplete)],
        Name::Fmt => vec![
            ("files", files),
            ("errors", errors),
            ("incomplete", incomplete),
        ],
    }
}

/// The diagnostics reported on lines of the scope, by path, line and code, each once.
fn diagnostics(workspace: &Workspace, scope: &Scope) -> Vec<Diagnostic> {
    let mut found: Vec<Diagnostic> = workspace.diagnostics().collect();
    for (_, file) in workspace.files() {
        found.extend(file.syntax.diagnostics.iter().map(|diagnostic| Diagnostic {
            expected: diagnostic.expected.clone(),
            found: diagnostic.found.clone(),
            ..Diagnostic::at(diagnostic.code, &file.path, diagnostic.line)
        }));
    }
    found.retain(|diagnostic| {
        diagnostic
            .path
            .as_deref()
            .is_none_or(|path| scope.holds(workspace, path, diagnostic.line))
    });
    found.sort_by(|left, right| {
        (&left.path, left.line, left.code.name()).cmp(&(&right.path, right.line, right.code.name()))
    });
    found.dedup();
    found
}

/// The record of a diagnostic without location or item.
fn unlocated(code: Code, lines: Vec<String>) -> Record {
    Record {
        word: code.class().word().to_owned(),
        fields: vec![code.name().to_owned(), "-".to_owned(), "-".to_owned()],
        lines,
    }
}

/// A diagnostic as its record prints it.
fn record(workspace: &Workspace, scope: &Scope, diagnostic: &Diagnostic) -> Record {
    let location = match (&diagnostic.path, diagnostic.line) {
        (Some(path), Some(line)) => format!("{path}:{line}"),
        (Some(path), None) => path.clone(),
        _ => "-".to_owned(),
    };
    let writer = scope.package(workspace).and_then(|package| package.name());
    let item = diagnostic
        .path
        .as_deref()
        .zip(diagnostic.line)
        .and_then(|(path, line)| workspace.item_at(path, line))
        .map_or_else(
            || "-".to_owned(),
            |path| writer.map_or_else(|| path.canonical(), |package| path.written_for(package)),
        );
    let mut lines = Vec::new();
    if let (Some(path), Some(line)) = (&diagnostic.path, diagnostic.line) {
        let (_, file) = workspace
            .file(path)
            .expect("diagnostics lie in workspace files");
        lines.push(output::printable(file.syntax.line_text(line)));
    }
    lines.extend(
        diagnostic
            .expected
            .iter()
            .map(|expected| format!("expected: {expected}")),
    );
    lines.extend(
        diagnostic
            .found
            .iter()
            .map(|found| format!("found: {found}")),
    );
    lines.extend(
        diagnostic
            .because
            .iter()
            .map(|reason| format!("because {} - - {}", reason.role.word(), reason.subject)),
    );
    Record {
        word: diagnostic.code.class().word().to_owned(),
        fields: vec![diagnostic.code.name().to_owned(), location, item],
        lines,
    }
}

#[cfg(test)]
mod tests;
