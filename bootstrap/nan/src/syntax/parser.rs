//! The LL(1) parse over the generated grammar, under the resolutions the grammar publishes.

use super::grammar::{
    ANALYSIS, Atom, CHOICES, ContextWord, First, Keyword, Punct, Repeat, Rule, SEQS, SeqId, Term,
    TermSet,
};
use super::token::{Token, TokenKind};
use super::tree::Builder;
use crate::diagnostic::Code;

#[derive(Clone, Debug)]
pub struct Frame {
    seq: SeqId,
    elem: usize,
    count: usize,
    pub rule: Option<Rule>,
    /// The index of the first token the frame read.
    pub start: usize,
}

/// Why the parse stopped before consuming a token.
#[derive(Clone, Debug)]
pub enum Failure {
    /// A token the grammar does not accept, with the terminals it accepts there.
    Token { at: usize, expected: TermSet },
    /// A rejected alternative, parsed from the token at `start`.
    Rejected { code: Code, start: usize },
    /// `match`, or an `NL` inside a `{` the item opens, in a single-line item.
    MultipleLines { at: usize },
}

pub enum Fed {
    /// The token belongs to the parse and was consumed.
    Consumed,
    /// The start rule completed before the token.
    Done,
}

/// The next token, its index, and the token after it, which the context-word exceptions and the
/// resolution of `limits` commas read.
#[derive(Clone, Copy)]
pub struct Lookahead<'a> {
    pub token: &'a Token,
    pub next: Option<&'a Token>,
    pub at: usize,
}

/// A parse in progress: the frames of the expanded rules and what the item read so far opened.
#[derive(Clone, Debug)]
pub struct State {
    /// The start rule until the first token chooses its alternative.
    start: Option<Rule>,
    pub frames: Vec<Frame>,
    single_line: bool,
    braces: usize,
    pascal_path: bool,
    /// The first and end token index of the last prefix expression parsed.
    pub last_operand: Option<(usize, usize)>,
}

/// Receives the parse's tree events.
pub trait Sink {
    fn open(&mut self, rule: Rule);
    fn token(&mut self, at: usize);
    fn close(&mut self);
}

impl Sink for Builder {
    fn open(&mut self, rule: Rule) {
        Builder::open(self, rule);
    }

    fn token(&mut self, at: usize) {
        Builder::token(self, at as u32);
    }

    fn close(&mut self) {
        Builder::close(self);
    }
}

/// A sink for dry runs, which keep no tree.
pub struct Discard;

impl Sink for Discard {
    fn open(&mut self, _: Rule) {}
    fn token(&mut self, _: usize) {}
    fn close(&mut self) {}
}

/// How well a lookahead matches what can start a symbol; lower ranks win. A context word beats
/// `IDENT`, and an alternative outside every rejection beats a rejected one.
fn rank(first: &First, look: Look) -> Option<u8> {
    let context = look.context.map(Term::Context);
    [
        (context, &first.ok, 0),
        (context, &first.rejected, 1),
        (look.term, &first.ok, 2),
        (look.term, &first.rejected, 3),
    ]
    .into_iter()
    .find_map(|(term, set, rank)| term.filter(|term| set.contains(*term)).map(|_| rank))
}

#[derive(Clone, Copy)]
struct Look {
    term: Option<Term>,
    context: Option<ContextWord>,
}

impl Look {
    fn of(lookahead: Lookahead, source: &[u8]) -> Look {
        let next_kind = lookahead.next.map(|token| token.kind);
        let context = lookahead
            .token
            .context_word(source)
            .filter(|word| match word {
                ContextWord::Task => next_kind == Some(TokenKind::Keyword(Keyword::Fn)),
                ContextWord::All | ContextWord::Any => next_kind == Some(TokenKind::Ident),
                ContextWord::Manual => next_kind == Some(TokenKind::Text),
                _ => true,
            });
        Look {
            term: lookahead.token.kind.term(),
            context,
        }
    }
}

/// Whether the lookahead can start `rule`, through a rejected alternative or not.
pub fn starts(rule: Rule, lookahead: Lookahead, source: &[u8]) -> bool {
    rank(ANALYSIS.choice(rule.choice()), Look::of(lookahead, source)).is_some()
}

/// Whether an identifier has the PascalCase shape: an uppercase letter, then letters and digits
/// holding a lowercase letter unless it is one letter.
pub fn is_pascal(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_uppercase())
        && name.chars().all(|c| c.is_ascii_alphanumeric())
        && (name.len() == 1 || name.chars().any(|c| c.is_ascii_lowercase()))
}

impl State {
    pub fn new(start: Rule) -> State {
        State {
            start: Some(start),
            frames: Vec::new(),
            single_line: false,
            braces: 0,
            pascal_path: false,
            last_operand: None,
        }
    }

    fn push_rule(&mut self, rule: Rule, seq: SeqId, at: usize, sink: &mut dyn Sink) {
        sink.open(rule);
        let single_line_kind = matches!(
            rule,
            Rule::ImplItem
                | Rule::ConstItem
                | Rule::ConstAssertItem
                | Rule::RegistryItem
                | Rule::ContributeItem
                | Rule::RetiredItem
        );
        let single_line_fn = rule == Rule::FnEnd
            && matches!(
                SEQS[seq].elems[0].atom,
                Atom::Term(Term::Punct(Punct::Eq)) | Atom::Rule(Rule::ConventionClause)
            );
        self.single_line |= single_line_kind || single_line_fn;
        self.frames.push(Frame {
            seq,
            elem: 0,
            count: 0,
            rule: Some(rule),
            start: at,
        });
    }

    /// The alternative of a choice the lookahead selects, if any.
    fn choose(alts: &[SeqId], look: Look) -> Option<SeqId> {
        let ranked = alts
            .iter()
            .filter_map(|&alt| rank(ANALYSIS.seq(alt), look).map(|rank| (rank, alt)));
        let best = ranked.min_by_key(|(rank, _)| *rank).map(|(_, alt)| alt);
        best.or_else(|| alts.iter().copied().find(|&alt| ANALYSIS.seq(alt).nullable))
    }

    /// Advances the parse to the lookahead token: expands and completes rules until the token is
    /// consumed, the start rule completes before it, or the parse fails at it.
    pub fn feed(
        &mut self,
        lookahead: Lookahead,
        source: &[u8],
        sink: &mut dyn Sink,
    ) -> Result<Fed, Failure> {
        let look = Look::of(lookahead, source);
        let at = lookahead.at;
        let mut expected = TermSet::default();
        if let Some(start) = self.start.take() {
            let Some(alt) = State::choose(CHOICES[start.choice()], look) else {
                return Err(Failure::Token {
                    at,
                    expected: ANALYSIS.choice(start.choice()).ok,
                });
            };
            self.push_rule(start, alt, at, sink);
        }
        loop {
            let Some(frame) = self.frames.last_mut() else {
                return Ok(Fed::Done);
            };
            let elems = SEQS[frame.seq].elems;
            if frame.elem == elems.len() {
                let frame = self.frames.pop().expect("a frame");
                if let Some(code) = SEQS[frame.seq].rejection {
                    return Err(Failure::Rejected {
                        code,
                        start: frame.start,
                    });
                }
                if let Some(rule) = frame.rule {
                    sink.close();
                    if rule == Rule::PrefixExpr {
                        self.last_operand = Some((frame.start, at));
                    }
                }
                if let Some(parent) = self.frames.last_mut() {
                    parent.count += 1;
                }
                continue;
            }
            let elem = &elems[frame.elem];
            let (may, must) = match elem.repeat {
                Repeat::One => (frame.count == 0, frame.count == 0),
                Repeat::Opt => (frame.count == 0, false),
                Repeat::Star => (true, false),
                Repeat::Plus => (true, frame.count == 0),
            };
            if !may {
                frame.elem += 1;
                frame.count = 0;
                continue;
            }
            let first = ANALYSIS.atom(elem.atom);
            let starts = rank(&first, look).is_some();
            let resolved = match (frame.rule, elem.atom) {
                (Some(Rule::PathExpr), Atom::Rule(Rule::GenericArgs)) => self.pascal_path,
                (Some(Rule::LimitsClause), Atom::Group(_)) => lookahead.next.is_some_and(|next| {
                    let after = Lookahead {
                        token: next,
                        next: None,
                        at,
                    };
                    rank(
                        ANALYSIS.choice(Rule::Limit.choice()),
                        Look::of(after, source),
                    )
                    .is_some()
                }),
                _ => true,
            };
            if !must && !(starts && resolved) {
                if !starts {
                    expected.union(&first.ok);
                }
                frame.elem += 1;
                frame.count = 0;
                continue;
            }
            match elem.atom {
                Atom::Term(term) => {
                    if !starts {
                        expected.insert(term);
                        return Err(Failure::Token { at, expected });
                    }
                    frame.count += 1;
                    return self
                        .consume(lookahead, source, sink)
                        .map(|()| Fed::Consumed);
                }
                Atom::Rule(rule) => {
                    let Some(alt) = State::choose(CHOICES[rule.choice()], look) else {
                        expected.union(&first.ok);
                        return Err(Failure::Token { at, expected });
                    };
                    self.push_rule(rule, alt, at, sink);
                }
                Atom::Group(choice) => {
                    let Some(alt) = State::choose(CHOICES[choice], look) else {
                        expected.union(&first.ok);
                        return Err(Failure::Token { at, expected });
                    };
                    self.frames.push(Frame {
                        seq: alt,
                        elem: 0,
                        count: 0,
                        rule: None,
                        start: at,
                    });
                }
            }
        }
    }

    fn consume(
        &mut self,
        lookahead: Lookahead,
        source: &[u8],
        sink: &mut dyn Sink,
    ) -> Result<(), Failure> {
        let token = lookahead.token;
        match token.kind {
            TokenKind::Keyword(Keyword::Match) if self.single_line => {
                return Err(Failure::MultipleLines { at: lookahead.at });
            }
            TokenKind::Nl if self.single_line && self.braces > 0 => {
                return Err(Failure::MultipleLines { at: lookahead.at });
            }
            TokenKind::Punct(Punct::LBrace) => self.braces += 1,
            TokenKind::Punct(Punct::RBrace) => self.braces = self.braces.saturating_sub(1),
            _ => {}
        }
        if token.kind == TokenKind::Ident {
            self.pascal_path = is_pascal(token.text(source));
        }
        sink.token(lookahead.at);
        Ok(())
    }

    /// The rule of the innermost frame that expands one.
    pub fn innermost_rule(&self) -> Option<Rule> {
        self.frames.iter().rev().find_map(|frame| frame.rule)
    }

    /// The atom the innermost frame stands at, if it has not passed its end.
    pub fn current_atom(&self) -> Option<Atom> {
        let frame = self.frames.last()?;
        SEQS[frame.seq].elems.get(frame.elem).map(|elem| elem.atom)
    }

    /// Whether the parse accepts these tokens in order, each read with the next as lookahead.
    pub fn accepts(&self, tokens: &[Token], source: &[u8]) -> bool {
        let mut state = self.clone();
        tokens.iter().enumerate().all(|(index, token)| {
            let lookahead = Lookahead {
                token,
                next: tokens.get(index + 1),
                at: usize::MAX,
            };
            matches!(
                state.feed(lookahead, source, &mut Discard),
                Ok(Fed::Consumed)
            )
        })
    }
}
