//! The replacements the exact fixes of the checker's rules make in an item's file.

use super::Checker;
use super::db::{ItemId, PackageId};
use crate::diagnostic::{Code, Fix, Offer, Replacement};
use crate::syntax::ast::Tok;
use crate::syntax::grammar::Punct;
use crate::syntax::token::TokenKind;
use crate::workspace::Authority;

impl Checker<'_> {
    /// The fix replacing the bytes `start..end` of an item's file with a text.
    pub fn fix_bytes(&self, item: ItemId, start: u32, end: u32, text: &str) -> Fix {
        let replacement = Replacement {
            start,
            end,
            text: text.to_owned(),
        };
        Fix(vec![(self.db.item(item).file.path.clone(), replacement)])
    }

    /// The fix replacing an item's tokens `first..=last` with a text.
    pub fn fix_replacing(&self, item: ItemId, first: Tok, last: Tok, text: &str) -> Fix {
        let tokens = &self.db.item(item).file.syntax.tokens;
        self.fix_bytes(
            item,
            tokens[first as usize].start,
            tokens[last as usize].end,
            text,
        )
    }

    /// The fix inserting a text before one of an item's tokens.
    pub fn fix_inserting(&self, item: ItemId, before: Tok, text: &str) -> Fix {
        let start = self.db.item(item).file.syntax.tokens[before as usize].start;
        self.fix_bytes(item, start, start, text)
    }

    /// The fix inserting a text after one of an item's tokens.
    pub fn fix_appending(&self, item: ItemId, after: Tok, text: &str) -> Fix {
        let end = self.db.item(item).file.syntax.tokens[after as usize].end;
        self.fix_bytes(item, end, end, text)
    }

    /// The fix deleting an item's tokens `first..=last` and the spaces between them and the
    /// token before on their line.
    pub fn fix_deleting(&self, item: ItemId, first: Tok, last: Tok) -> Fix {
        let syntax = &self.db.item(item).file.syntax;
        let tokens = &syntax.tokens;
        let start = syntax
            .previous(first as usize)
            .filter(|&before| tokens[before].line == tokens[first as usize].line)
            .map_or(tokens[first as usize].start, |before| tokens[before].end);
        self.fix_bytes(item, start, tokens[last as usize].end, "")
    }

    /// The fix deleting an item's tokens `first..=last` and the spaces after them on their line.
    pub fn fix_deleting_forward(&self, item: ItemId, first: Tok, last: Tok) -> Fix {
        let syntax = &self.db.item(item).file.syntax;
        let tokens = &syntax.tokens;
        let next = &tokens[syntax.next(last as usize + 1)];
        let end = if next.line == tokens[last as usize].line && !next.kind.is_virtual() {
            next.start
        } else {
            tokens[last as usize].end
        };
        self.fix_bytes(item, tokens[first as usize].start, end, "")
    }

    /// Reports a diagnostic found resolving an item's signature, types or paths, on one token,
    /// with the fix its rule names: `.` for a `::` before a member, a redundant segment and its
    /// `::` deleted, or a limit deleted.
    pub fn report_problem(&self, item: ItemId, token: Tok, code: Code) {
        if code == Code::NameUnknown {
            let span = self.path_span(item, token);
            self.report_unknown(item, span, self.type_candidates(item), None);
            return;
        }
        let syntax = &self.db.item(item).file.syntax;
        let (construct, fix) = match code {
            Code::NameColonForMember => {
                let separator = syntax.previous(token as usize).unwrap_or(token as usize) as Tok;
                let fix = self.fix_replacing(item, separator, separator, ".");
                ((separator, token), Some(fix))
            }
            Code::NameRedundantPath => {
                let separator = syntax.next(token as usize + 1);
                let segment = syntax.next(separator + 1);
                let start = syntax.tokens[token as usize].start;
                let fix = self.fix_bytes(item, start, syntax.tokens[segment].start, "");
                ((token, segment as Tok), Some(fix))
            }
            Code::LimitDuplicate | Code::LimitRedundant | Code::KernelIrqLimit => {
                ((token, token), Some(self.limit_deleted(item, token)))
            }
            _ => ((token, token), None),
        };
        self.report_fixed(
            item,
            construct,
            code,
            fix.into_iter().collect(),
            Offer::Each,
        );
    }

    /// The fix deleting the limit at `token` from its clause, with a comma beside it, or the
    /// whole clause when it is the only limit.
    fn limit_deleted(&self, item: ItemId, token: Tok) -> Fix {
        let syntax = &self.db.item(item).file.syntax;
        let tokens = &syntax.tokens;
        let comma = |at: usize| tokens[at].kind == TokenKind::Punct(Punct::Comma);
        let last = if syntax.text(token as usize) == "stack" {
            syntax.next(token as usize + 1)
        } else {
            token as usize
        };
        let before = syntax.previous(token as usize).unwrap_or(token as usize);
        let after = syntax.next(last + 1);
        if comma(before) {
            self.fix_bytes(item, tokens[before].start, tokens[last].end, "")
        } else if comma(after) {
            let next = syntax.next(after + 1);
            self.fix_bytes(item, tokens[token as usize].start, tokens[next].start, "")
        } else {
            self.fix_deleting(item, before as Tok, last as Tok)
        }
    }

    /// The fix deleting the element `first..=last` of a comma-separated list with the comma
    /// before it, or after it when it comes first; none when it is the list's only element.
    pub fn fix_element_deleted(&self, item: ItemId, first: Tok, last: Tok) -> Option<Fix> {
        let syntax = &self.db.item(item).file.syntax;
        let tokens = &syntax.tokens;
        let comma = |at: usize| tokens[at].kind == TokenKind::Punct(Punct::Comma);
        let before = syntax.previous(first as usize)?;
        let after = syntax.next(last as usize + 1);
        if comma(before) {
            Some(self.fix_bytes(item, tokens[before].start, tokens[last as usize].end, ""))
        } else if comma(after) {
            let next = syntax.next(after + 1);
            Some(self.fix_bytes(item, tokens[first as usize].start, tokens[next].start, ""))
        } else {
            None
        }
    }

    /// The fix deleting a listed capture `first..=last`, or its whole list when it is the only
    /// one.
    pub fn fix_capture_deleted(&self, item: ItemId, first: Tok, last: Tok) -> Fix {
        self.fix_element_deleted(item, first, last)
            .unwrap_or_else(|| {
                let syntax = &self.db.item(item).file.syntax;
                let open = syntax.previous(first as usize).unwrap_or(first as usize) as Tok;
                let close = syntax.next(last as usize + 1) as Tok;
                self.fix_replacing(item, open, close, "")
            })
    }

    /// The fix listing a capture, written `capture`, in the list of the closure opened by `fn`
    /// at `opener`, writing the list when it has none.
    pub fn fix_capture_listed(&self, item: ItemId, opener: Tok, capture: &str) -> Fix {
        let syntax = &self.db.item(item).file.syntax;
        let open = syntax.next(opener as usize + 1);
        if syntax.tokens[open].kind != TokenKind::Punct(Punct::LBracket) {
            return self.fix_appending(item, opener, &format!("[{capture}]"));
        }
        let close = (open..syntax.tokens.len())
            .find(|&at| syntax.tokens[at].kind == TokenKind::Punct(Punct::RBracket))
            .unwrap_or(open);
        let start = syntax.tokens[close].start;
        self.fix_bytes(item, start, start, &format!(", {capture}"))
    }

    /// The fix deleting the physical lines of an item's tokens `first..=last`.
    pub fn fix_deleting_lines(&self, item: ItemId, first: Tok, last: Tok) -> Fix {
        let syntax = &self.db.item(item).file.syntax;
        let line = |token: Tok| syntax.tokens[token as usize].line as usize;
        let start = syntax.lines[line(first) - 1].start;
        let end = syntax
            .lines
            .get(line(last))
            .map_or(syntax.source.len() as u32, |next| next.start);
        self.fix_bytes(item, start, end, "")
    }

    /// The fix deleting an item's lines, its doc comment's included, with the blank line after
    /// them, or before them at the end of the file.
    pub fn fix_deleting_item(&self, item: ItemId) -> Fix {
        let data = self.db.item(item);
        let syntax = data.syntax();
        let lines = syntax.item_lines(&syntax.items[data.index]);
        let start = |line: u32| {
            syntax
                .lines
                .get(line as usize - 1)
                .map_or(syntax.source.len() as u32, |line| line.start)
        };
        let blank = |line: u32| {
            line >= 1
                && syntax
                    .lines
                    .get(line as usize - 1)
                    .is_some_and(|line| line.start == line.end)
        };
        let (mut first, mut last) = (*lines.start(), *lines.end());
        if blank(last + 1) {
            last += 1;
        } else if blank(first - 1) {
            first -= 1;
        }
        self.fix_bytes(item, start(first), start(last + 1), "")
    }

    /// The fix adding a name to a package's `allow` directive for an authority, writing the
    /// directive before the manifest's closing line when it has none.
    pub fn fix_granting(
        &self,
        package: PackageId,
        authority: &Authority,
        name: &str,
    ) -> Option<Fix> {
        let data = self.db.package(package);
        let manifest = data.source.manifest.as_ref()?;
        let file = &data.source.files[0];
        let syntax = &file.syntax;
        let replacement = match manifest
            .allows
            .iter()
            .find(|allow| allow.authority == *authority)
        {
            Some(allow) => {
                let end = syntax
                    .tokens
                    .iter()
                    .filter(|token| token.line == allow.line && !token.kind.is_virtual())
                    .map(|token| token.end)
                    .max()?;
                Replacement {
                    start: end,
                    end,
                    text: format!(", {name}"),
                }
            }
            None => {
                let closer = syntax
                    .tokens
                    .iter()
                    .rev()
                    .find(|token| token.kind == TokenKind::Punct(Punct::RBrace))?;
                let start = syntax.lines[closer.line as usize - 1].start;
                Replacement {
                    start,
                    end: start,
                    text: format!("    allow {} in {name}\n", authority.written()),
                }
            }
        };
        Some(Fix(vec![(file.path.clone(), replacement)]))
    }

    /// The fix adding a member line, indented one level, before the line closing an item.
    pub fn member_added(&self, item: ItemId, member: &str) -> Option<Fix> {
        let data = self.db.item(item);
        let syntax = &data.file.syntax;
        let span = &syntax.items[data.index];
        let closer = (span.first..span.end)
            .rev()
            .find(|&at| syntax.tokens[at].kind == TokenKind::Punct(Punct::RBrace))?;
        let line = syntax.lines[syntax.tokens[closer].line as usize - 1].start;
        Some(self.fix_bytes(item, line, line, &format!("    {member}\n")))
    }

    /// The spaces indenting the line of one of an item's tokens.
    pub fn indentation(&self, item: ItemId, token: Tok) -> String {
        let syntax = &self.db.item(item).file.syntax;
        let line = &syntax.lines[syntax.tokens[token as usize].line as usize - 1];
        let width = syntax.source[line.start as usize..line.end as usize]
            .iter()
            .take_while(|byte| **byte == b' ')
            .count();
        " ".repeat(width)
    }

    /// The text of an item's tokens `first..=last` as written.
    pub fn written(&self, item: ItemId, first: Tok, last: Tok) -> String {
        let syntax = &self.db.item(item).file.syntax;
        let (start, end) = (
            syntax.tokens[first as usize].start as usize,
            syntax.tokens[last as usize].end as usize,
        );
        String::from_utf8_lossy(&syntax.source[start..end]).into_owned()
    }
}
