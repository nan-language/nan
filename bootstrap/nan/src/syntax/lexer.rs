//! Line-local lexing of a source file into tokens, virtual tokens and lexical diagnostics.

use super::grammar::{Foreign, Keyword, Punct, REJECTED, REJECTED_SPACE};
use super::token::{Token, TokenKind};
use crate::diagnostic::{Code, Replacement};

/// A physical line: its byte range without the LF, the offset of its plain comment, and whether it
/// holds a lexical diagnostic.
#[derive(Clone, Copy, Debug)]
pub struct Line {
    pub start: u32,
    pub end: u32,
    pub comment: Option<u32>,
    pub lexical_error: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexDiagnostic {
    pub line: u32,
    pub code: Code,
    /// The bytes of the offending character or token.
    pub span: (u32, u32),
    pub fix: Option<Vec<Replacement>>,
}

pub struct Lexed {
    pub tokens: Vec<Token>,
    pub lines: Vec<Line>,
    pub diagnostics: Vec<LexDiagnostic>,
}

pub fn lex(source: &[u8]) -> Lexed {
    let mut ranges = Vec::new();
    let mut start = 0;
    for (index, byte) in source.iter().enumerate() {
        if *byte == b'\n' {
            ranges.push((start, index));
            start = index + 1;
        }
    }
    let unterminated = start < source.len() || source.is_empty();
    if unterminated {
        ranges.push((start, source.len()));
    }

    let mut lexed = Lexed {
        tokens: Vec::new(),
        lines: Vec::new(),
        diagnostics: Vec::new(),
    };
    let mut pending_nl: Option<Token> = None;
    for (index, &(start, end)) in ranges.iter().enumerate() {
        let number = index as u32 + 1;
        let is_last = index + 1 == ranges.len();
        let scan = lex_line(&source[start..end], unterminated && is_last);
        lexed.lines.push(Line {
            start: start as u32,
            end: end as u32,
            comment: scan.comment.map(|at| (start + at) as u32),
            lexical_error: scan.error.is_some(),
        });
        if scan.tokens.is_empty() && scan.error.is_none() {
            continue;
        }
        let at = |offset: usize| (start + offset) as u32;
        let mut last = None;
        for &(kind, from, to) in &scan.tokens {
            if from == 0 {
                lexed.tokens.push(Token {
                    kind: TokenKind::Col0,
                    start: at(0),
                    end: at(0),
                    line: number,
                });
            }
            let token = Token {
                kind,
                start: at(from),
                end: at(to),
                line: number,
            };
            lexed.tokens.push(token);
            last = Some(token);
        }
        if let Some((offset, code)) = scan.error {
            let line = &source[start..end];
            let width = if code == Code::LexTrailingWhitespace {
                line.len() - offset
            } else {
                line[offset..]
                    .utf8_chunks()
                    .next()
                    .and_then(|chunk| chunk.valid().chars().next())
                    .map_or(1, char::len_utf8)
            };
            lexed.diagnostics.push(LexDiagnostic {
                line: number,
                code,
                span: (at(offset), at(offset + width.min(line.len() - offset))),
                fix: line_fix(line, offset, code).map(|replacements| {
                    replacements
                        .into_iter()
                        .map(|(from, to, text)| Replacement {
                            start: at(from),
                            end: at(to),
                            text,
                        })
                        .collect()
                }),
            });
            let position = at(offset);
            lexed.tokens.push(Token {
                kind: TokenKind::LexError,
                start: position,
                end: position,
                line: number,
            });
            lexed.tokens.push(Token {
                kind: TokenKind::Nl,
                start: position,
                end: position,
                line: number,
            });
            pending_nl = None;
            continue;
        }
        let last = last.expect("a line without diagnostic yielding nothing was skipped");
        let nl = Token {
            kind: TokenKind::Nl,
            start: last.end,
            end: last.end,
            line: number,
        };
        let run_continues = last.kind == TokenKind::TextLine
            && ranges.get(index + 1).is_some_and(|&(next, next_end)| {
                source[next..next_end]
                    .iter()
                    .skip_while(|byte| **byte == b' ')
                    .take(2)
                    .eq(b"\\\\")
            });
        if last.kind.is_continuing() || run_continues {
            pending_nl = Some(nl);
        } else {
            lexed.tokens.push(nl);
            pending_nl = None;
        }
    }
    lexed.tokens.extend(pending_nl);
    let last_line = ranges.len() as u32;
    let eof = source.len() as u32;
    lexed.tokens.push(Token {
        kind: TokenKind::Eof,
        start: eof,
        end: eof,
        line: last_line,
    });
    lexed
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ch {
    Char(char),
    Invalid,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Literal {
    Text,
    Bytes,
    Char,
    Byte,
    TextLine,
}

impl Literal {
    fn holds_bytes(self) -> bool {
        matches!(self, Literal::Bytes | Literal::Byte)
    }

    fn kind(self) -> TokenKind {
        match self {
            Literal::Text => TokenKind::Text,
            Literal::Bytes => TokenKind::Bytes,
            Literal::Char => TokenKind::Char,
            Literal::Byte => TokenKind::Byte,
            Literal::TextLine => TokenKind::TextLine,
        }
    }
}

struct LineScan {
    tokens: Vec<(TokenKind, usize, usize)>,
    comment: Option<usize>,
    error: Option<(usize, Code)>,
}

/// Lexes one physical line; offsets are relative to the line. The line keeps only the tokens
/// ending before its first offending character.
fn lex_line(bytes: &[u8], last_unterminated: bool) -> LineScan {
    let mut chars = Vec::with_capacity(bytes.len());
    let mut offset = 0;
    for chunk in bytes.utf8_chunks() {
        for (at, c) in chunk.valid().char_indices() {
            chars.push((offset + at, Ch::Char(c)));
        }
        offset += chunk.valid().len();
        if !chunk.invalid().is_empty() {
            chars.push((offset, Ch::Invalid));
            offset += chunk.invalid().len();
        }
    }

    let mut error: Option<(usize, Code)> = None;
    let mut report = |at: usize, code: Code| {
        if error.is_none_or(|(first, _)| at < first) {
            error = Some((at, code));
        }
    };
    let indentation = bytes.iter().take_while(|byte| **byte == b' ').count();
    if indentation < bytes.len() && indentation % 4 != 0 {
        report(0, Code::LexIndentation);
    }
    let rejected = chars.iter().find_map(|&(at, ch)| match ch {
        Ch::Invalid => Some((at, Code::LexInvalidUtf8)),
        Ch::Char(c) => rejected_code(c).map(|code| (at, code)),
    });
    if let Some((at, code)) = rejected {
        report(at, code);
    }
    let trailing = bytes.iter().rev().take_while(|byte| **byte == b' ').count();
    if trailing > 0 {
        report(bytes.len() - trailing, Code::LexTrailingWhitespace);
    }
    if last_unterminated {
        report(bytes.len(), Code::LexMissingFinalNewline);
    }

    let first = chars
        .iter()
        .position(|&(at, _)| at >= indentation)
        .unwrap_or(chars.len());
    let mut scanner = Scanner {
        chars: &chars,
        len: bytes.len(),
        index: first,
        tokens: Vec::new(),
        comment: None,
    };
    if let Err((at, code)) = scanner.run() {
        report(at, code);
    }
    let limit = error.map_or(usize::MAX, |(at, _)| at);
    let tokens = scanner
        .tokens
        .into_iter()
        .filter(|&(_, _, end)| end <= limit)
        .collect();
    let comment = scanner.comment.filter(|_| error.is_none());
    LineScan {
        tokens,
        comment,
        error,
    }
}

/// The exact fix of a line's lexical error at byte `at`, as replacements of byte ranges of the
/// line.
fn line_fix(bytes: &[u8], at: usize, code: Code) -> Option<Vec<(usize, usize, String)>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let literals = literals(text);
    let held = |offset: usize| literals[offset];
    let fix = match code {
        Code::LexCarriageReturn => text
            .match_indices('\r')
            .map(|(offset, _)| (offset, offset + 1, String::new()))
            .collect(),
        Code::LexTab => {
            let indentation = text.len() - text.trim_start_matches([' ', '\t']).len();
            text.match_indices('\t')
                .map(|(offset, _)| {
                    let replacement = if offset < indentation {
                        "    "
                    } else if held(offset).is_some() {
                        "\\t"
                    } else {
                        " "
                    };
                    (offset, offset + 1, replacement.to_owned())
                })
                .collect()
        }
        Code::LexTrailingWhitespace => {
            let kept = text.trim_end_matches(' ').len();
            if held(text.len() - 1) == Some(Literal::TextLine) {
                vec![(text.len() - 1, text.len(), "\\x20".to_owned())]
            } else {
                vec![(kept, text.len(), String::new())]
            }
        }
        Code::LexMissingFinalNewline => vec![(text.len(), text.len(), "\n".to_owned())],
        Code::LexRejectedCodePoint => {
            let c = text[at..].chars().next()?;
            let replacement = match held(at) {
                Some(Literal::Text | Literal::Char | Literal::TextLine) => {
                    format!("\\u{{{:X}}}", u32::from(c))
                }
                Some(Literal::Bytes | Literal::Byte) if c.is_ascii() => {
                    format!("\\x{:02X}", u32::from(c))
                }
                Some(_) => return None,
                None if in_ranges(c, &REJECTED_SPACE) => " ".to_owned(),
                None => String::new(),
            };
            vec![(at, at + c.len_utf8(), replacement)]
        }
        Code::LexNotAtLineStart
            if text[at..].starts_with("///") || text[at..].starts_with("//!") =>
        {
            vec![(at, at + 3, "//".to_owned())]
        }
        Code::LexLiteralSuffix => {
            let rest = &text.as_bytes()[at..];
            let mut length = 0;
            while let Some(&byte) = rest.get(length) {
                let fraction = byte == b'.' && rest.get(length + 1).is_some_and(u8::is_ascii_digit);
                if !(byte.is_ascii_alphanumeric() || byte == b'_' || fraction) {
                    break;
                }
                length += 1;
            }
            let run = &text[at..at + length];
            let suffix = dec_int(run).filter(|rest| matches!(*rest, "f32" | "f64"))?;
            let before = at + run.len() - suffix.len();
            vec![(before, before, ".0".to_owned())]
        }
        Code::LexEscapeOutOfRange => {
            let digits = text[at..].strip_prefix("\\x")?.get(..2)?;
            vec![(
                at,
                at + 4,
                format!("\\u{{{}}}", digits.to_ascii_uppercase()),
            )]
        }
        Code::LexUnknownEscape if held(at).is_some_and(Literal::holds_bytes) => {
            let rest = text[at..].strip_prefix("\\u{")?;
            let digits = &rest[..rest.find('}')?];
            let value = u32::from_str_radix(digits, 16)
                .ok()
                .filter(|value| *value < 0x80)?;
            vec![(at, at + 4 + digits.len(), format!("\\x{value:02X}"))]
        }
        _ => return None,
    };
    Some(fix)
}

/// The literal holding each byte of a line, read as the scanner reads literals, past the
/// characters it rejects; none in code and comments.
fn literals(text: &str) -> Vec<Option<Literal>> {
    let mut held = vec![None; text.len() + 1];
    let bytes = text.as_bytes();
    let first = text.len() - text.trim_start_matches(' ').len();
    if text[first..].starts_with("\\\\") {
        held[first..].fill(Some(Literal::TextLine));
        return held;
    }
    let mut at = 0;
    while at < bytes.len() {
        let (literal, contents) = match (bytes[at], bytes.get(at + 1)) {
            (b'/', Some(b'/')) => break,
            (b'"', _) => (Literal::Text, at + 1),
            (b'\'', _) => (Literal::Char, at + 1),
            (b'b', Some(b'"')) => (Literal::Bytes, at + 2),
            (b'b', Some(b'\'')) => (Literal::Byte, at + 2),
            (byte, _) if byte.is_ascii_alphanumeric() || byte == b'_' => {
                while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
                    at += 1;
                }
                continue;
            }
            _ => {
                at += 1;
                continue;
            }
        };
        let quote = bytes[contents - 1];
        let mut end = contents;
        while end < bytes.len() && bytes[end] != quote {
            end += if bytes[end] == b'\\' { 2 } else { 1 };
        }
        let end = (end + 1).min(bytes.len());
        held[at..end].fill(Some(literal));
        at = end;
    }
    held
}

fn in_ranges(c: char, ranges: &[(u32, u32)]) -> bool {
    let value = c as u32;
    ranges
        .iter()
        .any(|&(low, high)| (low..=high).contains(&value))
}

pub(crate) fn is_rejected(c: char) -> bool {
    !(0x20..0x7F).contains(&(c as u32)) && in_ranges(c, &REJECTED)
}

fn rejected_code(c: char) -> Option<Code> {
    match c {
        '\t' => Some(Code::LexTab),
        '\r' => Some(Code::LexCarriageReturn),
        c if is_rejected(c) => Some(Code::LexRejectedCodePoint),
        _ => None,
    }
}

type Scanned = Result<(), (usize, Code)>;

/// Scans the tokens of one line. A rejected or ill-formed character stops the scan without an
/// error of its own: the line's character scan reports it.
struct Scanner<'a> {
    chars: &'a [(usize, Ch)],
    len: usize,
    index: usize,
    tokens: Vec<(TokenKind, usize, usize)>,
    comment: Option<usize>,
}

impl Scanner<'_> {
    fn char_at(&self, index: usize) -> Option<char> {
        match self.chars.get(index) {
            Some((_, Ch::Char(c))) => Some(*c),
            _ => None,
        }
    }

    fn offset(&self, index: usize) -> usize {
        self.chars.get(index).map_or(self.len, |(at, _)| *at)
    }

    fn push(&mut self, kind: TokenKind, from: usize, to: usize) {
        self.tokens.push((kind, self.offset(from), self.offset(to)));
        self.index = to;
    }

    fn run(&mut self) -> Scanned {
        while self.index < self.chars.len() {
            let start = self.index;
            let Some(c) = self.char_at(start) else {
                return Ok(());
            };
            if c == ' ' {
                self.index += 1;
                continue;
            }
            if is_rejected(c) {
                return Ok(());
            }
            let first = self.tokens.is_empty();
            let next = self.char_at(start + 1);
            match c {
                '/' if next == Some('/') => {
                    let kind = match self.char_at(start + 2) {
                        Some('/') => TokenKind::Doc,
                        Some('!') => TokenKind::FileDoc,
                        _ => {
                            self.comment = Some(self.offset(start));
                            return Ok(());
                        }
                    };
                    if !first {
                        return Err((self.offset(start), Code::LexNotAtLineStart));
                    }
                    self.push(kind, start, self.chars.len());
                }
                '\\' if next == Some('\\') => {
                    if !first {
                        return Err((self.offset(start), Code::LexNotAtLineStart));
                    }
                    let end = self.chars.len();
                    self.content(start + 2, end, Literal::TextLine)?;
                    self.push(TokenKind::TextLine, start, end);
                }
                '"' => self.quoted(start, start + 1, Literal::Text)?,
                '\'' => self.quoted(start, start + 1, Literal::Char)?,
                'b' if next == Some('"') => self.quoted(start, start + 2, Literal::Bytes)?,
                'b' if next == Some('\'') => self.quoted(start, start + 2, Literal::Byte)?,
                c if c.is_ascii_alphabetic() || c == '_' => self.word(start),
                c if c.is_ascii_digit() => self.number(start)?,
                _ => self.punct(start, c)?,
            }
        }
        Ok(())
    }

    fn word(&mut self, start: usize) {
        let mut end = start + 1;
        while self
            .char_at(end)
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            end += 1;
        }
        let text: String = (start..end)
            .filter_map(|index| self.char_at(index))
            .collect();
        let kind = Keyword::from_spelling(&text).map_or(TokenKind::Ident, TokenKind::Keyword);
        self.push(kind, start, end);
    }

    fn number(&mut self, start: usize) -> Scanned {
        let mut end = start + 1;
        loop {
            match self.char_at(end) {
                Some(c) if c.is_ascii_alphanumeric() || c == '_' => end += 1,
                Some('.') if self.char_at(end + 1).is_some_and(|c| c.is_ascii_digit()) => end += 2,
                _ => break,
            }
        }
        let run: String = (start..end)
            .filter_map(|index| self.char_at(index))
            .collect();
        let kind = classify_number(&run).map_err(|code| (self.offset(start), code))?;
        self.push(kind, start, end);
        Ok(())
    }

    fn punct(&mut self, start: usize, c: char) -> Scanned {
        let ahead: String = (start..start + 3)
            .map_while(|index| self.char_at(index))
            .collect();
        let punct = Punct::ALL
            .iter()
            .map(|punct| (punct.spelling(), TokenKind::Punct(*punct)))
            .chain(
                Foreign::ALL
                    .iter()
                    .map(|foreign| (foreign.spelling(), TokenKind::Foreign(*foreign))),
            )
            .filter(|(spelling, _)| ahead.starts_with(spelling))
            .max_by_key(|(spelling, _)| spelling.len());
        let Some((spelling, kind)) = punct else {
            let code = if c.is_ascii() {
                Code::LexUnexpectedCharacter
            } else {
                Code::LexNonAscii
            };
            return Err((self.offset(start), code));
        };
        self.push(kind, start, start + spelling.len());
        Ok(())
    }

    /// Scans a literal from its opener at `open`, its contents starting at `contents`.
    fn quoted(&mut self, open: usize, contents: usize, literal: Literal) -> Scanned {
        let quote = self
            .char_at(contents - 1)
            .expect("a literal opens with a quote");
        let mut index = contents;
        let close = loop {
            match self.chars.get(index) {
                None => return Err((self.offset(open), Code::LexUnterminatedLiteral)),
                Some((_, Ch::Char('\\'))) => index += 2,
                Some((_, Ch::Char(c))) if *c == quote => break index,
                Some(_) => index += 1,
            }
        };
        self.content(contents, close, literal)?;
        if matches!(literal, Literal::Char | Literal::Byte)
            && self.units(contents, close, literal) != 1
        {
            return Err((self.offset(open), Code::LexCharLiteralSize));
        }
        if literal == Literal::Byte
            && let Some(at) = (contents..close).find(|&index| self.is_non_ascii(index))
        {
            return Err((self.offset(at), Code::LexNonAsciiByte));
        }
        self.push(literal.kind(), open, close + 1);
        Ok(())
    }

    /// A character that is neither ASCII nor rejected, which a byte literal cannot hold.
    fn is_non_ascii(&self, index: usize) -> bool {
        self.char_at(index)
            .is_some_and(|c| !c.is_ascii() && !is_rejected(c))
    }

    /// Checks the escapes of a literal's contents in order, and the characters of a `BYTES`.
    fn content(&self, from: usize, to: usize, literal: Literal) -> Scanned {
        let mut index = from;
        while index < to {
            if self.char_at(index) == Some('\\') {
                index += self.escape(index, to, literal)?;
            } else if literal == Literal::Bytes && self.is_non_ascii(index) {
                return Err((self.offset(index), Code::LexNonAsciiByte));
            } else {
                index += 1;
            }
        }
        Ok(())
    }

    /// The number of characters and escapes of contents whose escapes are valid.
    fn units(&self, from: usize, to: usize, literal: Literal) -> usize {
        let mut index = from;
        let mut units = 0;
        while index < to {
            if self.char_at(index) == Some('\\') {
                index += self
                    .escape(index, to, literal)
                    .expect("escapes were checked");
            } else {
                index += 1;
            }
            units += 1;
        }
        units
    }

    /// The length in characters of the escape at `at`, which ends before `to`.
    fn escape(&self, at: usize, to: usize, literal: Literal) -> Result<usize, (usize, Code)> {
        let unknown = Err((self.offset(at), Code::LexUnknownEscape));
        let out_of_range = Err((self.offset(at), Code::LexEscapeOutOfRange));
        let get = |index: usize| {
            if index < to {
                self.char_at(index)
            } else {
                None
            }
        };
        match get(at + 1) {
            Some('\\' | '"' | '\'' | 'n' | 'r' | 't' | '0') => Ok(2),
            Some('x') => {
                let digits: Option<String> = (at + 2..at + 4).map(&get).collect();
                let Some(value) = digits.and_then(|digits| u32::from_str_radix(&digits, 16).ok())
                else {
                    return unknown;
                };
                if value > 0x7F && !literal.holds_bytes() {
                    out_of_range
                } else {
                    Ok(4)
                }
            }
            Some('u') if !literal.holds_bytes() && get(at + 2) == Some('{') => {
                let digits = (at + 3..)
                    .map_while(|index| get(index).filter(char::is_ascii_hexdigit))
                    .count();
                if !(1..=6).contains(&digits) || get(at + 3 + digits) != Some('}') {
                    return unknown;
                }
                let text: String = (at + 3..at + 3 + digits).filter_map(&get).collect();
                let value = u32::from_str_radix(&text, 16).expect("hexadecimal digits");
                if char::from_u32(value).is_none() {
                    out_of_range
                } else {
                    Ok(digits + 4)
                }
            }
            _ => unknown,
        }
    }
}

/// Classifies a number run as an `INT` or `FLOAT` token, or the error it is.
fn classify_number(run: &str) -> Result<TokenKind, Code> {
    let int_suffix = |rest: &str| {
        rest.is_empty()
            || [
                "u8", "u16", "u32", "u64", "usize", "i8", "i16", "i32", "i64", "isize",
            ]
            .contains(&rest)
    };
    let float_suffix = |rest: &str| ["f32", "f64"].contains(&rest);
    let integer = if let Some(rest) = run.strip_prefix("0x") {
        digits(rest, |c| c.is_ascii_hexdigit())
    } else if let Some(rest) = run.strip_prefix("0b") {
        digits(rest, |c| c == '0' || c == '1')
    } else {
        dec_int(run)
    };
    if integer.is_some_and(int_suffix) {
        return Ok(TokenKind::Int);
    }
    let fraction = dec_int(run)
        .and_then(|rest| rest.strip_prefix('.'))
        .and_then(|rest| digits(rest, |c| c.is_ascii_digit()));
    if fraction.is_some_and(|rest| rest.is_empty() || float_suffix(rest)) {
        return Ok(TokenKind::Float);
    }
    let binary = run
        .strip_prefix("0b")
        .and_then(|rest| digits(rest, |c| c == '0' || c == '1'));
    if [dec_int(run), binary]
        .into_iter()
        .flatten()
        .any(float_suffix)
        || fraction.is_some_and(int_suffix)
    {
        return Err(Code::LexLiteralSuffix);
    }
    Err(Code::LexMalformedNumber)
}

/// Matches `"0" | "1" .. "9" ( "_"? DIGIT )*` at the start of `text`, returning the rest.
fn dec_int(text: &str) -> Option<&str> {
    match text.strip_prefix('0') {
        Some(rest) => Some(rest),
        None => text
            .starts_with(|c: char| c.is_ascii_digit())
            .then(|| digits(text, |c| c.is_ascii_digit()))?,
    }
}

/// Matches `D ( "_"? D )*` at the start of `text` for the digits `D`, returning the rest.
fn digits(text: &str, is_digit: impl Fn(char) -> bool) -> Option<&str> {
    let bytes = text.as_bytes();
    let digit = |index: usize| bytes.get(index).is_some_and(|byte| is_digit(*byte as char));
    if !digit(0) {
        return None;
    }
    let mut end = 1;
    loop {
        if digit(end) {
            end += 1;
        } else if bytes.get(end) == Some(&b'_') && digit(end + 1) {
            end += 2;
        } else {
            return Some(&text[end..]);
        }
    }
}

#[cfg(test)]
mod tests;
