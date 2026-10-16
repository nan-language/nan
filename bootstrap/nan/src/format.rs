//! `nan fmt`: the canonical format of source files.

pub mod bindings;
mod item;
pub mod normal;
pub mod punning;

use std::collections::HashMap;
use std::ops::RangeInclusive;

use crate::scope::Scope;
use crate::syntax::file::SourceFile;
use crate::syntax::token::TokenKind;
use crate::workspace::Workspace;

/// The files of the scope whose canonical format differs from their text, with that format, in
/// path order.
pub fn workspace(workspace: &Workspace, scope: &Scope) -> Vec<(String, Vec<u8>)> {
    let mut written = Vec::new();
    for (_, file) in workspace.files() {
        let lines = match scope {
            Scope::Lines(path, lines) if *path == file.path => Some(lines.clone()),
            Scope::Lines(..) => continue,
            _ if !scope.holds(workspace, &file.path, None) => continue,
            _ => None,
        };
        let formatted = source_file(&file.syntax, lines);
        if formatted != file.syntax.source {
            written.push((file.path.clone(), formatted));
        }
    }
    written.sort();
    written
}

/// The canonical format of one item of a file, its lines joined by LF.
pub fn item_text(file: &SourceFile, item: &crate::syntax::file::ItemSyntax) -> String {
    item::lines(file, item).join("\n")
}

/// A run of a file's lines between items, or an item.
enum Part {
    /// An item to format, by index.
    Item(usize),
    /// Lines kept as written: an abandoned item, or tokens outside every item, to the line
    /// before the next parsed item.
    Kept(RangeInclusive<u32>),
    Summary,
    Comment(String),
    Blank,
}

/// The canonical format of a file, or the file with only its items overlapping `only` in their
/// canonical format.
pub fn source_file(file: &SourceFile, only: Option<RangeInclusive<u32>>) -> Vec<u8> {
    if std::str::from_utf8(&file.source).is_err() {
        return file.source.clone();
    }
    match only {
        Some(only) => items_in(file, &only),
        None => whole(file),
    }
}

fn items_in(file: &SourceFile, only: &RangeInclusive<u32>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut copied = 0;
    for item in file.items.iter().filter(|item| !item.abandoned) {
        let lines = file.item_lines(item);
        if lines.start() > only.end() || only.start() > lines.end() {
            continue;
        }
        let start = file.lines[*lines.start() as usize - 1].start as usize;
        let end = file.lines[*lines.end() as usize - 1].end as usize + 1;
        out.extend(&file.source[copied..start]);
        for line in item::lines(file, item) {
            out.extend(line.bytes());
            out.push(b'\n');
        }
        copied = end;
    }
    out.extend(&file.source[copied..]);
    out
}

fn whole(file: &SourceFile) -> Vec<u8> {
    let mut out: Vec<String> = Vec::new();
    let mut blank_due = false;
    let mut blank_seen = false;
    for part in parts(file) {
        if matches!(part, Part::Blank) {
            blank_seen = true;
            continue;
        }
        if (blank_due || blank_seen) && !out.is_empty() {
            out.push(String::new());
        }
        blank_seen = false;
        blank_due = matches!(part, Part::Item(_) | Part::Summary);
        match part {
            Part::Summary => out.push(opener_spaced(file.text(file.summary.expect("a summary")))),
            Part::Comment(text) => out.push(text),
            Part::Kept(lines) => out.extend(lines.map(|line| verbatim(file, line))),
            Part::Item(index) => out.extend(item::lines(file, &file.items[index])),
            Part::Blank => unreachable!("blank lines were skipped"),
        }
    }
    let mut text = out.join("\n");
    text.push('\n');
    text.into_bytes()
}

/// Splits a file after its summary into items, kept regions, comment lines and blank lines.
fn parts(file: &SourceFile) -> Vec<Part> {
    let mut parts = Vec::new();
    let summary = file.summary.map(|summary| file.tokens[summary].line);
    let mut line = 1;
    let items: Vec<(usize, RangeInclusive<u32>)> = file
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| (index, file.item_lines(item)))
        .collect();
    let misplaced_docs: HashMap<u32, usize> = (0..file.tokens.len())
        .filter(|&token| {
            file.comment[token]
                && matches!(file.tokens[token].kind, TokenKind::Doc | TokenKind::FileDoc)
        })
        .map(|token| (file.tokens[token].line, token))
        .collect();
    let mut next_item = 0;
    let last = file.lines.len() as u32;
    while line <= last {
        if let Some((index, lines)) = items
            .get(next_item)
            .filter(|(_, lines)| *lines.start() == line)
        {
            next_item += 1;
            if !file.items[*index].abandoned {
                parts.push(Part::Item(*index));
                line = lines.end() + 1;
                continue;
            }
            let end = items[next_item..]
                .iter()
                .find(|(other, _)| !file.items[*other].abandoned)
                .map_or(last, |(_, lines)| lines.start() - 1);
            next_item = items
                .iter()
                .position(|(_, lines)| *lines.start() > end)
                .unwrap_or(items.len());
            parts.push(Part::Kept(line..=end));
            line = end + 1;
            continue;
        }
        let physical = &file.lines[line as usize - 1];
        if physical.lexical_error {
            parts.push(Part::Kept(line..=line));
        } else if summary == Some(line) {
            parts.push(Part::Summary);
        } else if let Some(&doc) = misplaced_docs.get(&line) {
            parts.push(Part::Comment(opener_spaced(file.text(doc))));
        } else if let Some(comment) = physical.comment {
            let text = &file.source[comment as usize..physical.end as usize];
            parts.push(Part::Comment(opener_spaced(
                std::str::from_utf8(text).expect("well-formed UTF-8"),
            )));
        } else {
            parts.push(Part::Blank);
        }
        line += 1;
    }
    parts
}

fn verbatim(file: &SourceFile, line: u32) -> String {
    String::from_utf8(file.line_text(line).to_vec()).expect("well-formed UTF-8")
}

/// A comment, doc comment or file summary line with one space after its opener when its text is
/// not empty and does not start with one.
fn opener_spaced(text: &str) -> String {
    let opener = if text.starts_with("///") || text.starts_with("//!") {
        3
    } else {
        2
    };
    let (opener, rest) = text.split_at(opener);
    if rest.is_empty() || rest.starts_with(' ') {
        text.to_owned()
    } else {
        format!("{opener} {rest}")
    }
}

/// A number literal with its digits grouped as the canonical format writes them.
fn group_number(text: &str) -> String {
    let (prefix, rest) = match text.get(..2) {
        Some("0x") => ("0x", &text[2..]),
        Some("0b") => ("0b", &text[2..]),
        _ => ("", text),
    };
    let is_digit = |c: char| {
        if prefix == "0x" {
            c.is_ascii_hexdigit()
        } else {
            c.is_ascii_digit() || c == '.'
        }
    };
    let body_end = rest
        .find(|c: char| !(is_digit(c) || c == '_'))
        .unwrap_or(rest.len());
    let (body, suffix) = rest.split_at(body_end);
    let digits: String = body.chars().filter(|c| *c != '_').collect();
    let (integer, fraction) = match digits.split_once('.') {
        Some((integer, fraction)) => (integer.to_owned(), Some(fraction.to_owned())),
        None => (digits, None),
    };
    let (integer, group, threshold) = match prefix {
        "0x" => (integer.to_ascii_uppercase(), 4, 5),
        "0b" => (integer, 4, 5),
        _ => (integer, 3, 4),
    };
    let mut grouped = String::new();
    for (index, c) in integer.chars().enumerate() {
        let left = integer.len() - index;
        if index > 0 && integer.len() >= threshold && left % group == 0 {
            grouped.push('_');
        }
        grouped.push(c);
    }
    match fraction {
        Some(fraction) => format!("{prefix}{grouped}.{fraction}{suffix}"),
        None => format!("{prefix}{grouped}{suffix}"),
    }
}

#[cfg(test)]
mod tests;
