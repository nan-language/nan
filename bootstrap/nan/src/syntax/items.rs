//! Reading items from their trees, and the rules of item heads, labels and clause regions.

use super::file::{FileDiagnostic, SourceFile};
use super::grammar::{Keyword, Punct, Rule};
use super::token::TokenKind;
use super::tree::{Child, NodeId, Tree};
use crate::diagnostic::Code;

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
        (Rule::DecreasesClause, _) => 3,
        (Rule::TodoClause, _) => 4,
        _ => 5,
    }
}

/// Checks the rules of item shape that the parse leaves to the parsed item.
pub fn check(item: Item) -> Vec<FileDiagnostic> {
    let mut found = Vec::new();
    let mut report = |line: u32, code: Code| {
        found.push(FileDiagnostic {
            line,
            code,
            expected: None,
            found: None,
        })
    };
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
        report(item.line(token), Code::ItemVisibilityNotAllowed);
    }

    let fn_end = (kind == Rule::FnItem)
        .then(|| tree.find(node, Rule::FnEnd))
        .flatten();
    let fn_end_first = fn_end.and_then(|end| tree.children(end).first().copied());
    let transparent = fn_end_first.is_some_and(|child| {
        matches!(child, Child::Token(token) if item.kind_of(token) == TokenKind::Punct(Punct::Eq))
    });
    let mut type_modifiers: Vec<(String, u32)> = Vec::new();
    let mut fn_modifier = false;
    for (rule, token) in item.modifiers() {
        let word = item.text(token).to_owned();
        match rule {
            Rule::TypeModifier if !matches!(kind, Rule::StructItem | Rule::EnumItem) => {
                report(item.line(token), Code::ItemModifierMisplaced)
            }
            Rule::TypeModifier => {
                let openness = |word: &str| word == "closed" || word == "open";
                let conflict = type_modifiers
                    .iter()
                    .any(|(other, _)| *other == word || (openness(other) && openness(&word)));
                if conflict {
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
            Rule::FnModifier if fn_modifier => report(item.line(token), Code::ItemModifierConflict),
            Rule::FnModifier => fn_modifier = true,
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
            (false, Some((_, token))) => report(item.line(*token), Code::ItemOpennessNotPub),
            _ => {}
        }
    }

    match kind {
        Rule::FnItem => check_fn(item, node, &mut report),
        Rule::InterfaceItem => {
            for member in tree
                .child_nodes(node)
                .filter(|member| tree.rule(*member) == Rule::InterfaceMember)
            {
                check_head(item, member, true, &mut report);
                let method_end = tree.find(member, Rule::MethodEnd);
                if let Some(region) = method_end.and_then(|end| tree.find(end, Rule::ClauseRegion))
                {
                    check_clause_order(item, region, &mut report);
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
                            .find(|token| item.kind_of(*token) == TokenKind::Punct(Punct::RBrace));
                        report(
                            item.line(closer.expect("a body closes")),
                            Code::ItemLabelMismatch,
                        );
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
        report(
            item.line(tree.span(label).expect("a label holds tokens").0),
            Code::ItemLabelMismatch,
        );
    }
    found
}

fn check_fn(item: Item, node: NodeId, report: &mut dyn FnMut(u32, Code)) {
    let tree = item.tree;
    let Some(head) = tree.find(node, Rule::FnHead) else {
        return;
    };
    let Some(end) = tree.find(node, Rule::FnEnd) else {
        return;
    };
    check_head(item, head, false, report);
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
        report(
            item.line(tree.span(limits).expect("limits hold tokens").0),
            Code::ItemTransparentLimits,
        );
    }
    if let Some(region) = region {
        check_clause_order(item, region, report);
    }
}

/// Checks a function head or interface method head: its result and its `self` parameters.
fn check_head(item: Item, head: NodeId, method: bool, report: &mut dyn FnMut(u32, Code)) {
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
            report(
                item.line(tree.span(result).expect("a result holds tokens").0),
                Code::ItemUnitResult,
            );
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

/// Reports each clause line that follows a line of a later group.
fn check_clause_order(item: Item, region: NodeId, report: &mut dyn FnMut(u32, Code)) {
    let tree = item.tree;
    let mut latest = 0;
    for line in tree.child_nodes(region) {
        let Some(clause) = tree.child_nodes(line).next() else {
            continue;
        };
        let group = clause_group(tree, clause);
        if group < latest {
            report(
                item.line(tree.span(clause).expect("a clause holds tokens").0),
                Code::ItemClauseOrder,
            );
        }
        latest = latest.max(group);
    }
}

#[cfg(test)]
mod tests;
