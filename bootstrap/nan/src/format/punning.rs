//! Punned labels: in a call, an argument after the first written `name: name`, its value a binding
//! or parameter in scope alone or after a mode, loses its label.

use std::collections::{HashMap, HashSet};

use crate::syntax::file::SourceFile;
use crate::syntax::grammar::{Punct, Rule};
use crate::syntax::parser::is_pascal;
use crate::syntax::token::TokenKind;
use crate::syntax::tree::{Child, NodeId, Tree};

/// The label and colon tokens of the item's arguments that `nan fmt` puns.
pub fn punned(file: &SourceFile, tree: &Tree) -> HashSet<u32> {
    let mut walk = Walk {
        file,
        tree,
        scopes: Vec::new(),
        bound: HashMap::new(),
        punned: HashSet::new(),
    };
    let mut steps = vec![Step::Node(tree.root())];
    while let Some(step) = steps.pop() {
        match step {
            Step::Node(node) => {
                let plan = walk.node(node);
                steps.extend(plan.into_iter().rev());
            }
            Step::Push(names) => {
                walk.scopes.push(Vec::new());
                walk.bind(names);
            }
            Step::Pop => walk.pop(),
            Step::Bind(statement) => {
                let names = walk.bound(statement);
                walk.bind(names);
            }
        }
    }
    walk.punned
}

/// A step of the walk, taken once the steps before it are done.
enum Step {
    Node(NodeId),
    /// Opens a scope binding the names.
    Push(Vec<String>),
    Pop,
    /// Adds the names a statement binds to the innermost scope.
    Bind(NodeId),
}

/// The steps walking `nodes` in a scope binding `names`.
fn scoped(names: Vec<String>, nodes: Vec<NodeId>) -> Vec<Step> {
    let mut steps = vec![Step::Push(names)];
    steps.extend(nodes.into_iter().map(Step::Node));
    steps.push(Step::Pop);
    steps
}

struct Walk<'a> {
    file: &'a SourceFile,
    tree: &'a Tree,
    /// The names each open scope binds, innermost last.
    scopes: Vec<Vec<String>>,
    /// How many times the open scopes bind each name.
    bound: HashMap<String, usize>,
    punned: HashSet<u32>,
}

impl Walk<'_> {
    fn text(&self, token: u32) -> &str {
        self.file.text(token as usize)
    }

    fn is_ident(&self, token: u32) -> bool {
        self.file.tokens[token as usize].kind == TokenKind::Ident
    }

    fn bind(&mut self, names: Vec<String>) {
        for name in &names {
            *self.bound.entry(name.clone()).or_default() += 1;
        }
        self.scopes.last_mut().expect("an open scope").extend(names);
    }

    fn pop(&mut self) {
        for name in self.scopes.pop().expect("an open scope") {
            let count = self.bound.get_mut(&name).expect("a bound name");
            *count -= 1;
            if *count == 0 {
                self.bound.remove(&name);
            }
        }
    }

    /// The steps walking a node, after recording the punned labels of the calls it suffixes.
    fn node(&mut self, node: NodeId) -> Vec<Step> {
        let tree = self.tree;
        let children: Vec<NodeId> = tree.child_nodes(node).collect();
        match tree.rule(node) {
            Rule::FnItem | Rule::TestItem | Rule::InterfaceMember | Rule::ClosureExpr => {
                scoped(self.parameters(node), children)
            }
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
                let names = tree
                    .find(node, Rule::Pattern)
                    .map(|pattern| self.pattern_names(pattern))
                    .unwrap_or_default();
                let (body, rest): (Vec<NodeId>, Vec<NodeId>) = children
                    .into_iter()
                    .filter(|child| tree.rule(*child) != Rule::Pattern)
                    .partition(|child| tree.rule(*child) == Rule::LoopBlock);
                let mut steps: Vec<Step> = rest.into_iter().map(Step::Node).collect();
                steps.extend(scoped(names, body));
                steps
            }
            Rule::MatchArm => {
                let names = children
                    .iter()
                    .filter(|child| tree.rule(**child) == Rule::Pattern)
                    .flat_map(|pattern| self.pattern_names(*pattern))
                    .collect();
                scoped(names, children)
            }
            Rule::QuantifierExpr => {
                let variable = tree
                    .child_tokens(node)
                    .nth(1)
                    .map(|token| self.text(token).to_owned());
                match children[..] {
                    [domain, condition] => {
                        let mut steps = vec![Step::Node(domain)];
                        steps.extend(scoped(variable.into_iter().collect(), vec![condition]));
                        steps
                    }
                    _ => Vec::new(),
                }
            }
            Rule::PostfixExpr => {
                self.pun_calls(node);
                children.into_iter().map(Step::Node).collect()
            }
            _ => children.into_iter().map(Step::Node).collect(),
        }
    }

    /// The names of the parameters a function, test, interface method or closure declares.
    fn parameters(&self, node: NodeId) -> Vec<String> {
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
            _ => tree.list_elements(tree.find(node, Rule::FnTypeParamList), Rule::FnTypeParam),
        };
        params
            .into_iter()
            .filter_map(|param| tree.child_tokens(param).find(|token| self.is_ident(*token)))
            .map(|token| self.text(token).to_owned())
            .collect()
    }

    /// The names a `let` or `var` statement binds.
    fn bound(&self, statement: NodeId) -> Vec<String> {
        let tree = self.tree;
        match tree.rule(statement) {
            Rule::LetStmt => tree
                .find(statement, Rule::Pattern)
                .map(|pattern| self.pattern_names(pattern))
                .unwrap_or_default(),
            Rule::VarStmt => tree
                .child_tokens(statement)
                .filter(|token| self.is_ident(*token))
                .map(|token| self.text(token).to_owned())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The bindings a pattern introduces: a lone lowercase name, and those of its field patterns.
    fn pattern_names(&self, pattern: NodeId) -> Vec<String> {
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
                names.push(self.text(*token).to_owned());
                continue;
            }
            let fields =
                tree.list_elements(tree.find(named, Rule::FieldPatternList), Rule::FieldPattern);
            pending.extend(fields.into_iter().map(|field| {
                tree.child_nodes(field)
                    .last()
                    .expect("a field pattern holds a pattern")
            }));
        }
        names
    }

    /// Records the punned labels of each call suffix of a postfix expression. A suffix whose
    /// callee's last name is PascalCase is a construction, whose labels are never punned.
    fn pun_calls(&mut self, postfix: NodeId) {
        let tree = self.tree;
        let mut callee = tree
            .child_nodes(postfix)
            .next()
            .and_then(|primary| self.path_end(primary));
        for suffix in tree.child_nodes(postfix).skip(1) {
            let tokens: Vec<u32> = tree.child_tokens(suffix).collect();
            match self.file.tokens[tokens[0] as usize].kind {
                TokenKind::Punct(Punct::Dot) => callee = tokens.get(1).copied(),
                TokenKind::Punct(Punct::LParen) => {
                    let construction = callee.is_some_and(|name| is_pascal(self.text(name)));
                    if !construction {
                        self.pun_arguments(
                            tree.list_elements(tree.find(suffix, Rule::ArgList), Rule::Arg),
                        );
                    }
                    callee = None;
                }
                _ => callee = None,
            }
        }
    }

    /// The last identifier of a primary expression that is a path.
    fn path_end(&self, primary: NodeId) -> Option<u32> {
        let path = self
            .tree
            .find(self.tree.find(primary, Rule::PathExpr)?, Rule::Path)?;
        self.tree
            .child_tokens(path)
            .filter(|token| self.is_ident(*token))
            .last()
    }

    fn pun_arguments(&mut self, args: Vec<NodeId>) {
        for arg in args.into_iter().skip(1) {
            let exprs: Vec<NodeId> = self.tree.child_nodes(arg).collect();
            let [label, value] = exprs[..] else { continue };
            let (Some(label), Some(value)) = (self.lone_name(label), self.marked_name(value))
            else {
                continue;
            };
            if self.text(label) == self.text(value) && self.bound.contains_key(self.text(label)) {
                let colon = self
                    .tree
                    .child_tokens(arg)
                    .next()
                    .expect("a labelled argument has a colon");
                self.punned.extend([label, colon]);
            }
        }
    }

    /// The identifier an expression is, when it is a lone one-segment path.
    fn lone_name(&self, node: NodeId) -> Option<u32> {
        match self.tree.children(node) {
            [Child::Node(only)] => self.lone_name(*only),
            [Child::Token(token)]
                if self.tree.rule(node) == Rule::Path && self.is_ident(*token) =>
            {
                Some(*token)
            }
            _ => None,
        }
    }

    /// The identifier an expression is, alone or after a mode.
    fn marked_name(&self, node: NodeId) -> Option<u32> {
        match self.tree.children(node) {
            [Child::Node(only)] => self.marked_name(*only),
            [Child::Node(mode), Child::Node(rest)] if self.tree.rule(*mode) == Rule::Mode => {
                self.lone_name(*rest)
            }
            _ => self.lone_name(node),
        }
    }
}
