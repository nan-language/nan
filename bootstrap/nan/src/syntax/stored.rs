//! The syntax of source files as the build cache holds it: what parsing a file gives but its
//! source.

use super::file::{FileDiagnostic, ItemSyntax, SourceFile};
use super::grammar::{Foreign, Keyword, Punct, Rule};
use super::lexer::Line;
use super::token::{Token, TokenKind};
use super::tree::{Child, Node, Tree};
use crate::diagnostic::{Code, Offer, Replacement};
use crate::encoding::{bytes, svar, uvar};

const OFFERS: [Offer; 3] = [Offer::Each, Offer::First, Offer::Unique];

fn put(out: &mut Vec<u8>, value: impl Into<u128>) {
    uvar(out, value.into());
}

fn put_count(out: &mut Vec<u8>, value: usize) {
    put(out, value as u64);
}

/// A position as the step from the position before it.
fn put_step(out: &mut Vec<u8>, from: u32, to: u32) {
    svar(out, i128::from(to) - i128::from(from));
}

fn put_option(out: &mut Vec<u8>, value: Option<u32>) {
    put(out, value.is_some());
    if let Some(value) = value {
        put(out, value);
    }
}

fn put_text(out: &mut Vec<u8>, value: Option<&str>) {
    put(out, value.is_some());
    if let Some(value) = value {
        bytes(out, value.as_bytes());
    }
}

fn put_kind(out: &mut Vec<u8>, kind: TokenKind) {
    let (tag, index) = match kind {
        TokenKind::Ident => (0, None),
        TokenKind::Int => (1, None),
        TokenKind::Float => (2, None),
        TokenKind::Text => (3, None),
        TokenKind::Bytes => (4, None),
        TokenKind::Char => (5, None),
        TokenKind::Byte => (6, None),
        TokenKind::TextLine => (7, None),
        TokenKind::Doc => (8, None),
        TokenKind::FileDoc => (9, None),
        TokenKind::Keyword(keyword) => (10, Some(keyword as usize)),
        TokenKind::Punct(punct) => (11, Some(punct as usize)),
        TokenKind::Foreign(foreign) => (12, Some(foreign as usize)),
        TokenKind::Nl => (13, None),
        TokenKind::Col0 => (14, None),
        TokenKind::Eof => (15, None),
        TokenKind::LexError => (16, None),
    };
    put(out, tag as u8);
    if let Some(index) = index {
        put_count(out, index);
    }
}

impl SourceFile {
    /// The syntax of the parsed file, as `decoded` reads it.
    pub fn stored(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_count(&mut out, self.tokens.len());
        let (mut end, mut line) = (0, 0);
        for token in &self.tokens {
            put_kind(&mut out, token.kind);
            put_step(&mut out, end, token.start);
            put_step(&mut out, token.start, token.end);
            put_step(&mut out, line, token.line);
            (end, line) = (token.end, token.line);
        }
        put_count(&mut out, self.lines.len());
        let mut line_end = 0;
        for line in &self.lines {
            put_step(&mut out, line_end, line.start);
            put_step(&mut out, line.start, line.end);
            put_option(&mut out, line.comment);
            put(&mut out, line.lexical_error);
            line_end = line.end;
        }
        put_option(&mut out, self.summary.map(|at| at as u32));
        put_count(&mut out, self.items.len());
        for item in &self.items {
            put_count(&mut out, item.first);
            put_count(&mut out, item.end);
            put(&mut out, item.tree.is_some());
            if let Some(tree) = &item.tree {
                tree.store(&mut out);
            }
            put(&mut out, item.abandoned);
        }
        put_count(&mut out, self.comment.len());
        for read in &self.comment {
            put(&mut out, *read);
        }
        put_count(&mut out, self.diagnostics.len());
        for diagnostic in &self.diagnostics {
            diagnostic.store(&mut out);
        }
        out
    }
}

impl Tree {
    fn store(&self, out: &mut Vec<u8>) {
        put_count(out, self.nodes.len());
        let mut node_end = 0;
        for node in &self.nodes {
            put_count(out, node.rule as usize);
            put_step(out, node_end, node.children.0);
            put_step(out, node.children.0, node.children.1);
            node_end = node.children.1;
        }
        put_count(out, self.children.len());
        let mut last_token = 0;
        for child in &self.children {
            match *child {
                Child::Node(node) => put(out, u64::from(node) << 1),
                Child::Token(token) => {
                    put(out, 1u8);
                    put_step(out, last_token, token);
                    last_token = token;
                }
            }
        }
        put(out, self.root);
    }
}

impl FileDiagnostic {
    fn store(&self, out: &mut Vec<u8>) {
        put(out, self.line);
        bytes(out, self.code.name().as_bytes());
        put_text(out, self.expected.as_deref());
        put_text(out, self.found.as_deref());
        put(out, self.span.is_some());
        if let Some((start, end)) = self.span {
            put(out, start);
            put(out, end);
        }
        put_option(out, self.failing);
        put_count(out, self.fixes.len());
        for fix in &self.fixes {
            put_count(out, fix.len());
            for replacement in fix {
                put(out, replacement.start);
                put(out, replacement.end);
                bytes(out, replacement.text.as_bytes());
            }
        }
        let offer = OFFERS.iter().position(|offer| *offer == self.offer);
        put_count(out, offer.expect("an offer"));
    }
}

/// Stored bytes being read and where the next value starts; a read of a value malformed or past
/// their end gives none.
struct Reader<'b> {
    bytes: &'b [u8],
    at: usize,
}

impl<'b> Reader<'b> {
    fn uvar(&mut self) -> Option<u64> {
        let (mut value, mut shift) = (0u64, 0);
        loop {
            let byte = *self.bytes.get(self.at)?;
            self.at += 1;
            if shift > 63 {
                return None;
            }
            value |= u64::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
            shift += 7;
        }
    }

    fn number(&mut self) -> Option<u32> {
        u32::try_from(self.uvar()?).ok()
    }

    /// A count, none when larger than the bytes left could hold.
    fn count(&mut self) -> Option<usize> {
        let value = self.uvar()?;
        let left = (self.bytes.len() - self.at) as u64;
        (value <= left).then_some(value as usize)
    }

    /// The position a step leads to from `from`.
    fn step(&mut self, from: u32) -> Option<u32> {
        let held = self.uvar()?;
        let magnitude = i64::try_from(held >> 1).ok()?;
        let step = match held & 1 {
            0 => magnitude,
            _ => -magnitude - 1,
        };
        u32::try_from(i64::from(from).checked_add(step)?).ok()
    }

    fn flag(&mut self) -> Option<bool> {
        match self.uvar()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    fn option(&mut self) -> Option<Option<u32>> {
        match self.flag()? {
            true => Some(Some(self.number()?)),
            false => Some(None),
        }
    }

    fn text(&mut self) -> Option<String> {
        let length = self.count()?;
        let held = self.bytes.get(self.at..self.at + length)?;
        self.at += length;
        String::from_utf8(held.to_vec()).ok()
    }

    fn optional_text(&mut self) -> Option<Option<String>> {
        match self.flag()? {
            true => Some(Some(self.text()?)),
            false => Some(None),
        }
    }

    fn indexed<T: Copy>(&mut self, listed: &[T]) -> Option<T> {
        listed.get(usize::try_from(self.uvar()?).ok()?).copied()
    }

    fn kind(&mut self) -> Option<TokenKind> {
        Some(match self.uvar()? {
            0 => TokenKind::Ident,
            1 => TokenKind::Int,
            2 => TokenKind::Float,
            3 => TokenKind::Text,
            4 => TokenKind::Bytes,
            5 => TokenKind::Char,
            6 => TokenKind::Byte,
            7 => TokenKind::TextLine,
            8 => TokenKind::Doc,
            9 => TokenKind::FileDoc,
            10 => TokenKind::Keyword(self.indexed(&Keyword::ALL)?),
            11 => TokenKind::Punct(self.indexed(&Punct::ALL)?),
            12 => TokenKind::Foreign(self.indexed(&Foreign::ALL)?),
            13 => TokenKind::Nl,
            14 => TokenKind::Col0,
            15 => TokenKind::Eof,
            16 => TokenKind::LexError,
            _ => return None,
        })
    }

    /// The syntax of a file of `length` bytes, its source left empty.
    fn file(&mut self, length: usize) -> Option<SourceFile> {
        let token_count = self.count()?;
        let mut tokens = Vec::with_capacity(token_count);
        let (mut end, mut line) = (0, 0);
        for _ in 0..token_count {
            let kind = self.kind()?;
            let start = self.step(end)?;
            end = self.step(start)?;
            line = self.step(line)?;
            if start > end || end as usize > length {
                return None;
            }
            tokens.push(Token {
                kind,
                start,
                end,
                line,
            });
        }
        let line_count = self.count()?;
        let mut lines = Vec::with_capacity(line_count);
        let mut line_end = 0;
        for _ in 0..line_count {
            let start = self.step(line_end)?;
            line_end = self.step(start)?;
            lines.push(Line {
                start,
                end: line_end,
                comment: self.option()?,
                lexical_error: self.flag()?,
            });
        }
        let summary = self.option()?.map(|at| at as usize);
        let item_count = self.count()?;
        let mut items = Vec::with_capacity(item_count);
        for _ in 0..item_count {
            items.push(ItemSyntax {
                first: self.count()?,
                end: self.count()?,
                tree: match self.flag()? {
                    true => Some(self.tree()?),
                    false => None,
                },
                abandoned: self.flag()?,
            });
        }
        let comment_count = self.count()?;
        let comment = (0..comment_count)
            .map(|_| self.flag())
            .collect::<Option<Vec<bool>>>()?;
        let diagnostic_count = self.count()?;
        let diagnostics = (0..diagnostic_count)
            .map(|_| self.diagnostic())
            .collect::<Option<Vec<_>>>()?;
        let bounded = items
            .iter()
            .all(|item| item.first <= item.end && item.end <= tokens.len());
        (bounded && comment.len() == tokens.len()).then_some(SourceFile {
            source: Vec::new(),
            tokens,
            lines,
            summary,
            items,
            comment,
            diagnostics,
        })
    }

    fn tree(&mut self) -> Option<Tree> {
        let node_count = self.count()?;
        let mut nodes = Vec::with_capacity(node_count);
        let mut node_end = 0;
        for _ in 0..node_count {
            let rule = self.indexed(&Rule::ALL)?;
            let start = self.step(node_end)?;
            node_end = self.step(start)?;
            nodes.push(Node {
                rule,
                children: (start, node_end),
            });
        }
        let child_count = self.count()?;
        let mut children = Vec::with_capacity(child_count);
        let mut last_token = 0;
        for _ in 0..child_count {
            children.push(match self.uvar()? {
                1 => {
                    last_token = self.step(last_token)?;
                    Child::Token(last_token)
                }
                held if held & 1 == 0 => Child::Node(u32::try_from(held >> 1).ok()?),
                _ => return None,
            });
        }
        let root = self.number()?;
        let well_formed = (nodes.iter()).all(|node| {
            node.children.0 <= node.children.1 && node.children.1 as usize <= children.len()
        }) && (children.iter())
            .all(|child| !matches!(child, Child::Node(node) if *node as usize >= nodes.len()))
            && (root as usize) < nodes.len().max(1);
        well_formed.then_some(Tree {
            nodes,
            children,
            root,
        })
    }

    fn diagnostic(&mut self) -> Option<FileDiagnostic> {
        let line = self.number()?;
        let code = Code::from_name(&self.text()?)?;
        let expected = self.optional_text()?;
        let found = self.optional_text()?;
        let span = match self.flag()? {
            true => Some((self.number()?, self.number()?)),
            false => None,
        };
        let failing = self.option()?;
        let fix_count = self.count()?;
        let mut fixes = Vec::with_capacity(fix_count);
        for _ in 0..fix_count {
            let replacement_count = self.count()?;
            let mut fix = Vec::with_capacity(replacement_count);
            for _ in 0..replacement_count {
                fix.push(Replacement {
                    start: self.number()?,
                    end: self.number()?,
                    text: self.text()?,
                });
            }
            fixes.push(fix);
        }
        let offer = self.indexed(&OFFERS)?;
        Some(FileDiagnostic {
            line,
            code,
            expected,
            found,
            span,
            failing,
            fixes,
            offer,
        })
    }
}

/// The syntax of a file of `source` that `stored` holds; the source back when it holds none.
pub fn decoded(stored: &[u8], source: Vec<u8>) -> Result<SourceFile, Vec<u8>> {
    let mut reader = Reader {
        bytes: stored,
        at: 0,
    };
    match reader.file(source.len()) {
        Some(held) if reader.at == stored.len() => Ok(SourceFile { source, ..held }),
        _ => Err(source),
    }
}

#[cfg(test)]
mod tests;
