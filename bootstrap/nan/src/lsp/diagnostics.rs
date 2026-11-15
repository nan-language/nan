//! The diagnostics the server publishes, their exact fixes as quick fixes, and formatting.

use std::collections::BTreeMap;

use super::json::Json;
use super::{Server, text};
use crate::check;
use crate::diagnostic::{Class, Diagnostic, Fix, Replacement};
use crate::fix;
use crate::format;
use crate::scope::Scope;
use crate::syntax::file::parse;
use crate::workspace::Workspace;

/// The diagnostics `nan check` reports on the workspace package at `index`, each with the fixes
/// it offers, keeping the snapshot of its check in the build cache as `nan check` does.
pub fn of_package(workspace: &Workspace, index: usize) -> Vec<Diagnostic> {
    let scope = Scope::Package(index);
    let checkers = check::checked_scope(workspace, &scope);
    let mut diagnostics = check::reported_by(workspace, &checkers, &scope);
    for (_, checker) in &checkers {
        checker.keep();
    }
    let current = workspace.root.join(&workspace.packages[index].dir);
    fix::offer(workspace, &current, None, &mut diagnostics);
    diagnostics
}

/// The bytes a diagnostic covers in its file: its reported construct, or else its line without
/// its line end, or else the file's start.
fn covered(workspace: &Workspace, diagnostic: &Diagnostic) -> (u32, u32) {
    let text = diagnostic
        .path
        .as_deref()
        .and_then(|path| workspace.bytes(path))
        .unwrap_or_default();
    match (diagnostic.span, diagnostic.line) {
        (Some(span), _) => span,
        (None, Some(line)) => text::line_bytes(text, line),
        (None, None) => (0, 0),
    }
}

/// A diagnostic of the package at `index` as the protocol writes it: its message the lines of
/// its record after the header, without quotes, or its code when it has none; each reason with a
/// line in the workspace as related information.
pub fn written(
    server: &Server,
    workspace: &Workspace,
    index: usize,
    diagnostic: &Diagnostic,
) -> Json {
    let path = diagnostic.path.as_deref().unwrap_or_default();
    let text = workspace.bytes(path).unwrap_or_default();
    let (start, end) = covered(workspace, diagnostic);
    let scope = Scope::Package(index);
    let lines = crate::command::diagnostic_lines(workspace, &scope, diagnostic, false);
    let message = match lines.is_empty() {
        true => diagnostic.code.name().to_owned(),
        false => lines.join("\n"),
    };
    let severity = match diagnostic.code.class() {
        Class::Incompleteness => 2,
        Class::Error | Class::Internal => 1,
    };
    let mut members = vec![
        ("range", text::range(text, start, end, server.encoding)),
        ("severity", Json::number(severity)),
        ("code", Json::text(diagnostic.code.name())),
        ("source", Json::text("nan")),
        ("message", Json::Text(message)),
    ];
    let related: Vec<Json> = diagnostic
        .because
        .iter()
        .filter_map(|reason| {
            let (path, line) = reason.at.as_ref()?;
            let text = workspace.file(path).map(|(_, file)| &file.syntax.source)?;
            Some(Json::object(vec![
                (
                    "location",
                    server.location(workspace, path, text::line_bytes(text, *line)),
                ),
                (
                    "message",
                    Json::Text(format!("{} {}", reason.role.word(), reason.subject)),
                ),
            ]))
        })
        .collect();
    if !related.is_empty() {
        members.push(("relatedInformation", Json::Array(related)));
    }
    Json::object(members)
}

/// `textDocument/codeAction`: a quick fix per exact fix of each diagnostic of the document
/// covering bytes of the range, the first fix of each preferred.
pub fn code_actions(server: &Server, params: &Json) -> Option<Json> {
    let (path, workspace) = server.document(params)?;
    let text = workspace.bytes(&path)?;
    let range = params.get("range")?;
    let start = text::offset(text, range.get("start")?, server.encoding)?;
    let end = text::offset(text, range.get("end")?, server.encoding)?;
    let Some(index) = workspace.file_package(&path) else {
        return Some(Json::Array(Vec::new()));
    };
    let mut actions = Vec::new();
    for diagnostic in of_package(&workspace, index) {
        let (from, to) = covered(&workspace, &diagnostic);
        if diagnostic.path.as_deref() != Some(path.as_str()) || to < start || from > end {
            continue;
        }
        let written = written(server, &workspace, index, &diagnostic);
        for (number, fix) in diagnostic.fixes.iter().enumerate() {
            let title = match diagnostic.fixes.len() {
                1 => format!("Fix {}", diagnostic.code.name()),
                _ => format!("Fix {} ({})", diagnostic.code.name(), number + 1),
            };
            actions.push(Json::object(vec![
                ("title", Json::Text(title)),
                ("kind", Json::text("quickfix")),
                ("diagnostics", Json::Array(vec![written.clone()])),
                ("isPreferred", Json::Bool(number == 0)),
                ("edit", workspace_edit(server, &workspace, fix)),
            ]));
        }
    }
    Some(Json::Array(actions))
}

/// A fix as a workspace edit: per file, by path, its replacements by position.
pub fn workspace_edit(server: &Server, workspace: &Workspace, fix: &Fix) -> Json {
    let mut files: BTreeMap<&str, Vec<&Replacement>> = BTreeMap::new();
    for (path, replacement) in &fix.0 {
        files.entry(path).or_default().push(replacement);
    }
    let changes = files
        .into_iter()
        .map(|(path, mut replacements)| {
            replacements.sort_by_key(|replacement| replacement.start);
            let text = workspace.bytes(path).unwrap_or_default();
            let edits = replacements
                .into_iter()
                .map(|replacement| {
                    text_edit(
                        text,
                        (replacement.start, replacement.end),
                        &replacement.text,
                        server,
                    )
                })
                .collect();
            (server.uri(path), Json::Array(edits))
        })
        .collect();
    Json::object(vec![("changes", Json::Object(changes))])
}

/// An edit replacing bytes of a text.
fn text_edit(text: &[u8], (start, end): (u32, u32), new: &str, server: &Server) -> Json {
    Json::object(vec![
        ("range", text::range(text, start, end, server.encoding)),
        ("newText", Json::text(new)),
    ])
}

/// `textDocument/formatting`: the document in canonical format, as one edit replacing its text,
/// or none when it is in canonical format.
pub fn formatting(server: &Server, params: &Json) -> Option<Json> {
    let (path, workspace) = server.document(params)?;
    let text = workspace.bytes(&path)?;
    let formatted = format::source_file(&parse(text.to_vec()), None);
    if formatted == text {
        return Some(Json::Array(Vec::new()));
    }
    let new = String::from_utf8_lossy(&formatted);
    let edit = text_edit(text, (0, text.len() as u32), &new, server);
    Some(Json::Array(vec![edit]))
}
