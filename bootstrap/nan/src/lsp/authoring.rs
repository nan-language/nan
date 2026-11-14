//! Authoring: completions, signature help, inlay hints and semantic tokens.

use super::json::Json;
use super::{Server, text};
use crate::check::{self, Class, Offered};

/// The semantic token types, by the index a token's type is written with.
const TOKEN_TYPES: [&str; 15] = [
    "namespace",
    "type",
    "struct",
    "enum",
    "interface",
    "parameter",
    "variable",
    "property",
    "enumMember",
    "function",
    "method",
    "keyword",
    "comment",
    "string",
    "number",
];

/// The semantic token modifiers, by the bit a token's modifiers set.
const TOKEN_MODIFIERS: [&str; 2] = ["declaration", "readonly"];

/// The legend of the semantic tokens, which the server gives for whole documents.
pub fn semantic_tokens_options() -> Json {
    let names = |names: &[&str]| Json::Array(names.iter().map(|name| Json::text(name)).collect());
    Json::object(vec![
        (
            "legend",
            Json::object(vec![
                ("tokenTypes", names(&TOKEN_TYPES)),
                ("tokenModifiers", names(&TOKEN_MODIFIERS)),
            ]),
        ),
        ("full", Json::Bool(true)),
    ])
}

/// The completion item kind of what a completion offers.
fn kind(offered: Offered) -> u64 {
    match offered {
        Offered::Method => 2,
        Offered::Function => 3,
        Offered::Field => 5,
        Offered::Local => 6,
        Offered::Interface => 8,
        Offered::Module => 9,
        Offered::Enum => 13,
        Offered::Keyword => 14,
        Offered::Variant => 20,
        Offered::Const => 21,
        Offered::Struct => 22,
    }
}

/// `textDocument/completion`: the names that may follow the position, the identifier ending
/// there written in part.
pub fn completion(server: &Server, params: &Json) -> Option<Json> {
    let (path, workspace) = server.document(params)?;
    let offset = server.offset(&workspace, &path, params)?;
    let items = check::completions(&workspace, &path, offset)
        .into_iter()
        .map(|completion| {
            let mut members = vec![
                ("label", Json::Text(completion.label)),
                ("kind", Json::number(kind(completion.offered))),
            ];
            if !completion.detail.is_empty() {
                members.push(("detail", Json::Text(completion.detail)));
            }
            Json::object(members)
        })
        .collect();
    Some(Json::object(vec![
        ("isIncomplete", Json::Bool(false)),
        ("items", Json::Array(items)),
    ]))
}

/// `textDocument/signatureHelp`: the head of the function the call being written at the
/// position calls, with the parameter its argument there takes.
pub fn signature_help(server: &Server, params: &Json) -> Option<Json> {
    let (path, workspace) = server.document(params)?;
    let offset = server.offset(&workspace, &path, params)?;
    let Some(signature) = check::signature(&workspace, &path, offset) else {
        return Some(Json::Null);
    };
    let parameters = signature
        .parameters
        .iter()
        .map(|parameter| Json::object(vec![("label", Json::text(parameter))]))
        .collect();
    let written = Json::object(vec![
        ("label", Json::Text(signature.label)),
        ("parameters", Json::Array(parameters)),
    ]);
    Some(Json::object(vec![
        ("signatures", Json::Array(vec![written])),
        ("activeSignature", Json::number(0)),
        ("activeParameter", Json::number(signature.active as u64)),
    ]))
}

/// `textDocument/inlayHint`: the type of each binding of the range written without one, after
/// its name.
pub fn inlay_hints(server: &Server, params: &Json) -> Option<Json> {
    let (path, workspace) = server.document(params)?;
    let text = workspace.bytes(&path)?;
    let range = params.get("range")?;
    let start = text::offset(text, range.get("start")?, server.encoding)?;
    let end = text::offset(text, range.get("end")?, server.encoding)?;
    let hints = check::type_hints(&workspace, &path, (start, end))
        .into_iter()
        .map(|(at, ty)| {
            Json::object(vec![
                ("position", text::position(text, at, server.encoding)),
                ("label", Json::Text(format!(": {ty}"))),
                ("kind", Json::number(1)),
            ])
        })
        .collect();
    Some(Json::Array(hints))
}

/// The index of a class among the token types.
fn token_type(class: Class) -> u64 {
    match class {
        Class::Namespace => 0,
        Class::Type => 1,
        Class::Struct => 2,
        Class::Enum => 3,
        Class::Interface => 4,
        Class::Parameter => 5,
        Class::Variable => 6,
        Class::Property => 7,
        Class::EnumMember => 8,
        Class::Function => 9,
        Class::Method => 10,
        Class::Keyword => 11,
        Class::Comment => 12,
        Class::Text => 13,
        Class::Number => 14,
    }
}

/// `textDocument/semanticTokens/full`: the document's classed names, keywords, literals and
/// comments, each relative to the one before it.
pub fn semantic_tokens(server: &Server, params: &Json) -> Option<Json> {
    let (path, workspace) = server.document(params)?;
    let text = workspace.bytes(&path)?;
    let mut data = Vec::new();
    let (mut line, mut character) = (0, 0);
    for classed in check::classes(&workspace, &path) {
        let (at_line, at_character) = text::line_character(text, classed.start, server.encoding);
        let (_, end_character) = text::line_character(text, classed.end, server.encoding);
        let relative = match at_line == line {
            true => at_character - character,
            false => at_character,
        };
        let modifiers = u64::from(classed.declaration) | u64::from(classed.readonly) << 1;
        data.extend([
            u64::from(at_line - line),
            u64::from(relative),
            u64::from(end_character - at_character),
            token_type(classed.class),
            modifiers,
        ]);
        (line, character) = (at_line, at_character);
    }
    Some(Json::object(vec![(
        "data",
        Json::Array(data.into_iter().map(Json::number).collect()),
    )]))
}
