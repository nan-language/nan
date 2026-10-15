//! Closing a prefix: the fewest tokens after which a source file derives the prefix's part before
//! its first dead token, typed holes uncounted, written as canonical source text.

use std::sync::LazyLock;

use super::file::{SourceFile, parse};
use super::grammar::{Atom, CHOICES, ChoiceId, Class, Punct, Repeat, Rule, SEQS, SeqId, Term};
use super::parser::{Discard, Fed, Lookahead, State};
use super::token::TokenKind;
use crate::diagnostic::Code;

/// A prefix closed: the byte of its first dead token, and what closing
/// appends, as pieces and as the source the two make.
pub struct Closed {
    pub dead: Option<u32>,
    pub pieces: Vec<Piece>,
    pub source: Vec<u8>,
}

/// What closing appends on one line of the closed source: a closer, a maximal run of appended
/// tokens other than typed holes, or a typed hole.
pub struct Piece {
    pub line: u32,
    /// The bytes it occupies in the closed source.
    pub span: (u32, u32),
    pub text: String,
    pub hole: bool,
}

/// A token closing appends.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Appended {
    Token(String),
    Nl,
    Col0,
    Hole,
}

/// The cost of deriving each choice and sequence minimally: tokens counted, virtual ones
/// included and typed holes not; rejected alternatives derive nothing.
struct Costs {
    choices: Vec<u32>,
    seqs: Vec<u32>,
}

const UNBOUNDED: u32 = u32::MAX / 4;

static COSTS: LazyLock<Costs> = LazyLock::new(Costs::compute);

impl Costs {
    fn compute() -> Costs {
        let mut costs = Costs {
            choices: vec![UNBOUNDED; CHOICES.len()],
            seqs: vec![UNBOUNDED; SEQS.len()],
        };
        loop {
            let mut changed = false;
            for (seq, def) in SEQS.iter().enumerate() {
                if def.rejection.is_some() {
                    continue;
                }
                let cost = def
                    .elems
                    .iter()
                    .filter(|elem| matches!(elem.repeat, Repeat::One | Repeat::Plus))
                    .map(|elem| costs.atom(elem.atom))
                    .fold(0, |sum, cost| (sum + cost).min(UNBOUNDED));
                if cost < costs.seqs[seq] {
                    costs.seqs[seq] = cost;
                    changed = true;
                }
            }
            for (choice, alternatives) in CHOICES.iter().enumerate() {
                let cost = alternatives
                    .iter()
                    .map(|&seq| costs.seqs[seq])
                    .min()
                    .unwrap_or(UNBOUNDED);
                if cost < costs.choices[choice] {
                    costs.choices[choice] = cost;
                    changed = true;
                }
            }
            if !changed {
                return costs;
            }
        }
    }

    fn atom(&self, atom: Atom) -> u32 {
        match atom {
            Atom::Term(_) => 1,
            Atom::Rule(Rule::TodoExpr) => 0,
            Atom::Rule(rule) => self.choices[rule.choice()],
            Atom::Group(choice) => self.choices[choice],
        }
    }

    /// The cheapest alternative of a choice, the one written first among equals.
    fn cheapest(&self, choice: ChoiceId) -> SeqId {
        CHOICES[choice]
            .iter()
            .copied()
            .filter(|&seq| SEQS[seq].rejection.is_none())
            .min_by_key(|&seq| self.seqs[seq])
            .expect("a choice has an alternative")
    }
}

/// Closes a prefix of a source file: keeps its part before its first dead token and appends the
/// fewest tokens completing the item it ends in.
pub fn close(prefix: &[u8]) -> Closed {
    let (mut kept, ends_line) = cut(prefix);
    let mut file = parse_prefix(&prefix[..kept]);
    let last = file
        .tokens
        .iter()
        .rev()
        .find(|token| !token.kind.is_virtual())
        .map_or(0, |token| token.end);
    let dead = file
        .diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.failing)
        .filter(|&failing| failing < last)
        .min();
    let mut ends_line = ends_line;
    if let Some(dead) = dead {
        let (part, part_ends_line) = cut(&prefix[..dead as usize]);
        kept = part;
        ends_line = part_ends_line;
        file = parse_prefix(&prefix[..kept]);
    }
    let appended = file
        .items
        .last()
        .and_then(|item| finishing(&file, item.first, ends_line))
        .unwrap_or_default();
    let mut source = prefix[..kept].to_vec();
    let pieces = render(&file, ends_line, &appended, &mut source);
    Closed {
        dead,
        pieces,
        source,
    }
}

/// The bytes of a prefix kept before closing, and whether they end a line: a prefix not ending
/// with LF loses the token reaching its cut, an unterminated literal included, and the spaces
/// before it.
fn cut(prefix: &[u8]) -> (usize, bool) {
    if prefix.is_empty() || prefix.ends_with(b"\n") {
        return (prefix.len(), true);
    }
    let file = parse_prefix(prefix);
    let line = file.lines.last().expect("a prefix has a line");
    let reaching = file
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == Code::LexUnterminatedLiteral)
        .filter_map(|diagnostic| diagnostic.failing)
        .find(|&at| at >= line.start)
        .or_else(|| {
            file.tokens
                .iter()
                .find(|token| !token.kind.is_virtual() && token.end as usize == prefix.len())
                .map(|token| token.start)
        })
        .map_or(prefix.len(), |start| start as usize);
    let trimmed = prefix[..reaching]
        .iter()
        .rposition(|byte| *byte != b' ')
        .map_or(0, |at| at + 1)
        .max(line.start as usize);
    (trimmed, false)
}

/// The parse of a prefix's kept part, lexed as a file ending in a line end.
fn parse_prefix(kept: &[u8]) -> SourceFile {
    let mut source = kept.to_vec();
    if !source.is_empty() && !source.ends_with(b"\n") {
        source.push(b'\n');
    }
    parse(source)
}

/// The tokens completing the item starting at `first`, fed up to the prefix's end: no `EOF`, and
/// no `NL` after a last token that the prefix does not end its line with, or that continues its
/// logical line or is a `TEXT_LINE`.
fn finishing(file: &SourceFile, first: usize, ends_line: bool) -> Option<Vec<Appended>> {
    let mut end = file.tokens.len() - 1;
    let last = (0..end)
        .rev()
        .find(|&at| !file.tokens[at].kind.is_virtual())?;
    let kind = file.tokens[last].kind;
    if !ends_line || kind.is_continuing() || kind == TokenKind::TextLine {
        end = last + 1;
    }
    let mut state = State::new(Rule::Item);
    let mut at = first;
    while at < end {
        let next = file.next(at + 1);
        let lookahead = Lookahead {
            token: &file.tokens[at],
            next: (next < end).then(|| &file.tokens[next]),
            at,
        };
        match state.feed(lookahead, &file.source, &mut Discard) {
            Ok(Fed::Consumed) => at = next,
            Ok(Fed::Done) => return Some(Vec::new()),
            Err(_) => return None,
        }
    }
    let label = file.declared_name(first, &state).unwrap_or_default();
    Some(completion(&state, &label))
}

/// The tokens completing a parse: the remaining parts of each open frame, innermost first, each
/// derived minimally.
fn completion(state: &State, label: &str) -> Vec<Appended> {
    let mut out = Vec::new();
    let frames = &state.frames;
    for (depth, frame) in frames.iter().enumerate().rev() {
        let elems = SEQS[frame.seq].elems;
        for index in frame.elem..elems.len() {
            let mut count = 0;
            if index == frame.elem {
                count = frame.count + usize::from(depth + 1 < frames.len());
            }
            let closing_label =
                index > 0 && elems[index - 1].atom == Atom::Term(Term::Punct(Punct::RBrace));
            derive_elem(
                elems[index].atom,
                elems[index].repeat,
                count,
                closing_label,
                label,
                &mut out,
            );
        }
    }
    out
}

/// Derives an element of a sequence minimally when it must appear: an optional or repeated part
/// is skipped.
fn derive_elem(
    atom: Atom,
    repeat: Repeat,
    count: usize,
    closing_label: bool,
    label: &str,
    out: &mut Vec<Appended>,
) {
    if !matches!(repeat, Repeat::One | Repeat::Plus) || count > 0 {
        return;
    }
    let ident = matches!(
        atom,
        Atom::Rule(Rule::ItemLabel) | Atom::Term(Term::Class(Class::Ident))
    );
    if closing_label && ident {
        out.push(Appended::Token(label.to_owned()));
    } else {
        derive(atom, label, out);
    }
}

/// Derives an atom minimally: a typed hole for `todo_expr`, and otherwise the cheapest
/// alternative, the one written first among equals.
fn derive(atom: Atom, label: &str, out: &mut Vec<Appended>) {
    let choice = match atom {
        Atom::Term(term) => {
            out.push(appended(term));
            return;
        }
        Atom::Rule(Rule::TodoExpr) => {
            out.push(Appended::Hole);
            return;
        }
        Atom::Rule(rule) => rule.choice(),
        Atom::Group(choice) => choice,
    };
    let seq = COSTS.cheapest(choice);
    let elems = SEQS[seq].elems;
    for (index, elem) in elems.iter().enumerate() {
        let closing_label =
            index > 0 && elems[index - 1].atom == Atom::Term(Term::Punct(Punct::RBrace));
        derive_elem(elem.atom, elem.repeat, 0, closing_label, label, out);
    }
}

/// An appended terminal: its spelling, `hole` for an identifier, `0` for an integer and
/// `"hole"` for a text.
fn appended(term: Term) -> Appended {
    let text = match term {
        Term::Keyword(keyword) => keyword.spelling(),
        Term::Punct(punct) => punct.spelling(),
        Term::Context(word) => word.spelling(),
        Term::Class(Class::Nl) => return Appended::Nl,
        Term::Class(Class::Col0) => return Appended::Col0,
        Term::Class(Class::Int) => "0",
        Term::Class(Class::Float) => "0.0",
        Term::Class(Class::Text | Class::TextLine) => "\"hole\"",
        Term::Class(Class::Bytes) => "b\"hole\"",
        Term::Class(Class::Char) => "'h'",
        Term::Class(Class::Byte) => "b'h'",
        Term::Class(_) => "hole",
    };
    Appended::Token(text.to_owned())
}

/// Whether canonical spacing puts a space between two tokens on a line.
fn spaced(before: &str, after: &str) -> bool {
    let tight = ["(", "[", ".", "::", "..<", "..="];
    let closing = [")", "]", ",", ":", ".", "::", "..<", "..="];
    let called = (after == "(" || after == "[")
        && (before == ")"
            || before == "]"
            || before == "self"
            || before == "fn"
            || before == "dbg"
            || before.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_'));
    !(tight.contains(&before) || closing.contains(&after) || called)
}

/// Writes the appended tokens after the kept source: a line end for `NL`, column 0 after `COL0`,
/// otherwise the indentation of the braces open in the source, and canonical spacing on a line.
/// Returns the closers and typed holes, each on its line.
fn render(
    file: &SourceFile,
    ends_line: bool,
    appended: &[Appended],
    source: &mut Vec<u8>,
) -> Vec<Piece> {
    let mut depth: usize = file
        .tokens
        .iter()
        .enumerate()
        .filter(|(at, _)| !file.comment[*at])
        .map(|(_, token)| match token.kind {
            TokenKind::Punct(Punct::LBrace) => 1,
            TokenKind::Punct(Punct::RBrace) => -1,
            _ => 0,
        })
        .sum::<i64>()
        .max(0) as usize;
    let mut line = file.lines.len() as u32 + u32::from(ends_line && !file.lines.is_empty());
    if source.is_empty() {
        line = 1;
    }
    let mut line_start = ends_line;
    let mut column_zero = false;
    let mut previous = file
        .tokens
        .iter()
        .rev()
        .find(|token| !token.kind.is_virtual())
        .map(|token| token.text(&file.source).to_owned())
        .unwrap_or_default();
    let mut pieces: Vec<Piece> = Vec::new();
    for token in appended {
        let (text, hole) = match token {
            Appended::Nl => {
                source.push(b'\n');
                line += 1;
                line_start = true;
                column_zero = false;
                continue;
            }
            Appended::Col0 => {
                if !line_start {
                    source.push(b'\n');
                    line += 1;
                    line_start = true;
                }
                column_zero = true;
                continue;
            }
            Appended::Token(text) => (text.as_str(), false),
            Appended::Hole => ("todo \"hole\"", true),
        };
        if text == "}" {
            depth = depth.saturating_sub(1);
        }
        if line_start {
            let indentation = if column_zero { 0 } else { 4 * depth };
            source.extend(std::iter::repeat_n(b' ', indentation));
        } else if spaced(&previous, text) {
            source.push(b' ');
        }
        let start = source.len() as u32;
        source.extend_from_slice(text.as_bytes());
        let end = source.len() as u32;
        match pieces.last_mut() {
            Some(piece) if !hole && !piece.hole && piece.line == line => {
                piece.text = String::from_utf8_lossy(&source[piece.span.0 as usize..end as usize])
                    .into_owned();
                piece.span.1 = end;
            }
            _ => pieces.push(Piece {
                line,
                span: (start, end),
                text: text.to_owned(),
                hole,
            }),
        }
        if text == "{" {
            depth += 1;
        }
        previous = text.to_owned();
        line_start = false;
        column_zero = false;
    }
    if !line_start {
        source.push(b'\n');
    }
    pieces
}

#[cfg(test)]
mod tests;
