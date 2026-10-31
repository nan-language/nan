//! The `nan` command line: reading it, running the command it names, and printing the result.

mod api;
mod build;
mod check;
mod dep;
mod diff;
mod edit;
mod key;
mod merge;
mod publish;
mod read;
mod seal;
mod test;
mod upgrade;
mod verify;

use std::fs;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use crate::diagnostic::{Code, Diagnostic, Role};
use crate::fix;
use crate::journal;
use crate::output::{self, Format, Output, Record, Verdict};
use crate::scope::Scope;
use crate::toolchain;
use crate::workspace::{self, Workspace};

pub use build::Launch;

/// What a run printed and the status it ends with, or for `nan run` of a built program, the
/// program to start in its place: the image, or the emulator booting it.
pub struct Run {
    pub output: String,
    pub status: i32,
    pub launch: Option<Launch>,
}

/// What a command ends with: its output, the program `nan run` starts, or the status `nan run`
/// ends with when it cannot start one.
enum Outcome {
    Output(Output),
    Launch(Launch),
    Ended(i32),
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
    Test,
    ApiUpdate,
    DepAdd,
    DepSync,
    DepApprove,
    Verify,
    Publish,
    Merge,
    Upgrade,
    Diff,
    Seal,
    KeyNew,
    KeyShow,
    KeyTrust,
}

impl Name {
    /// The command named by the words a command line starts with, and how many it takes.
    fn read(args: &[String]) -> Option<(Name, usize)> {
        let second = args.get(1).map(String::as_str);
        let pair = match (args.first()?.as_str(), second) {
            ("edit", Some("replace")) => Some(Name::EditReplace),
            ("edit", Some("rename")) => Some(Name::EditRename),
            ("edit", Some("move")) => Some(Name::EditMove),
            ("api", Some("update")) => Some(Name::ApiUpdate),
            ("dep", Some("add")) => Some(Name::DepAdd),
            ("dep", Some("sync")) => Some(Name::DepSync),
            ("dep", Some("approve")) => Some(Name::DepApprove),
            ("key", Some("new")) => Some(Name::KeyNew),
            ("key", Some("show")) => Some(Name::KeyShow),
            ("key", Some("trust")) => Some(Name::KeyTrust),
            ("edit" | "api" | "dep" | "key", _) => return None,
            _ => None,
        };
        if let Some(name) = pair {
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
            "test" => Name::Test,
            "verify" => Name::Verify,
            "publish" => Name::Publish,
            "merge" => Name::Merge,
            "upgrade" => Name::Upgrade,
            "diff" => Name::Diff,
            "seal" => Name::Seal,
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
            Name::Test => "test",
            Name::ApiUpdate => "api update",
            Name::DepAdd => "dep add",
            Name::DepSync => "dep sync",
            Name::DepApprove => "dep approve",
            Name::Verify => "verify",
            Name::Publish => "publish",
            Name::Merge => "merge",
            Name::Upgrade => "upgrade",
            Name::Diff => "diff",
            Name::Seal => "seal",
            Name::KeyNew => "key new",
            Name::KeyShow => "key show",
            Name::KeyTrust => "key trust",
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
            | Name::KeyNew
            | Name::KeyShow
            | Name::KeyTrust => (&[], &[]),
            Name::Run => (&["--keep-resets"], &[]),
            Name::Build => (&[], &["--compare-with"]),
            Name::EditReplace => (&[], &["--expect", "--from"]),
            Name::Show => (&["--api"], &[]),
            Name::Find => (&["--contributors"], &[]),
            Name::Fix => (&["--propagate"], &[]),
            Name::Test => (&["--affected"], &["--replay"]),
            Name::ApiUpdate | Name::DepAdd | Name::DepSync | Name::DepApprove | Name::Verify => {
                (&[], &[])
            }
            Name::Publish => (&[], &["--local", "--name", "--as"]),
            Name::Merge => (&[], &["--path"]),
            Name::Upgrade => (&[], &["--language"]),
            Name::Diff => (&["--green"], &["--path", "--agent"]),
            Name::Seal => (&[], &["--as"]),
        }
    }

    /// The options taking every argument after them up to the next option.
    fn listing(self) -> &'static [&'static str] {
        match self {
            Name::ApiUpdate => &["--confirm-docs"],
            Name::Publish => &["--breaking", "--author"],
            _ => &[],
        }
    }

    /// The most operands the command takes.
    fn operands(self) -> usize {
        match self {
            Name::Find | Name::EditRename | Name::EditMove | Name::DepAdd | Name::KeyTrust => 2,
            Name::Merge => 3,
            Name::Diff => 2,
            _ => 1,
        }
    }

    /// The option of its form taking no scope and no other option of its own.
    fn alone(self) -> Option<&'static str> {
        match self {
            Name::Check => Some("--prefix"),
            Name::Test => Some("--replay"),
            _ => None,
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
                | Name::DepAdd
                | Name::Publish
                | Name::Merge
                | Name::Diff
                | Name::Seal
                | Name::KeyNew
                | Name::KeyShow
                | Name::KeyTrust
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
                | Name::Test
                | Name::ApiUpdate
                | Name::DepAdd
                | Name::DepSync
                | Name::DepApprove
                | Name::Verify
                | Name::Publish
                | Name::Merge
                | Name::Upgrade
                | Name::Seal
                | Name::KeyTrust
        )
    }

    /// The names of the counts its summary gives.
    fn counts(self) -> &'static [&'static str] {
        match self {
            Name::Check | Name::Fix => &["errors", "incomplete"],
            Name::Test | Name::Verify => &["errors", "incomplete", "tests", "failed"],
            Name::Fmt
            | Name::EditReplace
            | Name::EditRename
            | Name::EditMove
            | Name::Build
            | Name::Run
            | Name::ApiUpdate
            | Name::DepAdd
            | Name::DepSync
            | Name::DepApprove
            | Name::Publish
            | Name::Upgrade
            | Name::Seal
            | Name::KeyNew
            | Name::KeyTrust => &["files", "errors", "incomplete"],
            Name::Find
            | Name::Holes
            | Name::Show
            | Name::Read
            | Name::Explain
            | Name::Expand
            | Name::Primer
            | Name::Diff
            | Name::KeyShow => &["records"],
            Name::Context => &["entries", "radius"],
            Name::Merge => &["files", "conflicts", "errors"],
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

    /// The values a listing option was given, in order.
    fn listed(&self, option: &str) -> Vec<&str> {
        self.values
            .iter()
            .filter(|(given, _)| *given == option)
            .map(|(_, value)| *value)
            .collect()
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
        let alone = self
            .name
            .alone()
            .is_some_and(|option| self.value(option).is_some());
        if self.name.scoped() && self.scope().is_none() && !alone {
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
        Ok(Ok(Outcome::Ended(status))) => {
            return Run {
                output: String::new(),
                status,
                launch: None,
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
            _ if name.listing().contains(&arg) && invocation.listed(arg).is_empty() => {
                let listed: Vec<&String> = rest[index + 1..]
                    .iter()
                    .take_while(|value| !value.starts_with("--"))
                    .collect();
                if listed.is_empty() {
                    return None;
                }
                index += listed.len();
                invocation
                    .values
                    .extend(listed.into_iter().map(|value| (arg, value.as_str())));
            }
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
    let alone = name
        .alone()
        .is_some_and(|option| invocation.value(option).is_some());
    let others = invocation.operands.len() + invocation.flags.len() + invocation.values.len();
    (!alone || others == 1).then_some(invocation)
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
    if invocation.name == Name::Merge {
        return merge::run(invocation, current, trace).map(Outcome::Output);
    }
    if invocation.name == Name::KeyNew {
        return key::new(invocation).map(Outcome::Output);
    }
    if invocation.name == Name::KeyShow {
        return key::show(invocation).map(Outcome::Output);
    }
    if invocation.name == Name::Diff
        && invocation.value("--path").is_none()
        && !invocation.flag("--green")
    {
        return diff::workspaces(invocation, current, trace).map(Outcome::Output);
    }
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
        Name::Test => test::run(invocation, &workspace, current, trace),
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
        Name::ApiUpdate => api::update(invocation, &workspace, current, trace),
        Name::DepAdd => dep::add(invocation, &workspace, current, trace),
        Name::DepSync => dep::sync(invocation, &workspace, current, trace),
        Name::DepApprove => dep::approve(invocation, &workspace, current, trace),
        Name::Verify => verify::run(invocation, &workspace, current, trace),
        Name::Publish => publish::run(invocation, &workspace, current, trace),
        Name::Merge => unreachable!("merge runs outside a workspace"),
        Name::Upgrade if invocation.value("--language").is_some() => {
            upgrade::language(invocation, &workspace, current, trace)
        }
        Name::Upgrade => dep::upgrade(invocation, &workspace, current, trace),
        Name::Diff => diff::run(invocation, &workspace, current, trace),
        Name::Seal => seal::run(invocation, &workspace, current, trace),
        Name::KeyTrust => key::trust(invocation, &workspace, current, trace),
        Name::KeyNew | Name::KeyShow => {
            unreachable!("key new and key show run outside a workspace")
        }
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

/// The file a reason lies in: a `diagnostic` reason's in a release the workspace's locks read,
/// any other's in the workspace, and otherwise in std.
fn reason_file<'a>(
    workspace: &'a Workspace,
    role: Role,
    path: &str,
) -> (&'a workspace::Package, &'a workspace::File) {
    let local = match role {
        Role::Diagnostic => workspace
            .release_file(path)
            .or_else(|| workspace.file(path)),
        _ => workspace
            .file(path)
            .or_else(|| workspace.release_file(path)),
    };
    local
        .or_else(|| crate::stdlib::workspace().file(path))
        .expect("reasons lie in workspace, release or std files")
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
        let text = workspace
            .line_text(path, line)
            .expect("diagnostics lie in workspace files");
        lines.push(output::printable(text));
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
        let (package, file) = reason_file(workspace, reason.role, path);
        let item = package.item_at(file, *line).map_or_else(
            || "-".to_owned(),
            |item| written_for(&item.canonical(), writer),
        );
        lines.push(format!(
            "because {} {path}:{line} {item} {}",
            reason.role.word(),
            reason.subject
        ));
        lines.push(output::printable(file.syntax.line_text(*line)));
    }
    lines.extend(fix::shown(workspace, &diagnostic.fixes));
    Record {
        word: diagnostic.code.class().word().to_owned(),
        fields: vec![diagnostic.code.name().to_owned(), location, item],
        lines,
    }
}

#[cfg(test)]
mod tests;
