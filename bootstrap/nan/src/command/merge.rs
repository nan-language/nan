//! `nan merge <base> <ours> <theirs> [--path <path>]`: the merge driver, merging three versions
//! of the workspace, or with `--path` of one file, into `<ours>`.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{Invocation, Trace, record};
use crate::check;
use crate::diagnostic::{Class, Code, Diagnostic};
use crate::merge::merge;
use crate::output::{Output, Record, Verdict};
use crate::scope::Scope;
use crate::workspace::{self, Workspace};

pub fn run(invocation: &Invocation, current: &Path, trace: &mut Trace) -> io::Result<Output> {
    let written = invocation.args.join(" ");
    let [base, ours, theirs] = invocation.operands[..] else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    trace.scope = Some(written.clone());
    let (base, ours_path, theirs) = (current.join(base), current.join(ours), current.join(theirs));
    let mut diagnostics = Vec::new();
    let mut checked: Vec<Record> = Vec::new();
    let mut files = 0;
    let mut wrote = Vec::new();
    match invocation.value("--path") {
        Some(path) => {
            let read = |file: &Path| fs::read(file).ok();
            let versions = [read(&base), read(&ours_path), read(&theirs)];
            let merged = merge(
                path,
                versions[0].as_deref(),
                versions[1].as_deref(),
                versions[2].as_deref(),
            );
            diagnostics.extend(
                merged
                    .conflicts
                    .iter()
                    .map(|line| Diagnostic::at(Code::MergeConflict, path, *line)),
            );
            let root = workspace::root(current);
            if workspace::holds_package(&root)? {
                let workspace = Workspace::load(root, &mut Vec::new())?;
                if workspace.file(path).is_some() {
                    let version = |content: Option<&Vec<u8>>| {
                        let source = content.cloned().unwrap_or_default();
                        workspace.with_sources(&HashMap::from([(path.to_owned(), source)]))
                    };
                    let [base, ours, theirs] =
                        versions.each_ref().map(|content| version(content.as_ref()));
                    let result = version(merged.content.as_ref());
                    for diagnostic in
                        check::semantic_conflicts(&base, &ours, &theirs, &result, Some(path))
                    {
                        checked.push(record(&result, &Scope::Workspace, &diagnostic));
                        diagnostics.push(diagnostic);
                    }
                }
            }
            replace(&ours_path, merged.content.as_deref())?;
            files = 1;
            wrote.push(ours.to_owned());
        }
        None => {
            let versions = [&base, &ours_path, &theirs]
                .into_iter()
                .map(|root| match workspace::holds_package(root) {
                    Ok(true) => Workspace::load(root.clone(), &mut Vec::new()).map(Some),
                    Ok(false) => Ok(None),
                    Err(error) => Err(error),
                })
                .collect::<io::Result<Vec<Option<Workspace>>>>()?;
            let mut paths = Vec::new();
            for root in [&base, &ours_path, &theirs] {
                walk(root, "", &mut paths)?;
            }
            paths.sort();
            paths.dedup();
            for path in &paths {
                let read = |root: &Path| fs::read(root.join(path)).ok();
                let (b, o, t) = (read(&base), read(&ours_path), read(&theirs));
                let merged = merge(path, b.as_deref(), o.as_deref(), t.as_deref());
                diagnostics.extend(
                    merged
                        .conflicts
                        .iter()
                        .map(|line| Diagnostic::at(Code::MergeConflict, path, *line)),
                );
                if merged.content != o {
                    replace(&ours_path.join(path), merged.content.as_deref())?;
                    files += 1;
                    wrote.push(path.clone());
                }
            }
            let merged = Workspace::load(ours_path.clone(), &mut Vec::new())?;
            let mut found = check::reported(&merged, &Scope::Workspace);
            if let [Some(base), Some(ours), Some(theirs)] = &versions[..] {
                found.extend(check::semantic_conflicts(base, ours, theirs, &merged, None));
            }
            for diagnostic in found {
                checked.push(record(&merged, &Scope::Workspace, &diagnostic));
                diagnostics.push(diagnostic);
            }
        }
    }
    let conflicts = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == Code::MergeConflict)
        .count();
    let errors = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code.class() == Class::Error)
        .count();
    let records: Vec<Record> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == Code::MergeConflict)
        .map(conflict_record)
        .chain(checked)
        .chain(wrote.iter().map(|path| Record {
            word: "wrote".to_owned(),
            fields: vec![path.clone()],
            lines: Vec::new(),
        }))
        .collect();
    Ok(Output {
        command: invocation.command(&written, true),
        elision: invocation.command(&written, false),
        verdict: if errors > 0 {
            Verdict::Failed
        } else {
            Verdict::Ok
        },
        summary: vec![
            ("files", files),
            ("conflicts", conflicts),
            ("errors", errors),
        ],
        records,
    })
}

/// The record of a conflict, which lies in a version outside any workspace and quotes no line.
fn conflict_record(diagnostic: &Diagnostic) -> Record {
    let location = match (&diagnostic.path, diagnostic.line) {
        (Some(path), Some(line)) => format!("{path}:{line}"),
        _ => "-".to_owned(),
    };
    Record {
        word: diagnostic.code.class().word().to_owned(),
        fields: vec![diagnostic.code.name().to_owned(), location, "-".to_owned()],
        lines: Vec::new(),
    }
}

/// Collects the files under `dir`, relative to the version's root, skipping `.git` and the
/// root's `.nan`, which holds what commands build.
fn walk(root: &Path, dir: &str, found: &mut Vec<String>) -> io::Result<()> {
    let absolute: PathBuf = root.join(dir);
    let Ok(entries) = fs::read_dir(&absolute) else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = if dir.is_empty() {
            name.clone()
        } else {
            format!("{dir}/{name}")
        };
        let kind = entry.file_type()?;
        let skipped = name == ".git" || (dir.is_empty() && name == ".nan");
        if kind.is_dir() && !skipped {
            walk(root, &path, found)?;
        } else if kind.is_file() {
            found.push(path);
        }
    }
    Ok(())
}

/// Replaces a file whole with new content, renaming a new file into place, or deletes it.
fn replace(path: &Path, content: Option<&[u8]>) -> io::Result<()> {
    match content {
        Some(content) => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let fresh = path.with_file_name(format!(".{name}.{}.new", std::process::id()));
            fs::write(&fresh, content)?;
            fs::rename(fresh, path)
        }
        None => match fs::remove_file(path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        },
    }
}

#[cfg(test)]
mod tests;
