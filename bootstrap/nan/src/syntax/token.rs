//! Tokens, including the virtual tokens the lexer adds.

use super::grammar::{Class, ContextWord, Foreign, Keyword, Punct, Term};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
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
    Keyword(Keyword),
    Punct(Punct),
    Foreign(Foreign),
    Nl,
    Col0,
    Eof,
    /// The position after the tokens of a line holding a lexical diagnostic.
    LexError,
}

impl TokenKind {
    pub fn is_virtual(self) -> bool {
        matches!(
            self,
            TokenKind::Nl | TokenKind::Col0 | TokenKind::Eof | TokenKind::LexError
        )
    }

    /// A token after which the logical line continues on the next line holding a token.
    pub fn is_continuing(self) -> bool {
        match self {
            TokenKind::Punct(punct) => {
                !matches!(punct, Punct::RParen | Punct::RBracket | Punct::RBrace)
            }
            TokenKind::Foreign(foreign) => matches!(foreign, Foreign::AmpAmp | Foreign::PipePipe),
            TokenKind::Keyword(keyword) => {
                matches!(keyword, Keyword::And | Keyword::Or | Keyword::Not)
            }
            _ => false,
        }
    }

    /// The terminal the token is in the grammar, an `IDENT` before any context word reading.
    pub fn term(self) -> Option<Term> {
        Some(match self {
            TokenKind::Ident => Term::Class(Class::Ident),
            TokenKind::Int => Term::Class(Class::Int),
            TokenKind::Float => Term::Class(Class::Float),
            TokenKind::Text => Term::Class(Class::Text),
            TokenKind::Bytes => Term::Class(Class::Bytes),
            TokenKind::Char => Term::Class(Class::Char),
            TokenKind::Byte => Term::Class(Class::Byte),
            TokenKind::TextLine => Term::Class(Class::TextLine),
            TokenKind::Doc => Term::Class(Class::Doc),
            TokenKind::FileDoc => Term::Class(Class::FileDoc),
            TokenKind::Keyword(keyword) => Term::Keyword(keyword),
            TokenKind::Punct(punct) => Term::Punct(punct),
            TokenKind::Nl => Term::Class(Class::Nl),
            TokenKind::Col0 => Term::Class(Class::Col0),
            TokenKind::Eof => Term::Class(Class::Eof),
            TokenKind::Foreign(_) | TokenKind::LexError => return None,
        })
    }
}

/// A token of a source file. Virtual tokens are empty; `line` is the physical line, from 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub start: u32,
    pub end: u32,
    pub line: u32,
}

impl Token {
    /// The token's characters; a token never holds bytes that are not well-formed UTF-8.
    pub fn text<'a>(&self, source: &'a [u8]) -> &'a str {
        std::str::from_utf8(&source[self.start as usize..self.end as usize])
            .expect("tokens hold well-formed UTF-8")
    }

    /// The context word an `IDENT` token spells, if any.
    pub fn context_word(&self, source: &[u8]) -> Option<ContextWord> {
        match self.kind {
            TokenKind::Ident => ContextWord::from_spelling(self.text(source)),
            _ => None,
        }
    }

    /// The written form of the token in grammar notation, as a failure reports it.
    pub fn written(&self) -> String {
        match self.kind {
            TokenKind::Foreign(foreign) => format!("\"{}\"", foreign.spelling()),
            kind => kind
                .term()
                .expect("a token the parse reads is a terminal")
                .written(),
        }
    }
}
