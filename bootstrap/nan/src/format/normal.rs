//! The normalised form of an item, its interface form and its interface text: the canonical
//! format flat and without comments, its paths, literals, order-free members and bindings
//! written canonically, and the parts of it that code naming the item sees.

use std::collections::{HashMap, HashSet};

use super::bindings::{Bindings, bindings};
use super::item::{Facts, Layout, Roles, Tok, tokens, trailing_commas};
use super::punning::{Punning, punning};
use crate::syntax::file::{ItemSyntax, SourceFile};
use crate::syntax::grammar::{Keyword, Punct, Rule};
use crate::syntax::literal::text_value;
use crate::syntax::token::TokenKind;
use crate::syntax::tree::{Child, NodeId, Tree};

/// What the checker knows of an item beyond its syntax.
pub struct Known<'a> {
    /// What a path written in the item's module names: an item's canonical path or a core
    /// vocabulary name; none when it names neither.
    pub resolve: &'a dyn Fn(&[&str]) -> Option<String>,
    /// The canonical path of the item, or of a tombstone's old form.
    pub own: Option<String>,
    /// Whether a transparent function's body enters its interface form or text.
    pub transparent: bool,
    /// The fields of a struct its interface form or text shows.
    pub fields: Reach,
    /// The facts line of a struct, enum or function.
    pub facts: Option<String>,
}

/// Which fields of a struct a reader sees.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Every field: a reader in the struct's module.
    Module,
    /// Its `pkg` and `pub` fields: a reader in its package.
    Package,
    /// Its `pub` fields.
    Consumer,
}

/// An item's normalised form, and its interface form, which a const's value decides and none
/// gives here.
pub struct Forms {
    pub normalised: String,
    pub interface: Option<String>,
}

/// The normalised and interface forms of a parsed item.
pub fn forms(file: &SourceFile, item: &ItemSyntax, known: &Known) -> Forms {
    let tree = item.tree.as_ref().expect("a parsed item has a tree");
    let facts = Facts::collect(file, tree);
    let bindings = bindings(file, tree);
    let punning = punning(file, tree, &bindings);
    let mut toks = uncommented(tokens(file, item, &facts, &punning.labels));
    for tok in &mut toks {
        tok.newline = false;
    }
    trailing_commas(&mut toks, &facts, true);
    let normal = Normal {
        file,
        tree,
        facts: &facts,
        bindings: &bindings,
    };
    let renamed = normal.substitute(&mut toks, known, &punning);
    for run in normal.runs() {
        normal.sort(&mut toks, &run);
    }
    normal.number(&mut toks, &renamed);
    let normalised = text(&normal.render(&toks));
    let interface = match reduction(file, tree, known, Reading::Form) {
        None => None,
        Some(reduction) => {
            let mut lines = normal.render(&reduction.apply(toks));
            lines.extend(known.facts.clone());
            Some(text(&lines))
        }
    };
    Forms {
        normalised,
        interface,
    }
}

/// The interface text of a parsed item: the lines of its canonical format, without comments,
/// that hold its interface form.
pub fn interface_text(file: &SourceFile, item: &ItemSyntax, known: &Known) -> Vec<String> {
    let tree = item.tree.as_ref().expect("a parsed item has a tree");
    let facts = Facts::collect(file, tree);
    let punned = punning(file, tree, &bindings(file, tree)).labels;
    let mut toks = uncommented(tokens(file, item, &facts, &punned));
    let roles = trailing_commas(&mut toks, &facts, false);
    let reduction = reduction(file, tree, known, Reading::Text).expect("an interface text");
    let kept: Vec<bool> = toks
        .iter()
        .map(|tok| {
            tok.source
                .is_none_or(|source| !reduction.dropped.contains(&source))
        })
        .collect();
    let roles = roles.kept(&kept);
    let toks: Vec<Tok> = reduction.apply(toks);
    Layout {
        toks: &toks,
        facts: &facts,
        roles: &roles,
    }
    .render()
}

/// Which bindings a fragment's normalised text numbers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Numbering {
    /// Those it introduces, in order of introduction; every other binding keeps its name.
    Introduced,
    /// Every binding it names, in order of first occurrence in it.
    Named,
}

/// The tokens `first..=last` of a parsed item on one line, written as its normalised form
/// writes them: paths canonical, literals canonical, order-free members sorted, and bindings
/// numbered as `numbering` states.
pub fn normalised_fragment(
    file: &SourceFile,
    item: &ItemSyntax,
    known: &Known,
    (first, last): (u32, u32),
    numbering: Numbering,
) -> String {
    let tree = item.tree.as_ref().expect("a parsed item has a tree");
    let facts = Facts::collect(file, tree);
    let bindings = bindings(file, tree);
    let punning = punning(file, tree, &bindings);
    let mut toks = uncommented(tokens(file, item, &facts, &punning.labels));
    for tok in &mut toks {
        tok.newline = false;
    }
    trailing_commas(&mut toks, &facts, true);
    let normal = Normal {
        file,
        tree,
        facts: &facts,
        bindings: &bindings,
    };
    let renamed = normal.substitute(&mut toks, known, &punning);
    for run in normal.runs() {
        normal.sort(&mut toks, &run);
    }
    let within = |token: u32| (first..=last).contains(&token);
    toks.retain(|tok| tok.source.is_some_and(within));
    let mut numbers: HashMap<u32, usize> = HashMap::new();
    for tok in &mut toks {
        let Some(&binding) = tok.source.and_then(|source| renamed.get(&source)) else {
            continue;
        };
        let at = tok
            .text
            .rfind('$')
            .expect("a renamed binding is written `$`");
        if numbering == Numbering::Introduced && !within(binding) {
            tok.text.replace_range(at..=at, file.text(binding as usize));
            continue;
        }
        let next = numbers.len() + 1;
        let number = *numbers.entry(binding).or_insert(next);
        tok.text.replace_range(at..=at, &format!("${number}"));
    }
    normal.render(&toks).join(" ").trim_start().to_owned()
}

/// The flat text of the tokens `first..=last` of a parsed item: on one line, without comments,
/// each breakable list flat and each text run written as the normalised form writes it.
pub fn flat_text(file: &SourceFile, item: &ItemSyntax, (first, last): (u32, u32)) -> String {
    let tree = item.tree.as_ref().expect("a parsed item has a tree");
    let facts = Facts::collect(file, tree);
    let punned = punning(file, tree, &bindings(file, tree)).labels;
    let mut toks = uncommented(tokens(file, item, &facts, &punned));
    for tok in &mut toks {
        tok.newline = false;
    }
    trailing_commas(&mut toks, &facts, true);
    toks.retain(|tok| {
        tok.source
            .is_some_and(|source| (first..=last).contains(&source))
    });
    for tok in &mut toks {
        if matches!(tok.kind, TokenKind::Text | TokenKind::TextLine)
            && let Some(text) = literal(tok.kind, &tok.text)
        {
            tok.text = text;
        }
    }
    merge_runs(&mut toks);
    Layout {
        toks: &toks,
        facts: &facts,
        roles: &Roles::flat(toks.len(), facts.lists.len()),
    }
    .render()
    .join(" ")
    .trim_start()
    .to_owned()
}

/// The tokens without their comments and doc comments.
fn uncommented(toks: Vec<Tok>) -> Vec<Tok> {
    toks.into_iter()
        .filter(|tok| !matches!(tok.kind, TokenKind::Doc | TokenKind::FileDoc))
        .map(|mut tok| {
            tok.comments.clear();
            tok.trailing = None;
            tok
        })
        .collect()
}

fn text(lines: &[String]) -> String {
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

struct Normal<'a> {
    file: &'a SourceFile,
    tree: &'a Tree,
    facts: &'a Facts,
    bindings: &'a Bindings,
}

/// A list of order-free members to sort, at its depth in the tree.
struct Run {
    elements: Vec<(u32, u32)>,
    depth: usize,
}

impl Normal<'_> {
    fn kind(&self, token: u32) -> TokenKind {
        self.file.tokens[token as usize].kind
    }

    fn token_text(&self, token: u32) -> &str {
        self.file.text(token as usize)
    }

    fn render(&self, toks: &[Tok]) -> Vec<String> {
        Layout {
            toks,
            facts: self.facts,
            roles: &Roles::flat(toks.len(), self.facts.lists.len()),
        }
        .render()
    }

    /// Writes the item's declared names, paths and literals canonically and its bindings as
    /// `$`; returns the binding each renamed token belongs to.
    fn substitute(
        &self,
        toks: &mut Vec<Tok>,
        known: &Known,
        punning: &Punning,
    ) -> HashMap<u32, u32> {
        let mut written: HashMap<u32, Option<String>> = HashMap::new();
        let mut replace = |(first, last): (u32, u32), text: String| {
            written.insert(first, Some(text));
            for token in first + 1..=last {
                written.insert(token, None);
            }
        };
        if let Some(own) = &known.own {
            for span in self.declared() {
                replace(span, own.clone());
            }
        }
        for (span, segments) in self.paths() {
            if let Some(path) = (known.resolve)(&segments) {
                replace(span, path);
            }
        }
        let renamed: HashMap<u32, u32> = self
            .bindings
            .of
            .iter()
            .filter(|(_, first)| !self.bindings.parameters.contains(first))
            .map(|(token, first)| (*token, *first))
            .collect();
        toks.retain_mut(|tok| {
            let Some(source) = tok.source else {
                return true;
            };
            match written.get(&source) {
                Some(None) => return false,
                Some(Some(text)) => tok.text = text.clone(),
                None if renamed.contains_key(&source) => tok.text = "$".to_owned(),
                None => {
                    if let Some(text) = literal(tok.kind, &tok.text) {
                        tok.text = text;
                    }
                }
            }
            if let Some(name) = punning.values.get(&source)
                && renamed.contains_key(name)
            {
                tok.text = format!("{}: {}", self.token_text(*name), tok.text);
            }
            true
        });
        merge_runs(toks);
        renamed
    }

    /// The spans of the item's declared name and closing label, and of a tombstone's old form.
    fn declared(&self) -> Vec<(u32, u32)> {
        let tree = self.tree;
        let mut spans = Vec::new();
        for node in tree.descendants(tree.root()) {
            match tree.rule(node) {
                Rule::FnHead | Rule::FnBody => spans.extend(
                    tree.find(node, Rule::ItemLabel)
                        .and_then(|label| tree.span(label)),
                ),
                Rule::StructItem
                | Rule::EnumItem
                | Rule::InterfaceItem
                | Rule::LayoutItem
                | Rule::RegistersItem
                | Rule::PackageItem => {
                    spans.extend(self.first_ident(node).map(|token| (token, token)));
                    spans.extend(
                        tree.find(node, Rule::ItemLabel)
                            .and_then(|label| tree.span(label)),
                    );
                }
                Rule::RegistryItem | Rule::TestItem | Rule::ConstItem | Rule::ContributeItem => {
                    spans.extend(self.first_ident(node).map(|token| (token, token)));
                }
                Rule::ProtocolItem => {
                    let name = (tree.child_tokens(node))
                        .filter(|token| self.kind(*token) == TokenKind::Ident)
                        .nth(1);
                    spans.extend(name.map(|token| (token, token)));
                    spans.extend(
                        tree.find(node, Rule::ItemLabel)
                            .and_then(|label| tree.span(label)),
                    );
                }
                Rule::RetiredOld => {
                    let path = tree.find(node, Rule::Path).and_then(|path| tree.span(path));
                    let member = tree
                        .child_tokens(node)
                        .filter(|token| self.kind(*token) == TokenKind::Ident)
                        .last();
                    spans.extend(path.map(|(first, last)| (first, member.unwrap_or(last))));
                }
                _ => {}
            }
        }
        spans
    }

    fn first_ident(&self, node: NodeId) -> Option<u32> {
        self.tree
            .child_tokens(node)
            .find(|token| self.kind(*token) == TokenKind::Ident)
    }

    /// The paths of the item that may name an item, with their segments: not a label, a
    /// binding, a parameter, a generic parameter, a field of the struct, `Self` or a tombstone's
    /// old form.
    fn paths(&self) -> Vec<((u32, u32), Vec<&str>)> {
        let tree = self.tree;
        let mut local: HashSet<&str> = HashSet::from(["Self"]);
        for node in tree.descendants(tree.root()) {
            if matches!(
                tree.rule(node),
                Rule::GenericParam | Rule::ImplParam | Rule::Field
            ) {
                local.extend(self.first_ident(node).map(|token| self.token_text(token)));
            }
        }
        let mut found = Vec::new();
        let mut pending = vec![tree.root()];
        while let Some(node) = pending.pop() {
            let children: Vec<NodeId> = tree.child_nodes(node).collect();
            match tree.rule(node) {
                Rule::Arg | Rule::FieldPattern | Rule::PayloadField if children.len() == 2 => {
                    pending.push(children[1]);
                }
                Rule::RetiredOld => pending.extend(
                    children
                        .into_iter()
                        .filter(|child| tree.rule(*child) != Rule::Path),
                ),
                Rule::Path => {
                    let tokens: Vec<u32> = tree.child_tokens(node).collect();
                    let segments: Vec<&str> = tokens
                        .iter()
                        .filter(|token| self.kind(**token) == TokenKind::Ident)
                        .map(|token| self.token_text(*token))
                        .collect();
                    let local = segments.len() == 1
                        && (self.bindings.of.contains_key(&tokens[0])
                            || local.contains(segments[0]));
                    if !local {
                        found.push(((tokens[0], tokens[tokens.len() - 1]), segments));
                    }
                }
                _ => pending.extend(children),
            }
        }
        found
    }

    /// The lists of order-free members: the fields and invariant lines of a struct, the
    /// variants of an enum and their labelled payload fields, unless the type derives `Ord`,
    /// `Render` or `Generate`, the entries of `limits` and `derives` clauses and the arms of a
    /// `match` that is not `match first`; innermost first.
    fn runs(&self) -> Vec<Run> {
        let tree = self.tree;
        let ordered = tree
            .descendants(tree.root())
            .into_iter()
            .find(|node| tree.rule(*node) == Rule::DerivesClause)
            .is_some_and(|derives| {
                tree.child_tokens(derives)
                    .any(|token| matches!(self.token_text(token), "Ord" | "Render" | "Generate"))
            });
        let span = |node: &NodeId| tree.span(*node).expect("a member holds tokens");
        let mut runs = Vec::new();
        let mut pending = vec![(tree.root(), 0)];
        while let Some((node, depth)) = pending.pop() {
            let children: Vec<NodeId> = tree.child_nodes(node).collect();
            let mut add = |elements: Vec<(u32, u32)>| runs.push(Run { elements, depth });
            match tree.rule(node) {
                Rule::StructItem => {
                    let (fields, invariants): (Vec<NodeId>, Vec<NodeId>) = children
                        .iter()
                        .copied()
                        .filter(|child| tree.rule(*child) == Rule::StructMember)
                        .partition(|member| tree.find(*member, Rule::Field).is_some());
                    if !ordered {
                        add(fields.iter().map(span).collect());
                    }
                    add(invariants.iter().map(span).collect());
                }
                Rule::EnumItem if !ordered => add(children
                    .iter()
                    .filter(|child| tree.rule(**child) == Rule::EnumMember)
                    .map(span)
                    .collect()),
                Rule::VariantPayload if !ordered => {
                    let fields =
                        tree.list_elements(tree.find(node, Rule::PayloadList), Rule::PayloadField);
                    if fields
                        .iter()
                        .all(|field| tree.child_nodes(*field).count() == 2)
                    {
                        add(fields.iter().map(span).collect());
                    }
                }
                Rule::LimitsClause => add(children.iter().map(span).collect()),
                Rule::DerivesClause => add(tree
                    .child_tokens(node)
                    .filter(|token| self.kind(*token) == TokenKind::Ident)
                    .map(|token| (token, token))
                    .collect()),
                Rule::MatchExpr
                    if !tree
                        .child_tokens(node)
                        .any(|token| self.kind(token) == TokenKind::Keyword(Keyword::First)) =>
                {
                    add(children
                        .iter()
                        .filter(|child| tree.rule(**child) == Rule::MatchArm)
                        .map(span)
                        .collect());
                }
                _ => {}
            }
            pending.extend(children.into_iter().map(|child| (child, depth + 1)));
        }
        runs.retain(|run| run.elements.len() > 1);
        runs.sort_by_key(|run| std::cmp::Reverse(run.depth));
        runs
    }

    /// Sorts the members of a run in byte order of their text: each moves to the slot of its
    /// rank, the tokens between members keeping their place.
    fn sort(&self, toks: &mut [Tok], run: &Run) {
        let mut spans: Vec<std::ops::Range<usize>> = run
            .elements
            .iter()
            .filter_map(|&(first, last)| {
                let held = |tok: &Tok| {
                    tok.source
                        .is_some_and(|source| (first..=last).contains(&source))
                };
                let start = toks.iter().position(held)?;
                let end = toks.iter().rposition(held)? + 1;
                Some(start..end)
            })
            .collect();
        if spans.len() < 2 {
            return;
        }
        spans.sort_by_key(|span| span.start);
        let keys: Vec<String> = spans
            .iter()
            .map(|span| self.render(&toks[span.clone()]).join("\n"))
            .collect();
        let mut ranked: Vec<usize> = (0..spans.len()).collect();
        ranked.sort_by(|left, right| keys[*left].cmp(&keys[*right]));
        let start = spans[0].start;
        let end = spans[spans.len() - 1].end;
        let mut order: Vec<usize> = Vec::with_capacity(end - start);
        for (slot, element) in ranked.iter().enumerate() {
            order.extend(spans[*element].clone());
            if let Some(next) = spans.get(slot + 1) {
                order.extend(spans[slot].end..next.start);
            }
        }
        let moved: Vec<Tok> = order.iter().map(|at| toks[*at].clone()).collect();
        for (offset, tok) in moved.into_iter().enumerate() {
            toks[start + offset] = tok;
        }
    }

    /// Numbers the bindings `$1`, `$2` and so on in order of their first introduction.
    fn number(&self, toks: &mut [Tok], renamed: &HashMap<u32, u32>) {
        let mut numbers: HashMap<u32, usize> = HashMap::new();
        for tok in toks {
            let Some(first) = tok.source.and_then(|source| renamed.get(&source)) else {
                continue;
            };
            let next = numbers.len() + 1;
            let number = *numbers.entry(*first).or_insert(next);
            let at = tok
                .text
                .rfind('$')
                .expect("a renamed binding is written `$`");
            tok.text.replace_range(at..=at, &format!("${number}"));
        }
    }
}

/// What a reduction serves: an interface form, or an interface text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reading {
    Form,
    Text,
}

/// The tokens an interface form or text leaves out of an item, and the default bodies it
/// writes `default`.
struct Reduction {
    dropped: HashSet<u32>,
    defaults: HashSet<u32>,
}

impl Reduction {
    fn apply(&self, toks: Vec<Tok>) -> Vec<Tok> {
        toks.into_iter()
            .filter(|tok| {
                tok.source
                    .is_none_or(|source| !self.dropped.contains(&source))
            })
            .map(|mut tok| {
                if tok
                    .source
                    .is_some_and(|source| self.defaults.contains(&source))
                {
                    tok.text = "default".to_owned();
                    tok.kind = TokenKind::Ident;
                }
                tok
            })
            .collect()
    }
}

/// The reduction of an item to its interface form or text; none for the interface form of a
/// const.
fn reduction(file: &SourceFile, tree: &Tree, known: &Known, reading: Reading) -> Option<Reduction> {
    let mut reduction = Reduction {
        dropped: HashSet::new(),
        defaults: HashSet::new(),
    };
    let tokens = |node: NodeId| {
        let (first, last) = tree.span(node).expect("a node holding tokens");
        first..=last
    };
    let kind = |token: u32| file.tokens[token as usize].kind;
    let item = tree
        .find(tree.root(), Rule::ItemStart)
        .and_then(|start| tree.find(start, Rule::ItemKind))
        .and_then(|kind| tree.child_nodes(kind).next());
    let Some(item) = item else {
        return Some(reduction);
    };
    let mut clause_regions = Vec::new();
    match tree.rule(item) {
        Rule::FnItem => {
            if let Some(end) = tree.find(item, Rule::FnEnd) {
                let transparent = tree
                    .child_tokens(end)
                    .any(|token| kind(token) == TokenKind::Punct(Punct::Eq));
                for child in tree.children(end) {
                    match *child {
                        Child::Token(token) if kind(token) == TokenKind::Punct(Punct::LBrace) => {
                            reduction.dropped.insert(token);
                        }
                        Child::Token(token)
                            if kind(token) == TokenKind::Punct(Punct::Eq) && !known.transparent =>
                        {
                            reduction.dropped.insert(token);
                        }
                        Child::Node(node) if tree.rule(node) == Rule::FnBody => {
                            reduction.dropped.extend(tokens(node));
                        }
                        Child::Node(node) if transparent && tree.rule(node) == Rule::Expr => {
                            if !known.transparent {
                                reduction.dropped.extend(tokens(node));
                            }
                        }
                        Child::Node(node) if tree.rule(node) == Rule::ClauseRegion => {
                            clause_regions.push(node);
                        }
                        _ => {}
                    }
                }
            }
        }
        Rule::StructItem => {
            let mut hidden = HashSet::new();
            let mut invariants = Vec::new();
            for member in tree.child_nodes(item) {
                if let Some(field) = tree.find(member, Rule::Field) {
                    let visibility = tree
                        .find(field, Rule::Visibility)
                        .and_then(|visibility| tree.child_tokens(visibility).next())
                        .map(kind);
                    let seen = match known.fields {
                        Reach::Module => true,
                        Reach::Package => visibility.is_some(),
                        Reach::Consumer => visibility == Some(TokenKind::Keyword(Keyword::Pub)),
                    };
                    if !seen {
                        let name = tree
                            .child_tokens(field)
                            .find(|token| kind(*token) == TokenKind::Ident)
                            .map(|token| file.text(token as usize));
                        hidden.extend(name);
                        reduction.dropped.extend(tokens(member));
                    }
                } else if tree.rule(member) == Rule::StructMember {
                    invariants.push(member);
                }
            }
            for invariant in invariants {
                let names_hidden = tree.descendants(invariant).into_iter().any(|node| {
                    tree.rule(node) == Rule::Path
                        && matches!(tree.children(node), [Child::Token(token)]
                            if hidden.contains(file.text(*token as usize)))
                });
                if names_hidden {
                    reduction.dropped.extend(tokens(invariant));
                }
            }
        }
        Rule::InterfaceItem => {
            for member in tree.child_nodes(item) {
                let Some(end) = tree.find(member, Rule::MethodEnd) else {
                    continue;
                };
                clause_regions.extend(tree.find(end, Rule::ClauseRegion));
                if let Some(body) = tree.find(end, Rule::MethodBody) {
                    let mut body_tokens = tokens(body);
                    let opener = body_tokens.next().expect("a method body opens");
                    reduction.defaults.insert(opener);
                    reduction
                        .dropped
                        .extend(body_tokens.filter(|token| kind(*token) != TokenKind::Nl));
                }
            }
        }
        Rule::TestItem if reading == Reading::Form => {
            let kept: HashSet<u32> = tree
                .children(item)
                .iter()
                .filter_map(|child| match *child {
                    Child::Token(token) => Some(token),
                    Child::Node(_) => None,
                })
                .skip_while(|token| kind(*token) != TokenKind::Keyword(Keyword::Test))
                .take(3)
                .chain(
                    tree.find(item, Rule::TestTarget)
                        .into_iter()
                        .flat_map(tokens),
                )
                .collect();
            reduction
                .dropped
                .extend(tokens(item).filter(|token| !kept.contains(token)));
        }
        Rule::PackageItem if reading == Reading::Form => {
            for directive in tree.child_nodes(item) {
                let kept = tree.child_nodes(directive).next().is_some_and(|inner| {
                    matches!(
                        tree.rule(inner),
                        Rule::LanguageDirective | Rule::TargetsDirective
                    )
                });
                if tree.rule(directive) == Rule::Directive && !kept {
                    reduction.dropped.extend(tokens(directive));
                }
            }
        }
        Rule::ConstItem if reading == Reading::Form => return None,
        Rule::ProtocolItem => {
            for operation in tree
                .child_nodes(item)
                .filter(|member| tree.rule(*member) == Rule::ProtocolOperation)
            {
                let visibility = tree
                    .find(operation, Rule::Visibility)
                    .and_then(|visibility| tree.child_tokens(visibility).next())
                    .map(kind);
                let seen = match known.fields {
                    Reach::Module => true,
                    Reach::Package => visibility.is_some(),
                    Reach::Consumer => visibility == Some(TokenKind::Keyword(Keyword::Pub)),
                };
                match seen {
                    true => clause_regions.extend(tree.find(operation, Rule::ClauseRegion)),
                    false => reduction.dropped.extend(tokens(operation)),
                }
            }
        }
        _ => {}
    }
    for region in clause_regions {
        for line in tree.child_nodes(region) {
            let Some(clause) = tree.child_nodes(line).next() else {
                continue;
            };
            let kept = match tree.rule(clause) {
                Rule::RequiresClause
                | Rule::EnsuresClause
                | Rule::ExampleClause
                | Rule::UndoClause => true,
                Rule::TodoClause => tree.child_nodes(clause).next().is_some(),
                _ => false,
            };
            if !kept {
                reduction.dropped.extend(tokens(line));
            }
        }
    }
    Some(reduction)
}

/// A literal written canonically; a text run's line as its content, which its run joins.
fn literal(kind: TokenKind, text: &str) -> Option<String> {
    Some(match kind {
        TokenKind::Int => integer(text),
        TokenKind::Float => float(text),
        TokenKind::Text | TokenKind::Char => quoted(text_value(text).bytes(), &text[..1], false),
        TokenKind::Bytes | TokenKind::Byte => {
            format!(
                "b{}",
                quoted(byte_value(text).into_iter(), &text[1..2], true)
            )
        }
        TokenKind::TextLine => text_value(&format!("\"{}\"", &text[2..])),
        _ => return None,
    })
}

/// Writes each text run as one text literal.
fn merge_runs(toks: &mut Vec<Tok>) {
    let mut merged: Vec<Tok> = Vec::with_capacity(toks.len());
    let mut in_run = false;
    for tok in std::mem::take(toks) {
        let line = tok.kind == TokenKind::TextLine;
        if line && in_run {
            let first = merged.last_mut().expect("a run's first line");
            first.text.push('\n');
            first.text.push_str(&tok.text);
            continue;
        }
        in_run = line;
        merged.push(tok);
    }
    for tok in &mut merged {
        if tok.kind == TokenKind::TextLine {
            tok.text = quoted(tok.text.bytes(), "\"", false);
            tok.kind = TokenKind::Text;
        }
    }
    *toks = merged;
}

/// An integer literal in decimal without separators or leading zeros, keeping its suffix.
fn integer(text: &str) -> String {
    let digits: String = text.chars().filter(|c| *c != '_').collect();
    let (radix, body) = match digits.get(..2) {
        Some("0x") => (16, &digits[2..]),
        Some("0b") => (2, &digits[2..]),
        _ => (10, digits.as_str()),
    };
    let end = body
        .find(|c: char| !c.is_digit(radix))
        .unwrap_or(body.len());
    format!("{}{}", decimal(&body[..end], radix), &body[end..])
}

/// The decimal digits, without leading zeros, of the natural number that digits of a radix
/// write, however large.
fn decimal(digits: &str, radix: u32) -> String {
    const LIMB: u64 = 1_000_000_000;
    let mut limbs: Vec<u64> = vec![0];
    for digit in digits.chars().filter_map(|c| c.to_digit(radix)) {
        let mut carry = u64::from(digit);
        for limb in &mut limbs {
            let value = *limb * u64::from(radix) + carry;
            *limb = value % LIMB;
            carry = value / LIMB;
        }
        if carry > 0 {
            limbs.push(carry);
        }
    }
    let mut text = limbs.last().copied().unwrap_or(0).to_string();
    for limb in limbs.iter().rev().skip(1) {
        text.push_str(&format!("{limb:09}"));
    }
    text
}

/// A float literal as the shortest decimal that reads back as its value, keeping its suffix.
fn float(text: &str) -> String {
    let digits: String = text.chars().filter(|c| *c != '_').collect();
    let end = digits
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(digits.len());
    let (number, suffix) = digits.split_at(end);
    let shortest = if suffix == "f32" {
        number.parse::<f32>().expect("a float literal").to_string()
    } else {
        number.parse::<f64>().expect("a float literal").to_string()
    };
    if shortest.contains('.') {
        format!("{shortest}{suffix}")
    } else {
        format!("{shortest}.0{suffix}")
    }
}

/// The bytes of a byte or byte-string literal, its escapes replaced.
fn byte_value(text: &str) -> Vec<u8> {
    let mut value = Vec::new();
    let mut bytes = text.as_bytes()[2..text.len() - 1].iter().copied();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            value.push(byte);
            continue;
        }
        value.push(match bytes.next().expect("an escape has a character") {
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'0' => 0,
            b'x' => {
                let digits = [bytes.next(), bytes.next()];
                let digits: Vec<u8> = digits.into_iter().flatten().collect();
                u8::from_str_radix(std::str::from_utf8(&digits).expect("hex digits"), 16)
                    .expect("two hexadecimal digits")
            }
            other => other,
        });
    }
    value
}

/// A character as the normalised form writes a char literal holding it.
pub fn char_literal(c: char) -> String {
    quoted(c.to_string().bytes(), "'", false)
}

/// A literal between delimiters, writing `\xHH` for the delimiter, the backslash, each byte
/// below `0x20` or equal to `0x7F` and, in a byte literal, each byte above `0x7F`.
fn quoted(value: impl Iterator<Item = u8>, delimiter: &str, bytes: bool) -> String {
    let mut raw = Vec::new();
    for byte in value {
        let escaped = byte < 0x20
            || byte == 0x7F
            || byte == b'\\'
            || byte == delimiter.as_bytes()[0]
            || (bytes && byte > 0x7F);
        if escaped {
            raw.extend(format!("\\x{byte:02X}").bytes());
        } else {
            raw.push(byte);
        }
    }
    format!(
        "{delimiter}{}{delimiter}",
        String::from_utf8(raw).expect("well-formed UTF-8")
    )
}

#[cfg(test)]
mod tests;
