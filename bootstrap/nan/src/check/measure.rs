//! The measurements of the freeze that checking gives over a workspace's packages, under each
//! target they are checked for.

use super::db::ItemId;
use super::reading::obligation_kind;
use super::tir::{TBody, TExpr, TKind};
use super::{Checker, checked_closures, checked_packages};
use crate::nir::CheckKind;
use crate::prover::{Obligation, Status};
use crate::syntax::ast::{ItemKind, Mode, Param};
use crate::workspace::Workspace;

/// The measurements of a workspace's packages, each site written `<path>:<line>`.
pub struct Measured {
    pub discharge: Discharge,
    /// Each predicate obligation kept as a run-time check.
    pub predicate_checks: Vec<String>,
    /// The canonical path of each item whose code names `mem::Arena`.
    pub arena_items: Vec<String>,
    /// Each function item, interface method or test with more than 4 parameters, `self`
    /// included, by canonical path, with its parameter count.
    pub wide_heads: Vec<(String, usize)>,
    /// Each task closure outside a lexical spawn, by site, with the names it captures by `take`.
    pub task_closures: Vec<(String, Vec<String>)>,
}

/// The bounds and overflow obligations: how many there are, how many the prover proves, and
/// each other one as `<path>:<line> <kind>`, its kind as `nan explain` names it.
pub struct Discharge {
    pub total: usize,
    pub proved: usize,
    pub unproved: Vec<String>,
}

pub fn measure(workspace: &Workspace) -> Measured {
    let std = crate::stdlib::packages();
    let mut measured = Measured {
        discharge: Discharge {
            total: 0,
            proved: 0,
            unproved: Vec::new(),
        },
        predicate_checks: Vec::new(),
        arena_items: Vec::new(),
        wide_heads: Vec::new(),
        task_closures: Vec::new(),
    };
    for (_, checker) in checked_closures(workspace, std) {
        for (_, package) in checked_packages(&checker) {
            let db = &checker.db;
            for &module in &db.package(package).modules {
                for &item in &db.module(module).items {
                    checker.measure_item(item, &mut measured);
                }
            }
        }
    }
    measured
}

impl Checker<'_> {
    fn measure_item(&self, item: ItemId, measured: &mut Measured) {
        let db = &self.db;
        let data = db.item(item);
        let site = |token| format!("{}:{}", data.file.path, data.line(token));
        if self.names_arena(item) {
            measured.arena_items.push(db.canonical(item));
        }
        let heads: Vec<(String, &[Param])> = match &data.ast.kind {
            ItemKind::Fn(function) => vec![(db.canonical(item), &function.head.params)],
            ItemKind::Interface(interface) => interface
                .methods
                .iter()
                .map(|method| {
                    let path = format!("{}.{}", db.canonical(item), method.name.text);
                    (path, &method.head.params[..])
                })
                .collect(),
            ItemKind::Test(test) => {
                vec![(
                    db.canonical(item),
                    test.params.as_deref().unwrap_or_default(),
                )]
            }
            _ => Vec::new(),
        };
        for (path, params) in heads {
            if params.len() > 4 {
                measured.wide_heads.push((path, params.len()));
            }
        }
        if let Some(body) = self.typed_body(item)
            && let Some(root) = &body.root
        {
            task_closures(&body, root, &site, &mut measured.task_closures);
        }
        let Some(unit) = self.unit(item) else {
            return;
        };
        for obligated in self.obligations(item).iter() {
            let checked = matches!(obligated.status, Status::Checked | Status::Open);
            match obligated.obligation {
                Obligation::Check(CheckKind::Predicate) if checked => {
                    measured.predicate_checks.push(site(obligated.span.first));
                }
                Obligation::Bounds | Obligation::Overflow => {
                    let discharge = &mut measured.discharge;
                    discharge.total += 1;
                    match checked {
                        false => discharge.proved += 1,
                        true => discharge.unproved.push(format!(
                            "{} {}",
                            site(obligated.span.first),
                            obligation_kind(&unit, obligated)
                        )),
                    }
                }
                _ => {}
            }
        }
    }

    /// Whether an item's tokens name `mem::Arena`, the one path that reaches it.
    fn names_arena(&self, item: ItemId) -> bool {
        let data = self.db.item(item);
        let syntax = &data.file.syntax;
        let span = &syntax.items[data.index];
        let words: Vec<&str> = (span.first..span.end)
            .filter(|token| !syntax.comment[*token])
            .map(|token| syntax.text(token))
            .collect();
        words.windows(3).any(|path| path == ["mem", "::", "Arena"])
    }
}

/// The task closures an expression holds at any depth that no lexical spawn writes, each with
/// the names of the locals it captures by `take`.
fn task_closures(
    body: &TBody,
    expr: &TExpr,
    site: &dyn Fn(u32) -> String,
    found: &mut Vec<(String, Vec<String>)>,
) {
    if let TKind::Closure(closure) = &expr.kind
        && closure.task
        && closure.lent.is_none()
    {
        let taken = closure
            .captures
            .iter()
            .filter(|capture| matches!(capture.listed, Some((Mode::Take, _))))
            .map(|capture| body.locals[capture.outer.0 as usize].name.clone())
            .collect();
        found.push((site(expr.token), taken));
    }
    expr.each_child(&mut |child| task_closures(body, child, site, found));
}
