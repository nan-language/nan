//! The grammar of language 1, generated from `docs/spec/41-grammar.md`, and its LL(1) analysis.

use std::sync::LazyLock;

use crate::diagnostic::Code;

include!(concat!(env!("OUT_DIR"), "/grammar.rs"));

pub type ChoiceId = usize;
pub type SeqId = usize;

/// One alternative of a choice: elements in order, and the code it is rejected with, if any.
pub struct SeqDef {
    pub elems: &'static [ElemDef],
    pub rejection: Option<Code>,
}

pub struct ElemDef {
    pub atom: Atom,
    pub repeat: Repeat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Atom {
    Rule(Rule),
    Group(ChoiceId),
    Term(Term),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repeat {
    One,
    Opt,
    Star,
    Plus,
}

/// A terminal of the syntactic grammar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Term {
    Keyword(Keyword),
    Punct(Punct),
    Context(ContextWord),
    Class(Class),
}

/// The token classes and virtual tokens that syntactic rules name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Ident,
    Int,
    Float,
    Text,
    Bytes,
    Char,
    Byte,
    TextLine,
    Doc,
    FileDoc,
    Nl,
    Col0,
    Eof,
}

impl Class {
    const ALL: [Class; 13] = [
        Class::Ident,
        Class::Int,
        Class::Float,
        Class::Text,
        Class::Bytes,
        Class::Char,
        Class::Byte,
        Class::TextLine,
        Class::Doc,
        Class::FileDoc,
        Class::Nl,
        Class::Col0,
        Class::Eof,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Class::Ident => "IDENT",
            Class::Int => "INT",
            Class::Float => "FLOAT",
            Class::Text => "TEXT",
            Class::Bytes => "BYTES",
            Class::Char => "CHAR",
            Class::Byte => "BYTE",
            Class::TextLine => "TEXT_LINE",
            Class::Doc => "DOC",
            Class::FileDoc => "FILE_DOC",
            Class::Nl => "NL",
            Class::Col0 => "COL0",
            Class::Eof => "EOF",
        }
    }
}

const KEYWORDS: usize = Keyword::ALL.len();
const PUNCTS: usize = Punct::ALL.len();
const CONTEXTS: usize = ContextWord::ALL.len();
const TERMS: usize = KEYWORDS + PUNCTS + CONTEXTS + Class::ALL.len();
const WORDS: usize = TERMS.div_ceil(64);

impl Term {
    fn index(self) -> usize {
        match self {
            Term::Keyword(keyword) => keyword as usize,
            Term::Punct(punct) => KEYWORDS + punct as usize,
            Term::Context(word) => KEYWORDS + PUNCTS + word as usize,
            Term::Class(class) => KEYWORDS + PUNCTS + CONTEXTS + class as usize,
        }
    }

    fn from_index(index: usize) -> Term {
        if index < KEYWORDS {
            Term::Keyword(Keyword::ALL[index])
        } else if index < KEYWORDS + PUNCTS {
            Term::Punct(Punct::ALL[index - KEYWORDS])
        } else if index < KEYWORDS + PUNCTS + CONTEXTS {
            Term::Context(ContextWord::ALL[index - KEYWORDS - PUNCTS])
        } else {
            Term::Class(Class::ALL[index - KEYWORDS - PUNCTS - CONTEXTS])
        }
    }

    /// The terminal as grammar notation writes it: `"fn"`, `'language'` or `IDENT`.
    pub fn written(self) -> String {
        match self {
            Term::Keyword(keyword) => format!("\"{}\"", keyword.spelling()),
            Term::Punct(punct) => format!("\"{}\"", punct.spelling()),
            Term::Context(word) => format!("'{}'", word.spelling()),
            Term::Class(class) => class.name().to_owned(),
        }
    }
}

/// A set of terminals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TermSet([u64; WORDS]);

impl TermSet {
    pub fn single(term: Term) -> TermSet {
        let mut set = TermSet::default();
        set.insert(term);
        set
    }

    pub fn insert(&mut self, term: Term) {
        let index = term.index();
        self.0[index / 64] |= 1 << (index % 64);
    }

    pub fn contains(&self, term: Term) -> bool {
        let index = term.index();
        self.0[index / 64] & (1 << (index % 64)) != 0
    }

    pub fn union(&mut self, other: &TermSet) {
        for (word, other) in self.0.iter_mut().zip(other.0) {
            *word |= other;
        }
    }

    pub fn is_superset(&self, other: &TermSet) -> bool {
        self.0
            .iter()
            .zip(other.0)
            .all(|(word, other)| word & other == other)
    }

    pub fn terms(&self) -> impl Iterator<Item = Term> + '_ {
        (0..TERMS)
            .filter(|index| self.0[index / 64] & (1 << (index % 64)) != 0)
            .map(Term::from_index)
    }
}

/// What can start a symbol: terminals reached outside every rejected alternative (`ok`), terminals
/// reached through a rejected alternative (`rejected`), and whether it derives no token.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct First {
    pub ok: TermSet,
    pub rejected: TermSet,
    pub nullable: bool,
}

pub struct Analysis {
    choices: Vec<First>,
    seqs: Vec<First>,
}

pub static ANALYSIS: LazyLock<Analysis> = LazyLock::new(Analysis::compute);

impl Analysis {
    fn compute() -> Analysis {
        let mut analysis = Analysis {
            choices: vec![First::default(); CHOICES.len()],
            seqs: vec![First::default(); SEQS.len()],
        };
        loop {
            let mut changed = false;
            for (id, seq) in SEQS.iter().enumerate() {
                let mut first = analysis.elems(seq.elems);
                if seq.rejection.is_some() {
                    first.rejected.union(&first.ok);
                    first.ok = TermSet::default();
                }
                changed |= analysis.seqs[id] != first;
                analysis.seqs[id] = first;
            }
            for (id, alts) in CHOICES.iter().enumerate() {
                let mut first = First::default();
                for &alt in *alts {
                    first.ok.union(&analysis.seqs[alt].ok);
                    first.rejected.union(&analysis.seqs[alt].rejected);
                    first.nullable |= analysis.seqs[alt].nullable;
                }
                changed |= analysis.choices[id] != first;
                analysis.choices[id] = first;
            }
            if !changed {
                return analysis;
            }
        }
    }

    pub fn choice(&self, choice: ChoiceId) -> &First {
        &self.choices[choice]
    }

    pub fn seq(&self, seq: SeqId) -> &First {
        &self.seqs[seq]
    }

    pub fn atom(&self, atom: Atom) -> First {
        match atom {
            Atom::Term(term) => First {
                ok: TermSet::single(term),
                ..First::default()
            },
            Atom::Rule(rule) => self.choices[rule.choice()],
            Atom::Group(choice) => self.choices[choice],
        }
    }

    /// What can start a run of elements, through every element that may derive no token.
    fn elems(&self, elems: &[ElemDef]) -> First {
        let mut first = First {
            nullable: true,
            ..First::default()
        };
        for elem in elems {
            let atom = self.atom(elem.atom);
            first.ok.union(&atom.ok);
            first.rejected.union(&atom.rejected);
            if !(atom.nullable || matches!(elem.repeat, Repeat::Opt | Repeat::Star)) {
                first.nullable = false;
                break;
            }
        }
        first
    }
}

#[cfg(test)]
mod tests;
