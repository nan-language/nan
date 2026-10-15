//! Parsing a source file into items: its summary, each item's tree, the resynchronisation after
//! a syntax failure, misplaced doc comments, and the code each syntax failure is reported with.

use super::grammar::{Atom, Class, Keyword, Punct, Rule, Term, TermSet};
use super::idiom::{self, Idiom};
use super::items::{self, Item};
use super::lexer::{Line, lex};
use super::parser::{self, Discard, Failure, Fed, Lookahead, State};
use super::token::{Token, TokenKind};
use super::tree::{Builder, Tree};
use crate::diagnostic::{Code, Offer, Replacement};

/// A diagnostic of a file, before its path is known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiagnostic {
    pub line: u32,
    pub code: Code,
    pub expected: Option<String>,
    pub found: Option<String>,
    /// The bytes of the reported construct, when narrower than its line.
    pub span: Option<(u32, u32)>,
    /// For a syntax failure, the first byte of the token at which the parse failed.
    pub failing: Option<u32>,
    /// The fixes its rule names, each as replacements in this file.
    pub fixes: Vec<Vec<Replacement>>,
    pub offer: Offer,
}

impl FileDiagnostic {
    pub fn new(line: u32, code: Code) -> FileDiagnostic {
        FileDiagnostic {
            line,
            code,
            expected: None,
            found: None,
            span: None,
            failing: None,
            fixes: Vec::new(),
            offer: Offer::Each,
        }
    }
}

/// An item of a file, or a run of tokens between items that starts none.
pub struct ItemSyntax {
    /// The index of its first token, the `COL0` starting an item.
    pub first: usize,
    /// The index of the first token after the item.
    pub end: usize,
    /// The tree the parse built, closed where an abandoned item failed; none for tokens between
    /// items.
    pub tree: Option<Tree>,
    pub abandoned: bool,
}

pub struct SourceFile {
    pub source: Vec<u8>,
    pub tokens: Vec<Token>,
    pub lines: Vec<Line>,
    /// The `FILE_DOC` token of the file summary.
    pub summary: Option<usize>,
    pub items: Vec<ItemSyntax>,
    /// Whether each token is read as part of a plain comment: the `DOC` and `FILE_DOC` lines
    /// that document nothing, with their `COL0` and `NL`.
    pub comment: Vec<bool>,
    pub diagnostics: Vec<FileDiagnostic>,
}

impl SourceFile {
    pub fn text(&self, token: usize) -> &str {
        self.tokens[token].text(&self.source)
    }

    /// The first token at or after `from` that is not read as a comment, or `EOF` past it.
    pub fn next(&self, from: usize) -> usize {
        (from..self.tokens.len())
            .find(|&at| !self.comment[at])
            .unwrap_or(self.tokens.len() - 1)
    }

    /// The last token before `before` that is not read as a comment.
    pub fn previous(&self, before: usize) -> Option<usize> {
        (0..before).rev().find(|&at| !self.comment[at])
    }

    /// The identifiers among the tokens `range` that start a path of two or more segments.
    pub fn path_starts(&self, range: std::ops::Range<usize>) -> impl Iterator<Item = usize> + '_ {
        let separator = TokenKind::Punct(Punct::ColonColon);
        range.filter(move |&index| {
            self.tokens[index].kind == TokenKind::Ident
                && self.tokens.get(index + 1).map(|token| token.kind) == Some(separator)
                && (index == 0 || self.tokens[index - 1].kind != separator)
        })
    }

    /// The token after the longest text that `rule` derives from `start`.
    pub fn longest(&self, rule: Rule, start: usize) -> Option<usize> {
        let mut state = State::new(rule);
        let mut at = start;
        loop {
            let next = (at + 1 < self.tokens.len()).then(|| &self.tokens[self.next(at + 1)]);
            let lookahead = Lookahead {
                token: &self.tokens[at],
                next,
                at,
            };
            match state.feed(lookahead, &self.source, &mut Discard) {
                Ok(Fed::Consumed) => at = self.next(at + 1),
                Ok(Fed::Done) => return (at != start).then_some(at),
                Err(_) => return None,
            }
        }
    }

    /// The physical lines of an item, from its first token's to its last token's.
    pub fn item_lines(&self, item: &ItemSyntax) -> std::ops::RangeInclusive<u32> {
        self.tokens[item.first].line..=self.tokens[item.end - 1].line
    }

    /// The item holding a line, if the parse built its tree.
    pub fn item_at(&self, line: u32) -> Option<&ItemSyntax> {
        let after = self
            .items
            .partition_point(|item| *self.item_lines(item).start() <= line);
        self.items[..after]
            .last()
            .filter(|item| item.tree.is_some() && self.item_lines(item).contains(&line))
    }

    /// The text of a physical line, numbered from 1, without its line end.
    pub fn line_text(&self, line: u32) -> &[u8] {
        let line = &self.lines[line as usize - 1];
        &self.source[line.start as usize..line.end as usize]
    }
}

pub fn parse(source: Vec<u8>) -> SourceFile {
    parse_as(source, false)
}

/// Parses a source file of a std package, where a function may end at its head line or clause
/// region, without a body.
pub fn parse_std(source: Vec<u8>) -> SourceFile {
    parse_as(source, true)
}

fn parse_as(source: Vec<u8>, std: bool) -> SourceFile {
    let lexed = lex(&source);
    let mut parser = Parser {
        std,
        file: SourceFile {
            source,
            comment: vec![false; lexed.tokens.len()],
            tokens: lexed.tokens,
            lines: lexed.lines,
            summary: None,
            items: Vec::new(),
            diagnostics: Vec::new(),
        },
    };
    parser.run();
    let mut file = parser.file;
    let checked: Vec<FileDiagnostic> = file
        .items
        .iter()
        .filter(|item| !item.abandoned)
        .filter_map(|item| item.tree.as_ref())
        .flat_map(|tree| items::check(Item { file: &file, tree }))
        .collect();
    file.diagnostics.extend(checked);
    let lines = &file.lines;
    file.diagnostics
        .retain(|diagnostic| !lines[diagnostic.line as usize - 1].lexical_error);
    file.diagnostics.extend(
        lexed
            .diagnostics
            .into_iter()
            .map(|diagnostic| FileDiagnostic {
                span: Some(diagnostic.span),
                failing: Some(diagnostic.span.0),
                fixes: diagnostic.fix.into_iter().collect(),
                ..FileDiagnostic::new(diagnostic.line, diagnostic.code)
            }),
    );
    file
}

/// How a run of doc comment lines stands in the parse.
enum DocRun {
    /// The item being read ends before the run.
    ItemEnds,
    Admitted,
    Misplaced,
}

struct Parser {
    std: bool,
    file: SourceFile,
}

impl Parser {
    fn kind(&self, at: usize) -> TokenKind {
        self.file.tokens[at].kind
    }

    fn next(&self, from: usize) -> usize {
        self.file.next(from)
    }

    fn previous(&self, before: usize) -> Option<usize> {
        self.file.previous(before)
    }

    fn lookahead(&self, at: usize) -> Lookahead<'_> {
        let next = (at + 1 < self.file.tokens.len()).then(|| &self.file.tokens[self.next(at + 1)]);
        Lookahead {
            token: &self.file.tokens[at],
            next,
            at,
        }
    }

    fn report(&mut self, line: u32, code: Code) {
        self.file.diagnostics.push(FileDiagnostic::new(line, code));
    }

    /// Reports a syntax failure at the token `failing` on the tokens `first..=last`, with the
    /// fixes its rule names.
    fn report_fixed(
        &mut self,
        failing: usize,
        (first, last): (usize, usize),
        code: Code,
        fixes: Vec<Vec<Replacement>>,
        offer: Offer,
    ) {
        let (first, last) = (self.file.tokens[first], self.file.tokens[last]);
        self.file.diagnostics.push(FileDiagnostic {
            span: Some((first.start, last.end.max(first.start))),
            failing: Some(self.file.tokens[failing].start),
            fixes,
            offer,
            ..FileDiagnostic::new(first.line, code)
        });
    }

    fn run(&mut self) {
        let mut at = self.next(0);
        let at_column_zero = self.kind(at) == TokenKind::Col0;
        let first = if at_column_zero {
            self.next(at + 1)
        } else {
            at
        };
        if self.kind(first) == TokenKind::FileDoc {
            self.file.summary = Some(first);
            if !at_column_zero {
                self.report(self.file.tokens[first].line, Code::ItemIndented);
            }
            at = self.next(self.next(first + 1) + 1);
        }
        let described = self
            .file
            .summary
            .is_some_and(|doc| !self.file.text(doc)[3..].trim_matches(' ').is_empty());
        if !described {
            self.report(1, Code::DocSummaryMissing);
        }
        loop {
            match self.kind(at) {
                TokenKind::Eof => break,
                TokenKind::Col0 if self.starts_misplaced_file_doc(at) => {
                    at = self.misplace_file_doc(at)
                }
                TokenKind::Col0 if self.kind(self.next(at + 1)) == TokenKind::Doc => {
                    let expecting = State::new(Rule::Item);
                    match self.doc_run(&expecting, at) {
                        DocRun::Misplaced => at = self.misplace_doc_run(at),
                        DocRun::Admitted | DocRun::ItemEnds => at = self.item(at),
                    }
                }
                TokenKind::Col0 => at = self.item(at),
                _ => {
                    let expected = [Class::Col0, Class::Eof].into_iter().fold(
                        TermSet::default(),
                        |mut set, class| {
                            set.insert(Term::Class(class));
                            set
                        },
                    );
                    let expecting = State::new(Rule::Item);
                    self.token_failure(None, &expecting, &expecting, at, expected);
                    let end = self.resync(at);
                    self.file.items.push(ItemSyntax {
                        first: at,
                        end,
                        tree: None,
                        abandoned: true,
                    });
                    at = end;
                }
            }
        }
    }

    /// Reads the item whose `COL0` is at `first`, and returns the index of the token after it.
    fn item(&mut self, first: usize) -> usize {
        let mut builder = Builder::default();
        let mut state = State::new(Rule::Item);
        let mut at = first;
        loop {
            if self.starts_misplaced_file_doc(at) {
                at = self.misplace_file_doc(at);
                continue;
            }
            let starts_doc = self.kind(at) == TokenKind::Doc
                || (self.kind(at) == TokenKind::Col0
                    && self.kind(self.next(at + 1)) == TokenKind::Doc);
            if starts_doc && at != first && matches!(self.doc_run(&state, at), DocRun::Misplaced) {
                at = self.misplace_doc_run(at);
                continue;
            }
            let lookahead = self.lookahead(at);
            match state.feed(lookahead, &self.file.source, &mut builder) {
                Ok(Fed::Consumed) => at = self.next(at + 1),
                Ok(Fed::Done) => {
                    self.file.items.push(ItemSyntax {
                        first,
                        end: at,
                        tree: Some(builder.finish()),
                        abandoned: false,
                    });
                    return at;
                }
                Err(Failure::Token { at, .. })
                    if self.std && state.innermost_rule() == Some(Rule::FnEnd) =>
                {
                    let end = match self.previous(at) {
                        Some(previous) if self.kind(previous) == TokenKind::Col0 => previous,
                        _ => at,
                    };
                    self.file.items.push(ItemSyntax {
                        first,
                        end,
                        tree: Some(builder.finish()),
                        abandoned: false,
                    });
                    return end;
                }
                Err(failure) => {
                    let failed_at = match failure {
                        Failure::Token { at, expected } => {
                            let before = self.replay(first, at);
                            self.token_failure(Some(first), &state, &before, at, expected);
                            at
                        }
                        Failure::Rejected { code, start } => {
                            let (fixes, offer) = self.file.rejected(&state, code, start, at);
                            let last = self.previous(at).unwrap_or(start).max(start);
                            self.report_fixed(start, (start, last), code, fixes, offer);
                            at
                        }
                        Failure::MultipleLines { at } => {
                            self.report(self.file.tokens[first].line, Code::ItemMultipleLines);
                            at
                        }
                    };
                    let end = self.resync(failed_at);
                    self.file.items.push(ItemSyntax {
                        first,
                        end,
                        tree: Some(builder.finish()),
                        abandoned: true,
                    });
                    return end;
                }
            }
        }
    }

    /// The parse of the item at `first` just before the token at `at`.
    fn replay(&self, first: usize, at: usize) -> State {
        let mut state = State::new(Rule::Item);
        let mut index = first;
        while index < at {
            if self.file.comment[index] {
                index += 1;
                continue;
            }
            let fed = state.feed(self.lookahead(index), &self.file.source, &mut Discard);
            debug_assert!(
                matches!(fed, Ok(Fed::Consumed)),
                "replaying consumed tokens"
            );
            index += 1;
        }
        state
    }

    /// Skips to the next item: from the failing token, or the `COL0` right before it, to `EOF`
    /// or a `COL0` followed by a token that can start an item.
    fn resync(&self, failed_at: usize) -> usize {
        let mut at = match self.previous(failed_at) {
            Some(previous) if self.kind(previous) == TokenKind::Col0 => previous,
            _ => failed_at,
        };
        loop {
            match self.kind(at) {
                TokenKind::Eof => return at,
                TokenKind::Col0 if self.starts_item(self.next(at + 1)) => return at,
                _ => at = self.next(at + 1),
            }
        }
    }

    fn starts_item(&self, at: usize) -> bool {
        parser::starts(Rule::ItemStart, self.lookahead(at), &self.file.source)
    }

    fn starts_misplaced_file_doc(&self, at: usize) -> bool {
        self.kind(at) == TokenKind::FileDoc
            || (self.kind(at) == TokenKind::Col0
                && self.kind(self.next(at + 1)) == TokenKind::FileDoc)
    }

    /// Reads a `FILE_DOC` line other than the file summary as a plain comment.
    fn misplace_file_doc(&mut self, at: usize) -> usize {
        let doc = if self.kind(at) == TokenKind::Col0 {
            self.next(at + 1)
        } else {
            at
        };
        let nl = self.next(doc + 1);
        for index in [at, doc, nl] {
            self.file.comment[index] = true;
        }
        self.report(self.file.tokens[doc].line, Code::DocMisplaced);
        self.next(nl + 1)
    }

    /// The tokens of the run of doc comment lines starting at `at`, and the index after them.
    fn doc_run_tokens(&self, at: usize) -> (Vec<usize>, usize) {
        let mut tokens = Vec::new();
        let mut index = at;
        loop {
            let doc = match self.kind(index) {
                TokenKind::Doc => index,
                TokenKind::Col0 if self.kind(self.next(index + 1)) == TokenKind::Doc => {
                    self.next(index + 1)
                }
                _ => return (tokens, index),
            };
            let nl = self.next(doc + 1);
            tokens.extend((doc != index).then_some(index));
            tokens.extend([doc, nl]);
            index = self.next(nl + 1);
        }
    }

    /// Whether the run of doc comment lines at `at` documents what the parse reads next.
    fn doc_run(&self, state: &State, at: usize) -> DocRun {
        let (run, after) = self.doc_run_tokens(at);
        let mut probe: Vec<Token> = run.iter().map(|&index| self.file.tokens[index]).collect();
        probe.push(self.file.tokens[after]);
        if self.kind(after) == TokenKind::Col0 {
            probe.push(self.file.tokens[self.next(after + 1)]);
        }
        let mut dry = state.clone();
        let first = Lookahead {
            token: &probe[0],
            next: probe.get(1),
            at,
        };
        match dry.feed(first, &self.file.source, &mut Discard) {
            Ok(Fed::Done) => DocRun::ItemEnds,
            Ok(Fed::Consumed) if dry.accepts(&probe[1..], &self.file.source) => DocRun::Admitted,
            _ => DocRun::Misplaced,
        }
    }

    /// Reads a run of doc comment lines that documents nothing as plain comments.
    fn misplace_doc_run(&mut self, at: usize) -> usize {
        let (run, after) = self.doc_run_tokens(at);
        for &index in &run {
            self.file.comment[index] = true;
        }
        let first_doc = run
            .iter()
            .find(|&&index| self.kind(index) == TokenKind::Doc)
            .expect("a run holds a doc line");
        self.report(self.file.tokens[*first_doc].line, Code::DocMisplaced);
        after
    }

    /// Reports a token the parse does not accept. `failed` is the parse where it failed, `before`
    /// the parse just before the token, and `item` the first token of the item being read.
    fn token_failure(
        &mut self,
        item: Option<usize>,
        failed: &State,
        before: &State,
        at: usize,
        expected: TermSet,
    ) {
        let token = self.file.tokens[at];
        if token.kind == TokenKind::LexError {
            return;
        }
        if let Some(diagnostic) = self.item_rule(item, failed, at) {
            self.file.diagnostics.push(diagnostic);
            return;
        }
        let previous = self.previous(at);
        let at_column_zero = token.kind == TokenKind::Col0
            || previous.is_some_and(|previous| self.kind(previous) == TokenKind::Col0);
        if at_column_zero {
            let (col0, first) = match token.kind {
                TokenKind::Col0 => (at, self.next(at + 1)),
                _ => (previous.expect("a column-0 token follows its COL0"), at),
            };
            let fixes = item
                .filter(|&item| {
                    let parse = if col0 == at {
                        before.clone()
                    } else {
                        self.replay(item, col0)
                    };
                    parse.accepts(&[self.file.tokens[first]], &self.file.source)
                })
                .map(|item| vec![vec![self.file.reindented(item, first)]])
                .unwrap_or_default();
            self.report_fixed(at, (at, at), Code::ItemStrayColumnZero, fixes, Offer::Each);
            return;
        }
        let first_on_line =
            previous.is_none_or(|previous| self.file.tokens[previous].line < token.line);
        let col0 = Token {
            kind: TokenKind::Col0,
            ..token
        };
        if first_on_line
            && !token.kind.is_virtual()
            && before.accepts(&[col0, token], &self.file.source)
        {
            let fix = vec![self.file.unindented(at)];
            self.report_fixed(at, (at, at), Code::ItemIndented, vec![fix], Offer::Each);
            return;
        }
        let written = token.written();
        let idiom = idiom::foreign(&idiom::Failed {
            file: &self.file,
            state: failed,
            expected: &expected,
            at,
        });
        if let Some(Idiom::Rewrite {
            expected,
            found,
            span,
            fixes,
        }) = idiom
        {
            self.file.diagnostics.push(FileDiagnostic {
                expected,
                found: Some(found),
                span: Some(span),
                failing: Some(token.start),
                fixes: fixes.into_iter().map(|fix| vec![fix]).collect(),
                offer: Offer::First,
                ..FileDiagnostic::new(token.line, Code::SyntaxForeign)
            });
            return;
        }
        let (code, expected_text, fix) =
            if idiom.is_some() || matches!(token.kind, TokenKind::Foreign(_)) {
                (Code::SyntaxForeign, written_set(&expected), None)
            } else if let Some(closers) = self.unclosed(at) {
                let written: Vec<String> = closers
                    .iter()
                    .map(|closer| format!("\"{}\"", closer.spelling()))
                    .collect();
                let fix = self.file.closed(at, &closers);
                (Code::SyntaxUnclosedDelimiter, written.join(" "), fix)
            } else if let Some(closer) = self.mismatched(at) {
                let written = format!("\"{}\"", closer.spelling());
                let fix = Some(self.file.rematched(at, closer));
                (Code::SyntaxMismatchedDelimiter, written, fix)
            } else {
                (Code::SyntaxUnexpectedToken, written_set(&expected), None)
            };
        self.file.diagnostics.push(FileDiagnostic {
            expected: Some(expected_text),
            found: Some(written),
            span: Some((token.start, token.end)),
            failing: Some(token.start),
            fixes: fix.into_iter().map(|fix| vec![fix]).collect(),
            ..FileDiagnostic::new(token.line, code)
        });
    }

    /// The diagnostic a rule of an item kind names for a syntax failure, on its line, with the
    /// rule's fix.
    fn item_rule(&self, item: Option<usize>, failed: &State, at: usize) -> Option<FileDiagnostic> {
        let item = item?;
        let token = self.file.tokens[at];
        let line = token.line;
        let diagnostic = |code: Code, line: u32, fix: Option<Vec<Replacement>>| FileDiagnostic {
            span: Some((token.start, token.end)),
            failing: Some(token.start),
            fixes: fix.into_iter().collect(),
            ..FileDiagnostic::new(line, code)
        };
        let opener = if token.kind == TokenKind::Col0 {
            self.next(at + 1)
        } else {
            at
        };
        let next_kind = self.kind(self.next(at + 1));
        let opener_at_zero =
            token.kind == TokenKind::Col0 && next_kind == TokenKind::Punct(Punct::LBrace);
        let is_opener = token.kind == TokenKind::Punct(Punct::LBrace) || opener_at_zero;
        let rule = failed.innermost_rule();
        let atom = failed.current_atom();
        let clause_rules = [
            Rule::RequiresClause,
            Rule::EnsuresClause,
            Rule::ExampleClause,
            Rule::DecreasesClause,
            Rule::TodoClause,
            Rule::WaiveClause,
        ];
        let after_closer = self
            .previous(at)
            .is_some_and(|previous| self.kind(previous) == TokenKind::Punct(Punct::RBrace));
        let label = atom == Some(Atom::Rule(Rule::ItemLabel))
            || (rule == Some(Rule::MethodBody)
                && atom == Some(Atom::Term(Term::Class(Class::Ident))));
        if label && after_closer {
            let fix = self
                .file
                .declared_name(item, failed)
                .and_then(|name| self.file.relabelled(at, &name))
                .map(|fix| vec![fix]);
            return Some(diagnostic(Code::ItemLabelMismatch, line, fix));
        }
        if rule.is_some_and(|rule| clause_rules.contains(&rule)) && is_opener {
            let fix = self.file.opener_below(opener);
            return Some(diagnostic(Code::ItemBodyOpener, line, Some(fix)));
        }
        if rule == Some(Rule::InterfaceItem) && is_opener {
            return Some(diagnostic(Code::ItemBodyOpener, line, None));
        }
        if rule == Some(Rule::FnEnd) {
            let after_head = atom == Some(Atom::Rule(Rule::ClauseRegion));
            let after_clauses = atom == Some(Atom::Term(Term::Class(Class::Col0)));
            if after_head && opener_at_zero {
                let fix = self.file.opener_to_head(opener);
                return Some(diagnostic(Code::ItemBodyOpener, line, fix));
            }
            if after_clauses && is_opener {
                let fix = vec![self.file.unindented(opener)];
                return Some(diagnostic(Code::ItemBodyOpener, line, Some(fix)));
            }
            if after_head || after_clauses || atom == Some(Atom::Term(Term::Punct(Punct::LBrace))) {
                let foreign = self.item_has_keyword(item, Keyword::Foreign);
                let code = if foreign {
                    Code::ItemForeignForm
                } else {
                    Code::ItemMissingBody
                };
                return Some(diagnostic(code, self.head_line(item), None));
            }
        }
        let body = failed
            .frames
            .iter()
            .rev()
            .find(|frame| matches!(frame.rule, Some(Rule::FnBody | Rule::MethodBody)));
        let opener = match body {
            Some(body) if body.rule == Some(Rule::MethodBody) => body.start,
            Some(body) => self.previous(body.start)?,
            None if atom == Some(Atom::Rule(Rule::FnBody)) => self.previous(at)?,
            None => return None,
        };
        let first_statement = self.next(opener + 1);
        let failing_statement = failed
            .frames
            .iter()
            .rev()
            .find(|frame| frame.rule == Some(Rule::Stmt))
            .map_or(at, |frame| frame.start);
        let starts_clause = match self.kind(first_statement) {
            TokenKind::Keyword(keyword) => {
                matches!(
                    keyword,
                    Keyword::Requires
                        | Keyword::Ensures
                        | Keyword::Decreases
                        | Keyword::Waive
                        | Keyword::Todo
                )
            }
            TokenKind::Ident => self.file.text(first_statement) == "example",
            _ => false,
        };
        let opener_ends_head = self
            .previous(opener)
            .is_some_and(|previous| self.kind(previous) != TokenKind::Col0);
        (failing_statement == first_statement && opener_ends_head && starts_clause).then(|| {
            let fix = self.file.opener_below(opener);
            diagnostic(
                Code::ItemBodyOpener,
                self.file.tokens[first_statement].line,
                Some(fix),
            )
        })
    }

    fn item_has_keyword(&self, item: usize, keyword: Keyword) -> bool {
        (item..self.file.tokens.len())
            .take_while(|&at| self.kind(at) != TokenKind::Keyword(Keyword::Fn))
            .any(|at| self.kind(at) == TokenKind::Keyword(keyword))
    }

    /// The line of an item's first token after its doc comment.
    fn head_line(&self, item: usize) -> u32 {
        let head = (item..self.file.tokens.len())
            .find(|&at| {
                !self.file.comment[at]
                    && !matches!(
                        self.kind(at),
                        TokenKind::Col0 | TokenKind::Doc | TokenKind::Nl
                    )
            })
            .expect("an item has a head");
        self.file.tokens[head].line
    }

    /// The delimiters of the logical line of `at` still open before it, outermost first.
    fn open_delimiters(&self, at: usize) -> Vec<Punct> {
        let start = (0..at)
            .rev()
            .find(|&index| self.kind(index) == TokenKind::Nl)
            .map_or(0, |nl| nl + 1);
        let mut open = Vec::new();
        for index in start..at {
            if self.file.comment[index] {
                continue;
            }
            match self.kind(index) {
                TokenKind::Punct(punct @ (Punct::LParen | Punct::LBracket | Punct::LBrace)) => {
                    open.push(punct)
                }
                TokenKind::Punct(closer @ (Punct::RParen | Punct::RBracket | Punct::RBrace))
                    if open.last().is_some_and(|opener| closes(*opener) == closer) =>
                {
                    open.pop();
                }
                _ => {}
            }
        }
        open
    }

    /// The closers of the delimiters still open at an `NL` or `EOF`, innermost first, from the
    /// outermost `(` or `[`.
    fn unclosed(&self, at: usize) -> Option<Vec<Punct>> {
        if !matches!(self.kind(at), TokenKind::Nl | TokenKind::Eof) {
            return None;
        }
        let open = self.open_delimiters(at);
        let outermost = open
            .iter()
            .position(|punct| matches!(punct, Punct::LParen | Punct::LBracket))?;
        Some(
            open[outermost..]
                .iter()
                .rev()
                .map(|punct| closes(*punct))
                .collect(),
        )
    }

    /// The closer a closing token stands in place of.
    fn mismatched(&self, at: usize) -> Option<Punct> {
        let TokenKind::Punct(closer @ (Punct::RParen | Punct::RBracket | Punct::RBrace)) =
            self.kind(at)
        else {
            return None;
        };
        let at_column_zero = self
            .previous(at)
            .is_some_and(|previous| self.kind(previous) == TokenKind::Col0);
        let innermost = *self.open_delimiters(at).last()?;
        let delimiter =
            matches!(innermost, Punct::LParen | Punct::LBracket) && closes(innermost) != closer;
        (!at_column_zero && delimiter).then(|| closes(innermost))
    }
}

fn closes(opener: Punct) -> Punct {
    match opener {
        Punct::LParen => Punct::RParen,
        Punct::LBracket => Punct::RBracket,
        _ => Punct::RBrace,
    }
}

/// The terminals of a set as grammar notation writes them, in byte order, separated by `, `.
fn written_set(set: &TermSet) -> String {
    let mut written: Vec<String> = set.terms().map(Term::written).collect();
    written.sort();
    written.join(", ")
}

#[cfg(test)]
mod tests;
