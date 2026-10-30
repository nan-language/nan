//! Reading items from their trees, and the rules of item heads, labels and clause regions.

use super::file::{FileDiagnostic, SourceFile};
use super::grammar::{Keyword, Punct, Rule};
use super::token::TokenKind;
use super::tree::{Child, NodeId, Tree};
use crate::diagnostic::{Code, Replacement};

/// A view of one item's tree over its file.
#[derive(Clone, Copy)]
pub struct Item<'a> {
    pub file: &'a SourceFile,
    pub tree: &'a Tree,
}

impl<'a> Item<'a> {
    fn kind_of(&self, token: u32) -> TokenKind {
        self.file.tokens[token as usize].kind
    }

    fn text(&self, token: u32) -> &'a str {
        self.file.text(token as usize)
    }

    fn line(&self, token: u32) -> u32 {
        self.file.tokens[token as usize].line
    }

    fn start(&self) -> Option<NodeId> {
        self.tree.find(self.tree.root(), Rule::ItemStart)
    }

    /// The node of the item's kind: the rule after `item_kind`, past the `const` it may share.
    pub fn kind(&self) -> Option<(Rule, NodeId)> {
        let kind = self.tree.find(self.start()?, Rule::ItemKind)?;
        let node = self.tree.child_nodes(kind).next()?;
        Some((self.tree.rule(node), node))
    }

    pub fn visibility(&self) -> Option<u32> {
        let visibility = self.tree.find(self.start()?, Rule::Visibility)?;
        self.tree.child_tokens(visibility).next()
    }

    /// The modifiers before the kind keyword: the rule each is read by, and its token.
    fn modifiers(&self) -> Vec<(Rule, u32)> {
        let Some(start) = self.start() else {
            return Vec::new();
        };
        self.tree
            .child_nodes(start)
            .filter(|node| self.tree.rule(*node) == Rule::ItemModifier)
            .filter_map(|modifier| {
                let inner = self.tree.child_nodes(modifier).next()?;
                Some((self.tree.rule(inner), self.tree.span(inner)?.0))
            })
            .collect()
    }

    /// The text of the tokens directly under `node`, without spaces.
    fn tokens_text(&self, node: NodeId) -> String {
        self.tree
            .child_tokens(node)
            .map(|token| self.text(token))
            .collect()
    }

    /// The bytes from a token's start to the next token's on its line, or to its own end.
    fn with_space_after(&self, token: u32) -> (u32, u32) {
        let at = &self.file.tokens[token as usize];
        let next = &self.file.tokens[self.file.next(token as usize + 1)];
        let end = if next.line == at.line && !next.kind.is_virtual() {
            next.start
        } else {
            at.end
        };
        (at.start, end)
    }

    /// The bytes from the end of the token before `first` to the end of `last`.
    fn with_space_before(&self, first: u32, last: u32) -> (u32, u32) {
        let before = self
            .file
            .previous(first as usize)
            .map_or(self.file.tokens[first as usize].start, |before| {
                self.file.tokens[before].end
            });
        (before, self.file.tokens[last as usize].end)
    }

    /// A diagnostic on the tokens `first..=last`, with the fix replacing the bytes `range` with
    /// `text`.
    fn fixed(
        &self,
        (first, last): (u32, u32),
        code: Code,
        (start, end): (u32, u32),
        text: &str,
    ) -> FileDiagnostic {
        let at = &self.file.tokens[first as usize];
        FileDiagnostic {
            span: Some((at.start, self.file.tokens[last as usize].end)),
            fixes: vec![vec![Replacement {
                start,
                end,
                text: text.to_owned(),
            }]],
            ..FileDiagnostic::new(at.line, code)
        }
    }

    /// The name the item's head declares, as the head spells it.
    pub fn name(&self) -> Option<String> {
        let (rule, node) = self.kind()?;
        match rule {
            Rule::FnItem => Some(
                self.tokens_text(
                    self.tree
                        .find(self.tree.find(node, Rule::FnHead)?, Rule::ItemLabel)?,
                ),
            ),
            Rule::StructItem
            | Rule::EnumItem
            | Rule::InterfaceItem
            | Rule::LayoutItem
            | Rule::RegistersItem
            | Rule::TestItem
            | Rule::PackageItem
            | Rule::RegistryItem
            | Rule::ConstItem
            | Rule::ContributeItem => {
                let ident = self
                    .tree
                    .child_tokens(node)
                    .find(|token| self.kind_of(*token) == TokenKind::Ident)?;
                Some(self.text(ident).to_owned())
            }
            _ => None,
        }
    }

    /// Whether a byte of the item lies in its declaration rather than in a body: for a function,
    /// before the `{` or `=` opening its body; for a test, never; for other kinds, always.
    pub fn declares(&self, position: u32) -> bool {
        match self.kind() {
            Some((Rule::FnItem, node)) => {
                let opener = self.tree.find(node, Rule::FnEnd).and_then(|end| {
                    self.tree.child_tokens(end).find(|&token| {
                        matches!(
                            self.kind_of(token),
                            TokenKind::Punct(Punct::LBrace | Punct::Eq)
                        )
                    })
                });
                opener.is_none_or(|opener| position < self.file.tokens[opener as usize].start)
            }
            Some((Rule::TestItem, _)) => false,
            _ => true,
        }
    }

    /// The last token of a function item's head.
    pub fn head_end(&self) -> Option<u32> {
        let (rule, node) = self.kind()?;
        if rule != Rule::FnItem {
            return None;
        }
        Some(self.tree.span(self.tree.find(node, Rule::FnHead)?)?.1)
    }

    /// The line of the item's first token after its doc comment.
    pub fn head_line(&self) -> Option<u32> {
        let start = self.start()?;
        let head = self
            .tree
            .children(start)
            .iter()
            .find_map(|child| match child {
                Child::Node(node) => self.tree.span(*node).map(|(first, _)| first),
                Child::Token(_) => None,
            })?;
        Some(self.line(head))
    }
}

/// The group of a clause in the order of the clause region.
pub fn clause_group(tree: &Tree, clause: NodeId) -> u8 {
    let rule = tree.rule(clause);
    let marked = tree
        .child_nodes(clause)
        .next()
        .map(|marked| tree.rule(marked))
        .filter(|_| rule == Rule::TodoClause);
    match (rule, marked) {
        (Rule::RequiresClause, _) | (_, Some(Rule::RequiresClause)) => 0,
        (Rule::EnsuresClause, _) | (_, Some(Rule::EnsuresClause)) => 1,
        (Rule::ExampleClause, _) | (_, Some(Rule::ExampleClause)) => 2,
        (Rule::UndoClause, _) => 3,
        (Rule::DecreasesClause, _) => 4,
        (Rule::TodoClause, _) => 5,
        _ => 6,
    }
}

/// Checks the rules of item shape that the parse leaves to the parsed item.
pub fn check(item: Item) -> Vec<FileDiagnostic> {
    let mut found = Vec::new();
    let mut fixed = Vec::new();
    let mut report = |line: u32, code: Code| found.push(FileDiagnostic::new(line, code));
    let Some((kind, node)) = item.kind() else {
        return found;
    };
    let tree = item.tree;

    let visibility = item.visibility();
    let no_visibility = matches!(
        kind,
        Rule::ImplItem
            | Rule::ConstAssertItem
            | Rule::ContributeItem
            | Rule::TestItem
            | Rule::RetiredItem
            | Rule::PackageItem
    );
    if let Some(token) = visibility.filter(|_| no_visibility) {
        let deleted = item.with_space_after(token);
        fixed.push(item.fixed((token, token), Code::ItemVisibilityNotAllowed, deleted, ""));
    }

    let fn_end = (kind == Rule::FnItem)
        .then(|| tree.find(node, Rule::FnEnd))
        .flatten();
    let fn_end_first = fn_end.and_then(|end| tree.children(end).first().copied());
    let transparent = fn_end_first.is_some_and(|child| {
        matches!(child, Child::Token(token) if item.kind_of(token) == TokenKind::Punct(Punct::Eq))
    });
    let mut type_modifiers: Vec<(String, u32)> = Vec::new();
    let mut fn_modifier: Option<String> = None;
    for (rule, token) in item.modifiers() {
        let word = item.text(token).to_owned();
        match rule {
            Rule::TypeModifier if !matches!(kind, Rule::StructItem | Rule::EnumItem) => {
                report(item.line(token), Code::ItemModifierMisplaced)
            }
            Rule::TypeModifier => {
                let openness = |word: &str| word == "closed" || word == "open";
                if type_modifiers.iter().any(|(other, _)| *other == word) {
                    let deleted = item.with_space_after(token);
                    fixed.push(item.fixed((token, token), Code::ItemModifierConflict, deleted, ""));
                } else if type_modifiers
                    .iter()
                    .any(|(other, _)| openness(other) && openness(&word))
                {
                    report(item.line(token), Code::ItemModifierConflict);
                }
                type_modifiers.push((word, token));
            }
            Rule::FnModifier if kind != Rule::FnItem => {
                report(item.line(token), Code::ItemModifierMisplaced)
            }
            Rule::FnModifier if transparent && word == "foreign" => {
                report(item.line(token), Code::ItemForeignForm)
            }
            Rule::FnModifier if transparent && word != "proj" => {
                report(item.line(token), Code::ItemModifierMisplaced)
            }
            Rule::FnModifier if fn_modifier == Some(word.clone()) => {
                let deleted = item.with_space_after(token);
                fixed.push(item.fixed((token, token), Code::ItemModifierConflict, deleted, ""));
            }
            Rule::FnModifier if fn_modifier.is_some() => {
                report(item.line(token), Code::ItemModifierConflict)
            }
            Rule::FnModifier => fn_modifier = Some(word),
            _ => {}
        }
    }
    if matches!(kind, Rule::StructItem | Rule::EnumItem) {
        let openness = type_modifiers
            .iter()
            .find(|(word, _)| word == "closed" || word == "open");
        let public =
            visibility.is_some_and(|token| item.kind_of(token) == TokenKind::Keyword(Keyword::Pub));
        match (public, openness) {
            (true, None) => report(
                item.head_line().expect("a parsed item has a head"),
                Code::ItemOpennessMissing,
            ),
            (false, Some((_, token))) => {
                let deleted = item.with_space_after(*token);
                fixed.push(item.fixed((*token, *token), Code::ItemOpennessNotPub, deleted, ""));
            }
            _ => {}
        }
    }

    match kind {
        Rule::FnItem => check_fn(item, node, &mut report, &mut fixed),
        Rule::InterfaceItem => {
            for member in tree
                .child_nodes(node)
                .filter(|member| tree.rule(*member) == Rule::InterfaceMember)
            {
                check_head(item, member, true, &mut report, &mut fixed);
                let method_end = tree.find(member, Rule::MethodEnd);
                if let Some(region) = method_end.and_then(|end| tree.find(end, Rule::ClauseRegion))
                {
                    check_clause_order(item, region, &mut fixed);
                }
                let body = method_end.and_then(|end| tree.find(end, Rule::MethodBody));
                let name = tree
                    .child_tokens(member)
                    .find(|token| item.kind_of(*token) == TokenKind::Ident);
                if let (Some(body), Some(name)) = (body, name) {
                    let label = tree
                        .child_tokens(body)
                        .find(|token| item.kind_of(*token) == TokenKind::Ident);
                    if label.is_none_or(|label| item.text(label) != item.text(name)) {
                        let closer = tree
                            .child_tokens(body)
                            .find(|token| item.kind_of(*token) == TokenKind::Punct(Punct::RBrace))
                            .expect("a body closes");
                        let name = item.text(name);
                        fixed.push(match label {
                            Some(label) => {
                                let at = &item.file.tokens[label as usize];
                                let written = (at.start, at.end);
                                item.fixed((label, label), Code::ItemLabelMismatch, written, name)
                            }
                            None => {
                                let end = item.file.tokens[closer as usize].end;
                                let written = format!(" {name}");
                                item.fixed(
                                    (closer, closer),
                                    Code::ItemLabelMismatch,
                                    (end, end),
                                    &written,
                                )
                            }
                        });
                    }
                }
            }
        }
        Rule::TestItem => {
            if let Some(test_params) = tree.find(node, Rule::TestParams) {
                for param in
                    tree.list_elements(tree.find(test_params, Rule::ParamList), Rule::Param)
                {
                    report_self(item, param, &mut report);
                }
            }
        }
        _ => {}
    }

    let closing = match kind {
        Rule::FnItem => fn_end.and_then(|end| tree.find(end, Rule::FnBody)),
        Rule::TestItem => tree.find(node, Rule::FnBody),
        _ => Some(node),
    };
    let label = closing.and_then(|closing| {
        tree.child_nodes(closing)
            .filter(|child| tree.rule(*child) == Rule::ItemLabel)
            .last()
    });
    if let (Some(label), Some(name)) = (label, item.name())
        && item.tokens_text(label) != name
    {
        let (first, last) = tree.span(label).expect("a label holds tokens");
        let written = (
            item.file.tokens[first as usize].start,
            item.file.tokens[last as usize].end,
        );
        fixed.push(item.fixed((first, last), Code::ItemLabelMismatch, written, &name));
    }
    found.extend(fixed);
    found
}

fn check_fn(
    item: Item,
    node: NodeId,
    report: &mut dyn FnMut(u32, Code),
    fixed: &mut Vec<FileDiagnostic>,
) {
    let tree = item.tree;
    let Some(head) = tree.find(node, Rule::FnHead) else {
        return;
    };
    let Some(end) = tree.find(node, Rule::FnEnd) else {
        return;
    };
    check_head(item, head, false, report, fixed);
    let foreign = item
        .modifiers()
        .iter()
        .any(|(_, token)| item.kind_of(*token) == TokenKind::Keyword(Keyword::Foreign));
    let first = tree.children(end)[0];
    let first_line = match first {
        Child::Token(token) => item.line(token),
        Child::Node(node) => item.line(tree.span(node).expect("a clause holds tokens").0),
    };
    let convention =
        matches!(first, Child::Node(node) if tree.rule(node) == Rule::ConventionClause);
    let transparent =
        matches!(first, Child::Token(token) if item.kind_of(token) == TokenKind::Punct(Punct::Eq));
    let region = tree.find(end, Rule::ClauseRegion);
    if foreign && !convention && !transparent {
        let line = region.map_or(first_line, |region| {
            item.line(tree.span(region).expect("clauses hold tokens").0)
        });
        report(line, Code::ItemForeignForm);
    } else if convention && !foreign {
        report(first_line, Code::ItemForeignForm);
    }
    if let Some(limits) = tree.find(head, Rule::LimitsClause).filter(|_| transparent) {
        let (first, last) = tree.span(limits).expect("limits hold tokens");
        let deleted = item.with_space_before(first, last);
        fixed.push(item.fixed((first, last), Code::ItemTransparentLimits, deleted, ""));
    }
    if let Some(region) = region {
        check_clause_order(item, region, fixed);
    }
}

/// Checks a function head or interface method head: its result and its `self` parameters.
fn check_head(
    item: Item,
    head: NodeId,
    method: bool,
    report: &mut dyn FnMut(u32, Code),
    fixed: &mut Vec<FileDiagnostic>,
) {
    let tree = item.tree;
    if let Some(result) = tree.find(head, Rule::FnResult) {
        let unit = tree.find(result, Rule::TypeExpr).is_some_and(|ty| {
            let mut children = tree.children(ty).iter();
            let path = match children.next() {
                Some(Child::Node(path)) if tree.rule(*path) == Rule::Path => *path,
                _ => return false,
            };
            children.next().is_none() && item.tokens_text(path) == "Unit"
        });
        if unit {
            let (first, last) = tree.span(result).expect("a result holds tokens");
            let deleted = item.with_space_before(first, last);
            fixed.push(item.fixed((first, last), Code::ItemUnitResult, deleted, ""));
        }
    }
    let type_member = method
        || tree.find(head, Rule::ItemLabel).is_some_and(|label| {
            tree.child_tokens(label)
                .any(|token| item.kind_of(token) == TokenKind::Punct(Punct::Dot))
        });
    for (index, param) in tree
        .list_elements(tree.find(head, Rule::ParamList), Rule::Param)
        .into_iter()
        .enumerate()
    {
        if !(type_member && index == 0) {
            report_self(item, param, report);
        }
    }
}

fn report_self(item: Item, param: NodeId, report: &mut dyn FnMut(u32, Code)) {
    let self_token = item
        .tree
        .child_tokens(param)
        .find(|token| item.kind_of(*token) == TokenKind::Keyword(Keyword::SelfValue));
    if let Some(token) = self_token {
        report(item.line(token), Code::ItemSelfMisplaced);
    }
}

/// Reports each clause line that follows a line of a later group, with the fix moving it to just
/// before the first line of a later group.
fn check_clause_order(item: Item, region: NodeId, fixed: &mut Vec<FileDiagnostic>) {
    let tree = item.tree;
    let lines: Vec<(u8, u32, u32)> = tree
        .child_nodes(region)
        .filter_map(|line| {
            let clause = tree.child_nodes(line).next()?;
            let (first, last) = tree.span(clause)?;
            Some((clause_group(tree, clause), first, last))
        })
        .collect();
    let physical = |token: u32| &item.file.lines[item.line(token) as usize - 1];
    let mut latest = 0;
    for &(group, first, last) in &lines {
        if group < latest {
            let later = lines
                .iter()
                .find(|(other, _, _)| *other > group)
                .expect("a later group precedes the line");
            let start = physical(first).start;
            let end = item
                .file
                .lines
                .get(item.line(last) as usize)
                .map_or(item.file.source.len() as u32, |next| next.start);
            let moved = String::from_utf8_lossy(&item.file.source[start as usize..end as usize]);
            let target = physical(later.1).start;
            let at = &item.file.tokens[first as usize];
            fixed.push(FileDiagnostic {
                span: Some((at.start, item.file.tokens[last as usize].end)),
                fixes: vec![vec![
                    Replacement {
                        start: target,
                        end: target,
                        text: moved.into_owned(),
                    },
                    Replacement {
                        start,
                        end,
                        text: String::new(),
                    },
                ]],
                ..FileDiagnostic::new(at.line, Code::ItemClauseOrder)
            });
        }
        latest = latest.max(group);
    }
}

#[cfg(test)]
mod tests;
