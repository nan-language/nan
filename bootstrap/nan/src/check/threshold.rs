//! Thresholds: the nesting depth and length of a function's block body, and the waivers
//! accepting their excess.

use super::Checker;
use super::db::ItemId;
use crate::diagnostic::{Code, Offer};
use crate::syntax::ast::{
    ArmBody, Block, Clause, ClauseKind, ClosureBody, Else, Expr, ExprKind, StmtKind, Tok,
    TrustedBody, WhenBody,
};

/// The deepest level a body may hold without a waiver.
const DEPTH: usize = 4;

/// The most lines a body may hold without a waiver.
const LENGTH: u32 = 80;

impl Checker<'_> {
    /// The thresholds of a function's or interface method's block body under its waivers.
    pub fn check_thresholds(&self, item: ItemId, clauses: &[Clause], body: Option<&Block>) {
        let data = self.db.item(item);
        let deep = body.and_then(first_deep);
        let long = body.is_some_and(|body| {
            let lines = data
                .line(body.end)
                .saturating_sub(data.line(body.token) + 1);
            lines > LENGTH
        });
        let mut waived = [false, false];
        for clause in clauses {
            let ClauseKind::Waive { length, reason } = &clause.kind else {
                continue;
            };
            if reason.value.trim_matches(' ').is_empty() {
                self.report(item, reason.token, Code::ThresholdEmptyReason);
            }
            let kind = usize::from(*length);
            let exceeded = if *length { long } else { deep.is_some() };
            if waived[kind] || !exceeded {
                let fix = self.fix_deleting_lines(item, clause.token, reason.token);
                self.report_fixed(
                    item,
                    (clause.token, reason.token),
                    Code::ThresholdUnusedWaiver,
                    vec![fix],
                    Offer::Each,
                );
            }
            waived[kind] = true;
        }
        if let Some(level) = deep
            && !waived[0]
        {
            self.report(item, level, Code::ThresholdDepth);
        }
        if let Some(body) = body
            && long
            && !waived[1]
        {
            self.report(item, body.end, Code::ThresholdLength);
        }
    }
}

/// Where the first level deeper than the threshold begins in a nesting root's block.
fn first_deep(root: &Block) -> Option<Tok> {
    let mut found = None;
    block_levels(root, 0, &mut found);
    found
}

/// The levels within a block standing at `depth`.
fn block_levels(block: &Block, depth: usize, found: &mut Option<Tok>) {
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::Let {
                value, otherwise, ..
            } => {
                expr_levels(value, depth, found);
                if let Some(otherwise) = otherwise {
                    level(otherwise, depth, found);
                }
            }
            StmtKind::Var { value, .. }
            | StmtKind::Discard(value)
            | StmtKind::Defer(value)
            | StmtKind::Undefer(value)
            | StmtKind::Expect(value)
            | StmtKind::Assume {
                condition: value, ..
            }
            | StmtKind::Expr(value) => expr_levels(value, depth, found),
            StmtKind::While { condition, body } => {
                expr_levels(condition, depth, found);
                level(&body.block, depth, found);
            }
            StmtKind::For { iterated, body, .. } => {
                expr_levels(iterated, depth, found);
                level(&body.block, depth, found);
            }
            StmtKind::Loop { body, .. } => level(&body.block, depth, found),
            StmtKind::Assign { target, value, .. } => {
                expr_levels(target, depth, found);
                expr_levels(value, depth, found);
            }
        }
    }
}

/// A block that is a level within one standing at `depth`.
fn level(block: &Block, depth: usize, found: &mut Option<Tok>) {
    if depth + 1 > DEPTH && found.is_none() {
        *found = Some(block.token);
    }
    block_levels(block, depth + 1, found);
}

/// The levels an expression holds at `depth`.
fn expr_levels(expr: &Expr, depth: usize, found: &mut Option<Tok>) {
    match &expr.kind {
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            expr_levels(condition, depth, found);
            level(then, depth, found);
            match otherwise.as_deref() {
                Some(Else::Block(block)) => level(block, depth, found),
                Some(Else::If(chained)) => expr_levels(chained, depth, found),
                None => {}
            }
        }
        ExprKind::Match {
            scrutinee, arms, ..
        } => {
            expr_levels(scrutinee, depth, found);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    expr_levels(guard, depth, found);
                }
                match &arm.body {
                    ArmBody::Block(block) => level(block, depth, found),
                    ArmBody::Expr(body) if one_call(body) => expr_levels(body, depth, found),
                    ArmBody::Expr(body) => {
                        if depth + 1 > DEPTH && found.is_none() {
                            *found = Some(body.token);
                        }
                        expr_levels(body, depth + 1, found);
                    }
                }
            }
        }
        ExprKind::Trusted { body, .. } => match body {
            TrustedBody::Block(block) => level(block, depth, found),
            TrustedBody::Expr(inner) => expr_levels(inner, depth, found),
        },
        ExprKind::When { body, .. } => match body {
            WhenBody::Block(block) => level(block, depth, found),
            WhenBody::Expr(inner) => expr_levels(inner, depth, found),
        },
        ExprKind::Call { callee, args, .. } => {
            expr_levels(callee, depth, found);
            for (index, arg) in args.iter().enumerate() {
                match &arg.value.kind {
                    ExprKind::Closure(closure) if index + 1 == args.len() => {
                        closure_levels(closure, None, found);
                    }
                    _ => expr_levels(&arg.value, depth, found),
                }
            }
        }
        ExprKind::Closure(closure) => closure_levels(closure, Some(depth), found),
        ExprKind::Paren(inner)
        | ExprKind::Dbg(inner)
        | ExprKind::Fail(inner)
        | ExprKind::Field { base: inner, .. }
        | ExprKind::Unary { operand: inner, .. }
        | ExprKind::Marked { inner, .. } => expr_levels(inner, depth, found),
        ExprKind::Return(value) => {
            if let Some(value) = value {
                expr_levels(value, depth, found);
            }
        }
        ExprKind::Index { base, index } => {
            expr_levels(base, depth, found);
            expr_levels(index, depth, found);
        }
        ExprKind::Binary { left, right, .. } => {
            expr_levels(left, depth, found);
            expr_levels(right, depth, found);
        }
        ExprKind::Quantifier { range, body, .. } => {
            expr_levels(range, depth, found);
            expr_levels(body, depth, found);
        }
        ExprKind::Literal(_)
        | ExprKind::Path { .. }
        | ExprKind::SelfValue
        | ExprKind::Todo(_)
        | ExprKind::Break(_)
        | ExprKind::Continue(_) => {}
    }
}

/// A closure's levels: its block body a nesting root of its own when it is a call's last
/// argument, a level at `depth` otherwise.
fn closure_levels(
    closure: &crate::syntax::ast::Closure,
    depth: Option<usize>,
    found: &mut Option<Tok>,
) {
    match (&closure.body, depth) {
        (ClosureBody::Block(block), None) => block_levels(block, 0, found),
        (ClosureBody::Block(block), Some(depth)) => level(block, depth, found),
        (ClosureBody::Short(body), depth) => expr_levels(body, depth.unwrap_or(0), found),
    }
}

/// Whether an arm's expression body is one call with its markers.
fn one_call(body: &Expr) -> bool {
    match &body.kind {
        ExprKind::Call { .. } => true,
        ExprKind::Marked { inner, .. } => matches!(inner.kind, ExprKind::Call { .. }),
        _ => false,
    }
}
