//! The idiom table: foreign spellings that make a syntax failure `syntax.foreign`, with the text
//! each matches and the rewrites that fix it.

use super::file::SourceFile;
use super::grammar::{ANALYSIS, Atom, Foreign, Keyword, Punct, Rule, TermSet};
use super::parser::State;
use super::token::TokenKind;
use crate::diagnostic::Replacement;

/// How the first matching entry reports the failure.
pub enum Idiom {
    /// Expected is the entry's first rewrite, absent for a rewrite stated in words; found is the
    /// matched text, whose bytes are `span`. Each fix is an alternative rewrite, in order.
    Rewrite {
        expected: Option<String>,
        found: String,
        span: (u32, u32),
        fixes: Vec<Replacement>,
    },
    /// An entry without rewrite: expected and found are those of `syntax.unexpected-token`.
    Unrewritable,
}

/// The failure the table is matched against: the parse where it failed, the terminals it
/// expected there, and the failing token.
pub struct Failed<'a> {
    pub file: &'a SourceFile,
    pub state: &'a State,
    pub expected: &'a TermSet,
    pub at: usize,
}

/// The tokens a foreign form matches, and the text each of its placeholders stands for.
struct Matched {
    first: usize,
    last: usize,
    holes: Vec<(&'static str, String)>,
}

impl Matched {
    fn span(first: usize, last: usize) -> Matched {
        Matched {
            first,
            last,
            holes: Vec::new(),
        }
    }
}

/// A rewrite column of the table.
enum Rewrite {
    /// "none".
    Absent,
    /// A rewrite the table states in words, as the replacement it makes.
    Words(fn(&Failed, &Matched) -> Replacement),
    /// Alternatives in order, in each of which a `{placeholder}` stands for the text it matched.
    Templates(&'static [&'static str]),
}

/// How an entry's foreign form matches a failure.
type Matcher = fn(&Failed) -> Option<Matched>;

/// The idiom table of language 1, in table order.
#[rustfmt::skip]
const TABLE: &[(Matcher, Rewrite)] = &[
    (|failed| failed.between(Foreign::AmpAmp), Rewrite::Templates(&["{a} and {b}"])),
    (|failed| failed.between(Foreign::PipePipe), Rewrite::Templates(&["{a} or {b}"])),
    (|failed| failed.empty_closure(), Rewrite::Templates(&["fn() => {e}"])),
    (|failed| failed.bar_closure(), Rewrite::Absent),
    (|failed| failed.assert_macro(), Rewrite::Templates(&["expect {e}"])),
    (|failed| failed.assert_pair("assert_eq"), Rewrite::Templates(&["expect {a} == {b}"])),
    (|failed| failed.assert_pair("assert_ne"), Rewrite::Templates(&["expect {a} != {b}"])),
    (|failed| failed.todo_macro(true), Rewrite::Templates(&["todo {t}"])),
    (|failed| failed.todo_macro(false), Rewrite::Templates(&["todo {t}"])),
    (|failed| failed.macro_name(), Rewrite::Absent),
    (|failed| failed.bang_operand(), Rewrite::Templates(&["not {e}", "(not {e})", "~{e}"])),
    (|failed| failed.tripled(Punct::EqEq), Rewrite::Templates(&["{a} == {b}"])),
    (|failed| failed.tripled(Punct::NotEq), Rewrite::Templates(&["{a} != {b}"])),
    (|failed| failed.question_call(), Rewrite::Templates(&["try {c}"])),
    (|failed| failed.alone(Foreign::Question), Rewrite::Absent),
    (|failed| failed.semicolon(true), Rewrite::Words(|failed, matched| failed.semicolon_deleted(matched))),
    (|failed| failed.semicolon(false), Rewrite::Words(|failed, matched| failed.semicolon_broken(matched))),
    (|failed| failed.between(Foreign::DotDot), Rewrite::Templates(&["{a}..<{b}"])),
    (|failed| failed.alone(Foreign::DotDot), Rewrite::Absent),
    (|failed| failed.let_mut(), Rewrite::Templates(&["var {x}"])),
    (|failed| failed.walrus(), Rewrite::Templates(&["let {x} = {e}", "var {x} = {e}"])),
    (
        |failed| failed.cast(),
        Rewrite::Templates(&[
            "{T}.widen({e})",
            "{T}.truncate({e})",
            "{T}.saturate({e})",
            "{T}.nearest({e})",
            "{e}",
        ]),
    ),
    (|failed| failed.reference(true), Rewrite::Templates(&["mut {e}"])),
    (|failed| failed.reference(false), Rewrite::Templates(&["{e}"])),
    (|failed| failed.self_reference(true), Rewrite::Templates(&["mut self"])),
    (|failed| failed.self_reference(false), Rewrite::Templates(&["self"])),
    (|failed| failed.typed_reference(true), Rewrite::Templates(&["mut {x}: {T}"])),
    (|failed| failed.typed_reference(false), Rewrite::Templates(&["{x}: {T}"])),
    (|failed| failed.dereference(), Rewrite::Absent),
    (|failed| failed.dot_await(), Rewrite::Templates(&["{m}await {c}"])),
    (|failed| failed.increment(), Rewrite::Templates(&["{p} += 1"])),
    (|failed| failed.decrement(), Rewrite::Templates(&["{p} -= 1"])),
    (|failed| failed.arrow_member(), Rewrite::Templates(&["{p}.{x}"])),
    (|failed| failed.unit_result(), Rewrite::Words(|failed, matched| failed.unit_result_deleted(matched))),
];

/// The first entry of the idiom table matching the failure, as it reports it.
pub fn foreign(failed: &Failed) -> Option<Idiom> {
    let (matched, rewrite) = TABLE
        .iter()
        .find_map(|(matches, rewrite)| matches(failed).map(|matched| (matched, rewrite)))?;
    let span = (
        failed.file.tokens[matched.first].start,
        failed.file.tokens[matched.last].end,
    );
    let fixes = match rewrite {
        Rewrite::Absent => return Some(Idiom::Unrewritable),
        Rewrite::Words(replacement) => vec![replacement(failed, &matched)],
        Rewrite::Templates(templates) => templates
            .iter()
            .map(|template| Replacement {
                start: span.0,
                end: span.1,
                text: matched
                    .holes
                    .iter()
                    .fold(template.to_string(), |text, (hole, value)| {
                        text.replace(&format!("{{{hole}}}"), value)
                    }),
            })
            .collect(),
    };
    let expected = match rewrite {
        Rewrite::Templates(_) => fixes.first().map(|fix| fix.text.clone()),
        _ => None,
    };
    Some(Idiom::Rewrite {
        expected,
        found: failed.text(matched.first, matched.last),
        span,
        fixes,
    })
}

impl Failed<'_> {
    fn kind(&self, at: usize) -> TokenKind {
        self.file.tokens[at].kind
    }

    fn next(&self, from: usize) -> usize {
        self.file.next(from)
    }

    fn previous(&self, before: usize) -> Option<usize> {
        self.file.previous(before)
    }

    /// The token after the failing one.
    fn after(&self) -> usize {
        self.next(self.at + 1)
    }

    fn is(&self, at: usize, kind: TokenKind) -> bool {
        self.kind(at) == kind
    }

    fn is_punct(&self, at: usize, punct: Punct) -> bool {
        self.is(at, TokenKind::Punct(punct))
    }

    fn is_foreign(&self, at: usize, foreign: Foreign) -> bool {
        self.is(at, TokenKind::Foreign(foreign))
    }

    fn is_keyword(&self, at: usize, keyword: Keyword) -> bool {
        self.is(at, TokenKind::Keyword(keyword))
    }

    fn is_ident(&self, at: usize, text: &str) -> bool {
        self.is(at, TokenKind::Ident) && self.file.text(at) == text
    }

    /// Whether the token at `at` directly follows the one before it, without a space.
    fn adjacent(&self, at: usize) -> bool {
        self.previous(at)
            .is_some_and(|previous| self.file.tokens[previous].end == self.file.tokens[at].start)
    }

    /// The source text of the tokens from `first` to `last`, its lines without comments and
    /// indentation joined by single spaces.
    fn text(&self, first: usize, last: usize) -> String {
        let (first, last) = (&self.file.tokens[first], &self.file.tokens[last]);
        let parts: Vec<String> = self.file.lines[first.line as usize - 1..last.line as usize]
            .iter()
            .map(|line| {
                let start = line.start.max(first.start) as usize;
                let end = line.comment.unwrap_or(line.end).min(last.end) as usize;
                String::from_utf8_lossy(&self.file.source[start..end])
                    .trim()
                    .to_owned()
            })
            .filter(|part| !part.is_empty())
            .collect();
        parts.join(" ")
    }

    /// Whether the parse expects all that can start `rule` where it failed.
    fn expects(&self, rule: Rule) -> bool {
        self.expected
            .is_superset(&ANALYSIS.choice(rule.choice()).ok)
    }

    /// Whether the parse failed at a type its innermost rule, one of `rules`, requires.
    fn type_expected_in(&self, rules: &[Rule]) -> bool {
        self.state
            .innermost_rule()
            .is_some_and(|rule| rules.contains(&rule))
            && self.state.current_atom() == Some(Atom::Rule(Rule::TypeExpr))
    }

    /// The first token of the prefix expression the parse completed just before `end`.
    fn operand_ending_at(&self, end: usize) -> Option<usize> {
        self.state
            .last_operand
            .filter(|&(_, operand_end)| operand_end == end)
            .map(|(start, _)| start)
    }

    /// The operand ending just before the failing token, as its first token and text.
    fn operand_before(&self) -> Option<(usize, String)> {
        let first = self.operand_ending_at(self.at)?;
        Some((first, self.text(first, self.previous(self.at)?)))
    }

    fn longest(&self, rule: Rule, start: usize) -> Option<usize> {
        self.file.longest(rule, start)
    }

    /// The longest text that `rule` derives from `start`, as its last token and text.
    fn derived(&self, rule: Rule, start: usize) -> Option<(usize, String)> {
        let last = self.previous(self.longest(rule, start)?)?;
        Some((last, self.text(start, last)))
    }

    /// An expression between parentheses from `start`, followed by `closer`.
    fn enclosed(&self, start: usize, closer: Punct) -> Option<(usize, String)> {
        let end = self.longest(Rule::Expr, start)?;
        let last = self.previous(end)?;
        self.is_punct(end, closer)
            .then(|| (end, self.text(start, last)))
    }

    /// Whether the operand from `first` to `last` ends in the argument list of a call.
    fn ends_in_call(&self, first: usize, last: usize) -> bool {
        if !self.is_punct(last, Punct::RParen) {
            return false;
        }
        let mut depth = 0;
        let mut at = last;
        loop {
            match self.kind(at) {
                TokenKind::Punct(Punct::RParen) => depth += 1,
                TokenKind::Punct(Punct::LParen) => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            match self.previous(at) {
                Some(previous) if previous >= first => at = previous,
                _ => return false,
            }
        }
        at > first
            && self.previous(at).is_some_and(|callee| {
                matches!(
                    self.kind(callee),
                    TokenKind::Ident
                        | TokenKind::Keyword(Keyword::SelfValue)
                        | TokenKind::Punct(Punct::RParen | Punct::RBracket)
                )
            })
    }

    /// Whether the tokens from `first` to before `end` form exactly a place.
    fn is_place(&self, first: usize, end: usize) -> bool {
        self.longest(Rule::Place, first) == Some(end)
    }

    /// The identifier starting the statement the parse is reading, when the failing token
    /// directly follows it.
    fn statement_identifier(&self) -> Option<usize> {
        let statement = self
            .state
            .frames
            .iter()
            .rev()
            .find(|frame| frame.rule == Some(Rule::Stmt))?
            .start;
        (self.previous(self.at) == Some(statement) && self.is(statement, TokenKind::Ident))
            .then_some(statement)
    }

    /// The first token of the postfix expression the parse is reading, with the markers before it.
    fn marked_call_start(&self) -> Option<usize> {
        let frames = &self.state.frames;
        let postfix = frames
            .iter()
            .rposition(|frame| frame.rule == Some(Rule::PostfixExpr))?;
        let mut start = frames[postfix].start;
        for frame in frames[..postfix].iter().rev() {
            match frame.rule {
                None => {}
                Some(
                    Rule::MarkedExpr
                    | Rule::AfterFailureMarker
                    | Rule::AfterAwait
                    | Rule::AfterMode,
                ) => start = frame.start,
                Some(_) => break,
            }
        }
        Some(start)
    }

    /// `a && b`, `a || b` and `a..b`: the operator after an operand.
    fn between(&self, operator: Foreign) -> Option<Matched> {
        if !self.is_foreign(self.at, operator) {
            return None;
        }
        let (first, a) = self.operand_before()?;
        let (last, b) = self.derived(Rule::PrefixExpr, self.after())?;
        Some(Matched {
            first,
            last,
            holes: vec![("a", a), ("b", b)],
        })
    }

    /// `|| e` where an operand is expected.
    fn empty_closure(&self) -> Option<Matched> {
        if !(self.is_foreign(self.at, Foreign::PipePipe) && self.expects(Rule::PrefixExpr)) {
            return None;
        }
        let (last, e) = self.derived(Rule::PrefixExpr, self.after())?;
        Some(Matched {
            first: self.at,
            last,
            holes: vec![("e", e)],
        })
    }

    /// `|x|` where an operand is expected.
    fn bar_closure(&self) -> Option<Matched> {
        let name = self.after();
        let closing = self.next(name + 1);
        let found = self.is_punct(self.at, Punct::Pipe)
            && self.expects(Rule::PrefixExpr)
            && self.is(name, TokenKind::Ident)
            && self.is_punct(closing, Punct::Pipe);
        found.then(|| Matched::span(self.at, closing))
    }

    /// `assert!(e)` where a statement is expected.
    fn assert_macro(&self) -> Option<Matched> {
        let bang = self.after();
        let open = self.next(bang + 1);
        let found = self.is_keyword(self.at, Keyword::Assert)
            && self.expects(Rule::Stmt)
            && self.is_foreign(bang, Foreign::Bang)
            && self.is_punct(open, Punct::LParen);
        if !found {
            return None;
        }
        let (close, e) = self.enclosed(self.next(open + 1), Punct::RParen)?;
        Some(Matched {
            first: self.at,
            last: close,
            holes: vec![("e", e)],
        })
    }

    /// `name!(a, b)` at the start of a statement.
    fn assert_pair(&self, name: &str) -> Option<Matched> {
        let statement = self.statement_identifier()?;
        let open = self.after();
        let found = self.is_ident(statement, name)
            && self.is_foreign(self.at, Foreign::Bang)
            && self.is_punct(open, Punct::LParen);
        if !found {
            return None;
        }
        let (comma, a) = self.enclosed(self.next(open + 1), Punct::Comma)?;
        let (close, b) = self.enclosed(self.next(comma + 1), Punct::RParen)?;
        Some(Matched {
            first: statement,
            last: close,
            holes: vec![("a", a), ("b", b)],
        })
    }

    /// `todo!(t)`, or with `keyword` false `unimplemented!(t)`.
    fn todo_macro(&self, keyword: bool) -> Option<Matched> {
        let name = self.previous(self.at)?;
        let named = if keyword {
            self.is_keyword(name, Keyword::Todo)
        } else {
            self.is_ident(name, "unimplemented")
        };
        let open = self.after();
        let text = self.next(open + 1);
        let close = self.next(text + 1);
        let found = named
            && self.is_foreign(self.at, Foreign::Bang)
            && self.is_punct(open, Punct::LParen)
            && self.is(text, TokenKind::Text)
            && self.is_punct(close, Punct::RParen);
        found.then(|| Matched {
            first: name,
            last: close,
            holes: vec![("t", self.file.text(text).to_owned())],
        })
    }

    /// `x!` after an identifier.
    fn macro_name(&self) -> Option<Matched> {
        let name = self
            .previous(self.at)
            .filter(|&name| self.is(name, TokenKind::Ident))?;
        self.is_foreign(self.at, Foreign::Bang)
            .then(|| Matched::span(name, self.at))
    }

    /// `!e` where an operand is expected.
    fn bang_operand(&self) -> Option<Matched> {
        if !(self.is_foreign(self.at, Foreign::Bang) && self.expects(Rule::PrefixExpr)) {
            return None;
        }
        let (last, e) = self.derived(Rule::PrefixExpr, self.after())?;
        Some(Matched {
            first: self.at,
            last,
            holes: vec![("e", e)],
        })
    }

    /// `a === b` and `a !== b`: an `=` directly after `operator`.
    fn tripled(&self, operator: Punct) -> Option<Matched> {
        let operator_at = self.previous(self.at)?;
        let found = self.is_punct(self.at, Punct::Eq)
            && self.is_punct(operator_at, operator)
            && self.adjacent(self.at);
        if !found {
            return None;
        }
        let first = self.operand_ending_at(operator_at)?;
        let a = self.text(first, self.previous(operator_at)?);
        let (last, b) = self.derived(Rule::PrefixExpr, self.after())?;
        Some(Matched {
            first,
            last,
            holes: vec![("a", a), ("b", b)],
        })
    }

    /// `c?` after an operand ending in a call, with no operand after it.
    fn question_call(&self) -> Option<Matched> {
        if !self.is_foreign(self.at, Foreign::Question) {
            return None;
        }
        let (first, c) = self.operand_before()?;
        let operand_after = self.longest(Rule::PrefixExpr, self.after()).is_some();
        (!operand_after && self.ends_in_call(first, self.previous(self.at)?)).then(|| Matched {
            first,
            last: self.at,
            holes: vec![("c", c)],
        })
    }

    /// The foreign token alone, anywhere.
    fn alone(&self, foreign: Foreign) -> Option<Matched> {
        self.is_foreign(self.at, foreign)
            .then(|| Matched::span(self.at, self.at))
    }

    /// `;` ending its logical line, or with `ends_line` false followed by more of it.
    fn semicolon(&self, ends_line: bool) -> Option<Matched> {
        let found = self.is_foreign(self.at, Foreign::Semi)
            && self.is(self.after(), TokenKind::Nl) == ends_line;
        found.then(|| Matched::span(self.at, self.at))
    }

    /// `let mut x`.
    fn let_mut(&self) -> Option<Matched> {
        let binding = self
            .previous(self.at)
            .filter(|&binding| self.is_keyword(binding, Keyword::Let))?;
        let name = self.after();
        let found = self.is_keyword(self.at, Keyword::Mut) && self.is(name, TokenKind::Ident);
        found.then(|| Matched {
            first: binding,
            last: name,
            holes: vec![("x", self.file.text(name).to_owned())],
        })
    }

    /// `x := e` at the start of a statement.
    fn walrus(&self) -> Option<Matched> {
        let statement = self.statement_identifier()?;
        let equals = self.after();
        if !(self.is_punct(self.at, Punct::Colon) && self.is_punct(equals, Punct::Eq)) {
            return None;
        }
        let (last, e) = self.derived(Rule::PrefixExpr, self.next(equals + 1))?;
        Some(Matched {
            first: statement,
            last,
            holes: vec![("x", self.file.text(statement).to_owned()), ("e", e)],
        })
    }

    /// `e as T` after an operand.
    fn cast(&self) -> Option<Matched> {
        if !self.is_ident(self.at, "as") {
            return None;
        }
        let (first, e) = self.operand_before()?;
        let (last, ty) = self.derived(Rule::TypeExpr, self.after())?;
        Some(Matched {
            first,
            last,
            holes: vec![("e", e), ("T", ty)],
        })
    }

    /// `&mut e`, or with `marked` false `&e`, where an operand is expected.
    fn reference(&self, marked: bool) -> Option<Matched> {
        let mutable = self.is_keyword(self.after(), Keyword::Mut);
        let found = self.is_punct(self.at, Punct::Amp)
            && self.expects(Rule::PrefixExpr)
            && mutable == marked;
        if !found {
            return None;
        }
        let start = if marked {
            self.next(self.after() + 1)
        } else {
            self.after()
        };
        let (last, e) = self.derived(Rule::PrefixExpr, start)?;
        Some(Matched {
            first: self.at,
            last,
            holes: vec![("e", e)],
        })
    }

    /// `&mut self`, or with `marked` false `&self`, where a parameter is expected.
    fn self_reference(&self, marked: bool) -> Option<Matched> {
        let mutable = self.is_keyword(self.after(), Keyword::Mut);
        let receiver = if mutable {
            self.next(self.after() + 1)
        } else {
            self.after()
        };
        let found = self.is_punct(self.at, Punct::Amp)
            && self.expects(Rule::Param)
            && mutable == marked
            && self.is_keyword(receiver, Keyword::SelfValue);
        found.then(|| Matched::span(self.at, receiver))
    }

    /// `x: &mut T`, or with `marked` false `x: &T`, where a parameter's type is expected.
    fn typed_reference(&self, marked: bool) -> Option<Matched> {
        let colon = self.previous(self.at)?;
        let name = self.previous(colon)?;
        let mutable = self.is_keyword(self.after(), Keyword::Mut);
        let found = self.is_punct(self.at, Punct::Amp)
            && self.type_expected_in(&[Rule::Param, Rule::FnTypeParam])
            && self.is_punct(colon, Punct::Colon)
            && self.is(name, TokenKind::Ident)
            && mutable == marked;
        if !found {
            return None;
        }
        let start = if marked {
            self.next(self.after() + 1)
        } else {
            self.after()
        };
        let (last, ty) = self.derived(Rule::TypeExpr, start)?;
        Some(Matched {
            first: name,
            last,
            holes: vec![("x", self.file.text(name).to_owned()), ("T", ty)],
        })
    }

    /// `*e` where an operand is expected.
    fn dereference(&self) -> Option<Matched> {
        (self.is_punct(self.at, Punct::Star) && self.expects(Rule::PrefixExpr))
            .then(|| Matched::span(self.at, self.at))
    }

    /// `c.await` after a call; a failure marker before `c` stays before the rewrite's `await`.
    fn dot_await(&self) -> Option<Matched> {
        let dot = self
            .previous(self.at)
            .filter(|&dot| self.is_punct(dot, Punct::Dot))?;
        let first = self.marked_call_start()?;
        let call_end = self.previous(dot)?;
        if !(self.is_keyword(self.at, Keyword::Await) && self.ends_in_call(first, call_end)) {
            return None;
        }
        let failure_marker = matches!(
            self.kind(first),
            TokenKind::Keyword(Keyword::Try | Keyword::Attempt)
        );
        let (marker, call) = if failure_marker {
            (format!("{} ", self.file.text(first)), self.next(first + 1))
        } else {
            (String::new(), first)
        };
        Some(Matched {
            first,
            last: self.at,
            holes: vec![("m", marker), ("c", self.text(call, call_end))],
        })
    }

    /// `p++`: a `+` directly after a `+` that follows a place.
    fn increment(&self) -> Option<Matched> {
        let plus = self
            .previous(self.at)
            .filter(|&plus| self.is_punct(plus, Punct::Plus) && self.adjacent(self.at))?;
        if !self.is_punct(self.at, Punct::Plus) {
            return None;
        }
        self.after_place(plus)
    }

    /// `p--` after a place.
    fn decrement(&self) -> Option<Matched> {
        if !self.is_foreign(self.at, Foreign::MinusMinus) {
            return None;
        }
        self.after_place(self.at)
    }

    /// The place ending before `operator` and the tokens after it up to the failing one.
    fn after_place(&self, operator: usize) -> Option<Matched> {
        let first = self.operand_ending_at(operator)?;
        let last = self.previous(operator)?;
        self.is_place(first, operator).then(|| Matched {
            first,
            last: self.at,
            holes: vec![("p", self.text(first, last))],
        })
    }

    /// `p->x` after a place.
    fn arrow_member(&self) -> Option<Matched> {
        if !self.is_punct(self.at, Punct::Arrow) {
            return None;
        }
        let (first, p) = self.operand_before()?;
        let name = self.after();
        (self.is_place(first, self.at) && self.is(name, TokenKind::Ident)).then(|| Matched {
            first,
            last: name,
            holes: vec![("p", p), ("x", self.file.text(name).to_owned())],
        })
    }

    /// The run of spaces ending at `end` on its line, as its first byte.
    fn spaces_before(&self, end: u32) -> u32 {
        let source = &self.file.source;
        let mut start = end;
        while start > 0 && source[start as usize - 1] == b' ' {
            start -= 1;
        }
        start
    }

    /// The rewrite of a `;` ending its logical line: it and the spaces before it deleted.
    fn semicolon_deleted(&self, matched: &Matched) -> Replacement {
        let token = &self.file.tokens[matched.first];
        Replacement {
            start: self.spaces_before(token.start),
            end: token.end,
            text: String::new(),
        }
    }

    /// The rewrite of a `;` followed by more of its logical line: it and the spaces after it
    /// replaced by a line break and the indentation of its line.
    fn semicolon_broken(&self, matched: &Matched) -> Replacement {
        let token = &self.file.tokens[matched.first];
        let line = &self.file.lines[token.line as usize - 1];
        let indentation = self.file.source[line.start as usize..]
            .iter()
            .take_while(|byte| **byte == b' ')
            .count();
        Replacement {
            start: token.start,
            end: self.file.tokens[self.after()].start,
            text: format!("\n{}", " ".repeat(indentation)),
        }
    }

    /// The rewrite of `-> ()`: the matched text and the space before it deleted.
    fn unit_result_deleted(&self, matched: &Matched) -> Replacement {
        Replacement {
            start: self.spaces_before(self.file.tokens[matched.first].start),
            end: self.file.tokens[matched.last].end,
            text: String::new(),
        }
    }

    /// `-> ()` where a function's result type is expected.
    fn unit_result(&self) -> Option<Matched> {
        let arrow = self
            .previous(self.at)
            .filter(|&arrow| self.is_punct(arrow, Punct::Arrow))?;
        let close = self.after();
        let found = self.is_punct(self.at, Punct::LParen)
            && self.type_expected_in(&[Rule::FnResult])
            && self.is_punct(close, Punct::RParen);
        found.then(|| Matched::span(arrow, close))
    }
}
