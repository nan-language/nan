//! Concrete syntax trees: one node per grammar rule the parse expands, over the file's tokens.

use super::grammar::Rule;

pub type NodeId = u32;
pub type TokenId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Child {
    Node(NodeId),
    Token(TokenId),
}

#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub rule: Rule,
    children: (u32, u32),
}

/// The tree of one item. Nodes refer to the tokens of their file by index.
#[derive(Clone, Debug, Default)]
pub struct Tree {
    nodes: Vec<Node>,
    children: Vec<Child>,
    root: NodeId,
}

impl Tree {
    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn rule(&self, node: NodeId) -> Rule {
        self.nodes[node as usize].rule
    }

    pub fn children(&self, node: NodeId) -> &[Child] {
        let (start, end) = self.nodes[node as usize].children;
        &self.children[start as usize..end as usize]
    }

    pub fn child_nodes(&self, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.children(node).iter().filter_map(|child| match child {
            Child::Node(node) => Some(*node),
            Child::Token(_) => None,
        })
    }

    pub fn child_tokens(&self, node: NodeId) -> impl Iterator<Item = TokenId> + '_ {
        self.children(node).iter().filter_map(|child| match child {
            Child::Token(token) => Some(*token),
            Child::Node(_) => None,
        })
    }

    /// The first child node of `node` expanding `rule`.
    pub fn find(&self, node: NodeId, rule: Rule) -> Option<NodeId> {
        self.child_nodes(node)
            .find(|child| self.rule(*child) == rule)
    }

    /// The element nodes of a list rule that nests itself after each comma, such as `param_list`.
    pub fn list_elements(&self, list: Option<NodeId>, element: Rule) -> Vec<NodeId> {
        let mut elements = Vec::new();
        let mut list = list;
        while let Some(node) = list {
            elements.extend(self.find(node, element));
            list = self
                .child_nodes(node)
                .find(|child| self.rule(*child) == self.rule(node));
        }
        elements
    }

    /// Every node under `node`, itself included, in source order.
    pub fn descendants(&self, node: NodeId) -> Vec<NodeId> {
        let mut found = Vec::new();
        let mut pending = vec![node];
        while let Some(node) = pending.pop() {
            found.push(node);
            let children = pending.len();
            pending.extend(self.child_nodes(node));
            pending[children..].reverse();
        }
        found
    }

    /// The first and last token under `node`, if it holds any.
    pub fn span(&self, node: NodeId) -> Option<(TokenId, TokenId)> {
        let first = self.first_token(node)?;
        let last = self.last_token(node)?;
        Some((first, last))
    }

    fn first_token(&self, node: NodeId) -> Option<TokenId> {
        let mut pending = vec![Child::Node(node)];
        while let Some(child) = pending.pop() {
            match child {
                Child::Token(token) => return Some(token),
                Child::Node(node) => pending.extend(self.children(node).iter().rev()),
            }
        }
        None
    }

    fn last_token(&self, node: NodeId) -> Option<TokenId> {
        let mut pending = vec![Child::Node(node)];
        while let Some(child) = pending.pop() {
            match child {
                Child::Token(token) => return Some(token),
                Child::Node(node) => pending.extend(self.children(node)),
            }
        }
        None
    }
}

/// Builds a tree from the parse's events: nodes open and close around the tokens they hold.
#[derive(Clone, Debug, Default)]
pub struct Builder {
    tree: Tree,
    open: Vec<(Rule, Vec<Child>)>,
}

impl Builder {
    pub fn open(&mut self, rule: Rule) {
        self.open.push((rule, Vec::new()));
    }

    pub fn token(&mut self, token: TokenId) {
        self.open
            .last_mut()
            .expect("a token inside a node")
            .1
            .push(Child::Token(token));
    }

    pub fn close(&mut self) {
        let (rule, children) = self.open.pop().expect("a node to close");
        let start = self.tree.children.len() as u32;
        self.tree.children.extend(children);
        let id = self.tree.nodes.len() as NodeId;
        self.tree.nodes.push(Node {
            rule,
            children: (start, self.tree.children.len() as u32),
        });
        match self.open.last_mut() {
            Some((_, siblings)) => siblings.push(Child::Node(id)),
            None => self.tree.root = id,
        }
    }

    /// Closes every open node, as an abandoned item leaves them, and returns the tree.
    pub fn finish(mut self) -> Tree {
        while !self.open.is_empty() {
            self.close();
        }
        self.tree
    }
}
