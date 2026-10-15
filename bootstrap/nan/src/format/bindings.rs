//! The bindings of an item: the names its parameters, patterns, statements, closures, quantifiers
//! and loops introduce, and the tokens naming each.

use std::collections::{HashMap, HashSet};

use crate::syntax::file::SourceFile;
use crate::syntax::grammar::{Keyword, Rule};
use crate::syntax::token::TokenKind;
use crate::syntax::tree::{Child, NodeId, Tree};

pub struct Bindings {
    /// The token of each binding's first introduction, by each token introducing or naming it.
    pub of: HashMap<u32, u32>,
    /// The first introductions of the item's own parameters.
    pub parameters: HashSet<u32>,
}

/// The bindings of an item's tree.
pub fn bindings(file: &SourceFile, tree: &Tree) -> Bindings {
    let mut walk = Walk {
        file,
        tree,
        scopes: Vec::new(),
        labels: Vec::new(),
        found: Bindings {
            of: HashMap::new(),
            parameters: HashSet::new(),
        },
    };
    let mut steps = vec![Step::Node(tree.root())];
    while let Some(step) = steps.pop() {
        match step {
            Step::Node(node) => {
                let plan = walk.node(node);
                steps.extend(plan.into_iter().rev());
            }
            Step::Push(tokens) => {
                walk.scopes.push(Vec::new());
                walk.introduce(tokens);
            }
            Step::Pop => {
                walk.scopes.pop();
            }
            Step::Bind(statement) => {
                let tokens = walk.bound(statement);
                walk.introduce(tokens);
            }
            Step::Label(token) => {
                walk.found.of.insert(token, token);
                walk.labels.push(token);
            }
            Step::Unlabel => {
                walk.labels.pop();
            }
        }
    }
    walk.found
}

/// A step of the walk, taken once the steps before it are done.
enum Step {
    Node(NodeId),
    /// Opens a scope introducing the names of these tokens.
    Push(Vec<u32>),
    Pop,
    /// Introduces the names a statement binds into the innermost scope.
    Bind(NodeId),
    /// Opens the loop labelled by this token.
    Label(u32),
    Unlabel,
}

/// The steps walking `nodes` in a scope introducing the names of `tokens`.
fn scoped(tokens: Vec<u32>, nodes: Vec<NodeId>) -> Vec<Step> {
    let mut steps = vec![Step::Push(tokens)];
    steps.extend(nodes.into_iter().map(Step::Node));
    steps.push(Step::Pop);
    steps
}

struct Walk<'a> {
    file: &'a SourceFile,
    tree: &'a Tree,
    /// The names each open scope introduces with their first introductions, innermost last.
    scopes: Vec<Vec<(&'a str, u32)>>,
    /// The labels of the loops open, innermost last.
    labels: Vec<u32>,
    found: Bindings,
}

impl<'a> Walk<'a> {
    fn text(&self, token: u32) -> &'a str {
        self.file.text(token as usize)
    }

    fn is_ident(&self, token: u32) -> bool {
        self.file.tokens[token as usize].kind == TokenKind::Ident
    }

    /// Introduces names into the innermost scope; a name it already introduces, as the
    /// alternatives of a match arm do, stays one binding.
    fn introduce(&mut self, tokens: Vec<u32>) {
        for token in tokens {
            let name = self.text(token);
            let scope = self.scopes.last_mut().expect("an open scope");
            let first = match scope.iter().find(|(bound, _)| *bound == name) {
                Some((_, first)) => *first,
                None => {
                    scope.push((name, token));
                    token
                }
            };
            self.found.of.insert(token, first);
        }
    }

    /// Records a token naming the binding of its name in scope, if any.
    fn name(&mut self, token: u32) {
        let name = self.text(token);
        let first = self
            .scopes
            .iter()
            .rev()
            .find_map(|scope| scope.iter().find(|(bound, _)| *bound == name));
        if let Some((_, first)) = first {
            self.found.of.insert(token, *first);
        }
    }

    /// The steps walking a node.
    fn node(&mut self, node: NodeId) -> Vec<Step> {
        let tree = self.tree;
        let children: Vec<NodeId> = tree.child_nodes(node).collect();
        match tree.rule(node) {
            Rule::FnItem | Rule::TestItem | Rule::InterfaceMember | Rule::RetiredItem => {
                let parameters = self.parameters(node);
                self.found.parameters.extend(parameters.iter().copied());
                scoped(parameters, children)
            }
            Rule::ClosureExpr => scoped(self.parameters(node), children),
            Rule::Block | Rule::LoopBlock | Rule::FnBody | Rule::MethodBody | Rule::TrustedBody => {
                let mut steps = vec![Step::Push(Vec::new())];
                for child in children {
                    steps.push(Step::Node(child));
                    if matches!(tree.rule(child), Rule::Stmt | Rule::TrustedLeadStmt) {
                        steps.extend(tree.child_nodes(child).next().map(Step::Bind));
                    }
                }
                steps.push(Step::Pop);
                steps
            }
            Rule::ForStmt => {
                let tokens = tree
                    .find(node, Rule::Pattern)
                    .map(|pattern| self.pattern_names(pattern))
                    .unwrap_or_default();
                let (body, rest): (Vec<NodeId>, Vec<NodeId>) = children
                    .into_iter()
                    .partition(|child| tree.rule(*child) == Rule::LoopBlock);
                let mut steps: Vec<Step> = rest.into_iter().map(Step::Node).collect();
                steps.extend(scoped(tokens, body));
                steps
            }
            Rule::LoopStmt => {
                let label = tree
                    .child_tokens(node)
                    .filter(|token| self.is_ident(*token))
                    .nth(1);
                let mut steps = Vec::new();
                steps.extend(label.map(Step::Label));
                steps.extend(children.into_iter().map(Step::Node));
                if label.is_some() {
                    steps.push(Step::Unlabel);
                }
                steps
            }
            Rule::JumpExpr => {
                let tokens: Vec<u32> = tree.child_tokens(node).collect();
                let jump = matches!(
                    self.file.tokens[tokens[0] as usize].kind,
                    TokenKind::Keyword(Keyword::Break | Keyword::Continue)
                );
                if jump && let Some(&label) = tokens.get(1) {
                    let named = self
                        .labels
                        .iter()
                        .rev()
                        .find(|open| self.text(**open) == self.text(label));
                    if let Some(open) = named {
                        self.found.of.insert(label, *open);
                    }
                }
                children.into_iter().map(Step::Node).collect()
            }
            Rule::MatchArm => {
                let tokens = children
                    .iter()
                    .filter(|child| tree.rule(**child) == Rule::Pattern)
                    .flat_map(|pattern| self.pattern_names(*pattern))
                    .collect();
                scoped(tokens, children)
            }
            Rule::QuantifierExpr => {
                let variable = tree.child_tokens(node).nth(1);
                match children[..] {
                    [domain, condition] => {
                        let mut steps = vec![Step::Node(domain)];
                        steps.extend(scoped(variable.into_iter().collect(), vec![condition]));
                        steps
                    }
                    _ => Vec::new(),
                }
            }
            Rule::Arg | Rule::FieldPattern if children.len() == 2 => vec![Step::Node(children[1])],
            Rule::Path | Rule::PlaceRoot => {
                if let [Child::Token(token)] = tree.children(node)
                    && self.is_ident(*token)
                {
                    self.name(*token);
                }
                Vec::new()
            }
            _ => children.into_iter().map(Step::Node).collect(),
        }
    }

    /// The name tokens of the parameters a function, test, interface method, tombstone or
    /// closure declares.
    fn parameters(&self, node: NodeId) -> Vec<u32> {
        let tree = self.tree;
        let params = match tree.rule(node) {
            Rule::FnItem => tree.list_elements(
                tree.find(node, Rule::FnHead)
                    .and_then(|head| tree.find(head, Rule::ParamList)),
                Rule::Param,
            ),
            Rule::TestItem => tree.list_elements(
                tree.find(node, Rule::TestParams)
                    .and_then(|params| tree.find(params, Rule::ParamList)),
                Rule::Param,
            ),
            Rule::InterfaceMember => {
                tree.list_elements(tree.find(node, Rule::ParamList), Rule::Param)
            }
            Rule::RetiredItem => tree.list_elements(
                tree.find(node, Rule::RetiredOld)
                    .and_then(|old| tree.find(old, Rule::RetiredHead))
                    .and_then(|head| tree.find(head, Rule::ParamList)),
                Rule::Param,
            ),
            _ => tree.list_elements(tree.find(node, Rule::FnTypeParamList), Rule::FnTypeParam),
        };
        params
            .into_iter()
            .filter_map(|param| tree.child_tokens(param).find(|token| self.is_ident(*token)))
            .collect()
    }

    /// The name tokens a `let` or `var` statement binds.
    fn bound(&self, statement: NodeId) -> Vec<u32> {
        let tree = self.tree;
        match tree.rule(statement) {
            Rule::LetStmt => tree
                .find(statement, Rule::Pattern)
                .map(|pattern| self.pattern_names(pattern))
                .unwrap_or_default(),
            Rule::VarStmt => tree
                .child_tokens(statement)
                .find(|token| self.is_ident(*token))
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The name tokens of the bindings a pattern introduces: a lone lowercase name, and those
    /// of its field patterns.
    fn pattern_names(&self, pattern: NodeId) -> Vec<u32> {
        let tree = self.tree;
        let mut names = Vec::new();
        let mut pending = vec![pattern];
        while let Some(pattern) = pending.pop() {
            let Some(named) = tree.find(pattern, Rule::NamedPattern) else {
                continue;
            };
            if let [Child::Node(path)] = tree.children(named)
                && let [Child::Token(token)] = tree.children(*path)
                && self
                    .text(*token)
                    .starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
            {
                names.push(*token);
                continue;
            }
            let fields =
                tree.list_elements(tree.find(named, Rule::FieldPatternList), Rule::FieldPattern);
            pending.extend(fields.into_iter().rev().map(|field| {
                tree.child_nodes(field)
                    .last()
                    .expect("a field pattern holds a pattern")
            }));
        }
        names
    }
}

#[cfg(test)]
mod tests;
