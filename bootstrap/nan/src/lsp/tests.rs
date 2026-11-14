use super::json::{self, Json};
use super::text::{self, Encoding};
use super::{serve, wire};
use crate::command::tests::MANIFEST;
use crate::workspace::tests::Scratch;

const GAUGE: &str = "//! Gauges.

pub const STEP: u32 = 5

/// A gauge, raised by [STEP].
pub closed struct Gauge {
    pub level: u32
    pub limit: u32
} Gauge

/// Something with a level.
pub interface Leveled {
    fn level_of(self) -> u32
} Leveled

impl Gauge: Leveled

pub fn Gauge.level_of(self) -> u32 {
    return self.level
} Gauge.level_of

pub fn Gauge.raised(self, by: u32) -> u32 {
    let next = self.level.saturating_add(by)
    return next
} Gauge.raised

pub fn total(gauge: Gauge) -> u32 {
    let raised = gauge.raised(STEP)
    return raised + gauge.level_of()
} total
";

fn request(id: u64, method: &str, params: Json) -> Json {
    Json::object(vec![
        ("jsonrpc", Json::text("2.0")),
        ("id", Json::number(id)),
        ("method", Json::text(method)),
        ("params", params),
    ])
}

fn notification(method: &str, params: Json) -> Json {
    Json::object(vec![
        ("jsonrpc", Json::text("2.0")),
        ("method", Json::text(method)),
        ("params", params),
    ])
}

/// The params naming a document of the workspace and a position in it.
fn at(root: &str, path: &str, line: u64, character: u64) -> Json {
    Json::object(vec![
        ("textDocument", document(root, path)),
        (
            "position",
            Json::object(vec![
                ("line", Json::number(line)),
                ("character", Json::number(character)),
            ]),
        ),
    ])
}

fn document(root: &str, path: &str) -> Json {
    Json::object(vec![("uri", Json::Text(format!("{root}/{path}")))])
}

/// What the server writes over a session in a workspace of `files`, the messages given the
/// workspace's URI, and the status it exits with; the session starts by initializing.
fn session(files: &[(&str, &str)], messages: impl FnOnce(&str) -> Vec<Json>) -> (Vec<Json>, i32) {
    let scratch = Scratch::new(files);
    let root = text::path_uri(&scratch.0);
    let mut sent = vec![request(
        0,
        "initialize",
        Json::object(vec![("rootUri", Json::Text(root.clone()))]),
    )];
    sent.extend(messages(&root));
    let mut input = Vec::new();
    for message in sent {
        let content = message.written();
        input.extend(format!("Content-Length: {}\r\n\r\n{content}", content.len()).bytes());
    }
    let mut output = Vec::new();
    let status = serve(&scratch.0, &mut &input[..], &mut output);
    let mut written = Vec::new();
    let mut reader = &output[..];
    while let Some(content) = wire::read(&mut reader).expect("output reads") {
        let message = json::parse(&content).expect("the server writes JSON");
        written.push(message.to_owned());
    }
    (written, status)
}

/// The result of the response to the request `id`.
fn result(written: &[Json], id: u64) -> &Json {
    written
        .iter()
        .find(|message| message.get("id").and_then(Json::as_u32) == Some(id as u32))
        .and_then(|message| message.get("result"))
        .expect("a result")
}

/// The start of a location's range, as a line and character.
fn start(location: &Json) -> (u32, u32) {
    let start = location.at(&["range", "start"]).expect("a range");
    let number = |name: &str| start.get(name).and_then(Json::as_u32).expect("a number");
    (number("line"), number("character"))
}

#[test]
fn json_reads_and_writes_values() {
    let parsed =
        json::parse(br#" {"a":[1,-2.5e3,true,null],"b":"\"\\\n\u00e9\ud834\udd1e\ud800x"} "#)
            .expect("a value");
    assert_eq!(
        parsed.written(),
        "{\"a\":[1,-2.5e3,true,null],\"b\":\"\\\"\\\\\\n\u{e9}\u{1d11e}\u{fffd}x\"}"
    );
    for invalid in [&b"[1,]"[..], b"{\"a\" 1}", b"01", b"\"\x01\"", b"1 2"] {
        assert_eq!(
            json::parse(invalid),
            None,
            "{}",
            String::from_utf8_lossy(invalid)
        );
    }
    let deep = "[".repeat(65) + &"]".repeat(65);
    assert_eq!(json::parse(deep.as_bytes()), None);
}

#[test]
fn positions_count_the_agreed_code_units() {
    let text = "let \u{e9} = \"\u{1d11e}\"\nnext\n".as_bytes();
    let after = text.iter().position(|byte| *byte == b'"').expect("a quote") as u32 + 5;
    for (encoding, character) in [(Encoding::Utf8, 14), (Encoding::Utf16, 11)] {
        let position = text::position(text, after, encoding);
        assert_eq!(
            position.get("character").and_then(Json::as_u32),
            Some(character)
        );
        assert_eq!(text::offset(text, &position, encoding), Some(after));
    }
    let past = json::parse(br#"{"line":1,"character":99}"#).expect("a position");
    assert_eq!(
        text::offset(text, &past, Encoding::Utf16),
        Some(text.len() as u32 - 1)
    );
    let path = std::path::Path::new("/tmp/a b/é.nan");
    let uri = text::path_uri(path);
    assert_eq!(uri, "file:///tmp/a%20b/%C3%A9.nan");
    assert_eq!(text::uri_path(&uri).as_deref(), Some(path));
}

#[test]
fn the_lifecycle_orders_requests() {
    let files = [("app/package.nan", MANIFEST), ("app/app.nan", GAUGE)];
    let (written, status) = session(&files, |_| {
        vec![
            request(1, "textDocument/unknown", Json::Null),
            request(2, "shutdown", Json::Null),
            request(3, "textDocument/hover", Json::Null),
            notification("exit", Json::Null),
        ]
    });
    assert!(
        result(&written, 0)
            .at(&["capabilities", "hoverProvider"])
            .is_some()
    );
    let error = |id: u32| {
        written
            .iter()
            .find(|message| message.get("id").and_then(Json::as_u32) == Some(id))
            .and_then(|message| message.at(&["error", "code"]))
            .map(Json::written)
    };
    assert_eq!(error(1).as_deref(), Some("-32601"));
    assert_eq!(result(&written, 2), &Json::Null);
    assert_eq!(error(3).as_deref(), Some("-32600"));
    assert_eq!(status, 0);
    let (_, status) = session(&files, |_| vec![notification("exit", Json::Null)]);
    assert_eq!(status, 1, "exiting without shutdown fails");
}

#[test]
fn an_opened_document_publishes_its_diagnostics_and_fixes() {
    let files = [("app/package.nan", MANIFEST), ("app/app.nan", GAUGE)];
    let broken = GAUGE.replace("gauge.level_of()\n", "gauge.level_of()  \n");
    let (written, _) = session(&files, |root| {
        let opened = Json::object(vec![(
            "textDocument",
            Json::object(vec![
                ("uri", Json::Text(format!("{root}/app/app.nan"))),
                ("text", Json::Text(broken.clone())),
            ]),
        )]);
        let range =
            json::parse(br#"{"start":{"line":28,"character":0},"end":{"line":29,"character":0}}"#)
                .expect("a range");
        let fixed = Json::object(vec![
            ("textDocument", document(root, "app/app.nan")),
            (
                "contentChanges",
                Json::Array(vec![Json::object(vec![("text", Json::text(GAUGE))])]),
            ),
        ]);
        vec![
            notification("textDocument/didOpen", opened),
            request(
                1,
                "textDocument/codeAction",
                Json::object(vec![
                    ("textDocument", document(root, "app/app.nan")),
                    ("range", range),
                ]),
            ),
            notification("textDocument/didChange", fixed),
        ]
    });
    let published: Vec<&Json> = written
        .iter()
        .filter(|message| {
            message.get("method").and_then(Json::as_str) == Some("textDocument/publishDiagnostics")
        })
        .filter_map(|message| message.at(&["params", "diagnostics"]))
        .collect();
    let [opened, changed] = published[..] else {
        panic!("two publications: {published:?}");
    };
    let [diagnostic] = opened.elements() else {
        panic!("one diagnostic: {opened:?}");
    };
    assert_eq!(
        diagnostic.get("code").and_then(Json::as_str),
        Some("lex.trailing-whitespace")
    );
    assert_eq!(start(diagnostic), (28, 36));
    let message = diagnostic
        .get("message")
        .and_then(Json::as_str)
        .unwrap_or_default();
    assert!(
        message.ends_with("\n+     return raised + gauge.level_of()"),
        "{message}"
    );
    assert_eq!(
        changed.elements(),
        &[],
        "a fixed document clears its diagnostics"
    );
    let [action] = result(&written, 1).elements() else {
        panic!("one quick fix");
    };
    let edits = action.at(&["edit", "changes"]).expect("changes");
    let Json::Object(files) = edits else {
        panic!("changes by URI");
    };
    let [edit] = files[0].1.elements() else {
        panic!("one edit");
    };
    assert_eq!(edit.get("newText").and_then(Json::as_str), Some(""));
    assert_eq!(start(edit), (28, 36));
}

#[test]
fn formatting_gives_the_canonical_format() {
    let files = [
        ("app/package.nan", MANIFEST),
        ("app/app.nan", "//! Gauges.\n\npub const STEP: u32 =   5\n"),
    ];
    let (written, _) = session(&files, |root| {
        let params = Json::object(vec![("textDocument", document(root, "app/app.nan"))]);
        vec![request(1, "textDocument/formatting", params)]
    });
    let [edit] = result(&written, 1).elements() else {
        panic!("one edit");
    };
    assert_eq!(
        edit.get("newText").and_then(Json::as_str),
        Some("//! Gauges.\n\npub const STEP: u32 = 5\n")
    );
}

#[test]
fn navigation_follows_names() {
    let files = [("app/package.nan", MANIFEST), ("app/app.nan", GAUGE)];
    let (written, _) = session(&files, |root| {
        let mut sent = Vec::new();
        for (id, line, character) in [(1, 27, 24), (2, 18, 16), (3, 23, 11), (4, 4, 25)] {
            sent.push(request(
                id,
                "textDocument/definition",
                at(root, "app/app.nan", line, character),
            ));
        }
        let mut references = at(root, "app/app.nan", 2, 10);
        if let Json::Object(members) = &mut references {
            let context = Json::object(vec![("includeDeclaration", Json::Bool(false))]);
            members.push(("context".to_owned(), context));
        }
        sent.push(request(5, "textDocument/references", references));
        sent.push(request(
            6,
            "textDocument/hover",
            at(root, "app/app.nan", 23, 11),
        ));
        sent.push(request(
            7,
            "textDocument/hover",
            at(root, "app/app.nan", 27, 24),
        ));
        sent
    });
    assert_eq!(
        start(result(&written, 1)),
        (21, 13),
        "a method call names its function"
    );
    assert_eq!(
        start(result(&written, 2)),
        (6, 8),
        "a field access names its field"
    );
    assert_eq!(
        start(result(&written, 3)),
        (22, 8),
        "a local names its binding"
    );
    assert_eq!(
        start(result(&written, 4)),
        (2, 10),
        "a doc reference names its item"
    );
    let found: Vec<(u32, u32)> = result(&written, 5).elements().iter().map(start).collect();
    assert_eq!(found, [(4, 24), (27, 30)]);
    let hovered = |id: u64| {
        result(&written, id)
            .at(&["contents", "value"])
            .and_then(Json::as_str)
            .map(str::to_owned)
    };
    assert_eq!(hovered(6).as_deref(), Some("```nan\nnext: u32\n```"));
    assert_eq!(
        hovered(7).as_deref(),
        Some(
            "```nan\npub fn Gauge.raised(self, by: u32) -> u32\ndigest: effects none | authority none | may-trap no | terminates\n```"
        )
    );
}

#[test]
fn symbols_implementations_and_calls_list_the_workspace() {
    let files = [("app/package.nan", MANIFEST), ("app/app.nan", GAUGE)];
    let (written, _) = session(&files, |root| {
        let file = Json::object(vec![("textDocument", document(root, "app/app.nan"))]);
        let query = Json::object(vec![("query", Json::text("LEVEL"))]);
        let raised = json::parse(
            format!(
                r#"{{"item":{{"name":"Gauge.raised","kind":6,"uri":"{root}/app/app.nan","range":{{"start":{{"line":21,"character":0}},"end":{{"line":24,"character":14}}}},"selectionRange":{{"start":{{"line":21,"character":13}},"end":{{"line":21,"character":19}}}}}}}}"#
            )
            .as_bytes(),
        )
        .expect("an item");
        vec![
            request(1, "textDocument/documentSymbol", file),
            request(2, "workspace/symbol", query),
            request(
                3,
                "textDocument/implementation",
                at(root, "app/app.nan", 11, 15),
            ),
            request(
                4,
                "textDocument/implementation",
                at(root, "app/app.nan", 12, 8),
            ),
            request(
                5,
                "textDocument/prepareCallHierarchy",
                at(root, "app/app.nan", 21, 15),
            ),
            request(6, "callHierarchy/incomingCalls", raised),
        ]
    });
    let names: Vec<&str> = result(&written, 1)
        .elements()
        .iter()
        .filter_map(|symbol| symbol.get("name").and_then(Json::as_str))
        .collect();
    assert_eq!(
        names,
        [
            "STEP",
            "Gauge",
            "Leveled",
            "Gauge.level_of",
            "Gauge.raised",
            "total"
        ]
    );
    let symbols: Vec<&str> = result(&written, 2)
        .elements()
        .iter()
        .filter_map(|symbol| symbol.get("name").and_then(Json::as_str))
        .collect();
    assert_eq!(symbols, ["Gauge.level_of", "Leveled"]);
    let lines = |id: u64| -> Vec<(u32, u32)> {
        result(&written, id).elements().iter().map(start).collect()
    };
    assert_eq!(lines(3), [(15, 0)], "an interface's impl line");
    assert_eq!(lines(4), [(17, 13)], "an interface method's function");
    assert_eq!(lines(5), [(21, 0)]);
    let [incoming] = result(&written, 6).elements() else {
        panic!("one caller");
    };
    assert_eq!(
        incoming.at(&["from", "name"]).and_then(Json::as_str),
        Some("total")
    );
    let calls: Vec<(u32, u32)> = incoming
        .get("fromRanges")
        .map_or(&[][..], Json::elements)
        .iter()
        .map(|range| {
            let start = range.get("start").expect("a start");
            (
                start.get("line").and_then(Json::as_u32).unwrap_or_default(),
                start
                    .get("character")
                    .and_then(Json::as_u32)
                    .unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(calls, [(27, 17)]);
}

/// The sample package's text with a line written in part after `total`'s first statement, and
/// the position ending it.
fn written_in_part(line: &str) -> (String, u64, u64) {
    let marker = "    let raised = gauge.raised(STEP)\n";
    let text = GAUGE.replace(marker, &format!("{marker}{line}\n"));
    (text, 28, line.len() as u64)
}

#[test]
fn authoring_completes_signs_hints_and_classes() {
    let files = [("app/package.nan", MANIFEST), ("app/app.nan", GAUGE)];
    let lines = [
        "    let x = gauge.",
        "    let y = ra",
        "    let z = app::",
        "    let w = gauge.raised(",
    ];
    let (written, _) = session(&files, |root| {
        let uri = format!("{root}/app/app.nan");
        let mut sent = Vec::new();
        for (id, line) in (1..).zip(lines) {
            let (text, line, character) = written_in_part(line);
            let change = Json::object(vec![
                (
                    "textDocument",
                    Json::object(vec![
                        ("uri", Json::Text(uri.clone())),
                        ("text", Json::Text(text.clone())),
                    ]),
                ),
                (
                    "contentChanges",
                    Json::Array(vec![Json::object(vec![("text", Json::Text(text))])]),
                ),
            ]);
            let method = match id {
                1 => "textDocument/didOpen",
                _ => "textDocument/didChange",
            };
            sent.push(notification(method, change));
            let asked = match id {
                4 => "textDocument/signatureHelp",
                _ => "textDocument/completion",
            };
            sent.push(request(id, asked, at(root, "app/app.nan", line, character)));
        }
        let whole = Json::object(vec![
            ("textDocument", document(root, "app/app.nan")),
            (
                "contentChanges",
                Json::Array(vec![Json::object(vec![("text", Json::text(GAUGE))])]),
            ),
        ]);
        sent.push(notification("textDocument/didChange", whole));
        let range =
            json::parse(br#"{"start":{"line":0,"character":0},"end":{"line":40,"character":0}}"#)
                .expect("a range");
        sent.push(request(
            5,
            "textDocument/inlayHint",
            Json::object(vec![
                ("textDocument", document(root, "app/app.nan")),
                ("range", range),
            ]),
        ));
        sent.push(request(
            6,
            "textDocument/semanticTokens/full",
            Json::object(vec![("textDocument", document(root, "app/app.nan"))]),
        ));
        sent
    });
    let labels = |id: u64| -> Vec<String> {
        result(&written, id)
            .get("items")
            .map_or(&[][..], Json::elements)
            .iter()
            .filter_map(|item| item.get("label").and_then(Json::as_str).map(str::to_owned))
            .collect()
    };
    assert_eq!(labels(1), ["level", "level_of", "limit", "raised"]);
    assert_eq!(
        labels(2),
        ["Range", "raised"],
        "a local and the core vocabulary"
    );
    assert_eq!(labels(3), ["Gauge", "Leveled", "STEP", "total"]);
    let signed = result(&written, 4);
    assert_eq!(
        signed
            .at(&["signatures"])
            .and_then(|all| all.elements().first())
            .and_then(|first| first.get("label"))
            .and_then(Json::as_str),
        Some("pub fn Gauge.raised(self, by: u32) -> u32")
    );
    assert_eq!(
        signed.get("activeParameter").and_then(Json::as_u32),
        Some(1)
    );
    let hints: Vec<(u32, u32, &str)> = result(&written, 5)
        .elements()
        .iter()
        .map(|hint| {
            let position = hint.get("position").expect("a position");
            (
                position
                    .get("line")
                    .and_then(Json::as_u32)
                    .unwrap_or_default(),
                position
                    .get("character")
                    .and_then(Json::as_u32)
                    .unwrap_or_default(),
                hint.get("label").and_then(Json::as_str).unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(hints, [(22, 12, ": u32"), (27, 14, ": u32")]);
    let data: Vec<u32> = result(&written, 6)
        .get("data")
        .map_or(&[][..], Json::elements)
        .iter()
        .filter_map(Json::as_u32)
        .collect();
    let mut tokens = Vec::new();
    let (mut line, mut character) = (0, 0);
    for token in data.chunks(5) {
        character = match token[0] {
            0 => character + token[1],
            _ => token[1],
        };
        line += token[0];
        tokens.push((line, character, token[2], token[3], token[4]));
    }
    assert_eq!(
        tokens[0],
        (0, 0, 11, 12, 0),
        "the file summary is a comment"
    );
    assert!(tokens.contains(&(2, 10, 4, 6, 3)), "STEP declares a const");
    assert!(
        tokens.contains(&(27, 23, 6, 10, 0)),
        "raised names a method"
    );
}
