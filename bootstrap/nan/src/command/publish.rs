//! `nan publish --local <dir>`: runs the release gate on the package in `<dir>`, moves the
//! tombstones of its source into its history, builds its artefact and appends it to the local
//! store as a named release whose parent is the history head.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::Path;

use super::test::fail_records;
use super::verify::judged;
use super::{Invocation, Trace, record};
use crate::cache::Cache;
use crate::check;
use crate::diagnostic::{Class as DiagnosticClass, Code, Diagnostic};
use crate::format;
use crate::journal;
use crate::machine::history::History;
use crate::output::{Output, Record, Verdict};
use crate::release;
use crate::scope::Scope;
use crate::store::{Append, Class, Entry, Store, Summary};
use crate::syntax::grammar::Rule;
use crate::syntax::items::Item;
use crate::workspace::{Member, Workspace};

pub fn run(
    invocation: &Invocation,
    workspace: &Workspace,
    _current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    let written = invocation.args.join(" ");
    let dir = invocation
        .value("--local")
        .unwrap_or_default()
        .trim_end_matches('/');
    let Some(index) = workspace
        .packages
        .iter()
        .position(|package| package.dir == dir)
    else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    let mut breaking = invocation.listed("--breaking");
    let reason = breaking.pop();
    if invocation.listed("--breaking").len() == 1 {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    }
    trace.scope = Some(written.clone());
    let package = &workspace.packages[index];
    let manifest = package
        .manifest
        .as_ref()
        .expect("a package with a manifest");
    let manifest_path = format!("{}/package.nan", package.dir);
    let (diagnostics, report) = judged(workspace, &[index])?;
    if !diagnostics.is_empty() || !report.failures.is_empty() {
        let mut records: Vec<Record> = diagnostics
            .iter()
            .map(|diagnostic| record(workspace, &Scope::Package(index), diagnostic))
            .collect();
        records.extend(fail_records(workspace, &Scope::Package(index), &report));
        return Ok(output(invocation, &written, records, &diagnostics, &[]));
    }
    let mut refused: Vec<Diagnostic> = manifest
        .deps
        .iter()
        .filter(|dep| dep.path)
        .map(|dep| Diagnostic::at(Code::VersionPathDependency, &manifest_path, dep.line))
        .collect();
    let cache = match Cache::open()? {
        Ok(cache) => cache,
        Err(exposed) => {
            refused.push(Diagnostic {
                path: Some(exposed.0),
                ..Diagnostic::new(Code::BuildCacheExposed)
            });
            return Ok(refusal(invocation, &written, workspace, index, &refused));
        }
    };
    let store = Store::new(cache.store());
    let entries = match store.entries()? {
        Ok(entries) => entries,
        Err(exposed) => {
            refused.push(Diagnostic {
                path: Some(exposed.0),
                ..Diagnostic::new(Code::BuildCacheExposed)
            });
            return Ok(refusal(invocation, &written, workspace, index, &refused));
        }
    };
    let history = package.machine.history();
    let parent: Option<&Entry> = match history.and_then(|history| history.head) {
        Some(head) => match entries.iter().find(|entry| entry.root() == head) {
            Some(entry) => Some(entry),
            None => {
                refused.push(Diagnostic::at(
                    Code::VersionParentMissing,
                    &manifest_path,
                    manifest.line,
                ));
                None
            }
        },
        None => None,
    };
    let name = invocation
        .value("--name")
        .map(str::to_owned)
        .or_else(|| history.map(|history| history.name.clone()))
        .unwrap_or_else(|| manifest.name.clone());
    if entries
        .iter()
        .any(|entry| entry.name.as_deref() == Some(name.as_str()) && entry.package != manifest.name)
    {
        refused.push(Diagnostic::at(
            Code::VersionStoreNameTaken,
            &manifest_path,
            manifest.line,
        ));
    }
    if !refused.is_empty() {
        return Ok(refusal(invocation, &written, workspace, index, &refused));
    }
    let Moved {
        changed,
        tombstones: moved,
    } = moved_tombstones(workspace, index);
    let mut modules: BTreeMap<String, Vec<String>> = history
        .map(|history| history.modules.iter().cloned().collect())
        .unwrap_or_default();
    for (module, tombstone) in moved {
        let held = modules.entry(module).or_default();
        if !held.contains(&tombstone) {
            held.push(tombstone);
        }
    }
    for held in modules.values_mut() {
        held.sort();
    }
    let history_path = format!("{}/.nan/history.nan", package.dir);
    let staged = History {
        name: name.clone(),
        head: history.and_then(|history| history.head),
        modules: modules.into_iter().collect(),
    };
    let mut files: HashMap<String, Vec<u8>> = changed.into_iter().collect();
    files.insert(history_path.clone(), staged.bytes().unwrap_or_default());
    let moved_workspace = workspace.rebuilt(&files, &mut HeldStore(&entries))?;
    let moved_package = &moved_workspace.packages[index];
    let Some(closure) = moved_workspace.closure_of(moved_package) else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    let built = check::root::build(&moved_workspace, closure, Member::Source(index));
    let exported = release::exported(&built.artefact);
    let parent_exported = parent
        .map(|entry| release::exported(&entry.artefact))
        .unwrap_or_default();
    let breaking: Vec<String> = breaking
        .iter()
        .map(|item| canonical(workspace, item))
        .collect();
    if breaking
        .iter()
        .any(|item| !exported.contains_key(item) || !parent_exported.contains_key(item))
    {
        let diagnostic = Diagnostic::at(Code::VersionBreakingTarget, &manifest_path, manifest.line);
        return Ok(refusal(
            invocation,
            &written,
            workspace,
            index,
            &[diagnostic],
        ));
    }
    let parent_summary = parent.and_then(|entry| entry.summary.as_ref());
    let epoch = |path: &str| {
        let kept = release::epoch(parent_summary, path);
        kept + u64::from(breaking.iter().any(|item| item == path))
    };
    let epochs = |path: &str| (release::epoch(parent_summary, path), epoch(path));
    let held = parent
        .map(|entry| release::forwards(&entry.artefact))
        .unwrap_or_default();
    let unpreserved = check::not_preserved(workspace, index, &parent_exported, &held, &exported);
    if !unpreserved.is_empty() {
        return Ok(refusal(
            invocation,
            &written,
            workspace,
            index,
            &unpreserved,
        ));
    }
    let compatible = check::compatible(&moved_workspace, index, &parent_exported, &exported);
    let classes = release::classes(
        &parent_exported,
        &exported,
        &epochs,
        &release::forwards(&built.artefact),
        &compatible,
    );
    let class = release::release_class(&classes, parent.is_some());
    let encode = check::encode_class(&moved_workspace, index, parent);
    let (functions, authority) = check::publish_summaries(&moved_workspace, closure, moved_package);
    let summary = Summary {
        class,
        encode,
        items: exported
            .keys()
            .map(|path| {
                let item_class = classes.get(path).copied().unwrap_or(Class::Additive);
                (path.clone(), item_class, epoch(path))
            })
            .collect(),
        bumps: breaking
            .iter()
            .map(|item| (item.clone(), reason.unwrap_or_default().to_owned()))
            .collect(),
        authority,
        functions,
    };
    let appended = store.append(Append {
        package: manifest.name.clone(),
        name: Some(name.clone()),
        parent: parent.map(Entry::root),
        summary: Some(summary),
        artefact: built.artefact,
        sources: built.sources,
    })?;
    let entry = match appended {
        Ok(entry) => entry,
        Err(exposed) => {
            let diagnostic = Diagnostic {
                path: Some(exposed.0),
                ..Diagnostic::new(Code::BuildCacheExposed)
            };
            return Ok(refusal(
                invocation,
                &written,
                workspace,
                index,
                &[diagnostic],
            ));
        }
    };
    let recorded = History {
        head: Some(entry.root()),
        ..staged
    };
    files.insert(history_path, recorded.bytes().unwrap_or_default());
    let mut changes: Vec<(String, Option<Vec<u8>>)> = files
        .into_iter()
        .filter(|(path, content)| workspace.bytes(path) != Some(content.as_slice()))
        .map(|(path, content)| (path, Some(content)))
        .collect();
    changes.sort();
    journal::apply(&workspace.root, &changes)?;
    let mut records = vec![Record {
        word: "published".to_owned(),
        fields: vec![
            name,
            entry.root().short(),
            class.word().to_owned(),
            encode.word().to_owned(),
        ],
        lines: Vec::new(),
    }];
    records.extend(changes.iter().map(|(path, _)| Record {
        word: "wrote".to_owned(),
        fields: vec![path.clone()],
        lines: Vec::new(),
    }));
    Ok(output(invocation, &written, records, &[], &changes))
}

/// The store a staged release's workspace reads: the entries already read.
struct HeldStore<'a>(&'a [Entry]);

impl crate::workspace::ReleaseStore for HeldStore<'_> {
    fn entry(&mut self, root: &crate::hash::Hash) -> io::Result<Option<Entry>> {
        Ok(self.0.iter().find(|entry| entry.root() == *root).cloned())
    }
}

/// The canonical path of an item named as commands name one.
fn canonical(workspace: &Workspace, named: &str) -> String {
    let scope = Scope::resolve(workspace, &workspace.root, Some(named));
    match scope {
        Some((Scope::Lines(path, lines), _)) => workspace
            .item_at(&path, *lines.start())
            .map_or_else(|| named.to_owned(), |item| item.canonical()),
        _ => named.to_owned(),
    }
}

/// The tombstones publishing moves out of a package's source: its source files without them, by
/// path, and each tombstone's module and text in canonical format.
struct Moved {
    changed: Vec<(String, Vec<u8>)>,
    tombstones: Vec<(String, String)>,
}

/// The tombstones of a package's source to move into its history: all but those keeping an
/// `Encode` key of a type deriving `Encode`.
fn moved_tombstones(workspace: &Workspace, index: usize) -> Moved {
    let package = &workspace.packages[index];
    let encoded = check::encode_key_tombstones(workspace, package);
    let mut changed = Vec::new();
    let mut moved = Vec::new();
    for file in package.files.iter().filter(|file| file.module.is_some()) {
        let syntax = &file.syntax;
        let mut removed: Vec<std::ops::RangeInclusive<u32>> = Vec::new();
        for (position, item) in syntax.items.iter().enumerate() {
            let Some(tree) = &item.tree else {
                continue;
            };
            let view = Item { file: syntax, tree };
            let retired = view
                .kind()
                .is_some_and(|(rule, _)| rule == Rule::RetiredItem);
            let lines = syntax.item_lines(item);
            if !retired || item.abandoned || encoded.contains(&(file.path.clone(), *lines.start()))
            {
                continue;
            }
            let module = package.modules[file.module.expect("a source file's module")]
                .name
                .clone();
            let text = format::item_text(syntax, &syntax.items[position]);
            moved.push((module, text));
            removed.push(lines);
        }
        if removed.is_empty() {
            continue;
        }
        let kept: Vec<&[u8]> = (1..=syntax.lines.len() as u32)
            .filter(|line| !removed.iter().any(|range| range.contains(line)))
            .map(|line| syntax.line_text(line))
            .collect();
        let mut text = kept.join(&b'\n');
        text.push(b'\n');
        let parsed = crate::syntax::file::parse(text);
        changed.push((file.path.clone(), format::source_file(&parsed, None)));
    }
    Moved {
        changed,
        tombstones: moved,
    }
}

/// The output of a refused publish: its diagnostics.
fn refusal(
    invocation: &Invocation,
    written: &str,
    workspace: &Workspace,
    index: usize,
    diagnostics: &[Diagnostic],
) -> Output {
    let records = diagnostics
        .iter()
        .map(|diagnostic| record(workspace, &Scope::Package(index), diagnostic))
        .collect();
    output(invocation, written, records, diagnostics, &[])
}

fn output(
    invocation: &Invocation,
    written: &str,
    records: Vec<Record>,
    diagnostics: &[Diagnostic],
    changes: &[(String, Option<Vec<u8>>)],
) -> Output {
    let count = |class: DiagnosticClass| {
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code.class() == class)
            .count()
    };
    let (errors, incomplete) = (
        count(DiagnosticClass::Error),
        count(DiagnosticClass::Incompleteness),
    );
    let refused = !diagnostics.is_empty() || records.iter().any(|record| record.word == "fail");
    Output {
        command: invocation.command(written, true),
        elision: invocation.command(written, false),
        verdict: if refused {
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
    }
}

#[cfg(test)]
mod tests;
