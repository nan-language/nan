//! The exact fixes of the syntax rules the parse reports: rejected alternatives, misplaced and
//! unclosed delimiters, indented lines, closing labels and body openers.

use super::file::SourceFile;
use super::grammar::{Keyword, Punct, Rule};
use super::parser::State;
use super::token::TokenKind;
use crate::diagnostic::{Code, Offer, Replacement};

impl SourceFile {
    fn replacing(&self, first: usize, last: usize, text: &str) -> Replacement {
        Replacement {
            start: self.tokens[first].start,
            end: self.tokens[last].end,
            text: text.to_owned(),
        }
    }

    fn inserting(&self, at: u32, text: &str) -> Replacement {
        Replacement {
            start: at,
            end: at,
            text: text.to_owned(),
        }
    }

    /// The replacement deleting the tokens `first..=last` and the spaces before them.
    fn deleting(&self, first: usize, last: usize) -> Replacement {
        let start = self
            .previous(first)
            .filter(|&before| self.tokens[before].line == self.tokens[first].line)
            .map_or(self.tokens[first].start, |before| self.tokens[before].end);
        Replacement {
            start,
            end: self.tokens[last].end,
            text: String::new(),
        }
    }

    /// The source text of the tokens `first..=last`.
    fn spelled(&self, first: usize, last: usize) -> &str {
        std::str::from_utf8(
            &self.source[self.tokens[first].start as usize..self.tokens[last].end as usize],
        )
        .expect("tokens hold well-formed UTF-8")
    }

    /// The byte at which the physical line of a token starts.
    fn line_start(&self, token: usize) -> u32 {
        self.lines[self.tokens[token].line as usize - 1].start
    }

    /// The byte at which the physical line after a token's starts, or the file's end.
    fn next_line_start(&self, token: usize) -> u32 {
        self.lines
            .get(self.tokens[token].line as usize)
            .map_or(self.source.len() as u32, |line| line.start)
    }

    /// The fixes of a rejected alternative parsed from `start` to before `at`, `failed` being the
    /// parse that rejected it, and how they are offered.
    pub fn rejected(
        &self,
        failed: &State,
        code: Code,
        start: usize,
        at: usize,
    ) -> (Vec<Vec<Replacement>>, Offer) {
        let last = self.previous(at).unwrap_or(start).max(start);
        let one = |replacement: Replacement| (vec![vec![replacement]], Offer::Each);
        match code {
            Code::ItemSelfFieldSet => one(self.replacing(start, last, "")),
            Code::EffectUsesUnknown => match self.previous(start) {
                Some(uses) => one(self.deleting(uses, start)),
                None => (Vec::new(), Offer::Each),
            },
            Code::TaskWhenTake => one(self.replacing(start, start, "mut")),
            Code::NameDotForPath => one(self.replacing(start, start, "::")),
            Code::ExprMarkerOrder => (
                self.marker_order(failed)
                    .into_iter()
                    .map(|fix| vec![fix])
                    .collect(),
                Offer::Each,
            ),
            Code::ExprChainedComparison => (
                self.chained(failed, start)
                    .into_iter()
                    .map(|fix| vec![fix])
                    .collect(),
                Offer::Each,
            ),
            Code::ExprParentheses => (self.parenthesised(failed, start), Offer::Unique),
            _ => (Vec::new(), Offer::Each),
        }
    }

    /// The first token of the operator expression the parse is reading.
    fn operator_start(&self, failed: &State) -> Option<usize> {
        let frame = failed.frames.iter().rev().find(|frame| {
            matches!(
                frame.rule,
                Some(Rule::Negation | Rule::LogicOperand | Rule::OperatorExpr)
            )
        })?;
        Some(frame.start)
    }

    /// The group of a marker in canonical order: `try` or `attempt`, then `await`, then a mode.
    fn marker_group(&self, at: usize) -> Option<usize> {
        match self.tokens[at].kind {
            TokenKind::Keyword(Keyword::Try | Keyword::Attempt) => Some(0),
            TokenKind::Keyword(Keyword::Await) => Some(1),
            TokenKind::Keyword(Keyword::Take | Keyword::Mut) => Some(2),
            _ => None,
        }
    }

    /// The first and last markers of the marked expression the parse is reading.
    fn marker_run(&self, failed: &State) -> Option<(usize, usize)> {
        let markers = [
            Rule::MarkedExpr,
            Rule::AfterFailureMarker,
            Rule::AfterAwait,
            Rule::AfterMode,
        ];
        let first = failed
            .frames
            .iter()
            .rev()
            .take_while(|frame| frame.rule.is_none_or(|rule| markers.contains(&rule)))
            .filter(|frame| frame.rule.is_some())
            .last()?
            .start;
        let mut last = first;
        let mut at = first;
        while self.marker_group(at).is_some() {
            last = at;
            at = self.next(at + 1);
        }
        Some((first, last))
    }

    /// The markers of the marked expression the parse is reading in canonical order, each once.
    fn marker_order(&self, failed: &State) -> Option<Replacement> {
        let (first, last) = self.marker_run(failed)?;
        let mut found: [Option<&str>; 3] = [None; 3];
        for at in (first..=last).filter(|&at| !self.comment[at]) {
            let group = self.marker_group(at)?;
            let word = self.text(at);
            match found[group] {
                Some(other) if other != word => return None,
                _ => found[group] = Some(word),
            }
        }
        let ordered: Vec<&str> = found.into_iter().flatten().collect();
        Some(self.replacing(first, last, &ordered.join(" ")))
    }

    /// `a < b < c` as `a < b and b < c`, when `b` is a name or a field path from one.
    fn chained(&self, failed: &State, operator: usize) -> Option<Replacement> {
        let (first, end) = failed.last_operand?;
        if end != operator {
            return None;
        }
        let last = self.previous(operator)?;
        let path = (first..=last)
            .filter(|&at| !self.comment[at])
            .enumerate()
            .all(|(index, at)| match self.tokens[at].kind {
                TokenKind::Ident => index % 2 == 0,
                TokenKind::Keyword(Keyword::SelfValue) => index == 0,
                TokenKind::Punct(Punct::Dot) => index % 2 == 1,
                _ => false,
            });
        let name = self.spelled(first, last);
        path.then(|| self.inserting(self.tokens[operator].start, &format!("and {name} ")))
    }

    /// The two parenthesisings of an operator combined with the one the parse just read: the
    /// earlier with its operands, then the rejected one with its operands; an operand starting
    /// with the rejected token has only the second.
    fn parenthesised(&self, failed: &State, operator: usize) -> Vec<Vec<Replacement>> {
        let enclose = |first: usize, last: usize| {
            vec![
                self.inserting(self.tokens[first].start, "("),
                self.inserting(self.tokens[last].end, ")"),
            ]
        };
        let Some((left, _)) = failed.last_operand.filter(|(_, end)| *end == operator) else {
            let rule = match self.tokens[operator].kind {
                TokenKind::Keyword(Keyword::Not) => Rule::Negation,
                _ => Rule::QuantifierExpr,
            };
            let end = self.longest(rule, operator);
            return end
                .and_then(|end| self.previous(end))
                .map(|last| enclose(operator, last))
                .into_iter()
                .collect();
        };
        let right = self
            .longest(Rule::Operand, self.next(operator + 1))
            .and_then(|end| self.previous(end));
        let mut candidates = Vec::new();
        if let (Some(start), Some(before)) = (self.operator_start(failed), self.previous(operator))
        {
            candidates.push(enclose(start, before));
        }
        if let Some(right) = right {
            candidates.push(enclose(left, right));
        }
        candidates
    }

    /// The fix of an unclosed delimiter: its closers, innermost first, after the last token of
    /// the logical line ending at `at`.
    pub fn closed(&self, at: usize, closers: &[Punct]) -> Option<Replacement> {
        let last = self.previous(at)?;
        let written: String = closers.iter().map(|closer| closer.spelling()).collect();
        Some(self.inserting(self.tokens[last].end, &written))
    }

    /// The fix of a mismatched delimiter: the expected closer in place of the token at `at`.
    pub fn rematched(&self, at: usize, closer: Punct) -> Replacement {
        self.replacing(at, at, closer.spelling())
    }

    /// The fix of an indented line: its indentation removed.
    pub fn unindented(&self, at: usize) -> Replacement {
        Replacement {
            start: self.line_start(at),
            end: self.tokens[at].start,
            text: String::new(),
        }
    }

    /// The fix of a token at column 0 that its block holds: the line indented by the depth of
    /// the braces open before it in its item.
    pub fn reindented(&self, item: usize, at: usize) -> Replacement {
        let depth: i32 = (item..at)
            .filter(|&index| !self.comment[index])
            .map(|index| match self.tokens[index].kind {
                TokenKind::Punct(Punct::LBrace) => 1,
                TokenKind::Punct(Punct::RBrace) => -1,
                _ => 0,
            })
            .sum();
        self.inserting(self.line_start(at), &" ".repeat(4 * depth.max(0) as usize))
    }

    /// The fix of a closing label: the name written after the `}` before `at`, in place of what
    /// stands there up to the line's end.
    pub fn relabelled(&self, at: usize, name: &str) -> Option<Replacement> {
        if matches!(
            self.tokens[at].kind,
            TokenKind::Nl | TokenKind::Eof | TokenKind::Col0
        ) {
            let closer = self.previous(at)?;
            return Some(self.inserting(self.tokens[closer].end, &format!(" {name}")));
        }
        let line = self.tokens[at].line;
        let last = (at..self.tokens.len())
            .take_while(|&index| {
                self.tokens[index].line == line && !self.tokens[index].kind.is_virtual()
            })
            .last()?;
        Some(self.replacing(at, last, name))
    }

    /// The name the label of the item or interface method being read must spell: a function's
    /// label up to its parameters, or the identifier after the kind keyword.
    pub fn declared_name(&self, item: usize, failed: &State) -> Option<String> {
        let first = failed
            .frames
            .iter()
            .rev()
            .find(|frame| frame.rule == Some(Rule::InterfaceMember))
            .map_or(item, |frame| frame.start);
        let mut at = self.next(first);
        while !matches!(
            self.tokens[at].kind,
            TokenKind::Keyword(
                Keyword::Fn | Keyword::Struct | Keyword::Enum | Keyword::Interface | Keyword::Test
            ) | TokenKind::Eof
        ) && !matches!(self.text(at), "layout" | "registers")
        {
            at = self.next(at + 1);
        }
        let name = self.next(at + 1);
        if self.tokens[at].kind != TokenKind::Keyword(Keyword::Fn) {
            return (self.tokens[name].kind == TokenKind::Ident)
                .then(|| self.text(name).to_owned());
        }
        let end = (name..self.tokens.len()).find(|&index| {
            matches!(
                self.tokens[index].kind,
                TokenKind::Punct(Punct::LParen | Punct::LBracket) | TokenKind::Nl | TokenKind::Eof
            )
        })?;
        Some(self.spelled(name, self.previous(end)?).to_owned())
    }

    /// The fix moving a body's `{` at column 0, after a head without clauses, to the end of the
    /// head's last line.
    pub fn opener_to_head(&self, opener: usize) -> Option<Vec<Replacement>> {
        let head_end = (0..opener)
            .rev()
            .find(|&index| !self.comment[index] && !self.tokens[index].kind.is_virtual())?;
        Some(vec![
            self.inserting(self.tokens[head_end].end, " {"),
            Replacement {
                start: self.line_start(opener),
                end: self.next_line_start(opener),
                text: String::new(),
            },
        ])
    }

    /// The fix moving a body's `{` ending a head or clause line to column 0 after the clause
    /// lines that follow it.
    pub fn opener_below(&self, opener: usize) -> Vec<Replacement> {
        let mut line = self.tokens[opener].line as usize;
        while let Some(next) = self.lines.get(line) {
            let first = self.tokens.iter().position(|token| {
                token.start >= next.start
                    && token.line as usize == line + 1
                    && !token.kind.is_virtual()
            });
            if !first.is_some_and(|first| self.starts_clause(first)) {
                break;
            }
            line += 1;
        }
        let body = self
            .lines
            .get(line)
            .map_or(self.source.len() as u32, |line| line.start);
        vec![self.deleting(opener, opener), self.inserting(body, "{\n")]
    }

    /// Whether the token starts a clause line.
    fn starts_clause(&self, at: usize) -> bool {
        match self.tokens[at].kind {
            TokenKind::Keyword(
                Keyword::Requires
                | Keyword::Ensures
                | Keyword::Decreases
                | Keyword::Waive
                | Keyword::Todo,
            ) => true,
            TokenKind::Ident => matches!(self.text(at), "example" | "undo"),
            _ => false,
        }
    }
}
