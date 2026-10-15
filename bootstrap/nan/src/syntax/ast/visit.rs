//! Walks over expressions: each expression an expression holds, through its blocks and
//! statements, in source order.

use super::{
    ArmBody, Block, ClosureBody, Else, Expr, ExprKind, Path, StmtKind, TrustedBody, WhenBody,
};

impl Expr {
    /// Visits this expression and each one it holds, before the ones they hold.
    pub fn visit(&self, visit: &mut dyn FnMut(&Expr)) {
        visit(self);
        match &self.kind {
            ExprKind::Paren(inner)
            | ExprKind::Dbg(inner)
            | ExprKind::Fail(inner)
            | ExprKind::Field { base: inner, .. }
            | ExprKind::Unary { operand: inner, .. }
            | ExprKind::Marked { inner, .. } => inner.visit(visit),
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                condition.visit(visit);
                then.visit(visit);
                match otherwise.as_deref() {
                    Some(Else::Block(block)) => block.visit(visit),
                    Some(Else::If(chained)) => chained.visit(visit),
                    None => {}
                }
            }
            ExprKind::Match {
                scrutinee, arms, ..
            } => {
                scrutinee.visit(visit);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        guard.visit(visit);
                    }
                    match &arm.body {
                        ArmBody::Block(block) => block.visit(visit),
                        ArmBody::Expr(body) => body.visit(visit),
                    }
                }
            }
            ExprKind::Closure(closure) => match &closure.body {
                ClosureBody::Short(body) => body.visit(visit),
                ClosureBody::Block(block) => block.visit(visit),
            },
            ExprKind::Trusted { body, .. } => match body {
                TrustedBody::Expr(inner) => inner.visit(visit),
                TrustedBody::Block(block) => block.visit(visit),
            },
            ExprKind::When { body, .. } => match body {
                WhenBody::Expr(inner) => inner.visit(visit),
                WhenBody::Block(block) => block.visit(visit),
            },
            ExprKind::Quantifier { range, body, .. } => {
                range.visit(visit);
                body.visit(visit);
            }
            ExprKind::Call { callee, args, .. } => {
                callee.visit(visit);
                for arg in args {
                    arg.value.visit(visit);
                }
            }
            ExprKind::Index { base, index } => {
                base.visit(visit);
                index.visit(visit);
            }
            ExprKind::Binary { left, right, .. } => {
                left.visit(visit);
                right.visit(visit);
            }
            ExprKind::Return(value) => {
                if let Some(value) = value {
                    value.visit(visit);
                }
            }
            ExprKind::Literal(_)
            | ExprKind::Path { .. }
            | ExprKind::SelfValue
            | ExprKind::Todo(_)
            | ExprKind::Break(_)
            | ExprKind::Continue(_) => {}
        }
    }

    /// Visits each path this expression and the ones it holds write.
    pub fn visit_paths(&self, visit: &mut dyn FnMut(&Path)) {
        self.visit(&mut |expr| {
            if let ExprKind::Path { path, .. } = &expr.kind {
                visit(path);
            }
        });
    }
}

impl Block {
    /// Visits each expression the block's statements hold.
    pub fn visit(&self, visit: &mut dyn FnMut(&Expr)) {
        for stmt in &self.stmts {
            match &stmt.kind {
                StmtKind::Let {
                    value, otherwise, ..
                } => {
                    value.visit(visit);
                    if let Some(otherwise) = otherwise {
                        otherwise.visit(visit);
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
                | StmtKind::Expr(value) => value.visit(visit),
                StmtKind::While { condition, body } => {
                    condition.visit(visit);
                    for invariant in &body.invariants {
                        invariant.visit(visit);
                    }
                    body.block.visit(visit);
                }
                StmtKind::For { iterated, body, .. } => {
                    iterated.visit(visit);
                    for invariant in &body.invariants {
                        invariant.visit(visit);
                    }
                    body.block.visit(visit);
                }
                StmtKind::Loop { body, .. } => {
                    for invariant in &body.invariants {
                        invariant.visit(visit);
                    }
                    body.block.visit(visit);
                }
                StmtKind::Assign { target, value, .. } => {
                    target.visit(visit);
                    value.visit(visit);
                }
            }
        }
    }
}
