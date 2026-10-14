//! The canonical format of one parsed item: canonical order, punned labels and trailing commas,
//! line breaks, depth and spacing.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

use super::{group_number, opener_spaced, punning};
use crate::syntax::file::{ItemSyntax, SourceFile};
use crate::syntax::grammar::{Keyword, Punct, Rule};
use crate::syntax::items::clause_group;
use crate::syntax::token::TokenKind;
use crate::syntax::tree::{Child, NodeId, Tree};

/// A token of the formatted item.
struct Tok {
    /// Its source token; none for a trailing comma the format adds.
    source: Option<u32>,
    kind: TokenKind,
    text: String,
    /// Whether the source breaks the line before it.
    newline: bool,
    /// Whether it is the last token of its line in the source, comments aside.
    ends_line: bool,
    /// Comment lines before it, their openers spaced.
    comments: Vec<String>,
    /// The comment ending its line.
    trailing: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ListKind {
    /// The parameters of a function item, an interface method or a test.
    Params,
    /// Any other breakable list.
    Breakable,
}

/// A breakable list.
struct List {
    kind: ListKind,
    opener: u32,
    closer: u32,
    commas: Vec<u32>,
    /// The last token of each element.
    ends: Vec<u32>,
}

/// A run of elements the canonical order sorts by key, the tokens between them staying in place.
struct Sorted {
    elements: Vec<(u32, u32)>,
    keys: Vec<(u8, String)>,
}

/// What the item's tree fixes about the format of its tokens.
#[derive(Default)]
struct Facts {
    lists: Vec<List>,
    /// For each token of a comma-separated list that lies on one line, the outermost such list
    /// holding it, numbered.
    one_line: HashMap<u32, usize>,
    unary: HashSet<u32>,
    target_dash: HashSet<u32>,
    test_opener: HashSet<u32>,
    trusted: HashSet<u32>,
    /// The `}` of each braced item's closing line.
    closing: HashSet<u32>,
    /// The first token of each clause line, and the first token of the head it belongs to.
    clause_heads: HashMap<u32, u32>,
    /// The `{` opening a body after a clause region, and the first token of its head.
    body_heads: HashMap<u32, u32>,
    sorted: Vec<Sorted>,
}

/// The canonical lines of an item the parse did not abandon.
pub fn lines(file: &SourceFile, item: &ItemSyntax) -> Vec<String> {
    let tree = item.tree.as_ref().expect("a parsed item has a tree");
    let facts = Facts::collect(file, tree);
    let mut toks = tokens(file, item, &facts, &punning::punned(file, tree));
    let roles = trailing_commas(&mut toks, &facts);
    Layout {
        toks: &toks,
        facts: &facts,
        roles: &roles,
    }
    .render()
}

impl Facts {
    fn collect(file: &SourceFile, tree: &Tree) -> Facts {
        let mut facts = Facts::default();
        let mut one_line = Vec::new();
        let kind = |token: u32| file.tokens[token as usize].kind;
        let text = |token: u32| file.text(token as usize);
        let punct = |punct: Punct| move |token: &u32| kind(*token) == TokenKind::Punct(punct);
        let head = tree.find(tree.root(), Rule::ItemStart).and_then(|start| {
            tree.children(start).iter().find_map(|child| match child {
                Child::Node(node) => tree.span(*node).map(|(first, _)| first),
                Child::Token(_) => None,
            })
        });
        for node in tree.descendants(tree.root()) {
            let tokens: Vec<u32> = tree.child_tokens(node).collect();
            let rule = tree.rule(node);
            match rule {
                Rule::FnHead | Rule::InterfaceMember | Rule::TestParams | Rule::RetiredHead => {
                    let kind = if rule == Rule::RetiredHead {
                        ListKind::Breakable
                    } else {
                        ListKind::Params
                    };
                    facts.paren_list(file, tree, node, Rule::ParamList, Rule::Param, kind);
                    if rule == Rule::TestParams {
                        facts
                            .test_opener
                            .extend(tokens.iter().copied().filter(punct(Punct::LParen)));
                    }
                }
                Rule::FnTypeHead | Rule::ClosureExpr => {
                    facts.paren_list(
                        file,
                        tree,
                        node,
                        Rule::FnTypeParamList,
                        Rule::FnTypeParam,
                        ListKind::Breakable,
                    );
                }
                Rule::PostfixSuffix if tokens.first().is_some_and(punct(Punct::LParen)) => {
                    facts.paren_list(
                        file,
                        tree,
                        node,
                        Rule::ArgList,
                        Rule::Arg,
                        ListKind::Breakable,
                    );
                }
                Rule::VariantPayload => facts.paren_list(
                    file,
                    tree,
                    node,
                    Rule::PayloadList,
                    Rule::PayloadField,
                    ListKind::Breakable,
                ),
                Rule::NamedPattern if tokens.iter().any(punct(Punct::LParen)) => {
                    facts.paren_list(
                        file,
                        tree,
                        node,
                        Rule::FieldPatternList,
                        Rule::FieldPattern,
                        ListKind::Breakable,
                    );
                }
                Rule::GenericParams
                | Rule::GenericArgs
                | Rule::ImplParams
                | Rule::CaptureList
                | Rule::DerivesClause
                | Rule::LimitsClause
                | Rule::UniqueList
                | Rule::TargetList
                | Rule::NameList => one_line.extend(tree.span(node)),
                Rule::WhenExpr => {
                    let accesses: Vec<NodeId> = tree
                        .child_nodes(node)
                        .filter(|child| tree.rule(*child) == Rule::WhenAccess)
                        .collect();
                    let first = tokens.first().copied();
                    let last = accesses
                        .last()
                        .and_then(|access| tree.span(*access))
                        .map(|(_, last)| last);
                    one_line.extend(first.zip(last));
                }
                _ => {}
            }
            match rule {
                Rule::PrefixExpr | Rule::PatternLiteral | Rule::GenericArg => {
                    let first = tree.children(node).first();
                    if let Some(Child::Token(token)) = first
                        && matches!(kind(*token), TokenKind::Punct(Punct::Minus | Punct::Tilde))
                    {
                        facts.unary.insert(*token);
                    }
                }
                Rule::Target => facts
                    .target_dash
                    .extend(tokens.iter().copied().filter(punct(Punct::Minus))),
                Rule::TrustedExpr => {
                    facts.trusted.extend(tokens.iter().copied().filter(|token| {
                        matches!(
                            kind(*token),
                            TokenKind::Punct(Punct::LBrace | Punct::RBrace)
                        )
                    }));
                }
                Rule::StructItem
                | Rule::EnumItem
                | Rule::InterfaceItem
                | Rule::LayoutItem
                | Rule::RegistersItem
                | Rule::PackageItem
                | Rule::FnBody => {
                    facts
                        .closing
                        .extend(tokens.iter().copied().rfind(punct(Punct::RBrace)));
                }
                _ => {}
            }
            match rule {
                Rule::FnEnd => {
                    let opener = tokens.iter().copied().find(punct(Punct::LBrace));
                    if let (Some(region), Some(head), Some(opener)) =
                        (tree.find(node, Rule::ClauseRegion), head, opener)
                    {
                        facts.clauses(tree, region, head);
                        facts.body_heads.insert(opener, head);
                    }
                }
                Rule::InterfaceMember => {
                    let member_head = tokens.iter().copied().find(|token| {
                        matches!(
                            kind(*token),
                            TokenKind::Keyword(Keyword::Proj | Keyword::Fn)
                        )
                    });
                    let end = tree.find(node, Rule::MethodEnd);
                    if let (Some(region), Some(member_head)) = (
                        end.and_then(|end| tree.find(end, Rule::ClauseRegion)),
                        member_head,
                    ) {
                        facts.clauses(tree, region, member_head);
                        let body = end.and_then(|end| tree.find(end, Rule::MethodBody));
                        if let Some(opener) = body.and_then(|body| tree.child_tokens(body).next()) {
                            facts.body_heads.insert(opener, member_head);
                        }
                    }
                }
                Rule::ItemStart => {
                    let modifiers: Vec<NodeId> = tree
                        .child_nodes(node)
                        .filter(|child| tree.rule(*child) == Rule::ItemModifier)
                        .collect();
                    let all_type = modifiers
                        .iter()
                        .all(|modifier| tree.find(*modifier, Rule::TypeModifier).is_some());
                    if all_type && modifiers.len() > 1 {
                        let rank = |modifier: &NodeId| match text(
                            tree.span(*modifier).expect("a modifier").0,
                        ) {
                            "closed" | "open" => 0,
                            "linear" => 1,
                            _ => 2,
                        };
                        facts.sort(tree, &modifiers, |modifier| (rank(modifier), String::new()));
                    }
                }
                Rule::PackageItem => {
                    let directives: Vec<NodeId> = tree
                        .child_nodes(node)
                        .filter(|child| tree.rule(*child) == Rule::Directive)
                        .collect();
                    facts.sort(tree, &directives, |directive| {
                        let inner = tree.child_nodes(*directive).next().expect("a directive");
                        let idents: Vec<String> = tree
                            .child_tokens(inner)
                            .filter(|token| kind(*token) == TokenKind::Ident)
                            .skip(1)
                            .map(|token| text(token).to_owned())
                            .collect();
                        match tree.rule(inner) {
                            Rule::LanguageDirective => (0, String::new()),
                            Rule::TargetsDirective => (1, String::new()),
                            Rule::EntryDirective => (2, String::new()),
                            Rule::DepDirective => (3, idents.join(" ")),
                            Rule::AllowDirective => {
                                let authority =
                                    tree.find(inner, Rule::Authority).expect("an authority");
                                (4, joined(file, tree, authority))
                            }
                            _ => (5, idents.join(" ")),
                        }
                    });
                }
                Rule::TargetList => {
                    let targets: Vec<NodeId> = tree.child_nodes(node).collect();
                    facts.sort(tree, &targets, |target| (0, joined(file, tree, *target)));
                }
                Rule::NameList | Rule::DerivesClause => {
                    let names: Vec<u32> = tokens
                        .iter()
                        .copied()
                        .filter(|token| kind(*token) == TokenKind::Ident)
                        .collect();
                    let order = [
                        "Copy", "Eq", "Ord", "Hash", "Default", "Render", "Encode", "Generate",
                        "Send", "Inert", "AnyBits",
                    ];
                    let key = |token: u32| match rule {
                        Rule::NameList => (0, text(token).to_owned()),
                        _ => (
                            order
                                .iter()
                                .position(|name| *name == text(token))
                                .unwrap_or(order.len()) as u8,
                            String::new(),
                        ),
                    };
                    facts.sorted.push(Sorted {
                        elements: names.iter().map(|token| (*token, *token)).collect(),
                        keys: names.iter().map(|token| key(*token)).collect(),
                    });
                }
                Rule::LimitsClause => {
                    let limits: Vec<NodeId> = tree.child_nodes(node).collect();
                    let order = ["pure", "notrap", "terminates", "noalloc", "stack"];
                    facts.sort(tree, &limits, |limit| {
                        let word = text(tree.span(*limit).expect("a limit").0);
                        (
                            order
                                .iter()
                                .position(|name| *name == word)
                                .unwrap_or(order.len()) as u8,
                            String::new(),
                        )
                    });
                }
                _ => {}
            }
        }
        one_line.sort_by_key(|&(first, last)| (first, Reverse(last)));
        let mut reach = None;
        for (index, (first, last)) in one_line.into_iter().enumerate() {
            if reach.is_some_and(|reach| first <= reach) {
                continue;
            }
            reach = Some(last);
            facts
                .one_line
                .extend((first..=last).map(|token| (token, index)));
        }
        facts
    }

    /// Records a parenthesised list whose elements are `element` nodes of a self-nesting `list`.
    fn paren_list(
        &mut self,
        file: &SourceFile,
        tree: &Tree,
        node: NodeId,
        list: Rule,
        element: Rule,
        kind: ListKind,
    ) {
        let tokens: Vec<u32> = tree.child_tokens(node).collect();
        let is = |token: &&u32, punct: Punct| {
            file.tokens[**token as usize].kind == TokenKind::Punct(punct)
        };
        let opener = *tokens
            .iter()
            .find(|token| is(token, Punct::LParen))
            .expect("a parenthesised list");
        let closer = *tokens
            .iter()
            .find(|token| is(token, Punct::RParen))
            .expect("a parenthesised list");
        let mut commas = Vec::new();
        let mut ends = Vec::new();
        let mut next = tree.find(node, list);
        while let Some(current) = next {
            commas.extend(tree.child_tokens(current).filter(|token| {
                file.tokens[*token as usize].kind == TokenKind::Punct(Punct::Comma)
            }));
            if let Some(last) = tree
                .find(current, element)
                .and_then(|element| tree.span(element))
            {
                ends.push(last.1);
            }
            next = tree.find(current, list);
        }
        self.lists.push(List {
            kind,
            opener,
            closer,
            commas,
            ends,
        });
    }

    /// Records the clause lines of a clause region and the head they belong to, and sorts them by
    /// group.
    fn clauses(&mut self, tree: &Tree, region: NodeId, head: u32) {
        let lines: Vec<NodeId> = tree.child_nodes(region).collect();
        for line in &lines {
            self.clause_heads
                .insert(tree.span(*line).expect("a clause holds tokens").0, head);
        }
        self.sort(tree, &lines, |line| {
            let clause = tree.child_nodes(*line).next().expect("a clause");
            (clause_group(tree, clause), String::new())
        });
    }

    fn sort(&mut self, tree: &Tree, nodes: &[NodeId], key: impl Fn(&NodeId) -> (u8, String)) {
        if nodes.len() < 2 {
            return;
        }
        self.sorted.push(Sorted {
            elements: nodes
                .iter()
                .map(|node| tree.span(*node).expect("an element holds tokens"))
                .collect(),
            keys: nodes.iter().map(key).collect(),
        });
    }
}

/// The text of a node's tokens, without spaces.
fn joined(file: &SourceFile, tree: &Tree, node: NodeId) -> String {
    let (first, last) = tree.span(node).expect("a node holding tokens");
    (first..=last)
        .map(|token| file.text(token as usize))
        .collect()
}

/// The item's tokens in canonical order, without punned labels, each with its comments.
fn tokens(file: &SourceFile, item: &ItemSyntax, facts: &Facts, punned: &HashSet<u32>) -> Vec<Tok> {
    let mut order: Vec<u32> = (item.first..item.end)
        .filter(|&token| !file.comment[token] && file.tokens[token].kind != TokenKind::Col0)
        .map(|token| token as u32)
        .collect();
    for sorted in &facts.sorted {
        reorder(&mut order, sorted);
    }
    let mut comments: HashMap<u32, Vec<String>> = HashMap::new();
    let mut trailing: HashMap<u32, String> = HashMap::new();
    let lines = file.item_lines(item);
    let mut pending = Vec::new();
    let mut index = item.first;
    for line in lines {
        let mut last_on_line = None;
        while index < item.end && file.tokens[index].line == line {
            let token = &file.tokens[index];
            if file.comment[index] && matches!(token.kind, TokenKind::Doc | TokenKind::FileDoc) {
                pending.push(opener_spaced(file.text(index)));
            } else if !file.comment[index] && !token.kind.is_virtual() {
                comments
                    .entry(index as u32)
                    .or_default()
                    .append(&mut pending);
                last_on_line = Some(index as u32);
            }
            index += 1;
        }
        let physical = &file.lines[line as usize - 1];
        if let Some(offset) = physical.comment {
            let text = opener_spaced(
                std::str::from_utf8(&file.source[offset as usize..physical.end as usize])
                    .expect("well-formed UTF-8"),
            );
            match last_on_line {
                Some(token) => {
                    trailing.insert(token, text);
                }
                None => pending.push(text),
            }
        }
    }
    let mut toks: Vec<Tok> = Vec::new();
    let mut carried: Vec<String> = Vec::new();
    for token in order {
        let source = &file.tokens[token as usize];
        let mut own = comments.remove(&token).unwrap_or_default();
        if punned.contains(&token) {
            carried.append(&mut own);
            continue;
        }
        carried.append(&mut own);
        let real = |at: &usize| !file.comment[*at] && !file.tokens[*at].kind.is_virtual();
        let next = (token as usize + 1..file.tokens.len()).find(real);
        let previous = (0..token as usize).rev().find(real);
        toks.push(Tok {
            source: Some(token),
            ends_line: next.is_some_and(|next| file.tokens[next].line > source.line),
            kind: source.kind,
            text: match source.kind {
                TokenKind::Doc | TokenKind::FileDoc => opener_spaced(file.text(token as usize)),
                TokenKind::Int | TokenKind::Float => group_number(file.text(token as usize)),
                TokenKind::Nl => String::new(),
                _ => file.text(token as usize).to_owned(),
            },
            newline: !source.kind.is_virtual()
                && previous.is_none_or(|previous| file.tokens[previous].line < source.line),
            comments: std::mem::take(&mut carried),
            trailing: trailing.remove(&token),
        });
    }
    toks
}

/// Rearranges the tokens of a sorted run: each element moves to the slot of its rank, the tokens
/// between elements keep their place.
fn reorder(order: &mut [u32], sorted: &Sorted) {
    let spans: Vec<Range<usize>> = sorted
        .elements
        .iter()
        .map(|&(first, last)| {
            let start = order
                .iter()
                .position(|token| *token >= first && *token <= last)
                .expect("an element holds tokens");
            let end = order
                .iter()
                .rposition(|token| *token >= first && *token <= last)
                .expect("an element holds tokens")
                + 1;
            start..end
        })
        .collect();
    let mut ranked: Vec<usize> = (0..spans.len()).collect();
    ranked.sort_by(|left, right| sorted.keys[*left].cmp(&sorted.keys[*right]));
    let start = spans[0].start;
    let end = spans[spans.len() - 1].end;
    let mut segment = Vec::with_capacity(end - start);
    for (slot, element) in ranked.iter().enumerate() {
        segment.extend_from_slice(&order[spans[*element].clone()]);
        if let Some(next) = spans.get(slot + 1) {
            segment.extend_from_slice(&order[spans[slot].end..next.start]);
        }
    }
    order[start..end].copy_from_slice(&segment);
}

/// A token's part in a list of the item.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Opener(usize),
    Comma(usize),
    Closer(usize),
}

/// The roles of the tokens in lists, and whether each breakable list is broken.
struct Roles {
    roles: Vec<Option<Role>>,
    broken: Vec<bool>,
}

/// Decides which breakable lists are broken, gives a broken list its trailing comma and takes a
/// flat list's away.
fn trailing_commas(toks: &mut Vec<Tok>, facts: &Facts) -> Roles {
    let positions = |toks: &[Tok]| -> HashMap<u32, usize> {
        toks.iter()
            .enumerate()
            .filter_map(|(index, tok)| Some((tok.source?, index)))
            .collect()
    };
    let at = positions(toks);
    let mut commented_before = vec![0; toks.len() + 1];
    for (index, tok) in toks.iter().enumerate() {
        let commented = !tok.comments.is_empty() || tok.trailing.is_some();
        commented_before[index + 1] = commented_before[index] + usize::from(commented);
    }
    let mut broken = vec![false; facts.lists.len()];
    let mut added = vec![false; toks.len()];
    let mut dropped = vec![false; toks.len()];
    for (list, facts_list) in facts.lists.iter().enumerate() {
        let (opener, closer) = (at[&facts_list.opener], at[&facts_list.closer]);
        let commas: Vec<usize> = facts_list
            .commas
            .iter()
            .filter_map(|comma| at.get(comma).copied())
            .collect();
        let commented = toks[opener].trailing.is_some()
            || commented_before[closer] > commented_before[opener + 1]
            || !toks[closer].comments.is_empty();
        broken[list] = match facts_list.kind {
            ListKind::Params => facts_list.ends.len() > 4 || commented,
            _ => toks[opener].ends_line || commas.iter().any(|comma| toks[*comma].ends_line),
        } && !facts_list.ends.is_empty();
        match (
            broken[list],
            commas.iter().find(|comma| **comma + 1 == closer),
        ) {
            (true, None) => added[at[facts_list.ends.last().expect("a last element")]] = true,
            (false, Some(comma)) => dropped[*comma] = true,
            _ => {}
        }
    }
    let mut edited = Vec::with_capacity(toks.len());
    let mut carried = Vec::new();
    for (index, mut tok) in std::mem::take(toks).into_iter().enumerate() {
        if dropped[index] {
            carried = tok.comments;
            continue;
        }
        tok.comments.splice(0..0, std::mem::take(&mut carried));
        edited.push(tok);
        if added[index] {
            edited.push(Tok {
                source: None,
                kind: TokenKind::Punct(Punct::Comma),
                text: ",".to_owned(),
                newline: false,
                ends_line: false,
                comments: Vec::new(),
                trailing: None,
            });
        }
    }
    *toks = edited;
    let at = positions(toks);
    let mut roles = vec![None; toks.len()];
    for (list, facts_list) in facts.lists.iter().enumerate() {
        let (opener, closer) = (at[&facts_list.opener], at[&facts_list.closer]);
        roles[opener] = Some(Role::Opener(list));
        for comma in facts_list.commas.iter().filter_map(|comma| at.get(comma)) {
            roles[*comma] = Some(Role::Comma(list));
        }
        roles[closer] = Some(Role::Closer(list));
        if toks[closer - 1].source.is_none() {
            roles[closer - 1] = Some(Role::Comma(list));
        }
    }
    Roles { roles, broken }
}

struct Layout<'a> {
    toks: &'a [Tok],
    facts: &'a Facts,
    roles: &'a Roles,
}

/// An opening delimiter the layout is inside.
struct Open {
    anchor: usize,
    list: Option<usize>,
    /// The depth of the line holding the first token of the current unit of its context.
    unit: Option<usize>,
}

impl Layout<'_> {
    fn is(&self, at: usize, punct: Punct) -> bool {
        self.toks[at].kind == TokenKind::Punct(punct)
    }

    fn source_in(&self, at: usize, set: &HashSet<u32>) -> bool {
        self.toks[at]
            .source
            .is_some_and(|source| set.contains(&source))
    }

    /// Whether the line breaks before the token at `at`, not the item's first.
    fn breaks_before(&self, at: usize) -> bool {
        let (previous, current) = (&self.toks[at - 1], &self.toks[at]);
        let role = |at: usize| self.roles.roles[at];
        if previous.kind == TokenKind::Nl
            || previous.trailing.is_some()
            || !current.comments.is_empty()
        {
            return true;
        }
        let one_line = |tok: &Tok| {
            tok.source
                .and_then(|source| self.facts.one_line.get(&source))
        };
        if one_line(previous).is_some() && one_line(previous) == one_line(current) {
            return false;
        }
        if let Some(Role::Closer(list)) = role(at) {
            return self.roles.broken[list];
        }
        if let Some(Role::Opener(list) | Role::Comma(list)) = role(at - 1) {
            return self.roles.broken[list];
        }
        if self.is(at - 1, Punct::LBrace) && !self.source_in(at - 1, &self.facts.trusted) {
            return true;
        }
        if self.is(at, Punct::RBrace) && !self.source_in(at, &self.facts.trusted) {
            return true;
        }
        current.newline
    }

    /// Whether one space separates two tokens on a line.
    fn spaced(&self, left: usize, right: usize) -> bool {
        let (a, b) = (&self.toks[left], &self.toks[right]);
        let tight = |tok: &Tok| {
            matches!(
                tok.kind,
                TokenKind::Punct(
                    Punct::Dot | Punct::ColonColon | Punct::DotDotLt | Punct::DotDotEq
                )
            )
        };
        if matches!(a.kind, TokenKind::Punct(Punct::LParen | Punct::LBracket)) {
            return false;
        }
        if matches!(
            b.kind,
            TokenKind::Punct(Punct::RParen | Punct::RBracket | Punct::Comma | Punct::Colon)
        ) {
            return false;
        }
        if tight(a)
            || tight(b)
            || self.source_in(left, &self.facts.target_dash)
            || self.source_in(right, &self.facts.target_dash)
        {
            return false;
        }
        if self.source_in(left, &self.facts.unary) {
            return self.is(left, Punct::Minus) && self.is(right, Punct::Minus);
        }
        let calls = matches!(
            a.kind,
            TokenKind::Ident
                | TokenKind::Keyword(Keyword::SelfValue | Keyword::Fn | Keyword::Dbg)
                | TokenKind::Punct(Punct::RParen | Punct::RBracket)
        );
        if matches!(b.kind, TokenKind::Punct(Punct::LParen | Punct::LBracket)) && calls {
            return self.source_in(right, &self.facts.test_opener);
        }
        true
    }

    fn render(&self) -> Vec<String> {
        let toks = self.toks;
        let mut lines: Vec<Vec<usize>> = Vec::new();
        for at in (0..toks.len()).filter(|at| toks[*at].kind != TokenKind::Nl) {
            if lines.is_empty() || self.breaks_before(at) {
                lines.push(vec![at]);
            } else {
                lines.last_mut().expect("a line").push(at);
            }
        }
        let mut out = Vec::new();
        let mut contexts = Contexts {
            open: Vec::new(),
            file_unit: None,
        };
        let mut depth_of: HashMap<u32, usize> = HashMap::new();
        let mut previous_depth = 0;
        for (index, line) in lines.iter().enumerate() {
            let first = line[0];
            let head_depth = |head: &u32| depth_of.get(head).copied().unwrap_or(0);
            let source = toks[first].source.unwrap_or(u32::MAX);
            let closes = matches!(
                toks[first].kind,
                TokenKind::Punct(Punct::RParen | Punct::RBracket | Punct::RBrace)
            );
            let base = contexts.open.last().map_or(0, |open| open.anchor + 1);
            let depth = if closes {
                let open = contexts
                    .open
                    .last()
                    .expect("a closer closes an open delimiter");
                let params = open.list.is_some_and(|list| {
                    self.facts.lists[list].kind == ListKind::Params && self.roles.broken[list]
                });
                let top_level = open.anchor == 0 && !self.facts.closing.contains(&source);
                open.anchor + usize::from(params || top_level)
            } else if let Some(head) = self.facts.clause_heads.get(&source) {
                head_depth(head) + 1
            } else if let Some(head) = self.facts.body_heads.get(&source) {
                head_depth(head)
            } else if toks[first].kind == TokenKind::TextLine
                && first > 0
                && toks[first - 1].kind == TokenKind::TextLine
            {
                previous_depth
            } else {
                contexts.unit().map_or(base, |unit| unit + 1)
            };
            let comment_depth = match self.facts.body_heads.get(&source) {
                _ if closes => base,
                Some(head) => head_depth(head) + 1,
                None => depth,
            };
            for comment in &toks[first].comments {
                out.push(format!("{}{comment}", "    ".repeat(comment_depth)));
            }
            let mut text = "    ".repeat(depth);
            for (position, &at) in line.iter().enumerate() {
                if position > 0 && self.spaced(line[position - 1], at) {
                    text.push(' ');
                }
                text.push_str(&toks[at].text);
            }
            if let Some(comment) = &toks[*line.last().expect("a line holds a token")].trailing {
                text.push(' ');
                text.push_str(comment);
            }
            out.push(text);
            depth_of.extend(
                line.iter()
                    .filter_map(|&at| toks[at].source)
                    .map(|source| (source, depth)),
            );
            previous_depth = depth;
            let end = lines.get(index + 1).map_or(toks.len(), |next| next[0]);
            for (tok, role) in toks[first..end].iter().zip(&self.roles.roles[first..end]) {
                contexts.track(tok, *role, depth);
            }
        }
        out
    }
}

/// The delimiters open at a point of the layout, and the current unit of each context.
struct Contexts {
    open: Vec<Open>,
    /// The depth of the line holding the first token of the file context's current unit.
    file_unit: Option<usize>,
}

impl Contexts {
    fn unit(&self) -> Option<usize> {
        self.open.last().map_or(self.file_unit, |open| open.unit)
    }

    fn unit_mut(&mut self) -> &mut Option<usize> {
        match self.open.last_mut() {
            Some(open) => &mut open.unit,
            None => &mut self.file_unit,
        }
    }

    /// Moves past a token on a line of `depth`: a separator ends its context's unit, an opener
    /// opens a context anchored at the unit holding it, and any other token starts a unit when
    /// none is current.
    fn track(&mut self, tok: &Tok, role: Option<Role>, depth: usize) {
        match tok.kind {
            TokenKind::Punct(Punct::LParen | Punct::LBracket | Punct::LBrace) => {
                let anchor = *self.unit_mut().get_or_insert(depth);
                let list = match role {
                    Some(Role::Opener(list)) => Some(list),
                    _ => None,
                };
                self.open.push(Open {
                    anchor,
                    list,
                    unit: None,
                });
            }
            TokenKind::Punct(Punct::RParen | Punct::RBracket | Punct::RBrace) => {
                self.open.pop();
            }
            TokenKind::Nl | TokenKind::Punct(Punct::Comma) => *self.unit_mut() = None,
            _ => {
                self.unit_mut().get_or_insert(depth);
            }
        }
    }
}
