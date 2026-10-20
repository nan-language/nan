//! Documentation: the doc comments of items and members and the summaries of files, with their
//! code blocks, digest claims and doc references, and the docs the variants of a module share.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use super::Checker;
use super::db::{ItemId, ModuleId, PackageId, TypeKey};
use super::interface::{narrower, openness};
use super::resolve::{Named, PathError};
use super::sig::SigKind;
use crate::diagnostic::{Code, Diagnostic, Fix, Reason, Replacement};
use crate::machine::api::DocKey;
use crate::syntax::ast::{self, ExprKind, ItemKind, RetiredNew, Visibility};
use crate::syntax::file::SourceFile;
use crate::syntax::grammar::{Keyword, Punct, Rule};
use crate::syntax::lexer::lex;
use crate::syntax::token::TokenKind;
use crate::syntax::tree::Tree;

/// The words starting a digest claim when a fact word follows them.
const CLAIM_WORDS: [&str; 13] = [
    "never", "not", "cannot", "can't", "doesn't", "don't", "won't", "may", "might", "always",
    "neither", "nor", "without",
];

/// The words naming a digest fact.
const FACT_WORDS: [&str; 23] = [
    "wait",
    "waits",
    "waiting",
    "block",
    "blocks",
    "suspend",
    "suspends",
    "suspending",
    "allocate",
    "allocates",
    "allocating",
    "trap",
    "traps",
    "trapping",
    "panic",
    "panics",
    "panicking",
    "terminate",
    "terminates",
    "terminating",
    "halt",
    "halts",
    "halting",
];

/// The words that are a digest claim alone.
const CLAIM_TERMS: [&str; 11] = [
    "blocking",
    "non-blocking",
    "non-allocating",
    "allocation-free",
    "noalloc",
    "trap-free",
    "panic-free",
    "notrap",
    "non-terminating",
    "pure",
    "side-effect-free",
];

/// The members a doc comment may document beside items.
const MEMBERS: [Rule; 6] = [
    Rule::StructMember,
    Rule::EnumMember,
    Rule::InterfaceMember,
    Rule::TagLine,
    Rule::LayoutMember,
    Rule::RegisterMember,
];

/// A doc comment: the `DOC` tokens of its lines, the member it documents as variants compare
/// docs, empty for its item, and the visibility of what it documents.
struct DocComment {
    lines: Vec<usize>,
    member: String,
    visibility: Visibility,
}

/// A line of doc text: its line number, the byte of its file where its text starts, and the text.
type DocLine<'t> = (u32, u32, &'t str);

/// The doc comments of an item's tree, its own first, the item's visibility given.
fn doc_comments(syntax: &SourceFile, tree: &Tree, visibility: Visibility) -> Vec<DocComment> {
    let mut found = Vec::new();
    for node in tree.descendants(tree.root()) {
        let rule = tree.rule(node);
        if rule != Rule::ItemStart && !MEMBERS.contains(&rule) {
            continue;
        }
        let lines: Vec<usize> = tree
            .child_tokens(node)
            .map(|token| token as usize)
            .filter(|token| syntax.tokens[*token].kind == TokenKind::Doc)
            .collect();
        if lines.is_empty() {
            continue;
        }
        let (member, visibility) = match rule {
            Rule::ItemStart => (String::new(), visibility),
            _ => documented(syntax, tree, node, visibility),
        };
        found.push(DocComment {
            lines,
            member,
            visibility,
        });
    }
    found
}

/// What the doc comment of a member documents: the member's name, or an invariant line's text,
/// and its visibility, a field's being the narrower of its own and its item's.
fn documented(
    syntax: &SourceFile,
    tree: &Tree,
    member: u32,
    item: Visibility,
) -> (String, Visibility) {
    let content = match tree.rule(member) {
        Rule::StructMember | Rule::EnumMember | Rule::RegisterMember => {
            tree.child_nodes(member).next().unwrap_or(member)
        }
        _ => member,
    };
    let tokens = || {
        let (first, last) = tree.span(content).unwrap_or_default();
        (first..=last).filter(|token| {
            !matches!(
                syntax.tokens[*token as usize].kind,
                TokenKind::Doc | TokenKind::Nl | TokenKind::Col0
            )
        })
    };
    if tree.rule(content) == Rule::InvariantLine {
        let text: Vec<&str> = tokens().map(|token| syntax.text(token as usize)).collect();
        return (text.join(" "), item);
    }
    let name = tokens()
        .find(|token| syntax.tokens[*token as usize].kind == TokenKind::Ident)
        .map_or("", |token| syntax.text(token as usize));
    let visibility = match tree.rule(content) {
        Rule::Field => {
            let own = match tree
                .find(content, Rule::Visibility)
                .and_then(|node| tree.span(node))
            {
                Some((token, _))
                    if syntax.tokens[token as usize].kind == TokenKind::Keyword(Keyword::Pub) =>
                {
                    Visibility::Pub
                }
                Some(_) => Visibility::Pkg,
                None => Visibility::Private,
            };
            narrower(own, item)
        }
        _ => item,
    };
    (name.to_owned(), visibility)
}

/// The text of a `DOC` or `FILE_DOC` line, without its opener and a space after it, and the byte
/// of its file where that text starts.
pub fn line_text(syntax: &SourceFile, token: usize) -> (u32, &str) {
    let token = &syntax.tokens[token];
    let text = token.text(&syntax.source);
    let opener = if text[3..].starts_with(' ') { 4 } else { 3 };
    (token.start + opener as u32, &text[opener..])
}

/// A code span or doc reference of a line of doc text: the bytes it spans, its delimiters
/// included.
enum Part {
    Code(Range<usize>),
    Reference(Range<usize>),
}

/// The code spans and doc references of a line of doc text, read left to right: a backtick opens
/// a code span ending at the next backtick, a `[` a doc reference ending at the next `]`.
fn parts(text: &str) -> Vec<Part> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let close = match bytes[at] {
            b'`' => b'`',
            b'[' => b']',
            _ => {
                at += 1;
                continue;
            }
        };
        let Some(end) = bytes[at + 1..].iter().position(|byte| *byte == close) else {
            at += 1;
            continue;
        };
        let range = at..at + end + 2;
        at = range.end;
        found.push(match close {
            b'`' => Part::Code(range),
            _ => Part::Reference(range),
        });
    }
    found
}

/// The doc references of a line of doc text, with the offsets of their texts.
pub fn references(text: &str) -> Vec<(usize, &str)> {
    parts(text)
        .into_iter()
        .filter_map(|part| match part {
            Part::Reference(range) => {
                Some((range.start + 1, &text[range.start + 1..range.end - 1]))
            }
            Part::Code(_) => None,
        })
        .collect()
}

/// The first line of a doc text whose text after leading spaces starts with three backticks.
fn code_block(lines: &[DocLine]) -> Option<u32> {
    lines
        .iter()
        .find(|(_, _, text)| text.trim_start_matches(' ').starts_with("```"))
        .map(|(line, _, _)| *line)
}

/// The lines of a doc text on which a digest claim starts: a claim word followed by a fact word
/// with only spaces and line ends between them, or a claim term, outside code spans.
fn claim_lines(lines: &[DocLine]) -> BTreeSet<u32> {
    let mut found = BTreeSet::new();
    let mut previous: Option<(String, u32)> = None;
    let mut judge = |word: String, line: u32, previous: &mut Option<(String, u32)>| {
        if CLAIM_TERMS.contains(&word.as_str()) {
            found.insert(line);
        }
        if let Some((claim, at)) = previous.take()
            && CLAIM_WORDS.contains(&claim.as_str())
            && FACT_WORDS.contains(&word.as_str())
        {
            found.insert(at);
        }
        *previous = Some((word, line));
    };
    for &(line, _, text) in lines {
        let code: Vec<Range<usize>> = parts(text)
            .into_iter()
            .filter_map(|part| match part {
                Part::Code(range) => Some(range),
                Part::Reference(_) => None,
            })
            .collect();
        let mut word = String::new();
        for (at, c) in text.char_indices() {
            let c = if c == '\u{2019}' { '\'' } else { c };
            let prose = !code.iter().any(|range| range.contains(&at));
            if prose && (c.is_ascii_alphanumeric() || c == '-' || c == '\'') {
                word.push(c.to_ascii_lowercase());
                continue;
            }
            if !word.is_empty() {
                judge(std::mem::take(&mut word), line, &mut previous);
            }
            if !prose || c != ' ' {
                previous = None;
            }
        }
        if !word.is_empty() {
            judge(word, line, &mut previous);
        }
    }
    found
}

/// A doc reference's path and the member it names, when its text is a `doc_reference`.
fn reference_path(text: &str) -> Option<(ast::Path, Option<String>)> {
    if text.contains(' ') {
        return None;
    }
    let lexed = lex(format!("{text}\n").as_bytes());
    if !lexed.diagnostics.is_empty() {
        return None;
    }
    let tokens: Vec<_> = lexed
        .tokens
        .iter()
        .filter(|token| !token.kind.is_virtual())
        .collect();
    let source = text.as_bytes();
    let mut segments = Vec::new();
    let mut member = None;
    let mut at = 0;
    loop {
        let name = tokens
            .get(at)
            .filter(|token| token.kind == TokenKind::Ident)?;
        segments.push(ast::Name {
            text: name.text(source).to_owned(),
            token: 0,
        });
        match tokens.get(at + 1).map(|token| token.kind) {
            None => break,
            Some(TokenKind::Punct(Punct::ColonColon)) => at += 2,
            Some(TokenKind::Punct(Punct::Dot)) => {
                let name = tokens
                    .get(at + 2)
                    .filter(|token| token.kind == TokenKind::Ident)?;
                member = Some(name.text(source).to_owned());
                if tokens.len() != at + 3 {
                    return None;
                }
                break;
            }
            Some(_) => return None,
        }
    }
    Some((ast::Path { segments }, member))
}

/// What is wrong with a doc reference: the rule it breaks and the fixes that rule names.
type Problem = (Code, Vec<Fix>);

/// The doc comments of an item a variant directory holds, by what each documents, with the path
/// of the item's file and its first line.
type HeldDocs<'f> = (&'f str, u32, BTreeMap<String, String>);

impl Checker<'_> {
    /// The public docs of an item consumers see, in source order: its own, keyed empty, and
    /// those of the members entering its interface form, keyed by name, an invariant line by its
    /// position among those; each with its doc text and first line.
    /// Reports an item whose public docs its module's API snapshot binds, with the same doc
    /// text, to another hash than its current interface hash, once, on the first such doc,
    /// listing each and the command confirming them.
    pub fn check_doc_binding(&self, item: ItemId) {
        let docs = self.public_docs(item);
        if docs.is_empty() {
            return;
        }
        let data = self.db.item(item);
        let package = self.db.package(self.db.package_of(item)).source;
        let Some(path) = self.canonical_path(item) else {
            return;
        };
        let Some(entry) = package
            .machine
            .snapshot(&self.db.module(data.module).name)
            .and_then(|snapshot| snapshot.entry(&path))
        else {
            return;
        };
        let hashes = self.hashes(item);
        let interface = hashes.interface.unwrap_or(hashes.item);
        let stale: Vec<&(DocKey, String, u32)> = docs
            .iter()
            .filter(|(key, text, _)| {
                entry
                    .docs
                    .iter()
                    .any(|doc| doc.key == *key && doc.text == *text && doc.bound != interface)
            })
            .collect();
        let Some((_, _, line)) = stale.first() else {
            return;
        };
        let mut because: Vec<Reason> = stale
            .iter()
            .map(|(key, _, _)| {
                Reason::note(&match key {
                    DocKey::Own => "doc".to_owned(),
                    DocKey::Member(name) => format!("doc {name}"),
                    DocKey::Invariant(position) => format!("doc invariant {position}"),
                })
            })
            .collect();
        because.push(Reason::note(&format!(
            "nan api update --confirm-docs {path}"
        )));
        self.report_diagnostic(
            item,
            Diagnostic {
                because,
                ..Diagnostic::at(Code::DocStale, "", *line)
            },
        );
    }

    pub fn public_docs(&self, item: ItemId) -> Vec<(DocKey, String, u32)> {
        let data = self.db.item(item);
        let syntax = data.syntax();
        let Some(tree) = &syntax.items[data.index].tree else {
            return Vec::new();
        };
        let interface: Vec<String> = self
            .interface_text(item, None)
            .iter()
            .map(|line| line.trim().to_owned())
            .collect();
        let mut invariants = 0;
        let mut docs = Vec::new();
        for doc in doc_comments(syntax, tree, data.ast.visibility) {
            let invariant = doc.member.starts_with("invariant ");
            let key = if doc.member.is_empty() {
                DocKey::Own
            } else if invariant {
                if !interface.contains(&doc.member) {
                    continue;
                }
                invariants += 1;
                DocKey::Invariant(invariants)
            } else {
                DocKey::Member(doc.member.clone())
            };
            if doc.visibility != Visibility::Pub {
                continue;
            }
            let texts: Vec<&str> = doc
                .lines
                .iter()
                .map(|&token| line_text(syntax, token).1)
                .collect();
            let line = syntax.tokens[doc.lines[0]].line;
            docs.push((key, texts.join("\n"), line));
        }
        docs
    }

    /// The rules of an item's doc comments and those of its members.
    pub fn check_docs(&self, item: ItemId) {
        let data = self.db.item(item);
        let syntax = data.syntax();
        let Some(tree) = &syntax.items[data.index].tree else {
            return;
        };
        for doc in doc_comments(syntax, tree, data.ast.visibility) {
            let lines: Vec<DocLine> = doc
                .lines
                .iter()
                .map(|&token| {
                    let (start, text) = line_text(syntax, token);
                    (syntax.tokens[token].line, start, text)
                })
                .collect();
            let report = |line: u32, span: Option<(u32, u32)>, (code, fixes): Problem| {
                self.report_diagnostic(
                    item,
                    Diagnostic {
                        span,
                        fixes,
                        ..Diagnostic::at(code, "", line)
                    },
                );
            };
            self.check_doc_text(
                &data.file.path,
                Some(data.module),
                Some(doc.visibility),
                &lines,
                &report,
            );
        }
    }

    /// The rules of the summaries of a package's files the target selects, the summary of
    /// `package.nan` read in its root module.
    pub fn check_summaries(&self, package: PackageId) {
        let data = self.db.package(package);
        let mut files = vec![(&data.source.files[0], data.root)];
        for &module in &data.modules {
            files.extend(
                self.db
                    .module(module)
                    .files
                    .iter()
                    .map(|file| (*file, Some(module))),
            );
        }
        for (file, module) in files {
            let syntax = &file.syntax;
            let Some(summary) = syntax.summary else {
                continue;
            };
            let (start, text) = line_text(syntax, summary);
            let lines = [(syntax.tokens[summary].line, start, text)];
            let report = |line: u32, span: Option<(u32, u32)>, (code, fixes): Problem| {
                self.found.borrow_mut().push(Diagnostic {
                    span,
                    fixes,
                    ..Diagnostic::at(code, &file.path, line)
                });
            };
            self.check_doc_text(&file.path, module, None, &lines, &report);
        }
    }

    /// The rules of one doc comment or file summary of a file: its code block, its digest claims
    /// and, read in its module when it has one, its doc references, the visibility of what it
    /// documents given for a doc comment.
    fn check_doc_text(
        &self,
        path: &str,
        module: Option<ModuleId>,
        documented: Option<Visibility>,
        lines: &[DocLine],
        report: &dyn Fn(u32, Option<(u32, u32)>, Problem),
    ) {
        if let Some(line) = code_block(lines) {
            report(line, None, (Code::DocCodeBlock, Vec::new()));
        }
        for line in claim_lines(lines) {
            report(line, None, (Code::DocDigestClaim, Vec::new()));
        }
        let Some(module) = module else {
            return;
        };
        for &(line, start, text) in lines {
            for (offset, written) in references(text) {
                let at = start + offset as u32;
                let span = (at, at + written.len() as u32);
                let problem = self.db.on_line(line, || {
                    self.reference_problem(path, module, documented, written, span)
                });
                if let Some(problem) = problem {
                    report(line, Some(span), problem);
                }
            }
        }
    }

    /// The rule a doc reference written in a module breaks: naming no live item or member
    /// visible there, with the fixes writing each candidate near it and a rename's new path, or,
    /// in the doc comment of something of visibility `documented`, naming something narrower.
    fn reference_problem(
        &self,
        path: &str,
        module: ModuleId,
        documented: Option<Visibility>,
        written: &str,
        (start, end): (u32, u32),
    ) -> Option<Problem> {
        let fix = |start: u32, end: u32, text: &str| {
            let replacement = Replacement {
                start,
                end,
                text: text.to_owned(),
            };
            Fix(vec![(path.to_owned(), replacement)])
        };
        let Some((reference, member)) = reference_path(written) else {
            return Some((Code::DocUnresolvedReference, Vec::new()));
        };
        let member_start = end - member.as_ref().map_or(0, |member| member.len() as u32);
        let path_end = member.as_ref().map_or(end, |_| member_start - 1);
        let named = match self.db.resolve(module, &reference) {
            Ok(Named::Item(item)) if self.db.visible(module, item) => Some(item),
            Ok(Named::Scalar(_) | Named::Unit | Named::Never | Named::Dyn | Named::Variant(..)) => {
                None
            }
            Err(PathError { code: None, .. }) => return None,
            _ => {
                let mut fixes: Vec<Fix> = self
                    .renamed(module, &reference, member.as_deref())
                    .into_iter()
                    .map(|renamed| fix(start, end, &renamed))
                    .collect();
                let mut candidates = self.item_candidates(module);
                candidates.extend(super::resolve::core_vocabulary().map(str::to_owned));
                let written_path = &written[..(path_end - start) as usize];
                fixes.extend(
                    super::unknown::near(written_path, candidates)
                        .iter()
                        .map(|candidate| fix(start, path_end, candidate)),
                );
                return Some((Code::DocUnresolvedReference, fixes));
            }
        };
        let key = match (named, &reference.segments[..]) {
            (Some(item), _) => match self.db.ast(item).kind {
                ItemKind::Struct(_) | ItemKind::Enum(_) | ItemKind::Layout(_) => {
                    Some(TypeKey::Item(item))
                }
                _ => None,
            },
            (None, [name]) => self.db.owner_key(
                module,
                &ast::Path {
                    segments: vec![name.clone()],
                },
            ),
            (None, _) => None,
        };
        let visibility = match &member {
            None => named.map_or(Visibility::Pub, |item| self.db.ast(item).visibility),
            Some(member) => match self.member_visibility(module, named, key, member) {
                Some(visibility) => visibility,
                None => {
                    let mut fixes: Vec<Fix> = self
                        .renamed(module, &reference, Some(member))
                        .into_iter()
                        .map(|renamed| fix(start, end, &renamed))
                        .collect();
                    if let Some(key) = key {
                        let candidates = self.member_candidates(module, &self.key_ty(key));
                        fixes.extend(
                            super::unknown::near(member, candidates)
                                .iter()
                                .map(|candidate| fix(member_start, end, candidate)),
                        );
                    }
                    return Some((Code::DocUnresolvedReference, fixes));
                }
            },
        };
        let narrower =
            documented.is_some_and(|documented| openness(visibility) < openness(documented));
        narrower.then(|| (Code::DocPrivateReference, Vec::new()))
    }

    /// The visibility of the member `member` of what a doc reference's path names, when it is
    /// live and visible from a module: a field's the narrower of its own and its type's, a
    /// function's its own, and another member's its item's.
    fn member_visibility(
        &self,
        module: ModuleId,
        named: Option<ItemId>,
        key: Option<TypeKey>,
        member: &str,
    ) -> Option<Visibility> {
        let functions = key.map_or(&[][..], |key| self.db.members(key, member));
        if let Some(function) = functions
            .iter()
            .find(|function| self.db.visible(module, **function))
        {
            return Some(self.db.ast(*function).visibility);
        }
        let item = named?;
        let own = self.db.ast(item).visibility;
        match &self.sig(item).kind {
            SigKind::Struct { fields } | SigKind::Layout { fields, .. } => {
                let field = fields.iter().find(|field| field.name == member)?;
                self.db
                    .field_visible(module, item, field.visibility)
                    .then(|| narrower(field.visibility, own))
            }
            SigKind::Enum { variants } => variants
                .iter()
                .any(|variant| variant.name == member)
                .then_some(own),
            SigKind::Interface { methods } => methods
                .iter()
                .any(|method| method.name == member)
                .then_some(own),
            _ => None,
        }
    }

    /// The new path of a rename tombstone whose old path a doc reference written in a module
    /// spells.
    fn renamed(&self, module: ModuleId, path: &ast::Path, member: Option<&str>) -> Option<String> {
        let form = match member {
            Some(member) => {
                let owner = match self.db.owner_key(module, path) {
                    Some(key) => self.db.type_path(key),
                    None => self.db.spelled(module, path)?,
                };
                format!("{owner}.{member}")
            }
            None => self.db.spelled(module, path)?,
        };
        self.db.retired.get(&form)?.iter().find_map(|&tombstone| {
            let ItemKind::Retired(retired) = &self.db.ast(tombstone).kind else {
                return None;
            };
            match &retired.new {
                RetiredNew::Rewrite {
                    expr,
                    preserves: true,
                } if retired.head.is_none()
                    && matches!(expr.kind, ExprKind::Path { .. } | ExprKind::Field { .. }) =>
                {
                    Some(self.written(tombstone, expr.token, expr.last))
                }
                _ => None,
            }
        })
    }

    /// The module summaries of the variants of a package's modules, and the doc comments of the
    /// items each variant must hold and of their members, as the first variant in byte order of
    /// path holding them writes them: each difference reported once per module or item, in the
    /// first variant that differs.
    pub fn check_variant_docs(&self, package: PackageId) {
        let source = self.db.package(package).source;
        for (index, module) in source.modules.iter().enumerate() {
            if module.dirs.iter().all(|dir| dir.selector.is_none()) {
                continue;
            }
            let mut dirs: Vec<&str> = module.dirs.iter().map(|dir| dir.path.as_str()).collect();
            dirs.sort();
            let summaries: Vec<(&str, u32, &str)> = dirs
                .iter()
                .filter_map(|dir| {
                    let path = format!("{dir}/{}.nan", module.name);
                    let file = source.files.iter().find(|file| file.path == path)?;
                    let summary = file.syntax.summary?;
                    let line = file.syntax.tokens[summary].line;
                    Some((file.path.as_str(), line, line_text(&file.syntax, summary).1))
                })
                .collect();
            if let Some((first, rest)) = summaries.split_first()
                && let Some((path, line, _)) = rest.iter().find(|(_, _, text)| *text != first.2)
            {
                self.report_file(path, *line, Code::DocVariantMismatch);
            }
            let mut docs: BTreeMap<String, Vec<HeldDocs>> = BTreeMap::new();
            for file in source
                .files
                .iter()
                .filter(|file| file.module == Some(index))
            {
                let syntax = &file.syntax;
                for (span, item) in syntax.items.iter().zip(file.lowered()) {
                    let (Some(tree), Some(item)) = (&span.tree, item) else {
                        continue;
                    };
                    let Some(identity) = super::package::exported_identity(item) else {
                        continue;
                    };
                    let comments = doc_comments(syntax, tree, item.visibility)
                        .into_iter()
                        .map(|doc| {
                            let text: Vec<&str> = doc
                                .lines
                                .iter()
                                .map(|&token| line_text(syntax, token).1)
                                .collect();
                            (doc.member, text.join("\n"))
                        })
                        .collect();
                    docs.entry(identity).or_default().push((
                        file.path.as_str(),
                        *syntax.item_lines(span).start(),
                        comments,
                    ));
                }
            }
            for mut holders in docs.into_values() {
                holders.sort_by(|left, right| (left.0, left.1).cmp(&(right.0, right.1)));
                if let Some((first, rest)) = holders.split_first()
                    && let Some((path, line, _)) =
                        rest.iter().find(|(_, _, comments)| *comments != first.2)
                {
                    self.report_file(path, *line, Code::DocVariantMismatch);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
