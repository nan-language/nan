//! The `nan` command line: reading it, running the command it names, and printing the result.

mod build;
mod check;
mod edit;
mod read;

use std::fs;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use crate::diagnostic::{Code, Diagnostic};
use crate::fix;
use crate::journal;
use crate::output::{self, Format, Output, Record, Verdict};
use crate::scope::Scope;
use crate::toolchain;
use crate::workspace::{self, Workspace};

pub use build::Launch;

/// What a run printed and the status it ends with, or for `nan run` of a built program, the
/// image to start in its place.
pub struct Run {
    pub output: String,
    pub status: i32,
    pub launch: Option<Launch>,
}

/// What a command ends with: its output, or the image `nan run` starts.
enum Outcome {
    Output(Output),
    Launch(Launch),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Name {
    Check,
    Fix,
    Fmt,
    Find,
    Holes,
    Show,
    Read,
    Context,
    Explain,
    Expand,
    EditReplace,
    EditRename,
    EditMove,
    Primer,
    Build,
    Run,
}

impl Name {
    /// The command named by the words a command line starts with, and how many it takes.
    fn read(args: &[String]) -> Option<(Name, usize)> {
        if args.first()? == "edit" {
            let name = match args.get(1)?.as_str() {
                "replace" => Name::EditReplace,
                "rename" => Name::EditRename,
                "move" => Name::EditMove,
                _ => return None,
            };
            return Some((name, 2));
        }
        let name = match args.first()?.as_str() {
            "check" => Name::Check,
            "fix" => Name::Fix,
            "fmt" => Name::Fmt,
            "find" => Name::Find,
            "holes" => Name::Holes,
            "show" => Name::Show,
            "read" => Name::Read,
            "context" => Name::Context,
            "explain" => Name::Explain,
            "expand" => Name::Expand,
            "primer" => Name::Primer,
            "build" => Name::Build,
            "run" => Name::Run,
            _ => return None,
        };
        Some((name, 1))
    }

    /// The command's name words.
    fn words(self) -> &'static str {
        match self {
            Name::Check => "check",
            Name::Fix => "fix",
            Name::Fmt => "fmt",
            Name::Find => "find",
            Name::Holes => "holes",
            Name::Show => "show",
            Name::Read => "read",
            Name::Context => "context",
            Name::Explain => "explain",
            Name::Expand => "expand",
            Name::EditReplace => "edit replace",
            Name::EditRename => "edit rename",
            Name::EditMove => "edit move",
            Name::Primer => "primer",
            Name::Build => "build",
            Name::Run => "run",
        }
    }

    /// The flags the command accepts, and its options taking a value.
    fn options(self) -> (&'static [&'static str], &'static [&'static str]) {
        match self {
            Name::Check => (&["--radius"], &["--prefix", "--agent"]),
            Name::Fmt
            | Name::Holes
            | Name::Read
            | Name::Context
            | Name::Explain
            | Name::Expand
            | Name::EditRename
            | Name::EditMove
            | Name::Primer
            | Name::Build
            | Name::Run => (&[], &[]),
            Name::EditReplace => (&[], &["--expect", "--from"]),
            Name::Show => (&["--api"], &[]),
            Name::Find => (&["--contributors"], &[]),
            Name::Fix => (&["--propagate"], &[]),
        }
    }

    /// The most operands the command takes.
    fn operands(self) -> usize {
        match self {
            Name::Find | Name::EditRename | Name::EditMove => 2,
            _ => 1,
        }
    }

    /// Whether its first operand is a scope, which may be omitted.
    fn scoped(self) -> bool {
        !matches!(
            self,
            Name::Find
                | Name::Show
                | Name::Read
                | Name::Context
                | Name::Explain
                | Name::Expand
                | Name::EditReplace
                | Name::EditRename
                | Name::EditMove
                | Name::Primer
        )
    }

    /// Whether the command writes source or machine files.
    fn writes(self) -> bool {
        matches!(
            self,
            Name::Fix
                | Name::Fmt
                | Name::EditReplace
                | Name::EditRename
                | Name::EditMove
                | Name::Build
                | Name::Run
        )
    }

    /// The names of the counts its summary gives.
    fn counts(self) -> &'static [&'static str] {
        match self {
            Name::Check | Name::Fix => &["errors", "incomplete"],
            Name::Fmt
            | Name::EditReplace
            | Name::EditRename
            | Name::EditMove
            | Name::Build
            | Name::Run => &["files", "errors", "incomplete"],
            Name::Find
            | Name::Holes
            | Name::Show
            | Name::Read
            | Name::Explain
            | Name::Expand
            | Name::Primer => &["records"],
            Name::Context => &["entries", "radius"],
        }
    }
}

/// A command line this implementation runs.
struct Invocation<'a> {
    name: Name,
    /// The arguments after the name words.
    args: &'a [String],
    operands: Vec<&'a str>,
    /// The arguments after `--`, which `nan run` passes to the program.
    passed: &'a [String],
    flags: Vec<&'a str>,
    values: Vec<(&'a str, &'a str)>,
    format: Format,
}

impl Invocation<'_> {
    /// The scope operand, when the command takes one first.
    fn scope(&self) -> Option<&str> {
        self.operands
            .first()
            .copied()
            .filter(|_| self.name.scoped())
    }

    fn flag(&self, flag: &str) -> bool {
        self.flags.contains(&flag)
    }

    /// The value given to an option.
    fn value(&self, option: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(given, _)| *given == option)
            .map(|(_, value)| *value)
    }

    /// The names of the counts its summary gives: prefix checking's own, or the command's.
    fn counts(&self) -> &'static [&'static str] {
        if self.value("--prefix").is_some() {
            &["dead", "pending", "holes"]
        } else {
            self.name.counts()
        }
    }

    /// The command line without `nan`, an omitted scope written after the name words, without
    /// `--skip` unless `skip` keeps it.
    fn command(&self, scope: &str, skip: bool) -> String {
        let mut words = vec![self.name.words().to_owned()];
        if self.name.scoped() && self.scope().is_none() && self.value("--prefix").is_none() {
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

    /// The output of a command that reported a `tool` error and did nothing else.
    fn tool_error(&self, code: Code, scope: &str) -> Output {
        let summary = self
            .counts()
            .iter()
            .map(|count| (*count, usize::from(matches!(*count, "errors" | "records"))))
            .collect();
        Output {
            command: self.command(scope, true),
            elision: self.command(scope, false),
            verdict: Verdict::Invalid,
            summary,
            records: vec![unlocated(code, Vec::new())],
        }
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
        Ok(Ok(Outcome::Output(output))) => output,
        Ok(Ok(Outcome::Launch(launch))) => {
            return Run {
                output: String::new(),
                status: 0,
                launch: Some(launch),
            };
        }
        Ok(Err(_)) | Err(_) => internal_error(&invocation, args, &trace),
    };
    Run {
        output: output.render(&invocation.format),
        status: output.verdict.status(),
        launch: None,
    }
}

/// The output of a command whose implementation failed, after writing its reproducer.
fn internal_error(invocation: &Invocation, args: &[String], trace: &Trace) -> Output {
    let scope = trace.scope.as_deref().or(invocation.scope()).unwrap_or(".");
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
    let (name, taken) = Name::read(args)?;
    let rest = &args[taken..];
    let (flags, valued) = name.options();
    let mut invocation = Invocation {
        name,
        args: rest,
        operands: Vec::new(),
        passed: &[],
        flags: Vec::new(),
        values: Vec::new(),
        format: Format {
            json: false,
            budget: None,
            skip: 0,
        },
    };
    let mut index = 0;
    while index < rest.len() {
        let arg = rest[index].as_str();
        match arg {
            "--" if name == Name::Run => {
                invocation.passed = &rest[index + 1..];
                break;
            }
            "--json" => invocation.format.json = true,
            "--budget" => {
                index += 1;
                let budget = number(rest.get(index)?).filter(|budget| *budget >= 100)?;
                invocation.format.budget = Some(budget);
            }
            "--skip" => {
                index += 1;
                invocation.format.skip = number(rest.get(index)?)?;
            }
            _ if flags.contains(&arg) && !invocation.flag(arg) => invocation.flags.push(arg),
            _ if valued.contains(&arg)
                && invocation.values.iter().all(|(option, _)| *option != arg) =>
            {
                index += 1;
                invocation.values.push((arg, rest.get(index)?.as_str()));
            }
            _ if arg.starts_with("--") || invocation.operands.len() == name.operands() => {
                return None;
            }
            _ => invocation.operands.push(arg),
        }
        index += 1;
    }
    Some(invocation)
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
    let counts = Name::read(args).map_or(&["records"][..], |(name, _)| name.counts());
    let output = Output {
        command: command.clone(),
        elision: command,
        verdict: Verdict::Invalid,
        summary: counts
            .iter()
            .map(|count| (*count, usize::from(matches!(*count, "errors" | "records"))))
            .collect(),
        records: vec![unlocated(Code::ToolUsage, Vec::new())],
    };
    Run {
        output: output.render(&Format {
            json: args.iter().any(|arg| arg == "--json"),
            budget: None,
            skip: 0,
        }),
        status: Verdict::Invalid.status(),
        launch: None,
    }
}

fn execute(invocation: &Invocation, current: &Path, trace: &mut Trace) -> io::Result<Outcome> {
    let root = workspace::root(current);
    if !workspace::holds_package(&root)? {
        return Ok(Outcome::Output(
            invocation.tool_error(Code::ToolNoWorkspace, "."),
        ));
    }
    trace.root = Some(root.clone());
    let _lock = journal::Lock::acquire(&root, invocation.name.writes())?;
    let workspace = Workspace::load(root, &mut trace.read)?;
    let output = match invocation.name {
        Name::Build | Name::Run => return build::run(invocation, &workspace, current, trace),
        Name::Check | Name::Fix | Name::Fmt => check::run(invocation, &workspace, current, trace),
        Name::Holes => read::holes(invocation, &workspace, current, trace),
        Name::Find => read::find(invocation, &workspace, current),
        Name::Show | Name::Read => read::show(invocation, &workspace, current, trace),
        Name::Context => read::context(invocation, &workspace, current, trace),
        Name::Explain => read::explain(invocation, &workspace, current, trace),
        Name::Expand => read::expand(invocation, &workspace, current, trace),
        Name::EditReplace => edit::replace(invocation, &workspace, current, trace),
        Name::EditRename => edit::rename(invocation, &workspace, current, trace),
        Name::EditMove => edit::move_item(invocation, &workspace, current, trace),
        Name::Primer => Ok(read::primer(invocation)),
    };
    output.map(Outcome::Output)
}

/// A canonical path as records write it for the scope's package `writer`: without the package
/// name when it lies in another module of that package.
fn written_for(path: &str, writer: Option<&str>) -> String {
    let rest = writer.and_then(|package| path.strip_prefix(package)?.strip_prefix("::"));
    match rest {
        Some(rest)
            if rest
                .find("::")
                .is_some_and(|at| !rest[..at].contains(['.', '['])) =>
        {
            rest.to_owned()
        }
        _ => path.to_owned(),
    }
}

/// The item holding a line of a workspace file as records write it for the scope's package: by
/// the canonical path `known` when the checker gave one, else by the path its syntax gives.
fn item_written(
    workspace: &Workspace,
    writer: Option<&str>,
    path: &str,
    line: u32,
    known: Option<&str>,
) -> String {
    known
        .map(str::to_owned)
        .or_else(|| workspace.item_at(path, line).map(|item| item.canonical()))
        .map_or_else(|| "-".to_owned(), |item| written_for(&item, writer))
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
    let item = match (&diagnostic.path, diagnostic.line) {
        (Some(path), Some(line)) => {
            item_written(workspace, writer, path, line, diagnostic.item.as_deref())
        }
        _ => "-".to_owned(),
    };
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
            .map(|expected| format!("expected: {}", output::printable(expected.as_bytes()))),
    );
    lines.extend(
        diagnostic
            .found
            .iter()
            .map(|found| format!("found: {}", output::printable(found.as_bytes()))),
    );
    if let Some((origin, quoted)) = &diagnostic.envelope {
        lines.push(format!("envelope {origin} {}", quoted.len()));
        lines.extend(
            quoted
                .iter()
                .map(|line| format!("| {}", output::printable(line.as_bytes()))),
        );
    }
    for reason in &diagnostic.because {
        let Some((path, line)) = &reason.at else {
            lines.push(format!(
                "because {} - - {}",
                reason.role.word(),
                reason.subject
            ));
            continue;
        };
        let home = match workspace.file(path) {
            Some(_) => workspace,
            None => crate::stdlib::workspace(),
        };
        let item = item_written(home, writer, path, *line, None);
        lines.push(format!(
            "because {} {path}:{line} {item} {}",
            reason.role.word(),
            reason.subject
        ));
        let (_, file) = home
            .file(path)
            .expect("reasons lie in workspace or std files");
        lines.push(output::printable(file.syntax.line_text(*line)));
    }
    for (number, fix) in diagnostic.fixes.iter().enumerate() {
        for edit in fix::edits(workspace, fix) {
            lines.push(format!("fix {} {}", number + 1, edit.path));
            let changed = |sign: char, line: &String| {
                if line.is_empty() {
                    sign.to_string()
                } else {
                    format!("{sign} {}", output::printable(line.as_bytes()))
                }
            };
            lines.extend(edit.old.iter().map(|line| changed('-', line)));
            lines.extend(edit.new.iter().map(|line| changed('+', line)));
        }
    }
    Record {
        word: diagnostic.code.class().word().to_owned(),
        fields: vec![diagnostic.code.name().to_owned(), location, item],
        lines,
    }
}

#[cfg(test)]
mod tests;
