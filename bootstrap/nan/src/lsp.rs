//! `nan lsp`: the language server, which speaks the Language Server Protocol over standard input
//! and output and answers from check-time queries over the workspace, the documents the client
//! opened read in place of their files.

mod authoring;
mod diagnostics;
mod json;
mod navigation;
mod text;
mod wire;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use crate::journal;
use crate::workspace::{self, Workspace};
use json::Json;
use text::Encoding;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
const SERVER_NOT_INITIALIZED: i64 = -32002;

/// A document the client opened in the workspace.
struct Document {
    uri: String,
    text: Vec<u8>,
}

/// Where the server stands in the protocol's lifecycle.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Starting,
    Running,
    ShutDown,
}

pub struct Server {
    /// The directory the server runs in.
    current: PathBuf,
    /// The workspace root, once the client named its folder.
    root: PathBuf,
    encoding: Encoding,
    stage: Stage,
    /// The open documents of the workspace, by workspace path.
    documents: BTreeMap<String, Document>,
    /// The workspace paths whose diagnostics were last published not empty.
    published: BTreeSet<String>,
}

/// A request's result, or its error's code and message.
type Answer = Result<Json, (i64, String)>;

/// A request's handler: its result, none when its params lack what it reads.
type Handler = fn(&Server, &Json) -> Option<Json>;

/// Serves the messages of `input` until the client's `exit` or the end of the input, writing to
/// `output`. Returns the status the server exits with: 0 after a `shutdown` request, else 1.
pub fn serve(current: &Path, input: &mut dyn BufRead, output: &mut dyn Write) -> i32 {
    let mut server = Server {
        current: current.to_path_buf(),
        root: current.to_path_buf(),
        encoding: Encoding::Utf16,
        stage: Stage::Starting,
        documents: BTreeMap::new(),
        published: BTreeSet::new(),
    };
    loop {
        let Ok(Some(content)) = wire::read(input) else {
            return server.status();
        };
        let handled = match json::parse(&content) {
            Some(message) if message.get("method").and_then(Json::as_str) == Some("exit") => {
                return server.status();
            }
            Some(message) => server.handle(&message, output),
            None => respond(
                output,
                Json::Null,
                Err((PARSE_ERROR, "the message is not JSON".to_owned())),
            ),
        };
        if handled.is_err() {
            return 1;
        }
    }
}

impl Server {
    fn status(&self) -> i32 {
        match self.stage {
            Stage::ShutDown => 0,
            _ => 1,
        }
    }

    /// Answers a request, or acts on a notification; a response from the client is dropped.
    fn handle(&mut self, message: &Json, output: &mut dyn Write) -> io::Result<()> {
        let Some(method) = message.get("method").and_then(Json::as_str) else {
            return Ok(());
        };
        let params = message.get("params").unwrap_or(&Json::Null);
        match message.get("id") {
            Some(id) => {
                let answer = self.request(method, params);
                respond(output, id.clone(), answer)
            }
            None => self.notified(method, params, output),
        }
    }

    fn request(&mut self, method: &str, params: &Json) -> Answer {
        match (self.stage, method) {
            (Stage::Starting, "initialize") => Ok(self.initialize(params)),
            (Stage::Starting, _) => Err((
                SERVER_NOT_INITIALIZED,
                "the server is not initialized".to_owned(),
            )),
            (Stage::ShutDown, _) => Err((INVALID_REQUEST, "the server is shut down".to_owned())),
            (Stage::Running, "initialize") => {
                Err((INVALID_REQUEST, "the server is initialized".to_owned()))
            }
            (Stage::Running, "shutdown") => {
                self.stage = Stage::ShutDown;
                Ok(Json::Null)
            }
            (Stage::Running, _) => self.answer(method, params),
        }
    }

    fn answer(&self, method: &str, params: &Json) -> Answer {
        let Some(handler) = handler(method) else {
            return Err((METHOD_NOT_FOUND, format!("no method {method}")));
        };
        match crate::command::caught(|| handler(self, params)) {
            Ok(Some(result)) => Ok(result),
            Ok(None) => Err((
                INVALID_PARAMS,
                format!("the params of {method} do not read"),
            )),
            Err(panicked) => Err((INTERNAL_ERROR, panicked)),
        }
    }

    /// Takes the client's workspace folder, or its root, and the position encoding, and gives
    /// the server's capabilities.
    fn initialize(&mut self, params: &Json) -> Json {
        let folder = params
            .get("workspaceFolders")
            .and_then(|folders| folders.elements().first())
            .and_then(|folder| folder.get("uri"))
            .or_else(|| params.get("rootUri"))
            .and_then(Json::as_str)
            .and_then(text::uri_path)
            .or_else(|| {
                params
                    .get("rootPath")
                    .and_then(Json::as_str)
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| self.current.clone());
        self.root = workspace::root(&folder);
        let offered = params
            .at(&["capabilities", "general", "positionEncodings"])
            .map_or(&[][..], Json::elements);
        if offered.iter().any(|offer| offer.as_str() == Some("utf-8")) {
            self.encoding = Encoding::Utf8;
        }
        self.stage = Stage::Running;
        Json::object(vec![
            ("capabilities", capabilities(self.encoding)),
            (
                "serverInfo",
                Json::object(vec![("name", Json::text("nan"))]),
            ),
        ])
    }

    /// Acts on a notification while running: a document opened, changed, saved or closed has
    /// the diagnostics of its package published.
    fn notified(&mut self, method: &str, params: &Json, output: &mut dyn Write) -> io::Result<()> {
        if self.stage != Stage::Running {
            return Ok(());
        }
        let Some(uri) = params.at(&["textDocument", "uri"]).and_then(Json::as_str) else {
            return Ok(());
        };
        let Some(path) = self.workspace_path(uri) else {
            return Ok(());
        };
        match method {
            "textDocument/didOpen" => {
                let Some(text) = params.at(&["textDocument", "text"]).and_then(Json::as_str) else {
                    return Ok(());
                };
                let document = Document {
                    uri: uri.to_owned(),
                    text: text.as_bytes().to_vec(),
                };
                self.documents.insert(path.clone(), document);
            }
            "textDocument/didChange" => {
                let encoding = self.encoding;
                let Some(document) = self.documents.get_mut(&path) else {
                    return Ok(());
                };
                let changes = params.get("contentChanges").map_or(&[][..], Json::elements);
                for change in changes {
                    changed(&mut document.text, change, encoding);
                }
            }
            "textDocument/didClose" => {
                self.documents.remove(&path);
            }
            "textDocument/didSave" => {}
            _ => return Ok(()),
        }
        self.publish(&path, output)
    }

    /// Publishes the diagnostics `nan check` reports on the package holding a workspace path,
    /// per file, and empty diagnostics for each of its files that no longer has any.
    fn publish(&mut self, path: &str, output: &mut dyn Write) -> io::Result<()> {
        let Some(workspace) = self.workspace() else {
            return Ok(());
        };
        let Some(index) = workspace.file_package(path) else {
            return Ok(());
        };
        let dir = format!("{}/", workspace.packages[index].dir);
        let mut found: BTreeMap<String, Vec<Json>> = self
            .published
            .iter()
            .filter(|published| published.starts_with(&dir))
            .map(|published| (published.clone(), Vec::new()))
            .collect();
        for diagnostic in diagnostics::of_package(&workspace, index) {
            let Some(at) = &diagnostic.path else {
                continue;
            };
            let written = diagnostics::written(self, &workspace, index, &diagnostic);
            found.entry(at.clone()).or_default().push(written);
        }
        for (path, written) in found {
            match written.is_empty() {
                true => self.published.remove(&path),
                false => self.published.insert(path.clone()),
            };
            let params = Json::object(vec![
                ("uri", Json::Text(self.uri(&path))),
                ("diagnostics", Json::Array(written)),
            ]);
            notify(output, "textDocument/publishDiagnostics", params)?;
        }
        Ok(())
    }

    /// The workspace with the open documents' texts in place of their files; none when the
    /// root holds no package or does not read.
    fn workspace(&self) -> Option<Workspace> {
        if !workspace::holds_package(&self.root).unwrap_or(false) {
            return None;
        }
        let _lock = journal::Lock::acquire(&self.root, false).ok()?;
        let loaded = Workspace::load(self.root.clone(), &mut Vec::new()).ok()?;
        if self.documents.is_empty() {
            return Some(loaded);
        }
        let open: HashMap<String, Vec<u8>> = self
            .documents
            .iter()
            .map(|(path, document)| (path.clone(), document.text.clone()))
            .collect();
        Some(loaded.with_sources(&open))
    }

    /// The workspace path a URI names, none for one outside the workspace.
    fn workspace_path(&self, uri: &str) -> Option<String> {
        text::relative(&self.root, &text::uri_path(uri)?)
    }

    /// The URI of a workspace path: the client's for an open document.
    fn uri(&self, path: &str) -> String {
        match self.documents.get(path) {
            Some(document) => document.uri.clone(),
            None => text::path_uri(&self.root.join(path)),
        }
    }

    /// The workspace path of the document a request's params name, with the workspace.
    fn document(&self, params: &Json) -> Option<(String, Workspace)> {
        let uri = params.at(&["textDocument", "uri"])?.as_str()?;
        let path = self.workspace_path(uri)?;
        Some((path, self.workspace()?))
    }

    /// The byte offset in a workspace file of the position a request's params give.
    fn offset(&self, workspace: &Workspace, path: &str, params: &Json) -> Option<u32> {
        text::offset(
            workspace.bytes(path)?,
            params.get("position")?,
            self.encoding,
        )
    }

    /// The location of bytes of a workspace file.
    fn location(&self, workspace: &Workspace, path: &str, (start, end): (u32, u32)) -> Json {
        let text = workspace.bytes(path).unwrap_or_default();
        Json::object(vec![
            ("uri", Json::Text(self.uri(path))),
            ("range", text::range(text, start, end, self.encoding)),
        ])
    }
}

/// The handler of a request method the server answers.
fn handler(method: &str) -> Option<Handler> {
    Some(match method {
        "textDocument/codeAction" => diagnostics::code_actions,
        "textDocument/formatting" => diagnostics::formatting,
        "textDocument/definition" => navigation::definition,
        "textDocument/references" => navigation::references,
        "textDocument/hover" => navigation::hover,
        "textDocument/documentSymbol" => navigation::document_symbols,
        "workspace/symbol" => navigation::workspace_symbols,
        "textDocument/implementation" => navigation::implementations,
        "textDocument/prepareCallHierarchy" => navigation::prepare_calls,
        "callHierarchy/incomingCalls" => navigation::incoming_calls,
        "callHierarchy/outgoingCalls" => navigation::outgoing_calls,
        "textDocument/completion" => authoring::completion,
        "textDocument/signatureHelp" => authoring::signature_help,
        "textDocument/inlayHint" => authoring::inlay_hints,
        "textDocument/semanticTokens/full" => authoring::semantic_tokens,
        _ => return None,
    })
}

/// What the server offers the client.
fn capabilities(encoding: Encoding) -> Json {
    let sync = Json::object(vec![
        ("openClose", Json::Bool(true)),
        ("change", Json::number(1)),
        (
            "save",
            Json::object(vec![("includeText", Json::Bool(false))]),
        ),
    ]);
    let triggers = |characters: &[&str]| {
        Json::Array(
            characters
                .iter()
                .map(|character| Json::text(character))
                .collect(),
        )
    };
    Json::object(vec![
        ("positionEncoding", Json::text(encoding.name())),
        ("textDocumentSync", sync),
        (
            "codeActionProvider",
            Json::object(vec![("codeActionKinds", triggers(&["quickfix"]))]),
        ),
        ("documentFormattingProvider", Json::Bool(true)),
        ("definitionProvider", Json::Bool(true)),
        ("referencesProvider", Json::Bool(true)),
        ("hoverProvider", Json::Bool(true)),
        ("documentSymbolProvider", Json::Bool(true)),
        ("workspaceSymbolProvider", Json::Bool(true)),
        ("implementationProvider", Json::Bool(true)),
        ("callHierarchyProvider", Json::Bool(true)),
        (
            "completionProvider",
            Json::object(vec![("triggerCharacters", triggers(&[".", ":"]))]),
        ),
        (
            "signatureHelpProvider",
            Json::object(vec![("triggerCharacters", triggers(&["(", ","]))]),
        ),
        ("inlayHintProvider", Json::Bool(true)),
        (
            "semanticTokensProvider",
            authoring::semantic_tokens_options(),
        ),
    ])
}

/// Applies one change of a `didChange` notification: the whole text, or the text of a range.
fn changed(text: &mut Vec<u8>, change: &Json, encoding: Encoding) {
    let Some(new) = change.get("text").and_then(Json::as_str) else {
        return;
    };
    let Some(range) = change.get("range") else {
        *text = new.as_bytes().to_vec();
        return;
    };
    let start = range
        .get("start")
        .and_then(|at| text::offset(text, at, encoding));
    let end = range
        .get("end")
        .and_then(|at| text::offset(text, at, encoding));
    if let (Some(start), Some(end)) = (start, end)
        && start <= end
    {
        text.splice(start as usize..end as usize, new.bytes());
    }
}

fn respond(output: &mut dyn Write, id: Json, answer: Answer) -> io::Result<()> {
    let outcome = match answer {
        Ok(result) => ("result", result),
        Err((code, message)) => (
            "error",
            Json::object(vec![
                ("code", Json::Number(code.to_string())),
                ("message", Json::Text(message)),
            ]),
        ),
    };
    let message = Json::object(vec![("jsonrpc", Json::text("2.0")), ("id", id), outcome]);
    wire::write(output, &message.written())
}

fn notify(output: &mut dyn Write, method: &str, params: Json) -> io::Result<()> {
    let message = Json::object(vec![
        ("jsonrpc", Json::text("2.0")),
        ("method", Json::text(method)),
        ("params", params),
    ]);
    wire::write(output, &message.written())
}

#[cfg(test)]
mod tests;
