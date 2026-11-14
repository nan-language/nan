//! Navigation: definitions, references, hovers, document and workspace symbols, implementations
//! and call hierarchies.

use super::json::Json;
use super::{Server, text};
use crate::check::{self, Function, Span};
use crate::syntax::ast::{self, ItemKind};
use crate::syntax::file::SourceFile;
use crate::workspace::{File, Workspace};

const METHOD: u64 = 6;
const FIELD: u64 = 8;
const ENUM: u64 = 10;
const INTERFACE: u64 = 11;
const FUNCTION: u64 = 12;
const VARIABLE: u64 = 13;
const CONSTANT: u64 = 14;
const ENUM_MEMBER: u64 = 22;
const STRUCT: u64 = 23;

/// The most symbols `workspace/symbol` answers with.
const SYMBOLS: usize = 256;

/// The location of a span.
fn located(server: &Server, workspace: &Workspace, span: &Span) -> Json {
    server.location(workspace, &span.path, (span.start, span.end))
}

/// The document a request's params name, the workspace, and the byte of its position.
fn positioned(server: &Server, params: &Json) -> Option<(String, Workspace, u32)> {
    let (path, workspace) = server.document(params)?;
    let offset = server.offset(&workspace, &path, params)?;
    Some((path, workspace, offset))
}

/// `textDocument/definition`: the name declaring what the name at the position names.
pub fn definition(server: &Server, params: &Json) -> Option<Json> {
    let (path, workspace, offset) = positioned(server, params)?;
    Some(match check::definition(&workspace, &path, offset) {
        Some(span) => located(server, &workspace, &span),
        None => Json::Null,
    })
}

/// `textDocument/references`: the names of the workspace writing what the name at the position
/// names.
pub fn references(server: &Server, params: &Json) -> Option<Json> {
    let (path, workspace, offset) = positioned(server, params)?;
    let declaration = params
        .at(&["context", "includeDeclaration"])
        .and_then(Json::as_bool)
        .unwrap_or(false);
    let spans = check::references(&workspace, &path, offset, declaration);
    let locations = spans
        .iter()
        .map(|span| located(server, &workspace, span))
        .collect();
    Some(Json::Array(locations))
}

/// `textDocument/hover`: what the name at the position names, described in a NaN code block.
pub fn hover(server: &Server, params: &Json) -> Option<Json> {
    let (path, workspace, offset) = positioned(server, params)?;
    let Some((span, lines)) = check::hover(&workspace, &path, offset) else {
        return Some(Json::Null);
    };
    let contents = Json::object(vec![
        ("kind", Json::text("markdown")),
        ("value", Json::Text(fenced(&lines))),
    ]);
    let text = workspace.bytes(&path)?;
    Some(Json::object(vec![
        ("contents", contents),
        (
            "range",
            text::range(text, span.start, span.end, server.encoding),
        ),
    ]))
}

/// Lines as a Markdown code block of NaN, fenced by more backticks than any run they hold.
fn fenced(lines: &[String]) -> String {
    let longest = lines
        .iter()
        .flat_map(|line| line.split(|character| character != '`'))
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}nan\n{}\n{fence}", lines.join("\n"))
}

/// `textDocument/documentSymbol`: the document's items, with their fields, variants and
/// interface methods.
pub fn document_symbols(server: &Server, params: &Json) -> Option<Json> {
    let (path, workspace) = server.document(params)?;
    let Some((_, file)) = workspace.file(&path) else {
        return Some(Json::Array(Vec::new()));
    };
    let syntax = &file.syntax;
    let text = &syntax.source;
    let range = |start: u32, end: u32| text::range(text, start, end, server.encoding);
    let mut symbols = Vec::new();
    for (index, item) in syntax.items.iter().enumerate() {
        let Some((name, kind, token)) = named_item(file, index) else {
            continue;
        };
        let lines = syntax.item_lines(item);
        let start = syntax.lines[*lines.start() as usize - 1].start;
        let end = syntax.lines[*lines.end() as usize - 1].end;
        let children: Vec<Json> = members(file, index)
            .into_iter()
            .map(|(member, kind, token)| {
                let (start, end) = text::line_bytes(text, syntax.tokens[token as usize].line);
                symbol(
                    member,
                    kind,
                    range(start, end),
                    token_range(syntax, token, server),
                )
            })
            .collect();
        let mut written = symbol(
            name,
            kind,
            range(start, end),
            token_range(syntax, token, server),
        );
        if let Json::Object(members) = &mut written
            && !children.is_empty()
        {
            members.push(("children".to_owned(), Json::Array(children)));
        }
        symbols.push(written);
    }
    Some(Json::Array(symbols))
}

/// A document symbol.
fn symbol(name: String, kind: u64, range: Json, selection: Json) -> Json {
    Json::object(vec![
        ("name", Json::Text(name)),
        ("kind", Json::number(kind)),
        ("range", range),
        ("selectionRange", selection),
    ])
}

/// The range of a token of a source file.
fn token_range(syntax: &SourceFile, token: ast::Tok, server: &Server) -> Json {
    let at = &syntax.tokens[token as usize];
    text::range(&syntax.source, at.start, at.end, server.encoding)
}

/// The name, symbol kind and name token of a file's item that a symbol shows: a function,
/// type, interface, protocol, const, registry, contribution or test.
fn named_item(file: &File, index: usize) -> Option<(String, u64, ast::Tok)> {
    let item = file.lowered().get(index)?.as_ref()?;
    let kind = match &item.kind {
        ItemKind::Fn(function) if function.label.owner.is_some() => METHOD,
        ItemKind::Fn(_) | ItemKind::Test(_) => FUNCTION,
        ItemKind::Struct(_) | ItemKind::Layout(_) | ItemKind::Registers(_) => STRUCT,
        ItemKind::Enum(_) => ENUM,
        ItemKind::Interface(_) | ItemKind::Protocol(_) => INTERFACE,
        ItemKind::Const(_) | ItemKind::Contribute(_) => CONSTANT,
        ItemKind::Registry(_) => VARIABLE,
        _ => return None,
    };
    Some((item.name()?, kind, check::declared_name(item)?.token))
}

/// The fields, variants and interface methods a file's item declares, with their symbol kinds
/// and name tokens.
fn members(file: &File, index: usize) -> Vec<(String, u64, ast::Tok)> {
    let Some(Some(item)) = file.lowered().get(index) else {
        return Vec::new();
    };
    let named = |name: &ast::Name, kind: u64| (name.text.clone(), kind, name.token);
    match &item.kind {
        ItemKind::Struct(structure) => structure
            .fields()
            .map(|field| named(&field.name, FIELD))
            .collect(),
        ItemKind::Enum(enumeration) => enumeration
            .variants
            .iter()
            .map(|variant| named(&variant.name, ENUM_MEMBER))
            .collect(),
        ItemKind::Interface(interface) => interface
            .methods
            .iter()
            .map(|method| named(&method.name, METHOD))
            .collect(),
        _ => Vec::new(),
    }
}

/// `workspace/symbol`: the items of the workspace's files whose folded name holds the folded
/// query, by name, path and position, at most `SYMBOLS`.
pub fn workspace_symbols(server: &Server, params: &Json) -> Option<Json> {
    let query = check::fold(params.get("query")?.as_str()?);
    let Some(workspace) = server.workspace() else {
        return Some(Json::Array(Vec::new()));
    };
    let mut found: Vec<(String, String, u32, u64, ast::Tok, String)> = Vec::new();
    for (package, file) in workspace.files() {
        for index in 0..file.syntax.items.len() {
            let Some((name, kind, token)) = named_item(file, index) else {
                continue;
            };
            if !check::fold(&name).contains(&query) {
                continue;
            }
            let line = file.syntax.tokens[token as usize].line;
            let container = package
                .item_at(file, line)
                .map(|path| {
                    let canonical = path.canonical();
                    canonical
                        .rsplit_once("::")
                        .map_or(canonical.clone(), |(container, _)| container.to_owned())
                })
                .unwrap_or_default();
            let start = file.syntax.tokens[token as usize].start;
            found.push((name, file.path.clone(), start, kind, token, container));
        }
    }
    found.sort_by(|left, right| (&left.0, &left.1, left.2).cmp(&(&right.0, &right.1, right.2)));
    found.truncate(SYMBOLS);
    let symbols = found
        .into_iter()
        .map(|(name, path, _, kind, token, container)| {
            let (_, file) = workspace
                .file(&path)
                .expect("symbols lie in workspace files");
            let at = &file.syntax.tokens[token as usize];
            Json::object(vec![
                ("name", Json::Text(name)),
                ("kind", Json::number(kind)),
                (
                    "location",
                    server.location(&workspace, &path, (at.start, at.end)),
                ),
                ("containerName", Json::Text(container)),
            ])
        })
        .collect();
    Some(Json::Array(symbols))
}

/// `textDocument/implementation`: the impl lines naming the interface or type the name at the
/// position names, or the functions implementing the interface method it names.
pub fn implementations(server: &Server, params: &Json) -> Option<Json> {
    let (path, workspace, offset) = positioned(server, params)?;
    let spans = check::implementations(&workspace, &path, offset);
    let locations = spans
        .iter()
        .map(|span| located(server, &workspace, span))
        .collect();
    Some(Json::Array(locations))
}

/// A function as a call hierarchy item.
fn call_item(server: &Server, workspace: &Workspace, function: &Function) -> Json {
    let text = workspace.bytes(&function.item.path).unwrap_or_default();
    let range = |span: &Span| text::range(text, span.start, span.end, server.encoding);
    let kind = match function.member {
        true => METHOD,
        false => FUNCTION,
    };
    Json::object(vec![
        ("name", Json::text(&function.name)),
        ("kind", Json::number(kind)),
        ("uri", Json::Text(server.uri(&function.item.path))),
        ("range", range(&function.item)),
        ("selectionRange", range(&function.selection)),
    ])
}

/// `textDocument/prepareCallHierarchy`: the function the name at the position names.
pub fn prepare_calls(server: &Server, params: &Json) -> Option<Json> {
    let (path, workspace, offset) = positioned(server, params)?;
    Some(match check::function_at(&workspace, &path, offset) {
        Some(function) => Json::Array(vec![call_item(server, &workspace, &function)]),
        None => Json::Null,
    })
}

/// The workspace path of a call hierarchy item of a request's params, the workspace, and the
/// byte its selection starts at.
fn called_item(server: &Server, params: &Json) -> Option<(String, Workspace, u32)> {
    let item = params.get("item")?;
    let path = server.workspace_path(item.get("uri")?.as_str()?)?;
    let workspace = server.workspace()?;
    let start = item.at(&["selectionRange", "start"])?;
    let offset = text::offset(workspace.bytes(&path)?, start, server.encoding)?;
    Some((path, workspace, offset))
}

/// Calls of other functions as the protocol writes them: each function, with the ranges of its
/// calls in the file `calls_in` names, or else in its own file.
fn calls(
    server: &Server,
    workspace: &Workspace,
    found: Vec<(Function, Vec<Span>)>,
    key: &str,
) -> Json {
    let written = found
        .iter()
        .map(|(function, calls)| {
            let ranges = calls
                .iter()
                .map(|call| {
                    let text = workspace.bytes(&call.path).unwrap_or_default();
                    text::range(text, call.start, call.end, server.encoding)
                })
                .collect();
            Json::object(vec![
                (key, call_item(server, workspace, function)),
                ("fromRanges", Json::Array(ranges)),
            ])
        })
        .collect();
    Json::Array(written)
}

/// `callHierarchy/incomingCalls`: the functions calling the item's function, each with its
/// calls.
pub fn incoming_calls(server: &Server, params: &Json) -> Option<Json> {
    let (path, workspace, offset) = called_item(server, params)?;
    let found = check::callers(&workspace, &path, offset);
    Some(calls(server, &workspace, found, "from"))
}

/// `callHierarchy/outgoingCalls`: the functions of the workspace the item's function calls,
/// each with the calls in it.
pub fn outgoing_calls(server: &Server, params: &Json) -> Option<Json> {
    let (path, workspace, offset) = called_item(server, params)?;
    let found = check::callees(&workspace, &path, offset);
    Some(calls(server, &workspace, found, "to"))
}
