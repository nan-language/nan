//! Punned arguments: in a call, an argument after the first whose value is a binding or parameter,
//! alone or after a mode, spelt like its label, which `nan fmt` writes without its label.

use std::collections::{HashMap, HashSet};

use super::bindings::Bindings;
use crate::syntax::file::SourceFile;
use crate::syntax::grammar::{Punct, Rule};
use crate::syntax::parser::is_pascal;
use crate::syntax::token::TokenKind;
use crate::syntax::tree::{Child, NodeId, Tree};

#[derive(Default)]
pub struct Punning {
    /// The label and colon tokens of the punned arguments written with their labels.
    pub labels: HashSet<u32>,
    /// The name token of each punned argument, by the first token of its value.
    pub values: HashMap<u32, u32>,
}

/// The punned arguments of an item's calls.
pub fn punning(file: &SourceFile, tree: &Tree, bindings: &Bindings) -> Punning {
    let mut walk = Walk {
        file,
        tree,
        bindings,
        found: Punning::default(),
    };
    for node in tree.descendants(tree.root()) {
        if tree.rule(node) == Rule::PostfixExpr {
            walk.calls(node);
        }
    }
    walk.found
}

struct Walk<'a> {
    file: &'a SourceFile,
    tree: &'a Tree,
    bindings: &'a Bindings,
    found: Punning,
}

impl Walk<'_> {
    fn text(&self, token: u32) -> &str {
        self.file.text(token as usize)
    }

    fn is_ident(&self, token: u32) -> bool {
        self.file.tokens[token as usize].kind == TokenKind::Ident
    }

    /// Records the punned arguments of each call suffix of a postfix expression. A suffix whose
    /// callee's last name is PascalCase is a construction, whose labels are never punned.
    fn calls(&mut self, postfix: NodeId) {
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
                        self.arguments(
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

    fn arguments(&mut self, args: Vec<NodeId>) {
        for arg in args.into_iter().skip(1) {
            let exprs: Vec<NodeId> = self.tree.child_nodes(arg).collect();
            let (label, value) = match exprs[..] {
                [label, value] => (Some(label), value),
                [value] => (None, value),
                _ => continue,
            };
            let Some(name) = self.marked_name(value) else {
                continue;
            };
            if !self.bindings.of.contains_key(&name) {
                continue;
            }
            let first = self.tree.span(value).expect("an argument holds tokens").0;
            match label.map(|label| self.lone_name(label)) {
                None => {}
                Some(Some(label)) if self.text(label) == self.text(name) => {
                    let colon = self
                        .tree
                        .child_tokens(arg)
                        .next()
                        .expect("a labelled argument has a colon");
                    self.found.labels.extend([label, colon]);
                }
                Some(_) => continue,
            }
            self.found.values.insert(first, name);
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
