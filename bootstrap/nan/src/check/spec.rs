//! Specs: an item's clauses, field and invariant lines and targeting tests as API snapshots
//! accept them, and the parts of an accepted spec that a current spec weakens.

use std::collections::HashMap;

use super::Checker;
use super::db::ItemId;
use super::testing::Target as TestTarget;
use crate::format::normal::Numbering;
use crate::hash::Hash;
use crate::machine::api::{Accepted, AcceptedTest, Method};
use crate::syntax::ast::{self, ClauseKind, ItemKind, StructMember};
use crate::syntax::grammar::Keyword;
use crate::syntax::token::TokenKind;

/// A clause of a spec by its kind, with its flat text.
fn kind_of(clause: &str) -> &str {
    clause.split(' ').next().unwrap_or_default()
}

/// The last token of a head: of its parameters' closer, result, failure, `uses` and limits.
pub(super) fn head_last(head: &ast::Head) -> ast::Tok {
    let mut last = head.close;
    for ty in [&head.result, &head.fails].into_iter().flatten() {
        last = last.max(ty.last);
    }
    if let Some(uses) = &head.uses {
        last = last.max(uses.word.token);
    }
    for limit in head.limits.iter().flat_map(|limits| &limits.limits) {
        last = last.max(limit.size.unwrap_or(limit.token));
    }
    last
}

impl Checker<'_> {
    /// The spec of an item: for a function, its head, its clauses that are not disputed and its
    /// targeting tests; for an interface, each method's head and clauses; for a struct, each field
    /// line with a `where` predicate and each invariant line. Empty for any other item.
    pub fn spec(&self, item: ItemId) -> Accepted {
        let data = self.db.item(item);
        let ast = self.db.ast(item);
        match &ast.kind {
            ItemKind::Fn(function) => {
                let clauses = self.clause_texts(item, &function.clauses);
                let tests = self.targeting_tests(item);
                if clauses.is_empty() && tests.is_empty() {
                    return Accepted::default();
                }
                Accepted {
                    head: Some(self.flat(item, (ast.token, head_last(&function.head)))),
                    clauses,
                    tests,
                    methods: Vec::new(),
                }
            }
            ItemKind::Interface(interface) => {
                let mut methods: Vec<Method> = interface
                    .methods
                    .iter()
                    .filter_map(|method| {
                        let clauses = self.clause_texts(item, &method.clauses);
                        (!clauses.is_empty()).then(|| Method {
                            name: method.name.text.clone(),
                            head: self.flat(item, (method.token, head_last(&method.head))),
                            clauses,
                            pending: Vec::new(),
                        })
                    })
                    .collect();
                methods.sort_by(|left, right| left.name.cmp(&right.name));
                Accepted {
                    methods,
                    ..Accepted::default()
                }
            }
            ItemKind::Struct(structure) => {
                let syntax = data.syntax();
                let line_end = |token: ast::Tok| {
                    let mut at = token as usize;
                    while !matches!(syntax.tokens[at + 1].kind, TokenKind::Nl) {
                        at += 1;
                    }
                    at as ast::Tok
                };
                let clauses = structure
                    .members
                    .iter()
                    .filter_map(|member| match member {
                        StructMember::Field(field) if field.where_clause.is_some() => {
                            Some(self.flat(item, (field.token, line_end(field.token))))
                        }
                        StructMember::Invariant(condition) => {
                            let keyword = condition.token - 1;
                            Some(self.flat(item, (keyword, line_end(keyword))))
                        }
                        StructMember::Field(_) => None,
                    })
                    .collect();
                Accepted {
                    clauses,
                    ..Accepted::default()
                }
            }
            _ => Accepted::default(),
        }
    }

    /// The flat texts of the requires, ensures and example clauses that are not disputed, in
    /// clause order.
    fn clause_texts(&self, item: ItemId, clauses: &[ast::Clause]) -> Vec<String> {
        clauses
            .iter()
            .filter(|clause| !clause.disputed)
            .filter_map(|clause| match &clause.kind {
                ClauseKind::Requires(expr)
                | ClauseKind::Ensures(expr)
                | ClauseKind::Example(expr) => Some(self.flat(item, (clause.token, expr.last))),
                _ => None,
            })
            .collect()
    }

    /// The targeting tests of a function: the tests of its package that are not disputed and
    /// whose target it is, each with its parameter types and expect hashes, in byte order of
    /// canonical path.
    pub fn targeting_tests(&self, function: ItemId) -> Vec<AcceptedTest> {
        let package = self.db.package_of(function);
        let mut tests = Vec::new();
        for &module in &self.db.package(package).modules {
            for &item in &self.db.module(module).items {
                let ItemKind::Test(test) = &self.db.ast(item).kind else {
                    continue;
                };
                let targets = matches!(self.test_target(item, test),
                    Some(TestTarget::Function(target)) if target == function);
                if test.disputed || !targets {
                    continue;
                }
                let Some(path) = self.canonical_path(item) else {
                    continue;
                };
                let params: Vec<String> = test
                    .params
                    .iter()
                    .flatten()
                    .filter_map(|param| match &param.kind {
                        ast::ParamKind::Named { ty, .. } => {
                            Some(self.flat(item, (ty.token, ty.last)))
                        }
                        ast::ParamKind::SelfValue => None,
                    })
                    .collect();
                let mut expects = self.expect_hashes(item);
                expects.sort_by_key(|hash| hash.0);
                tests.push(AcceptedTest {
                    path,
                    params: params.join(", "),
                    expects,
                });
            }
        }
        tests.sort_by(|left, right| left.path.cmp(&right.path));
        tests
    }

    /// The expect hashes of a test: one per `expect` statement at any depth, the hash of
    /// `expect`, LF and the statement's text in the test's normalised form with the bindings it
    /// names numbered from 1.
    fn expect_hashes(&self, test: ItemId) -> Vec<Hash> {
        let data = self.db.item(test);
        let syntax = data.syntax();
        let span = &syntax.items[data.index];
        (span.first..span.end)
            .filter(|&token| syntax.tokens[token].kind == TokenKind::Keyword(Keyword::Expect))
            .map(|token| {
                let mut last = token;
                while !matches!(syntax.tokens[last + 1].kind, TokenKind::Nl) {
                    last += 1;
                }
                let text = self.fragment(test, (token as u32, last as u32), Numbering::Named);
                Hash::of(format!("expect\n{text}").as_bytes())
            })
            .collect()
    }

    /// The weakened parts of an item's accepted spec that its current spec has: for a function,
    /// `requires` when the accepted requires do not hold each current requires, each accepted
    /// ensures and example the current spec lacks, and `test` with the path of each accepted test
    /// no current targeting test covers; for a struct, `predicates` when an accepted field or
    /// invariant line is gone. In byte order.
    pub fn weakened(&self, item: ItemId, accepted: &Accepted, current: &Accepted) -> Vec<String> {
        let mut parts = Vec::new();
        match &self.db.ast(item).kind {
            ItemKind::Struct(_) => {
                if accepted
                    .clauses
                    .iter()
                    .any(|clause| !current.clauses.contains(clause))
                {
                    parts.push("predicates".to_owned());
                }
            }
            _ => {
                parts.extend(weakened_clauses(&accepted.clauses, &current.clauses));
                for test in &accepted.tests {
                    let covered = current.tests.iter().any(|candidate| {
                        candidate.params == test.params
                            && includes(&candidate.expects, &test.expects)
                    });
                    if !covered {
                        parts.push(format!("test {}", test.path));
                    }
                }
            }
        }
        parts.sort();
        parts.dedup();
        parts
    }

    /// The weakened parts of each accepted method of an interface, by method name.
    pub fn weakened_methods(
        &self,
        accepted: &Accepted,
        current: &Accepted,
    ) -> HashMap<String, Vec<String>> {
        let mut parts = HashMap::new();
        for method in &accepted.methods {
            let now: &[String] = current
                .methods
                .iter()
                .find(|candidate| candidate.name == method.name)
                .map_or(&[], |candidate| &candidate.clauses);
            let mut weakened = weakened_clauses(&method.clauses, now);
            weakened.sort();
            weakened.dedup();
            if !weakened.is_empty() {
                parts.insert(method.name.clone(), weakened);
            }
        }
        parts
    }
}

/// The weakened parts among clauses: `requires` when a current requires is not among the
/// accepted ones, and each accepted ensures or example the current clauses lack.
/// Whether a spec taken as the current spec keeps an accepted spec: it weakens no clause, test
/// or method of it.
pub fn keeps(current: &Accepted, accepted: &Accepted) -> bool {
    let tested = accepted.tests.iter().all(|test| {
        current.tests.iter().any(|candidate| {
            candidate.params == test.params && includes(&candidate.expects, &test.expects)
        })
    });
    let methods = accepted.methods.iter().all(|method| {
        let now: &[String] = current
            .methods
            .iter()
            .find(|candidate| candidate.name == method.name)
            .map_or(&[], |candidate| &candidate.clauses);
        weakened_clauses(&method.clauses, now).is_empty()
    });
    weakened_clauses(&accepted.clauses, &current.clauses).is_empty() && tested && methods
}

fn weakened_clauses(accepted: &[String], current: &[String]) -> Vec<String> {
    let mut parts = Vec::new();
    let accepted_requires: Vec<&String> = accepted
        .iter()
        .filter(|clause| kind_of(clause) == "requires")
        .collect();
    if current
        .iter()
        .any(|clause| kind_of(clause) == "requires" && !accepted_requires.contains(&clause))
    {
        parts.push("requires".to_owned());
    }
    parts.extend(
        accepted
            .iter()
            .filter(|clause| kind_of(clause) != "requires" && !current.contains(clause))
            .cloned(),
    );
    parts
}

/// Whether a multiset of hashes includes another, counted with repetition.
fn includes(whole: &[Hash], part: &[Hash]) -> bool {
    let mut left: Vec<&Hash> = whole.iter().collect();
    part.iter()
        .all(|hash| match left.iter().position(|known| *known == hash) {
            Some(at) => {
                left.remove(at);
                true
            }
            None => false,
        })
}
