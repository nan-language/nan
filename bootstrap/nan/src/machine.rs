//! Machine files: the lock, the history and the API snapshots the toolchain writes in a package's
//! `.nan` directory. Each is a kind line, one record per logical line, and a check line holding
//! the hash of every byte before it.

pub mod api;
pub mod history;
pub mod lock;

use crate::diagnostic::Code;
use crate::hash::{Hash, parse_form};
use crate::syntax::lexer::{is_rejected, lex};
use crate::syntax::literal::text_value;
use crate::syntax::token::TokenKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Lock,
    History,
    Api,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Lock => "lock",
            Kind::History => "history",
            Kind::Api => "api",
        }
    }
}

/// Why a machine file is not valid: the code and the line it is reported on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Invalid {
    pub code: Code,
    pub line: u32,
}

impl Invalid {
    fn at(line: u32) -> Invalid {
        Invalid {
            code: Code::FormatMachineInvalid,
            line,
        }
    }
}

/// A token of a record: a word (an identifier or keyword), an integer, a text's value or a
/// punctuation mark.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tok {
    Word(String),
    Int(String),
    Text(String),
    Punct(String),
}

/// A record: the physical lines it spans, its tokens and its text.
pub struct Record {
    pub line: u32,
    pub last_line: u32,
    pub tokens: Vec<Tok>,
    pub text: String,
}

/// The records of a machine file between its kind line and its check line.
pub struct Parsed {
    pub records: Vec<Record>,
    check_line: u32,
    check_holds: bool,
}

impl Parsed {
    /// The file's validity once its records parse: an invalid check is reported on its line.
    pub fn checked<T>(&self, value: T) -> Result<T, Invalid> {
        match self.check_holds {
            true => Ok(value),
            false => Err(Invalid::at(self.check_line)),
        }
    }
}

/// Reads the common syntax of a machine file of a kind.
pub fn read(kind: Kind, bytes: &[u8]) -> Result<Parsed, Invalid> {
    let Ok(source) = std::str::from_utf8(bytes) else {
        let valid = std::str::from_utf8(&bytes[..first_invalid(bytes)]).unwrap_or_default();
        return Err(Invalid::at(valid.matches('\n').count() as u32 + 1));
    };
    let lexed = lex(bytes);
    let physical = lexed.lines.len() as u32;
    if let Some(diagnostic) = lexed.diagnostics.first() {
        return Err(Invalid::at(diagnostic.line));
    }
    if let Some(line) = lexed.lines.iter().position(|line| line.comment.is_some()) {
        return Err(Invalid::at(line as u32 + 1));
    }
    if !source.ends_with('\n') {
        return Err(Invalid::at(physical));
    }
    let mut records: Vec<Record> = Vec::new();
    let mut open: Option<Record> = None;
    for token in &lexed.tokens {
        match token.kind {
            TokenKind::Col0 | TokenKind::Eof => {
                records.extend(open.take());
            }
            TokenKind::Nl => {}
            kind => {
                let text = token.text(bytes);
                let tok = match kind {
                    TokenKind::Ident | TokenKind::Keyword(_) => Tok::Word(text.to_owned()),
                    TokenKind::Int => Tok::Int(text.to_owned()),
                    TokenKind::Text => {
                        let value = text_value(text);
                        if escaped(&value) != text {
                            return Err(Invalid::at(token.line));
                        }
                        Tok::Text(value)
                    }
                    TokenKind::Punct(_) => Tok::Punct(text.to_owned()),
                    _ => return Err(Invalid::at(token.line)),
                };
                let record = open.get_or_insert_with(|| Record {
                    line: token.line,
                    last_line: token.line,
                    tokens: Vec::new(),
                    text: String::new(),
                });
                record.last_line = token.line;
                record.tokens.push(tok);
            }
        }
    }
    records.extend(open);
    let line_text = |line: u32| -> &str {
        let line = &lexed.lines[line as usize - 1];
        &source[line.start as usize..line.end as usize]
    };
    let mut covered = 0;
    for record in &mut records {
        if record.line != covered + 1 {
            return Err(Invalid::at(covered + 1));
        }
        record.text = (record.line..=record.last_line)
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        covered = record.last_line;
    }
    if covered != physical {
        return Err(Invalid::at(covered + 1));
    }
    let Some(first) = records.first() else {
        return Err(Invalid::at(1));
    };
    if first.last_line != 1 {
        return Err(Invalid::at(1));
    }
    match &first.tokens[..] {
        [Tok::Word(nan), Tok::Word(word), Tok::Int(version)]
            if nan == "nan" && word == kind.word() =>
        {
            if version != "1" {
                return Err(Invalid {
                    code: Code::FormatMachineVersion,
                    line: 1,
                });
            }
        }
        _ => return Err(Invalid::at(1)),
    }
    let last = records.pop().expect("the kind line is a record");
    if records.is_empty() {
        return Err(Invalid::at(last.line));
    }
    let checked = &source[..source.len() - last.text.len() - 1];
    let check_holds = match &last.tokens[..] {
        [Tok::Word(check), Tok::Text(hash)] if check == "check" && last.last_line == last.line => {
            *hash == Hash::of(checked.as_bytes()).full()
        }
        _ => return Err(Invalid::at(last.line)),
    };
    records.remove(0);
    Ok(Parsed {
        records,
        check_line: last.line,
        check_holds,
    })
}

/// The offset of the first byte that is not part of well-formed UTF-8.
fn first_invalid(bytes: &[u8]) -> usize {
    std::str::from_utf8(bytes)
        .err()
        .map_or(bytes.len(), |error| error.valid_up_to())
}

/// Writes a machine file of a kind from its record lines: none when it holds no record.
pub fn write(kind: Kind, lines: &[String]) -> Option<Vec<u8>> {
    if lines.is_empty() {
        return None;
    }
    let mut text = format!("nan {} 1\n", kind.word());
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    let check = Hash::of(text.as_bytes()).full();
    text.push_str(&format!("check \"{check}\"\n"));
    Some(text.into_bytes())
}

/// A text as a machine file writes it: quoted, with `\`, `"` and LF escaped and every other code
/// point below U+0020, U+007F and every rejected code point written `\u{H}`.
pub fn escaped(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 || c == '\u{7F}' || is_rejected(c) => {
                out.push_str(&format!("\\u{{{:X}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The hash a text holds in full form.
pub fn hash_of(text: &str) -> Option<Hash> {
    let bytes = parse_form(text).filter(|bytes| bytes.len() == 33)?;
    Some(Hash(bytes.try_into().ok()?))
}

/// A record's tokens read in order.
pub struct Cursor<'a> {
    tokens: &'a [Tok],
    at: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(record: &'a Record) -> Cursor<'a> {
        Cursor {
            tokens: &record.tokens,
            at: 0,
        }
    }

    fn next(&mut self) -> Option<&'a Tok> {
        let token = self.tokens.get(self.at)?;
        self.at += 1;
        Some(token)
    }

    fn peek(&self) -> Option<&'a Tok> {
        self.tokens.get(self.at)
    }

    pub fn word(&mut self) -> Option<&'a str> {
        match self.peek()? {
            Tok::Word(word) => {
                self.at += 1;
                Some(word)
            }
            _ => None,
        }
    }

    /// Reads the word `expected` when it comes next.
    pub fn takes(&mut self, expected: &str) -> bool {
        let next = matches!(self.peek(), Some(Tok::Word(word)) if word == expected);
        self.at += usize::from(next);
        next
    }

    pub fn punct(&mut self, expected: &str) -> bool {
        let next = matches!(self.peek(), Some(Tok::Punct(punct)) if punct == expected);
        self.at += usize::from(next);
        next
    }

    pub fn text(&mut self) -> Option<&'a str> {
        match self.peek()? {
            Tok::Text(text) => {
                self.at += 1;
                Some(text)
            }
            _ => None,
        }
    }

    pub fn hash(&mut self) -> Option<Hash> {
        hash_of(self.text()?)
    }

    /// A decimal integer written without separators, suffix or leading zeros.
    pub fn int(&mut self) -> Option<u64> {
        let Tok::Int(digits) = self.next()? else {
            return None;
        };
        let canonical = digits.bytes().all(|byte| byte.is_ascii_digit())
            && (digits == "0" || !digits.starts_with('0'));
        canonical.then(|| digits.parse().ok()).flatten()
    }

    /// A target name: `any` or a word, `-` and a word.
    pub fn target(&mut self) -> Option<String> {
        let first = self.word()?.to_owned();
        if !self.punct("-") {
            return Some(first);
        }
        Some(format!("{first}-{}", self.word()?))
    }

    pub fn done(&self) -> bool {
        self.at == self.tokens.len()
    }
}

#[cfg(test)]
mod tests;
