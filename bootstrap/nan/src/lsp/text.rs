//! Positions and files as the protocol writes them: a position as a line from 0 and the code
//! units of its line before it, in the encoding the server and client agreed on; a file as a
//! `file` URI.

use std::path::{Component, Path, PathBuf};

use super::json::Json;

/// The code units positions count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
}

impl Encoding {
    pub fn name(self) -> &'static str {
        match self {
            Encoding::Utf8 => "utf-8",
            Encoding::Utf16 => "utf-16",
        }
    }

    /// The code units of the character a byte starts: none for a byte continuing one.
    fn units(self, byte: u8) -> u32 {
        match (self, byte) {
            (Encoding::Utf8, _) => 1,
            (Encoding::Utf16, 0x80..0xc0) => 0,
            (Encoding::Utf16, 0xf0..) => 2,
            (Encoding::Utf16, _) => 1,
        }
    }
}

/// The bytes of the character a byte starts, by its lead byte.
fn width(byte: u8) -> usize {
    match byte {
        0xc0..0xe0 => 2,
        0xe0..0xf0 => 3,
        0xf0.. => 4,
        _ => 1,
    }
}

/// The position of a byte offset in a text, an offset past the text's end being its end.
pub fn position(text: &[u8], offset: u32, encoding: Encoding) -> Json {
    let (line, character) = line_character(text, offset, encoding);
    Json::object(vec![
        ("line", Json::number(u64::from(line))),
        ("character", Json::number(u64::from(character))),
    ])
}

/// The line, from 0, and character of a byte offset in a text.
pub fn line_character(text: &[u8], offset: u32, encoding: Encoding) -> (u32, u32) {
    let offset = (offset as usize).min(text.len());
    let start = text[..offset]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |at| at + 1);
    let line = text[..start].iter().filter(|byte| **byte == b'\n').count() as u32;
    let character = text[start..offset]
        .iter()
        .map(|byte| encoding.units(*byte))
        .sum();
    (line, character)
}

/// The range of the bytes from `start` to `end` of a text.
pub fn range(text: &[u8], start: u32, end: u32, encoding: Encoding) -> Json {
    Json::object(vec![
        ("start", position(text, start, encoding)),
        ("end", position(text, end, encoding)),
    ])
}

/// The bytes of a text's line, from 1, without its line end.
pub fn line_bytes(text: &[u8], line: u32) -> (u32, u32) {
    let start = line_start(text, line - 1);
    let end = text[start..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(text.len(), |at| start + at);
    (start as u32, end as u32)
}

/// The offset of a line's start, lines counting from 0; the text's end for a line past its last.
fn line_start(text: &[u8], line: u32) -> usize {
    let mut start = 0;
    for _ in 0..line {
        match text[start..].iter().position(|byte| *byte == b'\n') {
            Some(at) => start += at + 1,
            None => return text.len(),
        }
    }
    start
}

/// The byte offset of a position given as JSON: its line's end for a character past it, the
/// start of a character whose units it falls within, the text's end for a line past its last.
pub fn offset(text: &[u8], position: &Json, encoding: Encoding) -> Option<u32> {
    let line = position.get("line")?.as_u32()?;
    let character = position.get("character")?.as_u32()?;
    let start = line_start(text, line);
    let end = text[start..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(text.len(), |at| start + at);
    let mut at = start;
    let mut counted = 0;
    while at < end {
        let next = (at + width(text[at])).min(end);
        let units: u32 = text[at..next]
            .iter()
            .map(|byte| encoding.units(*byte))
            .sum();
        if counted + units > character {
            break;
        }
        counted += units;
        at = next;
    }
    Some(at as u32)
}

/// The path a `file` URI names, its percent escapes decoded; none for another URI.
pub fn uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let mut decoded = Vec::new();
    let mut bytes = rest.bytes();
    while let Some(byte) = bytes.next() {
        if byte != b'%' {
            decoded.push(byte);
            continue;
        }
        let digits = [bytes.next()?, bytes.next()?];
        let text = std::str::from_utf8(&digits).ok()?;
        decoded.push(u8::from_str_radix(text, 16).ok()?);
    }
    let decoded = String::from_utf8(decoded).ok()?;
    let drive = decoded
        .as_bytes()
        .get(1..3)
        .is_some_and(|start| start[0].is_ascii_alphabetic() && start[1] == b':');
    Some(PathBuf::from(if drive { &decoded[1..] } else { &decoded }))
}

/// The `file` URI of an absolute path: its separators written `/`, each byte other than an
/// unreserved character, `/` and a drive letter's `:` percent-encoded.
pub fn path_uri(path: &Path) -> String {
    let written = path.to_string_lossy().replace('\\', "/");
    let written = match written.starts_with('/') {
        true => written,
        false => format!("/{written}"),
    };
    let mut uri = String::from("file://");
    for (index, byte) in written.bytes().enumerate() {
        let kept = byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/')
            || (byte == b':' && index == 2);
        match kept {
            true => uri.push(byte as char),
            false => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri
}

/// A path below `root` as a workspace path, its components joined by `/`; none for a path
/// outside it.
pub fn relative(root: &Path, path: &Path) -> Option<String> {
    let below = path.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for component in below.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?),
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}
