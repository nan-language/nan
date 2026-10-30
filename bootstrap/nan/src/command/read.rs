//! The reading commands, which print what check time knows of a scope and no diagnostic.

use std::io;
use std::path::Path;

use super::{Invocation, Name, Trace, item_written, written_for};
use crate::check;
use crate::check::ledger::Kind;
use crate::diagnostic::Code;
use crate::output::{self, Output, Record, Verdict};
use crate::scope::Scope;
use crate::workspace::Workspace;

/// The output of a reading command printing `records` for the scope written `written`.
fn listed_output(invocation: &Invocation, written: &str, records: Vec<Record>) -> Output {
    Output {
        command: invocation.command(written, true),
        elision: invocation.command(written, false),
        verdict: Verdict::Ok,
        summary: vec![("records", records.len())],
        records,
    }
}

/// `nan holes`: a record per hole of the scope, holding its source line and its producers.
pub fn holes(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    let Some((scope, written)) = Scope::resolve(workspace, current, invocation.scope()) else {
        return Ok(invocation.tool_error(Code::ToolUnknownScope, invocation.scope().unwrap_or(".")));
    };
    trace.scope = Some(written.clone());
    let writer = scope.package(workspace).and_then(|package| package.name());
    let records = check::holes(workspace)
        .into_iter()
        .filter(|hole| scope.holds(workspace, &hole.path, Some(hole.line)))
        .map(|hole| {
            let item = item_written(workspace, writer, &hole.path, hole.line, None);
            let (_, file) = workspace
                .file(&hole.path)
                .expect("holes lie in workspace files");
            let producers = format!("producers: {}", hole.producers.join(", "));
            Record {
                word: "hole".to_owned(),
                fields: vec![format!("{}:{}", hole.path, hole.line), item, hole.expected],
                lines: vec![
                    output::printable(file.syntax.line_text(hole.line)),
                    producers.trim_end().to_owned(),
                ],
            }
        })
        .collect();
    Ok(listed_output(invocation, &written, records))
}

/// `nan show <scope>`: a `module` record holding the summary of a module or package scope, then a
/// record per item of the scope holding its text, with its item and interface hashes; with
/// `--api`, a record per item of the scope that consumers see; a canonical path naming nothing in
/// the workspace names a std item. `nan read <location>`: the record of the item holding that
/// line.
pub fn show(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    let written = invocation.args.join(" ");
    let operand = match invocation.operands[..] {
        [operand] if invocation.name != Name::Read || is_location(operand) => operand,
        _ => return Ok(invocation.tool_error(Code::ToolUsage, &written)),
    };
    let std = crate::stdlib::workspace();
    let shown_in = Scope::resolve(workspace, current, Some(operand))
        .map(|resolved| (workspace, resolved))
        .or_else(|| Some((std, (Scope::canonical(std, operand)?, operand.to_owned()))));
    let Some((workspace, (scope, written))) = shown_in else {
        return Ok(invocation.tool_error(Code::ToolUnknownScope, operand));
    };
    trace.scope = Some(written.clone());
    let writer = scope.package(workspace).and_then(|package| package.name());
    if invocation.flag("--api") {
        let records = check::api(workspace, &scope)
            .into_iter()
            .map(|api| Record {
                word: "api".to_owned(),
                fields: vec![
                    written_for(&api.path, writer),
                    format!("{}:{}", api.file, api.line),
                ],
                lines: api.lines,
            })
            .collect();
        return Ok(listed_output(invocation, &written, records));
    }
    let mut records: Vec<Record> = summary(workspace, &scope).into_iter().collect();
    for shown in check::shown(workspace, &scope) {
        let (_, file) = workspace
            .file(&shown.file)
            .expect("items lie in workspace files");
        records.push(Record {
            word: "item".to_owned(),
            fields: vec![
                shown
                    .path
                    .map_or_else(|| "-".to_owned(), |path| written_for(&path, writer)),
                format!("{}:{}", shown.file, shown.line),
                shown.hashes.item.short(),
                shown
                    .hashes
                    .interface
                    .map_or_else(|| "-".to_owned(), |hash| hash.short()),
            ],
            lines: shown
                .lines
                .map(|line| output::printable(file.syntax.line_text(line)))
                .collect(),
        });
    }
    Ok(listed_output(invocation, &written, records))
}

/// `nan context <item or location>`: the context pack of the item, or of the line: the item's
/// text, its closure set's manifest directives, its module's names and a record per item, member
/// and expression type checking it, or that line, reads.
pub fn context(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    let written = invocation.args.join(" ");
    let [operand] = invocation.operands[..] else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    let Some((scope, written)) = Scope::resolve(workspace, current, Some(operand)) else {
        return Ok(invocation.tool_error(Code::ToolUnknownScope, operand));
    };
    if !matches!(scope, Scope::Lines(..)) {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    }
    trace.scope = Some(written.clone());
    let line = is_location(operand)
        .then(|| operand.rsplit_once(':')?.1.parse().ok())
        .flatten();
    let [pack] = &check::contexts(workspace, &scope, line)[..] else {
        return Ok(invocation.tool_error(Code::ToolUnknownScope, &written));
    };
    let writer = scope.package(workspace).and_then(|package| package.name());
    let records = pack_records(workspace, pack, writer);
    Ok(Output {
        command: invocation.command(&written, true),
        elision: invocation.command(&written, false),
        verdict: Verdict::Ok,
        summary: vec![
            ("entries", records.len()),
            ("radius", output::units_of(&records)),
        ],
        records,
    })
}

/// `nan explain <item or package>`: of a function, its digest lines as `digest:` records, a `fact`
/// record per line raising a digest fact and a `method` record per capability method it
/// reaches; of an item, its `ledger` records and its `radius`; of a package, its items' ledger
/// records.
pub fn explain(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    let written = invocation.args.join(" ");
    let [operand] = invocation.operands[..] else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    let Some((scope, written)) = Scope::resolve(workspace, current, Some(operand)) else {
        return Ok(invocation.tool_error(Code::ToolUnknownScope, operand));
    };
    trace.scope = Some(written.clone());
    let mut records = Vec::new();
    match scope {
        Scope::Package(_) => records.extend(ledger_records(workspace, &scope)),
        Scope::Lines(..) => {
            if let Some(explained) = check::explained(workspace, &scope) {
                for digest in explained.digests {
                    let mut words = digest.split(' ').map(str::to_owned);
                    records.push(Record {
                        word: words.next().unwrap_or_default(),
                        fields: words.collect(),
                        lines: Vec::new(),
                    });
                }
                for (fact, path, line) in explained.facts {
                    let (_, file) = workspace.file(&path).expect("items lie in workspace files");
                    records.push(Record {
                        word: "fact".to_owned(),
                        fields: vec![fact, format!("{path}:{line}")],
                        lines: vec![output::printable(file.syntax.line_text(line))],
                    });
                }
                records.extend(explained.methods.into_iter().map(|method| Record {
                    word: "method".to_owned(),
                    fields: vec![method],
                    lines: Vec::new(),
                }));
                for obligation in explained.obligations {
                    let (_, file) = workspace
                        .file(&obligation.path)
                        .expect("items lie in workspace files");
                    let mut lines = vec![output::printable(file.syntax.line_text(obligation.line))];
                    lines.extend(obligation.counterexample);
                    records.push(Record {
                        word: "obligation".to_owned(),
                        fields: vec![
                            format!("{}:{}", obligation.path, obligation.line),
                            obligation.kind,
                            obligation.status,
                        ],
                        lines,
                    });
                }
            }
            records.extend(ledger_records(workspace, &scope));
            let writer = scope.package(workspace).and_then(|package| package.name());
            if let [pack] = &check::contexts(workspace, &scope, None)[..] {
                records.push(Record {
                    word: "radius".to_owned(),
                    fields: vec![
                        output::units_of(&pack_records(workspace, pack, writer)).to_string(),
                    ],
                    lines: Vec::new(),
                });
            }
        }
        _ => return Ok(invocation.tool_error(Code::ToolUsage, &written)),
    }
    Ok(listed_output(invocation, &written, records))
}

/// A `ledger` record per ledger entry of a scope, holding its premise, condition, reason or
/// text, its kind followed by `escape` for a trust escape.
fn ledger_records(workspace: &Workspace, scope: &Scope) -> Vec<Record> {
    check::ledgered(workspace, scope)
        .into_iter()
        .map(|entry| {
            let mut fields = vec![entry.kind.word().to_owned()];
            if entry.escape {
                fields.push("escape".to_owned());
            }
            fields.push(format!("{}:{}", entry.path, entry.line));
            let lines = match entry.kind {
                Kind::Trusted | Kind::Assume | Kind::Todo => entry.texts,
                Kind::Waiver | Kind::Undo => entry.texts.into_iter().skip(1).collect(),
                _ => Vec::new(),
            };
            Record {
                word: "ledger".to_owned(),
                fields,
                lines: lines
                    .iter()
                    .map(|line| output::printable(line.as_bytes()))
                    .collect(),
            }
        })
        .collect()
}

/// `nan expand <type>`: an `item` record located `derived` per item the type's derives write,
/// holding its text.
pub fn expand(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    let written = invocation.args.join(" ");
    let [operand] = invocation.operands[..] else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    let Some((scope, written)) = Scope::resolve(workspace, current, Some(operand)) else {
        return Ok(invocation.tool_error(Code::ToolUnknownScope, operand));
    };
    trace.scope = Some(written.clone());
    let derived = match scope {
        Scope::Lines(..) => check::expanded(workspace, &scope),
        _ => None,
    };
    let Some(derived) = derived else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    let writer = scope.package(workspace).and_then(|package| package.name());
    let records = derived
        .into_iter()
        .map(|item| Record {
            word: "item".to_owned(),
            fields: vec![
                written_for(&item.path, writer),
                "derived".to_owned(),
                item.hashes.item.short(),
                item.hashes
                    .interface
                    .map_or_else(|| "-".to_owned(), |hash| hash.short()),
            ],
            lines: item.text.lines().map(str::to_owned).collect(),
        })
        .collect();
    Ok(listed_output(invocation, &written, records))
}

/// The records of a context pack.
pub fn pack_records(
    workspace: &Workspace,
    pack: &check::Context,
    writer: Option<&str>,
) -> Vec<Record> {
    let (_, file) = workspace
        .file(&pack.file)
        .expect("items lie in workspace files");
    let mut records = vec![
        Record {
            word: "item".to_owned(),
            fields: vec![
                written_for(&pack.path, writer),
                format!("{}:{}", pack.file, pack.line),
            ],
            lines: pack
                .lines
                .clone()
                .map(|line| output::printable(file.syntax.line_text(line)))
                .collect(),
        },
        Record {
            word: "manifest".to_owned(),
            fields: vec![pack.package.clone()],
            lines: pack.directives.clone(),
        },
        Record {
            word: "names".to_owned(),
            fields: vec![written_for(&pack.module, writer)],
            lines: vec![pack.names.join(", ")],
        },
    ];
    records.extend(pack.apis.iter().map(|api| Record {
        word: "api".to_owned(),
        fields: vec![
            written_for(&api.path, writer),
            format!("{}:{}", api.file, api.line),
        ],
        lines: if api.package == pack.package {
            api.lines.clone()
        } else {
            enveloped(&api.package, &api.lines)
        },
    }));
    records
}

/// Lines copied from another package, each maximal run of those holding a comment or a text,
/// byte or char literal written as an envelope.
fn enveloped(origin: &str, lines: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut run: Vec<String> = Vec::new();
    for line in lines.iter().chain([&String::new()]) {
        if line.contains(['"', '\'']) || line.contains("//") {
            run.push(format!("| {line}"));
            continue;
        }
        if !run.is_empty() {
            out.push(format!("envelope {origin} {}", run.len()));
            out.append(&mut run);
        }
        out.push(line.clone());
    }
    out.pop();
    out
}

/// Whether an argument is a location: a source file's path, `:` and a line number.
fn is_location(text: &str) -> bool {
    text.rsplit_once(':').is_some_and(|(path, line)| {
        path.ends_with(".nan") && !line.is_empty() && line.bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// The `module` record of a package or module scope, holding its module summary.
fn summary(workspace: &Workspace, scope: &Scope) -> Option<Record> {
    let (index, module) = match scope {
        Scope::Package(index) => {
            let package = &workspace.packages[*index];
            let root = package
                .modules
                .iter()
                .position(|module| module.dirs.iter().any(|dir| dir.path == package.dir))?;
            (*index, root)
        }
        Scope::Module(index, module) => (*index, *module),
        _ => return None,
    };
    let package = &workspace.packages[index];
    let name = package.name()?;
    let module = &package.modules[module];
    let dir = &module.dirs.first()?.path;
    let path = if *dir == package.dir {
        name.to_owned()
    } else {
        format!("{name}::{}", module.name)
    };
    let (_, file) = workspace.file(&format!("{dir}/{}.nan", module.name))?;
    let summary = file.syntax.text(file.syntax.summary?);
    let text = summary["//!".len()..].trim_start_matches(' ');
    Some(Record {
        word: "module".to_owned(),
        fields: vec![written_for(&path, Some(name))],
        lines: vec![output::printable(text.as_bytes())],
    })
}

/// `nan find <name>`: a record per item or member of that folded name, holding its first line;
/// with `--contributors`, a record per contribution to a registry in the dependency closure of a
/// package, judged with it as judging root.
pub fn find(invocation: &Invocation, workspace: &Workspace, current: &Path) -> io::Result<Output> {
    let written = invocation.args.join(" ");
    let writer = Scope::resolve(workspace, current, None)
        .and_then(|(scope, _)| scope.package(workspace).and_then(|package| package.name()));
    let short = |path: String| written_for(&path, writer);
    if invocation.flag("--contributors") {
        let [registry, rest @ ..] = &invocation.operands[..] else {
            return Ok(invocation.tool_error(Code::ToolUsage, &written));
        };
        let root = match rest.first() {
            Some(name) => workspace
                .packages
                .iter()
                .position(|package| package.name() == Some(name)),
            None => workspace
                .packages
                .iter()
                .position(|package| current.starts_with(workspace.root.join(&package.dir))),
        };
        let Some(listed) = root.and_then(|root| check::contributed(workspace, root, registry))
        else {
            return Ok(invocation.tool_error(Code::ToolUnknownScope, &written));
        };
        let records = listed
            .into_iter()
            .map(|contribution| Record {
                word: "contribution".to_owned(),
                fields: vec![
                    short(contribution.registry),
                    short(contribution.entry),
                    format!("{}:{}", contribution.file, contribution.line),
                    if contribution.admitted {
                        "admitted"
                    } else {
                        "not-admitted"
                    }
                    .to_owned(),
                ],
                lines: Vec::new(),
            })
            .collect();
        return Ok(listed_output(invocation, &written, records));
    }
    let [name] = &invocation.operands[..] else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    let records = check::declared(workspace, name)
        .into_iter()
        .map(|found| Record {
            word: "found".to_owned(),
            fields: vec![short(found.path), format!("{}:{}", found.file, found.line)],
            lines: vec![found.text],
        })
        .collect();
    Ok(listed_output(invocation, &written, records))
}

/// The primer chapter of the specification.
const PRIMER: &str = include_str!("../../../../docs/spec/39-primer.md");

/// The primer tiers: the word `nan primer` takes and the heading of its section.
const TIERS: [(&str, &str); 8] = [
    ("core", "Core"),
    ("low-level", "Low-level topic"),
    ("concurrency", "Concurrency topic"),
    ("kernel", "Kernel topic"),
    ("packages", "Packages and trust topic"),
    ("contracts", "Contracts topic"),
    ("components", "Components topic"),
    ("tools", "Tool card"),
];

/// `nan primer [<topic> or tools]`: the tier's section of the primer, each link outside code
/// replaced by its text.
pub fn primer(invocation: &Invocation) -> Output {
    let written = invocation.args.join(" ");
    let tier = match &invocation.operands[..] {
        [] => Some(TIERS[0]),
        [word] => TIERS
            .iter()
            .copied()
            .find(|(name, _)| name == word && *name != "core"),
        _ => None,
    };
    let Some((name, heading)) = tier else {
        return invocation.tool_error(Code::ToolUsage, &written);
    };
    let mut lines = printed(
        PRIMER
            .lines()
            .skip_while(|line| *line != format!("## {heading}"))
            .skip(1)
            .take_while(|line| !line.starts_with("## ")),
    );
    while lines.first().is_some_and(String::is_empty) {
        lines.remove(0);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let record = Record {
        word: "primer".to_owned(),
        fields: vec![name.to_owned()],
        lines,
    };
    listed_output(invocation, &written, vec![record])
}

/// Lines of the primer with each Markdown link outside code blocks replaced by its text.
fn printed<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut code = false;
    lines
        .map(|line| {
            code ^= line.starts_with("```");
            match code {
                true => line.to_owned(),
                false => unlinked(line),
            }
        })
        .collect()
}

/// A line with each Markdown link `[text](target)` outside code spans replaced by its text.
fn unlinked(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(at) = rest.find(['`', '[']) {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let kept = match rest.starts_with('`') {
            true => code_span(rest),
            false => match link(rest) {
                Some((text, length)) => {
                    out.push_str(&unlinked(text));
                    rest = &rest[length..];
                    continue;
                }
                None => 1,
            },
        };
        out.push_str(&rest[..kept]);
        rest = &rest[kept..];
    }
    out.push_str(rest);
    out
}

/// The length of the code span `text` starts with, or of its opening backticks when no run of
/// as many closes them.
fn code_span(text: &str) -> usize {
    let run = |from: usize| text[from..].len() - text[from..].trim_start_matches('`').len();
    let ticks = run(0);
    let mut at = ticks;
    while let Some(found) = text[at..].find('`') {
        let start = at + found;
        if run(start) == ticks {
            return start + ticks;
        }
        at = start + run(start);
    }
    ticks
}

/// The text of the link `text` starts with, whose brackets balance outside code spans, and the
/// link's length.
fn link(text: &str) -> Option<(&str, usize)> {
    let mut depth = 0;
    let mut at = 0;
    while at < text.len() {
        match text.as_bytes()[at] {
            b'`' => {
                at += code_span(&text[at..]);
                continue;
            }
            b'[' => depth += 1,
            b']' if depth == 1 => break,
            b']' => depth -= 1,
            _ => {}
        }
        at += 1;
    }
    let target = text.get(at + 1..)?.strip_prefix('(')?;
    let end = target.find(')')?;
    Some((&text[1..at], at + end + 3))
}

#[cfg(test)]
mod tests;
