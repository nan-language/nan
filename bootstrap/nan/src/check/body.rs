//! Bodies: the typing of the statements and expressions of every body an item holds, the rules
//! each reports, and the typed form ownership, digests and limits read.

mod call;
mod expr;
mod pattern;
mod retired;

use std::collections::HashSet;
use std::rc::Rc;

use super::Checker;
use super::db::{ItemId, ModuleId};
use super::decl::fold;
use super::resolve::PathError;
use super::sig::{FnSig, GenericDef, Problems, SigKind};
use super::tir::{
    ClauseRole, Local, LocalId, LocalKind, TBlock, TBody, TClause, TExample, TExpr, TKind,
    TPattern, TPatternKind, TStmt, TStmtKind,
};
use super::ty::{Param, Scalar, Ty};
use super::unused::{Binding, Usage};
use crate::diagnostic::{Code, Diagnostic, Fix, Offer};
use crate::syntax::ast::{self, ItemKind, Mode, Tok};

/// The function, closure or other body whose `return`, `fail` and `try` statements act on.
#[derive(Clone, Debug)]
pub struct Frame {
    pub result: Ty,
    /// F: the failure type `fail` and `try` convert to, none when nothing may fail.
    pub failure: Option<Ty>,
    /// Whether every failure converts unchanged, as in a test body.
    pub test: bool,
    /// The closure index, 0 for the item's own body.
    pub closure: u32,
    pub task: bool,
    /// The capture list of a closure: name, mode and token.
    pub listed: Vec<(String, Mode, Tok)>,
    /// The locals of enclosing frames this frame reaches, and the capture local standing for each.
    pub captures: Vec<(LocalId, LocalId)>,
    /// The index in `loops` where this frame's loops start.
    pub loops: usize,
    /// The index in `scopes` where this frame's scopes start.
    pub scopes: usize,
    /// Whether the body holds a `todo` expression.
    pub todo: bool,
    /// The type of the first failure the body may raise.
    pub may_fail: Option<Ty>,
    /// Whether the failures the body may raise have several types.
    pub several_failures: bool,
    /// Whether the body may wait other than through its own parameters.
    pub may_wait: bool,
}

impl Frame {
    /// Records a failure the body may raise.
    pub fn raise(&mut self, failure: &Ty) {
        match &self.may_fail {
            Some(first) => self.several_failures |= first != failure,
            None => self.may_fail = Some(failure.clone()),
        }
    }
}

/// The `fails` and `uses` clauses of a function head or closure as written, which its body's
/// effects judge.
pub struct Clauses<'h> {
    /// The token after which a missing `fails` clause is written: the result type's last, or
    /// the `)` closing the parameters.
    pub after: Tok,
    pub fails: Option<&'h ast::TypeExpr>,
    pub uses: Option<&'h ast::Uses>,
    /// The head's last token.
    pub last: Tok,
    /// Whether a missing clause has an exact fix.
    pub fixable: bool,
}

impl<'h> Clauses<'h> {
    pub fn of(head: &'h ast::Head, fixable: bool) -> Clauses<'h> {
        Clauses::written(
            head.close,
            head.result.as_ref(),
            head.fails.as_ref(),
            head.uses.as_ref(),
            head.limits.as_ref(),
            fixable,
        )
    }

    pub fn of_closure(closure: &'h ast::Closure) -> Clauses<'h> {
        Clauses::written(
            closure.close,
            closure.result.as_ref(),
            closure.fails.as_ref(),
            closure.uses.as_ref(),
            closure.limits.as_ref(),
            true,
        )
    }

    fn written(
        close: Tok,
        result: Option<&'h ast::TypeExpr>,
        fails: Option<&'h ast::TypeExpr>,
        uses: Option<&'h ast::Uses>,
        limits: Option<&'h ast::Limits>,
        fixable: bool,
    ) -> Clauses<'h> {
        let after = result.map_or(close, |result| result.last);
        let limited = limits
            .and_then(|limits| limits.limits.last())
            .map(|limit| limit.size.unwrap_or(limit.token));
        let last = limited
            .or(uses.map(|uses| uses.word.token))
            .or(fails.map(|fails| fails.last))
            .unwrap_or(after);
        Clauses {
            after,
            fails,
            uses,
            last,
            fixable: fixable
                && !matches!(
                    result,
                    Some(ast::TypeExpr {
                        kind: ast::TypeKind::Fn(_),
                        ..
                    })
                ),
        }
    }
}

/// What a closure argument is written as, beyond its parameter's function type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Written {
    /// The body closure of a `task::scope` call.
    ScopeBody,
    /// The task closure of a lexical spawn, in the scope whose body closure is numbered.
    Lent(u32),
    Other,
}

/// The final expression statement of a block whose value is unused.
pub struct Tail {
    pub token: Tok,
    pub last: Tok,
    pub ty: Ty,
}

#[derive(Clone, Debug)]
pub struct Loop {
    pub label: Option<String>,
    /// Whether a `break` names this loop.
    pub broken: bool,
    /// Whether a `break` or `continue` names its label.
    pub label_used: bool,
}

/// What the code being typed may contain, by where it lies.
#[derive(Clone, Copy, Debug, Default)]
pub struct Context {
    /// A contract term: requires, ensures, decreases, invariant, assume, predicate or `where`.
    pub contract: bool,
    /// An ensures clause, where `result` and `old` are read.
    pub ensures: bool,
    /// A requires clause.
    pub requires: bool,
    pub example: bool,
    /// Inside trusted code.
    pub trusted: bool,
    /// Inside the body of `await when`.
    pub when: bool,
    /// Inside `old(...)`.
    pub old: bool,
    /// An expect condition or contract term, where quantifiers are legal.
    pub quantifiers: bool,
}

pub struct Body<'c, 'a> {
    pub ck: &'c Checker<'a>,
    pub item: ItemId,
    pub module: ModuleId,
    pub generics: Vec<GenericDef>,
    pub self_param: Option<Param>,
    pub locals: Vec<Local>,
    /// The parameters of the function or test, in order.
    pub params: Vec<LocalId>,
    /// Lexical scopes of bound names.
    pub scopes: Vec<Vec<LocalId>>,
    pub frames: Vec<Frame>,
    pub loops: Vec<Loop>,
    pub closures: u32,
    pub cx: Context,
    /// Whether the body has minted its root grant.
    pub minted: bool,
    /// The type of `result` in an ensures clause.
    pub result: Option<Ty>,
    /// Whether exhaustiveness counts only the declared variants of open enums.
    pub closed: std::cell::Cell<bool>,
    /// The names the pattern being typed binds so far.
    pub bound_names: Vec<String>,
    /// The closures that are the body closures of `task::scope` calls.
    pub scope_bodies: Vec<u32>,
    /// The lexical spawns whose `task::scope` call is still being typed: its body closure, the
    /// child's failure type and the spawn's token.
    pub lexical_spawns: Vec<(u32, Ty, Tok)>,
    /// The first and last tokens of the expression in statement position being typed.
    pub statement: Option<(Tok, Tok)>,
    pub clauses: Vec<TClause>,
    pub examples: Vec<TExample>,
    /// What typing records of the use of the body's bindings.
    pub usage: Usage,
    /// The view bindings of a part, and those of a wrapped part, which projections return and
    /// `Some`, `Ok` and `Err` wrap unmarked.
    pub part_bindings: HashSet<LocalId>,
    pub wrapped_bindings: HashSet<LocalId>,
    /// The view bindings of what a read parameter holds, which `Some`, `Ok` and `Err` wrap
    /// unmarked.
    pub read_views: HashSet<LocalId>,
    /// The generic scope and the item being typed that the body replaced in the checker.
    enclosing: (Vec<GenericDef>, Option<ItemId>),
}

impl Drop for Body<'_, '_> {
    fn drop(&mut self) {
        let (scope, within) = std::mem::take(&mut self.enclosing);
        *self.ck.scope.borrow_mut() = scope;
        self.ck.within.set(within);
    }
}

impl<'c, 'a> Body<'c, 'a> {
    pub fn new(ck: &'c Checker<'a>, item: ItemId, generics: Vec<GenericDef>) -> Body<'c, 'a> {
        let self_param = generics
            .iter()
            .find(|def| def.param.is_self())
            .map(|def| def.param);
        let enclosing = (
            ck.scope.replace(generics.clone()),
            ck.within.replace(Some(item)),
        );
        Body {
            ck,
            item,
            module: ck.db.module_of(item),
            generics,
            self_param,
            locals: Vec::new(),
            params: Vec::new(),
            scopes: vec![Vec::new()],
            frames: Vec::new(),
            loops: Vec::new(),
            closures: 0,
            cx: Context::default(),
            minted: false,
            result: None,
            closed: std::cell::Cell::new(false),
            bound_names: Vec::new(),
            scope_bodies: Vec::new(),
            lexical_spawns: Vec::new(),
            statement: None,
            clauses: Vec::new(),
            examples: Vec::new(),
            usage: Usage::default(),
            part_bindings: HashSet::new(),
            wrapped_bindings: HashSet::new(),
            read_views: HashSet::new(),
            enclosing,
        }
    }

    pub fn report(&self, token: Tok, code: Code) {
        self.ck.report(self.item, token, code);
    }

    /// Types something only to judge it: returns what typing it gives and the diagnostics it
    /// finds on the body's item, leaving the body as it was.
    pub fn probe<T>(&mut self, typing: impl FnOnce(&mut Self) -> T) -> (T, Vec<Diagnostic>) {
        let saved = (
            self.locals.clone(),
            self.scopes.clone(),
            self.frames.clone(),
            self.closures,
            self.minted,
            self.scope_bodies.clone(),
            self.lexical_spawns.clone(),
            self.usage.clone(),
        );
        let ck = self.ck;
        let probed = ck.heard(self.item, || typing(self));
        (
            self.locals,
            self.scopes,
            self.frames,
            self.closures,
            self.minted,
            self.scope_bodies,
            self.lexical_spawns,
            self.usage,
        ) = saved;
        probed
    }

    /// Records the member a name of the body names.
    pub fn name_member(&self, token: Tok, member: super::Member) {
        self.ck
            .members_named
            .borrow_mut()
            .push((self.item, token, member));
    }

    /// Records the members a method resolution candidate names, reporting a function not visible
    /// from the body's module.
    pub fn name_candidate(&self, token: Tok, candidate: &super::conform::Candidate, name: &str) {
        match candidate {
            super::conform::Candidate::Function { item, .. } => {
                if !self.ck.db.visible(self.module, *item) {
                    self.report(token, Code::NamePrivate);
                }
                self.name_member(token, super::Member::Function(*item));
            }
            super::conform::Candidate::Method {
                bound, function, ..
            } => {
                self.name_member(
                    token,
                    super::Member::Method(bound.interface, name.to_owned()),
                );
                if let Some((function, _)) = function {
                    self.name_member(token, super::Member::Function(*function));
                }
            }
        }
    }

    /// Reports a diagnostic on the tokens `first..=last` with the fixes its rule names.
    pub fn report_fixed(&self, construct: (Tok, Tok), code: Code, fixes: Vec<Fix>) {
        self.ck
            .report_fixed(self.item, construct, code, fixes, Offer::Each);
    }

    pub fn report_path(&self, error: PathError) {
        match error.code {
            Some(Code::NameUnknown) => self.unknown_path(error.token, None),
            Some(code) => self.ck.report_problem(self.item, error.token, code),
            None => {}
        }
    }

    /// Reports the unknown path holding `token`, its candidates the names in scope, the items its
    /// module may name and the core vocabulary, or `stub` when none is near.
    pub fn unknown_path(&self, token: Tok, stub: Option<String>) {
        let mut candidates: Vec<String> = self
            .scopes
            .iter()
            .flatten()
            .map(|local| self.locals[local.0 as usize].name.clone())
            .collect();
        candidates.extend(self.ck.item_candidates(self.module));
        candidates.extend(super::resolve::core_vocabulary().map(str::to_owned));
        let span = self.ck.path_span(self.item, token);
        self.ck.report_unknown(self.item, span, candidates, stub);
    }

    /// Reports an unknown member of a type, its candidates the type's members the body's module
    /// may name, or `stub` when none is near.
    pub fn unknown_member(&self, name: &ast::Name, ty: &Ty, stub: Option<String>) {
        let candidates = self.ck.member_candidates(self.module, ty);
        self.ck
            .report_unknown(self.item, (name.token, name.token), candidates, stub);
    }

    pub fn report_mismatch(&self, token: Tok, expected: &Ty, found: &Ty) {
        self.ck.report_expected(
            self.item,
            token,
            Code::TypeMismatch,
            self.ck.show(expected, self.module),
            self.ck.show(found, self.module),
        );
    }

    pub fn text(&self, token: Tok) -> &str {
        self.ck.db.item(self.item).text(token)
    }

    pub fn line(&self, token: Tok) -> u32 {
        self.ck.db.item(self.item).line(token)
    }

    pub fn frame(&self) -> &Frame {
        self.frames.last().expect("a body has a frame")
    }

    pub fn push_frame(&mut self, frame: Frame) {
        self.frames.push(frame);
    }

    pub fn function_frame(result: Ty, failure: Option<Ty>) -> Frame {
        Frame {
            result,
            failure,
            test: false,
            closure: 0,
            task: false,
            listed: Vec::new(),
            captures: Vec::new(),
            loops: 0,
            scopes: 0,
            todo: false,
            may_fail: None,
            several_failures: false,
            may_wait: false,
        }
    }

    /// Declares a local in the innermost scope, after the rules on its name.
    pub fn declare(&mut self, name: &ast::Name, ty: Ty, kind: LocalKind) -> LocalId {
        self.check_local_name(name, &kind);
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(Local {
            name: name.text.clone(),
            ty,
            kind,
            token: name.token,
            captured: None,
            closure: self.frame().closure,
        });
        self.scopes
            .last_mut()
            .expect("a scope to declare in")
            .push(id);
        id
    }

    /// The rules on a binding's, parameter's or quantifier variable's name: its convention, no
    /// shadowing of a local whose scope overlaps, of a module value or core vocabulary name, and
    /// no folded clash with either.
    fn check_local_name(&self, name: &ast::Name, kind: &LocalKind) {
        if !matches!(kind, LocalKind::Capture(_)) && !super::decl::is_snake(&name.text) {
            self.report(name.token, Code::NameCase);
            self.ck
                .misnamed
                .borrow_mut()
                .push((self.item, name.token, super::decl::Case::Snake));
            return;
        }
        let visible: Vec<&Local> = self
            .scopes
            .iter()
            .flatten()
            .map(|id| &self.locals[id.0 as usize])
            .collect();
        if visible.iter().any(|local| local.name == name.text)
            || self
                .ck
                .db
                .lookup(self.module, &name.text)
                .is_some_and(|item| super::decl::in_values_namespace(self.ck.db.declaration(item)))
            || super::resolve::is_core_vocabulary(&name.text)
        {
            self.report(name.token, Code::StmtShadowing);
            return;
        }
        let folded = fold(&name.text);
        let clashes_local = visible.iter().any(|local| fold(&local.name) == folded);
        let clashes_value = self.ck.module_values(self.module).contains(&folded);
        let clashes_scalar = self.ck.db.package_of(self.item) != self.ck.db.core()
            && Scalar::ALL.iter().any(|scalar| scalar.name() == folded);
        if clashes_scalar {
            self.report(name.token, Code::NameCoreRedefined);
        } else if clashes_local || clashes_value {
            self.report(name.token, Code::NameClash);
        }
    }

    pub fn push_scope(&mut self) {
        self.scopes.push(Vec::new());
    }

    pub fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// The local a name names in scope, through the captures of the closures between.
    pub fn lookup_local(&mut self, name: &str) -> Option<LocalId> {
        let found = self
            .scopes
            .iter()
            .enumerate()
            .rev()
            .find_map(|(depth, scope)| {
                scope
                    .iter()
                    .rev()
                    .find(|id| self.locals[id.0 as usize].name == name)
                    .map(|id| (depth, *id))
            })?;
        let (depth, mut id) = found;
        for frame in 0..self.frames.len() {
            if self.frames[frame].closure == 0 || self.frames[frame].scopes <= depth {
                continue;
            }
            id = self.capture(frame, id);
        }
        Some(id)
    }

    /// The capture local standing in a closure's frame for a local of an enclosing frame.
    fn capture(&mut self, frame: usize, outer: LocalId) -> LocalId {
        if let Some((_, inner)) = self.frames[frame]
            .captures
            .iter()
            .find(|(captured, _)| *captured == outer)
        {
            return *inner;
        }
        let local = self.locals[outer.0 as usize].clone();
        let listed = self.frames[frame]
            .listed
            .iter()
            .find(|(name, _, _)| *name == local.name)
            .map(|(_, mode, _)| *mode);
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(Local {
            kind: LocalKind::Capture(listed.unwrap_or(Mode::Read)),
            captured: Some(outer),
            closure: self.frames[frame].closure,
            ..local
        });
        self.frames[frame].captures.push((outer, id));
        id
    }

    pub fn local(&self, id: LocalId) -> &Local {
        &self.locals[id.0 as usize]
    }

    /// The binding outside closures a local stands for.
    pub fn binding(&self, mut local: LocalId) -> LocalId {
        while let Some(outer) = self.local(local).captured {
            local = outer;
        }
        local
    }

    /// Records that an expression uses a local.
    pub fn note_use(&mut self, local: LocalId) {
        let binding = self.binding(local);
        self.usage.used.insert(binding);
    }

    /// The typed form, once typing ends and its unused bindings are reported.
    pub fn finish(mut self, root: Option<TExpr>) -> TBody {
        let roots: Vec<&TExpr> = root.iter().collect();
        self.ck
            .check_unused_locals(self.item, &self.locals, &self.usage, &roots);
        TBody {
            locals: std::mem::take(&mut self.locals),
            params: std::mem::take(&mut self.params),
            clauses: std::mem::take(&mut self.clauses),
            examples: std::mem::take(&mut self.examples),
            root,
        }
    }

    /// A block's statements; returns the typed block and whether it diverges.
    pub fn block(&mut self, block: &ast::Block) -> (TBlock, bool) {
        let (typed, diverges, tail) = self.tail_block(block);
        if let Some(tail) = tail {
            self.unused_result(&tail);
        }
        (typed, diverges)
    }

    /// Reports a value an expression statement leaves unused, with the fix inserting `discard`.
    pub fn unused_result(&self, tail: &Tail) {
        let fix = self.ck.fix_inserting(self.item, tail.token, "discard ");
        self.report_fixed((tail.token, tail.last), Code::StmtUnusedResult, vec![fix]);
    }

    /// A block and whether it diverges, the unused value of its final expression statement
    /// withheld for the caller to judge.
    pub fn tail_block(&mut self, block: &ast::Block) -> (TBlock, bool, Option<Tail>) {
        self.push_scope();
        let mut stmts = Vec::new();
        let mut diverged = false;
        let mut reported = false;
        let mut tail = None;
        for (index, stmt) in block.stmts.iter().enumerate() {
            if diverged && !reported {
                self.report(stmt.token, Code::StmtUnreachable);
                reported = true;
            }
            let (typed, diverges) = self.stmt(stmt);
            if let (TStmtKind::Expr(expr), ast::StmtKind::Expr(written)) = (&typed.kind, &stmt.kind)
                && !matches!(expr.ty, Ty::Unit | Ty::Never | Ty::Error)
            {
                let unused = Tail {
                    token: expr.token,
                    last: written.last,
                    ty: expr.ty.clone(),
                };
                if index + 1 == block.stmts.len() {
                    tail = Some(unused);
                } else {
                    self.unused_result(&unused);
                }
            }
            diverged |= diverges;
            stmts.push(typed);
        }
        self.pop_scope();
        let typed = TBlock {
            end: block.end,
            stmts,
        };
        (typed, diverged, tail)
    }

    /// The block body of a function or closure whose result type is `result`: one that does not
    /// diverge misses its `return`, and the unused value of its final statement is reported only
    /// when inserting `return` before that statement would not yield the result.
    pub fn result_block(&mut self, block: &ast::Block, result: &Ty) -> TBlock {
        let (typed, diverges, tail) = self.tail_block(block);
        let missing = !diverges && *result != Ty::Unit && !result.has_error();
        if missing {
            let fixes = block
                .stmts
                .last()
                .map(|last| self.ck.fix_inserting(self.item, last.token, "return "))
                .into_iter()
                .collect();
            self.report_fixed((block.end, block.end), Code::StmtMissingReturn, fixes);
        }
        if let Some(tail) = tail
            && !(missing && tail.ty == *result)
        {
            self.unused_result(&tail);
        }
        typed
    }

    /// One statement; returns its typed form and whether it diverges.
    fn stmt(&mut self, stmt: &ast::Stmt) -> (TStmt, bool) {
        let ck = self.ck;
        ck.db
            .on_line(self.line(stmt.token), || self.stmt_on_line(stmt))
    }

    fn stmt_on_line(&mut self, stmt: &ast::Stmt) -> (TStmt, bool) {
        let (kind, diverges) = match &stmt.kind {
            ast::StmtKind::Let {
                pattern,
                ty,
                value,
                otherwise,
            } => self.let_stmt(stmt, pattern, ty.as_ref(), value, otherwise.as_ref()),
            ast::StmtKind::Var { name, ty, value } => {
                let declared = ty.as_ref().map(|ty| self.type_expr(ty));
                let typed = self.initializer(value, declared.as_ref());
                let local_ty = declared.unwrap_or_else(|| typed.ty.clone());
                let diverges = typed.ty.is_never();
                let local = self.declare(name, local_ty, LocalKind::Var);
                if matches!(&typed.kind, TKind::Call(call) | TKind::Index(call) if call.proj) {
                    self.usage.parts.insert(local);
                }
                if self.is_part(&typed) {
                    self.part_bindings.insert(local);
                } else if self.is_wrapped_part(&typed) {
                    self.wrapped_bindings.insert(local);
                } else if self.is_read_view(&typed) {
                    self.read_views.insert(local);
                }
                self.usage.bindings.insert(
                    local,
                    Binding {
                        statement: stmt.token,
                        value: (value.token, value.last),
                        unit: matches!(typed.ty, Ty::Unit | Ty::Never),
                    },
                );
                (
                    TStmtKind::Var {
                        local,
                        value: typed,
                    },
                    diverges,
                )
            }
            ast::StmtKind::Discard(expr) => {
                let typed = self.expr(expr, None);
                if matches!(typed.ty, Ty::Unit | Ty::Never) {
                    let fix = self
                        .ck
                        .fix_deleting_forward(self.item, stmt.token, stmt.token);
                    self.report_fixed((stmt.token, stmt.token), Code::StmtDiscardUnit, vec![fix]);
                }
                let diverges = typed.ty.is_never();
                (TStmtKind::Discard(typed), diverges)
            }
            ast::StmtKind::While { condition, body } => {
                let condition = self.check(condition, &Ty::Scalar(Scalar::Bool));
                let (invariants, body) = self.loop_body(body, None);
                (
                    TStmtKind::While {
                        condition,
                        invariants,
                        body,
                    },
                    false,
                )
            }
            ast::StmtKind::For {
                pattern,
                iterated,
                body,
            } => (self.for_stmt(pattern, iterated, body), false),
            ast::StmtKind::Loop { label, body } => {
                if let Some(label) = label {
                    self.check_label(label);
                }
                self.loops.push(Loop {
                    label: label.as_ref().map(|label| label.text.clone()),
                    broken: false,
                    label_used: false,
                });
                let (invariants, body_block) = self.loop_body_in(body);
                let finished = self.loops.pop().expect("the loop pushed");
                if let Some(label) = label
                    && !finished.label_used
                {
                    let syntax = &self.ck.db.item(self.item).file.syntax;
                    let keyword = syntax
                        .previous(label.token as usize)
                        .unwrap_or(label.token as usize);
                    let fix = self.ck.fix_deleting(self.item, keyword as Tok, label.token);
                    self.report_fixed(
                        (keyword as Tok, label.token),
                        Code::StmtUnusedLabel,
                        vec![fix],
                    );
                }
                (
                    TStmtKind::Loop {
                        invariants,
                        body: body_block,
                    },
                    !finished.broken,
                )
            }
            ast::StmtKind::Defer(call) => (TStmtKind::Defer(self.defer(call)), false),
            ast::StmtKind::Undefer(place) => {
                let typed = self.expr(place, None);
                (TStmtKind::Undefer(typed), false)
            }
            ast::StmtKind::Expect(condition) => {
                let saved = self.cx;
                self.cx.quantifiers = true;
                let typed = self.check(condition, &Ty::Scalar(Scalar::Bool));
                self.cx = saved;
                self.check_expect_purity(condition);
                (TStmtKind::Expect(typed), false)
            }
            ast::StmtKind::Assume { condition, reason } => {
                if self.cx.trusted {
                    self.ck
                        .require(self.item, stmt.token, crate::workspace::Authority::Trusted);
                } else {
                    let written = self.ck.written(self.item, condition.token, condition.last);
                    let fix = self.ck.fix_replacing(
                        self.item,
                        stmt.token,
                        reason.token,
                        &format!("expect {written}"),
                    );
                    let construct = (stmt.token, reason.token);
                    self.report_fixed(construct, Code::ContractAssumeUntrusted, vec![fix]);
                    self.ck
                        .perform(self.item, crate::workspace::Authority::Trusted);
                }
                if reason.value.trim_matches(' ').is_empty() {
                    self.report(reason.token, Code::ContractEmptyReason);
                }
                let saved = self.cx;
                self.cx.contract = true;
                self.cx.quantifiers = true;
                let typed = self.check(condition, &Ty::Scalar(Scalar::Bool));
                self.cx = saved;
                (TStmtKind::Assume(typed), false)
            }
            ast::StmtKind::Expr(expr) => {
                let typed = self.statement_expr(expr);
                let diverges = typed.ty.is_never();
                (TStmtKind::Expr(typed), diverges)
            }
            ast::StmtKind::Assign {
                target,
                op,
                op_token,
                value,
            } => self.assign(target, *op, *op_token, value),
        };
        (
            TStmt {
                token: stmt.token,
                kind,
            },
            diverges,
        )
    }

    /// The expression of an expression statement, in statement position, its `if` and `match`
    /// yielding no value.
    fn statement_expr(&mut self, expr: &ast::Expr) -> TExpr {
        let enclosing = self.statement.replace((expr.token, expr.last));
        let typed = match &expr.kind {
            ast::ExprKind::If { .. } | ast::ExprKind::Match { .. } => self.statement_branches(expr),
            _ => self.expr_jump(expr, None, true),
        };
        self.statement = enclosing;
        typed
    }

    fn let_stmt(
        &mut self,
        stmt: &ast::Stmt,
        pattern: &ast::Pattern,
        ty: Option<&ast::TypeExpr>,
        value: &ast::Expr,
        otherwise: Option<&ast::Block>,
    ) -> (TStmtKind, bool) {
        let declared = ty.map(|ty| self.type_expr(ty));
        let typed = self.initializer(value, declared.as_ref());
        if matches!(pattern.kind, ast::PatternKind::Wildcard) {
            let syntax = &self.ck.db.item(self.item).file.syntax;
            let start = syntax.tokens[stmt.token as usize].start;
            let end = syntax.tokens[value.token as usize].start;
            let kept = if matches!(typed.ty, Ty::Unit | Ty::Never) {
                ""
            } else {
                "discard "
            };
            let fix = self.ck.fix_bytes(self.item, start, end, kept);
            self.report_fixed(
                (stmt.token, value.last),
                Code::StmtWildcardBinding,
                vec![fix],
            );
        }
        let scrutinee = declared.unwrap_or_else(|| typed.ty.clone());
        let diverges = typed.ty.is_never();
        let otherwise_block = otherwise;
        let otherwise = otherwise.map(|block| {
            let (block_typed, block_diverges) = self.block(block);
            if !block_diverges {
                self.report(stmt.token, Code::StmtElseNotDiverging);
            }
            block_typed
        });
        let typed_pattern = self.top_pattern(pattern, &scrutinee, LocalKind::Pattern);
        self.bind_views(&typed_pattern, &typed);
        if let (TPatternKind::Binding(local), None) = (&typed_pattern.kind, otherwise_block) {
            self.usage.bindings.insert(
                *local,
                Binding {
                    statement: stmt.token,
                    value: (value.token, value.last),
                    unit: matches!(typed.ty, Ty::Unit | Ty::Never),
                },
            );
        }
        if !typed.ty.has_error()
            && !scrutinee.has_error()
            && let Some(refutable) = self.refutable(&typed_pattern)
        {
            if refutable && otherwise.is_none() {
                self.report(pattern.token, Code::PatternRefutable);
            } else if let (false, Some(block)) = (refutable, otherwise_block) {
                let syntax = &self.ck.db.item(self.item).file.syntax;
                let keyword = syntax
                    .previous(block.token as usize)
                    .unwrap_or(block.token as usize);
                let fix = self.ck.fix_deleting(self.item, keyword as Tok, block.end);
                self.report_fixed(
                    (stmt.token, block.end),
                    Code::StmtIrrefutableElse,
                    vec![fix],
                );
            }
        }
        (
            TStmtKind::Let {
                pattern: typed_pattern,
                value: typed,
                otherwise,
            },
            diverges,
        )
    }

    /// Records the bindings a pattern matching `value` makes as view bindings: of a part, each
    /// of them; of a wrapped part, a binding of the whole as a wrapped part and each other as a
    /// part; of what a read parameter holds, each of them as a read view.
    pub fn bind_views(&mut self, pattern: &TPattern, value: &TExpr) {
        let mut bound = Vec::new();
        bindings_of(pattern, &mut bound);
        if self.is_part(value) {
            self.part_bindings.extend(bound);
        } else if self.is_wrapped_part(value) {
            match &pattern.kind {
                TPatternKind::Binding(local) => {
                    self.wrapped_bindings.insert(*local);
                }
                _ => self.part_bindings.extend(bound),
            }
        } else if self.is_read_view(value) {
            self.read_views.extend(bound);
        }
    }

    /// An initializer: checked against the declared type, or fixed without an expected type.
    fn initializer(&mut self, value: &ast::Expr, declared: Option<&Ty>) -> TExpr {
        match declared {
            Some(ty) => self.check(value, ty),
            None => self.expr(value, None),
        }
    }

    fn assign(
        &mut self,
        target: &ast::Expr,
        op: ast::AssignOp,
        op_token: Tok,
        value: &ast::Expr,
    ) -> (TStmtKind, bool) {
        let target_typed = self.place(target, op == ast::AssignOp::Set);
        if let Some(root) = target_typed.as_ref().and_then(root_local) {
            let binding = self.binding(root);
            self.usage.assigned.insert(binding);
            let writes_part = matches!(self.local(root).kind, LocalKind::Element(Mode::Mut))
                || self.usage.parts.contains(&binding);
            if op != ast::AssignOp::Set || writes_part {
                self.note_use(root);
            }
        }
        let Some(target_typed) = target_typed else {
            self.report(target.token, Code::StmtAssignTarget);
            let value = self.skim(value);
            return (
                TStmtKind::Assign {
                    target: TExpr::failed(target.token),
                    op: None,
                    value,
                },
                false,
            );
        };
        let value_typed = match op.binary() {
            None => self.check(value, &target_typed.ty),
            Some(binary) => {
                let result = self.binary_op(binary, op_token, &target_typed, target, value);
                if !result.ty.has_error()
                    && !target_typed.ty.has_error()
                    && result.ty != target_typed.ty
                {
                    self.report_mismatch(value.token, &target_typed.ty, &result.ty);
                }
                match result.kind {
                    TKind::Binary { right, .. } => *right,
                    _ => result,
                }
            }
        };
        let diverges = value_typed.ty.is_never();
        (
            TStmtKind::Assign {
                target: target_typed,
                op: op.binary(),
                value: value_typed,
            },
            diverges,
        )
    }

    /// The body of a `while` loop: its invariants and block, as a loop `break` may leave.
    fn loop_body(&mut self, body: &ast::LoopBlock, label: Option<String>) -> (Vec<TExpr>, TBlock) {
        self.loops.push(Loop {
            label,
            broken: false,
            label_used: false,
        });
        let typed = self.loop_body_in(body);
        self.loops.pop();
        typed
    }

    fn loop_body_in(&mut self, body: &ast::LoopBlock) -> (Vec<TExpr>, TBlock) {
        self.push_scope();
        let invariants = body
            .invariants
            .iter()
            .map(|invariant| {
                let saved = self.cx;
                self.cx.contract = true;
                self.cx.quantifiers = true;
                let typed = self.check(invariant, &Ty::Scalar(Scalar::Bool));
                self.cx = saved;
                typed
            })
            .collect();
        let (block, _) = self.block(&body.block);
        self.pop_scope();
        (invariants, block)
    }

    /// A loop label: no label of an enclosing loop of the same frame spells it.
    fn check_label(&self, label: &ast::Name) {
        let base = self.frame().loops;
        if self.loops[base..]
            .iter()
            .any(|known| known.label.as_deref() == Some(label.text.as_str()))
        {
            self.report(label.token, Code::StmtShadowing);
        }
        if !super::decl::is_snake(&label.text) {
            self.report(label.token, Code::NameCase);
            self.ck
                .misnamed
                .borrow_mut()
                .push((self.item, label.token, super::decl::Case::Snake));
        }
    }

    /// The type a type expression written in the body denotes, reporting its problems.
    pub fn type_expr(&mut self, ty: &ast::TypeExpr) -> Ty {
        let mut problems = Problems::default();
        let ctx = super::sig::TypeCtx {
            item: self.item,
            module: self.module,
            generics: &self.generics,
            self_param: self.self_param,
        };
        let resolved = self.ck.type_expr(ctx, ty, &mut problems);
        self.ck.report_problems(self.item, &problems);
        self.ck.check_value_type(self.item, &resolved, ty.token);
        resolved
    }
}

impl Checker<'_> {
    /// Types every body an item holds and reports their rules; returns the typed form of its
    /// function, test, const or contribution body.
    pub fn check_bodies(&self, item: ItemId) -> Option<TBody> {
        let sig = self.sig(item);
        let ast = self.db.ast(item).clone();
        match &ast.kind {
            ItemKind::Fn(function) => {
                let SigKind::Fn(fn_sig) = &sig.kind else {
                    return None;
                };
                Some(self.function_body(item, function, fn_sig))
            }
            ItemKind::Test(test) => Some(self.test_body(item, test)),
            ItemKind::Retired(ast::RetiredItem {
                head: Some(head),
                new: ast::RetiredNew::Rewrite { expr, .. },
                ..
            }) if self.head_rewrite_form(expr) => self.typed_rewrite(item, head, expr),
            ItemKind::Const(constant) => {
                let SigKind::Const { ty } = &sig.kind else {
                    return None;
                };
                let mut body = Body::new(self, item, Vec::new());
                body.push_frame(Body::function_frame(ty.clone(), None));
                let typed = if constant.ty.is_some() {
                    body.check(&constant.value, ty)
                } else {
                    body.expr(&constant.value, None)
                };
                Some(body.finish(Some(typed)))
            }
            ItemKind::ConstAssert(condition) => {
                let mut body = Body::new(self, item, Vec::new());
                body.push_frame(Body::function_frame(Ty::Scalar(Scalar::Bool), None));
                let typed = body.check(condition, &Ty::Scalar(Scalar::Bool));
                Some(body.finish(Some(typed)))
            }
            ItemKind::Contribute(contribution) => {
                let entry = self.contribution_entry(item, contribution);
                let mut body = Body::new(self, item, Vec::new());
                body.push_frame(Body::function_frame(entry.clone(), None));
                let typed = body.check(&contribution.value, &entry);
                Some(body.finish(Some(typed)))
            }
            ItemKind::Struct(structure) => {
                self.predicates(item, structure, &sig.generics);
                None
            }
            ItemKind::Interface(interface) => {
                if let SigKind::Interface { methods } = &sig.kind {
                    for (index, (method, method_sig)) in
                        interface.methods.iter().zip(methods).enumerate()
                    {
                        self.method_body(item, index, method, &method_sig.sig);
                    }
                }
                None
            }
            _ => None,
        }
    }

    fn function_body(&self, item: ItemId, function: &ast::FnItem, sig: &FnSig) -> TBody {
        let mut body = Body::new(self, item, sig.generics.clone());
        body.push_frame(Body::function_frame(sig.result.clone(), sig.fails.clone()));
        body.declare_params(&function.head.params, sig);
        body.clauses(&function.clauses, sig);
        let root = match &function.body {
            ast::FnBody::Block(block) => {
                let typed = body.result_block(block, &sig.result);
                Some(TExpr {
                    token: block.token,
                    last: block.end,
                    ty: Ty::Unit,
                    kind: TKind::Block(typed),
                })
            }
            ast::FnBody::Expr(expr) => Some(body.check(expr, &sig.result)),
            ast::FnBody::None => None,
        };
        if !matches!(function.body, ast::FnBody::None) {
            let frame = body.frames[0].clone();
            let token = self.db.ast(item).token;
            let entry = self.names_entry(item);
            let fixable = !entry || self.db.target == crate::workspace::Target::Host;
            let clauses = Clauses::of(&function.head, fixable);
            body.check_effects(token, &frame, &sig.fn_ty(), &clauses, entry);
        }
        body.finish(root)
    }

    /// Whether a function is the root module's function that its package's `entry` directive
    /// names, whose head forms are fixed.
    pub fn names_entry(&self, item: ItemId) -> bool {
        let module = self.db.module_of(item);
        let data = self.db.module(module);
        let package = self.db.package(data.package);
        let ItemKind::Fn(function) = &self.db.ast(item).kind else {
            return false;
        };
        data.root
            && function.label.owner.is_none()
            && package
                .source
                .manifest
                .as_ref()
                .and_then(|manifest| manifest.entry.as_ref())
                .is_some_and(|(name, _)| *name == function.label.name.text)
    }

    /// Whether a function is its package's entry function: the one the `entry` directive names,
    /// in a program declaring exactly one concrete target.
    pub fn is_entry(&self, item: ItemId) -> bool {
        let package = self.db.package(self.db.package_of(item));
        let program = package.source.manifest.as_ref().is_some_and(|manifest| {
            matches!(&manifest.targets[..], [target] if *target != crate::workspace::Target::Any)
        });
        program && self.names_entry(item)
    }

    fn method_body(&self, item: ItemId, index: usize, method: &ast::Method, sig: &FnSig) {
        let mut body = Body::new(self, item, sig.generics.clone());
        body.push_frame(Body::function_frame(sig.result.clone(), sig.fails.clone()));
        body.declare_params(&method.head.params, sig);
        body.clauses(&method.clauses, sig);
        let Some(block) = &method.body else {
            let typed = body.finish(None);
            self.method_bodies
                .borrow_mut()
                .insert((item, index), Rc::new(typed));
            return;
        };
        let typed = body.result_block(block, &sig.result);
        let frame = body.frames[0].clone();
        let clauses = Clauses::of(&method.head, true);
        body.check_effects(method.token, &frame, &sig.fn_ty(), &clauses, true);
        let typed = body.finish(Some(TExpr {
            token: block.token,
            last: block.end,
            ty: Ty::Unit,
            kind: TKind::Block(typed),
        }));
        self.check_flow(item, &typed, method.proj);
        self.check_float_use(item, &typed);
        self.method_bodies
            .borrow_mut()
            .insert((item, index), Rc::new(typed));
    }

    fn test_body(&self, item: ItemId, test: &ast::TestItem) -> TBody {
        let mut body = Body::new(self, item, Vec::new());
        let mut frame = Body::function_frame(Ty::Unit, None);
        frame.test = true;
        body.push_frame(frame);
        for param in test.params.iter().flatten() {
            if let ast::ParamKind::Named { name, ty } = &param.kind {
                let ty = body.type_expr(ty);
                let id = body.declare(name, ty, LocalKind::Param(param.mode));
                body.params.push(id);
            }
        }
        let (typed, _) = body.block(&test.body);
        body.finish(Some(TExpr {
            token: test.body.token,
            last: test.body.end,
            ty: Ty::Unit,
            kind: TKind::Block(typed),
        }))
    }

    /// The predicates of a struct: bool contract terms over its fields.
    fn predicates(&self, item: ItemId, structure: &ast::StructItem, generics: &[GenericDef]) {
        let SigKind::Struct { fields } = &self.sig(item).kind else {
            return;
        };
        let mut body = Body::new(self, item, generics.to_vec());
        body.push_frame(Body::function_frame(Ty::Scalar(Scalar::Bool), None));
        body.cx.contract = true;
        body.cx.quantifiers = true;
        let mut field_locals = Vec::new();
        for field in fields {
            let id = LocalId(body.locals.len() as u32);
            body.locals.push(Local {
                name: field.name.clone(),
                ty: field.ty.clone(),
                kind: LocalKind::Let,
                token: field.token,
                captured: None,
                closure: 0,
            });
            body.scopes[0].push(id);
            field_locals.push((id, field.name.clone()));
        }
        let mut conditions = Vec::new();
        for member in &structure.members {
            let predicate = match member {
                ast::StructMember::Field(field) => field.where_clause.as_ref(),
                ast::StructMember::Invariant(predicate) => Some(predicate),
            };
            if let Some(predicate) = predicate {
                conditions.push(body.check(predicate, &Ty::Scalar(Scalar::Bool)));
            }
        }
        let roots: Vec<&TExpr> = conditions.iter().collect();
        self.check_unused_locals(item, &body.locals, &body.usage, &roots);
        let typed = super::elaborate::TypedPredicates {
            body: body.finish(None),
            fields: field_locals,
            conditions,
        };
        self.typed_predicates
            .borrow_mut()
            .insert(item, Rc::new(typed));
    }

    /// The `where` conditions of an item's const parameters: bool contract terms over the const
    /// parameters.
    pub fn where_conditions(&self, item: ItemId, generics: &[ast::GenericParam]) {
        if generics.iter().all(|param| param.where_clause.is_none()) {
            return;
        }
        let defs = self.sig(item).generics.clone();
        let mut body = Body::new(self, item, defs.clone());
        body.push_frame(Body::function_frame(Ty::Scalar(Scalar::Bool), None));
        body.cx.contract = true;
        body.cx.quantifiers = true;
        let params: Vec<_> = defs
            .iter()
            .filter_map(|def| def.constant.map(|scalar| (def.param, scalar)))
            .collect();
        let mut conditions = Vec::new();
        for param in generics {
            if let Some(condition) = &param.where_clause {
                let line = self.db.item(item).line(condition.token);
                let typed = self
                    .db
                    .on_line(line, || body.check(condition, &Ty::Scalar(Scalar::Bool)));
                conditions.push(typed);
            }
        }
        let typed = super::elaborate::TypedWheres {
            body: body.finish(None),
            params,
            conditions,
        };
        self.typed_wheres.borrow_mut().insert(item, Rc::new(typed));
    }
}

impl Body<'_, '_> {
    /// Declares a head's parameters, `self` included.
    pub fn declare_params(&mut self, params: &[ast::Param], sig: &FnSig) {
        for (param, typed) in params.iter().zip(&sig.params) {
            let name = match &param.kind {
                ast::ParamKind::Named { name, .. } => name.clone(),
                ast::ParamKind::SelfValue => ast::Name {
                    text: "self".to_owned(),
                    token: param.token,
                },
            };
            let id = if param.is_self() {
                let id = LocalId(self.locals.len() as u32);
                self.locals.push(Local {
                    name: name.text,
                    ty: typed.ty.clone(),
                    kind: LocalKind::Param(param.mode),
                    token: param.token,
                    captured: None,
                    closure: 0,
                });
                self.scopes.last_mut().expect("a scope").push(id);
                id
            } else {
                self.declare(&name, typed.ty.clone(), LocalKind::Param(param.mode))
            };
            self.params.push(id);
        }
    }

    /// The clause region: requires, ensures and decreases are contract terms over the
    /// parameters; examples are checked as a parameterless function's returned expression.
    fn clauses(&mut self, clauses: &[ast::Clause], sig: &FnSig) {
        for clause in clauses {
            let ck = self.ck;
            ck.db
                .on_line(self.line(clause.token), || self.clause(clause, sig));
        }
    }

    fn clause(&mut self, clause: &ast::Clause, sig: &FnSig) {
        if clause.disputed {
            self.report(clause.token, Code::TodoDisputed);
        }
        if let ast::ClauseKind::Missing = clause.kind {
            self.report(clause.token, Code::TodoMissingSpec);
        }
        let saved = self.cx;
        let typed = match &clause.kind {
            ast::ClauseKind::Requires(term) => {
                self.cx.contract = true;
                self.cx.requires = true;
                self.cx.quantifiers = true;
                Some((
                    ClauseRole::Requires,
                    self.check(term, &Ty::Scalar(Scalar::Bool)),
                ))
            }
            ast::ClauseKind::Ensures(term) => {
                self.cx.contract = true;
                self.cx.ensures = true;
                self.cx.quantifiers = true;
                self.result = Some(sig.result.clone());
                let typed = self.check(term, &Ty::Scalar(Scalar::Bool));
                self.result = None;
                Some((ClauseRole::Ensures, typed))
            }
            ast::ClauseKind::Decreases(term) => {
                self.cx.contract = true;
                let typed = self.expr(term, None);
                let unsigned = typed
                    .ty
                    .scalar()
                    .is_some_and(|scalar| scalar.is_integer() && !scalar.is_signed());
                if !unsigned && !typed.ty.has_error() {
                    self.report_mismatch(term.token, &Ty::Scalar(Scalar::U64), &typed.ty);
                }
                Some((ClauseRole::Decreases, typed))
            }
            ast::ClauseKind::Example(example) => {
                let body = self.example(example);
                self.examples.push(TExample {
                    token: clause.token,
                    disputed: clause.disputed,
                    body,
                });
                None
            }
            ast::ClauseKind::Missing | ast::ClauseKind::Waive { .. } => None,
        };
        if let Some((role, expr)) = typed {
            self.clauses.push(TClause {
                token: clause.token,
                role,
                disputed: clause.disputed,
                expr,
            });
        }
        self.cx = saved;
    }

    /// A `for` statement: the cursor its form obtains from the iterated expression, and its
    /// pattern bound to each element.
    fn for_stmt(
        &mut self,
        pattern: &ast::Pattern,
        iterated: &ast::Expr,
        body: &ast::LoopBlock,
    ) -> TStmtKind {
        let (mode, inner) = match &iterated.kind {
            ast::ExprKind::Marked {
                failure: None,
                awaited: None,
                mode: Some((mode, _)),
                inner,
            } => (*mode, &**inner),
            _ => (Mode::Read, iterated),
        };
        let typed = match mode {
            Mode::Mut => self.mut_operand(inner, None),
            _ => self.expr(inner, None),
        };
        let (cursor, element) = self.cursor_of(&typed, mode, iterated.token);
        self.loops.push(Loop {
            label: None,
            broken: false,
            label_used: false,
        });
        self.push_scope();
        let typed_pattern = self.top_pattern(pattern, &element, LocalKind::Element(mode));
        if mode != Mode::Take && self.ck.next_projects(&cursor) {
            let mut bound = Vec::new();
            bindings_of(&typed_pattern, &mut bound);
            self.part_bindings.extend(bound);
        }
        if !element.has_error() && self.refutable(&typed_pattern) == Some(true) {
            self.report(pattern.token, Code::PatternRefutable);
        }
        let (invariants, block) = self.loop_body_in(body);
        self.pop_scope();
        self.loops.pop();
        TStmtKind::For {
            pattern: typed_pattern,
            mode,
            iterated: typed,
            cursor,
            invariants,
            body: block,
        }
    }

    /// The cursor a `for` form obtains, and the element type its `next` yields.
    fn cursor_of(&mut self, iterated: &TExpr, mode: Mode, token: Tok) -> (Ty, Ty) {
        if iterated.ty.has_error() {
            return (Ty::Error, Ty::Error);
        }
        let cursor = match mode {
            Mode::Read if self.cursor_element(&iterated.ty).is_some() => Some(iterated.ty.clone()),
            _ => {
                let name = match mode {
                    Mode::Read => "cursor",
                    Mode::Mut => "cursor_mut",
                    Mode::Take => "into_cursor",
                };
                self.ck
                    .method_candidates(&iterated.ty, name)
                    .into_iter()
                    .next()
                    .and_then(|candidate| self.candidate_result(candidate, &iterated.ty))
            }
        };
        match cursor.as_ref().and_then(|cursor| {
            self.cursor_element(cursor)
                .map(|element| (cursor.clone(), element))
        }) {
            Some(found) => found,
            None => {
                self.report(token, Code::OwnNotIterable);
                (Ty::Error, Ty::Error)
            }
        }
    }

    /// The element type a cursor type's `next(mut self) -> Option[T]` yields, when it has one
    /// stating neither `fails` nor `uses wait`.
    pub fn cursor_element(&self, cursor: &Ty) -> Option<Ty> {
        let candidate = self
            .ck
            .method_candidates(cursor, "next")
            .into_iter()
            .next()?;
        let head = self.candidate_head(candidate, cursor)?;
        let takes_mut_self =
            head.params.len() == 1 && head.params[0].is_self && head.params[0].mode == Mode::Mut;
        if !takes_mut_self || head.fails.is_some() || head.wait {
            return None;
        }
        self.ck
            .is_std(&head.result, "core::Option")
            .then(|| head.result.type_arg(0).cloned())
            .flatten()
    }

    /// The head a method candidate stands for, its type's and interface's arguments substituted.
    fn candidate_head(&self, candidate: super::conform::Candidate, self_ty: &Ty) -> Option<FnSig> {
        match candidate {
            super::conform::Candidate::Function { item, subst } => {
                let head = self.ck.fn_sig(item)?;
                Some(substituted(&head, &subst))
            }
            super::conform::Candidate::Method {
                function: Some((item, subst)),
                dynamic: false,
                ..
            } => {
                let head = self.ck.fn_sig(item)?;
                let mut full = super::conform::type_key(self_ty)
                    .map(|key| self.ck.type_subst(key, self_ty))
                    .unwrap_or_default();
                full.0.extend(subst.0);
                Some(substituted(&head, &full))
            }
            super::conform::Candidate::Method {
                bound,
                method,
                self_ty,
                ..
            } => self.ck.method_head(&bound, method, &self_ty),
        }
    }

    fn candidate_result(&self, candidate: super::conform::Candidate, self_ty: &Ty) -> Option<Ty> {
        self.candidate_head(candidate, self_ty)
            .map(|head| head.result)
    }

    /// A deferred call: the rules of deferred calls first, in order, then the call as written at
    /// the statement.
    fn defer(&mut self, call: &ast::Expr) -> TExpr {
        let (mode, postfix) = match &call.kind {
            ast::ExprKind::Marked { mode, inner, .. } => (*mode, &**inner),
            _ => (None, call),
        };
        let ast::ExprKind::Call { callee, args, .. } = &postfix.kind else {
            self.report(call.token, Code::DeferNotCall);
            return TExpr::failed(call.token);
        };
        let operand_form = |expr: &ast::Expr| defer_operand(expr);
        let receiver_ok = match &callee.kind {
            ast::ExprKind::Field { base, .. } => {
                self.type_target(base).is_some() || operand_form(base)
            }
            ast::ExprKind::Path { .. } => true,
            _ => {
                self.report(call.token, Code::DeferNotCall);
                return TExpr::failed(call.token);
            }
        };
        let args_ok = args.iter().all(|arg| {
            let value = match &arg.value.kind {
                ast::ExprKind::Marked {
                    failure: None,
                    awaited: None,
                    mode: Some(_),
                    inner,
                } => &**inner,
                _ => &arg.value,
            };
            operand_form(value)
        });
        if !receiver_ok || !args_ok {
            self.report(call.token, Code::DeferOperandForm);
            return TExpr::failed(call.token);
        }
        self.ck.muted.set(self.ck.muted.get() + 1);
        let probe = self.call(
            postfix,
            None,
            Some(call::Markers {
                mode,
                ..Default::default()
            }),
        );
        self.ck.muted.set(self.ck.muted.get() - 1);
        let TKind::Call(probed) = &probe.kind else {
            if !probe.ty.has_error() {
                self.report(call.token, Code::DeferNotCall);
            } else {
                self.call(
                    postfix,
                    None,
                    Some(call::Markers {
                        mode,
                        ..Default::default()
                    }),
                );
            }
            return TExpr::failed(call.token);
        };
        let waits = probed.head.wait
            || probed
                .args
                .iter()
                .any(|arg| matches!(&arg.expr.ty, Ty::Fn(fn_ty) if fn_ty.wait));
        let code = if waits {
            Some(Code::DeferWaits)
        } else if probed
            .head
            .fails
            .as_ref()
            .is_some_and(|fails| *fails != Ty::Never)
        {
            Some(Code::DeferFails)
        } else if !matches!(probed.head.result, Ty::Unit | Ty::Error) {
            Some(Code::DeferHasResult)
        } else {
            None
        };
        if let Some(code) = code {
            let fixes = match code {
                Code::DeferWaits => self.abandoned(call, postfix, probed).into_iter().collect(),
                _ => Vec::new(),
            };
            self.report_fixed((call.token, call.last), code, fixes);
            return probe;
        }
        self.marked_or_call(call)
    }

    /// The fix of a deferred call that waits: `take p.abandon()` in its place, when it passes its
    /// receiver `p` by `take` and the receiver's type has an abandon method.
    fn abandoned(
        &self,
        call: &ast::Expr,
        postfix: &ast::Expr,
        probed: &super::tir::Call,
    ) -> Option<Fix> {
        let receiver = probed.args.first()?;
        if receiver.mode != Mode::Take
            || self
                .ck
                .method_candidates(&receiver.expr.ty, "abandon")
                .is_empty()
        {
            return None;
        }
        let ast::ExprKind::Call { callee, .. } = &postfix.kind else {
            return None;
        };
        let ast::ExprKind::Field { base, .. } = &callee.kind else {
            return None;
        };
        let place = self.ck.written(self.item, base.token, base.last);
        Some(self.ck.fix_replacing(
            self.item,
            call.token,
            call.last,
            &format!("take {place}.abandon()"),
        ))
    }

    fn marked_or_call(&mut self, call: &ast::Expr) -> TExpr {
        match &call.kind {
            ast::ExprKind::Marked { .. } => self.marked(call, None),
            _ => self.call(call, None, None),
        }
    }

    /// An expect condition contains no `await`, `mut` or `take` marker.
    fn check_expect_purity(&self, condition: &ast::Expr) {
        if has_effect_marker(condition) {
            self.report(condition.token, Code::ContractImpureCondition);
        }
    }

    /// An example, typed as the returned expression of a function without parameters.
    fn example(&mut self, example: &ast::Expr) -> TBody {
        let mut body = Body::new(self.ck, self.item, Vec::new());
        body.push_frame(Body::function_frame(Ty::Scalar(Scalar::Bool), None));
        body.cx.example = true;
        let typed = body.check(example, &Ty::Scalar(Scalar::Bool));
        self.ck
            .check_unused_locals(self.item, &body.locals, &body.usage, &[&typed]);
        body.finish(Some(typed))
    }
}

/// A head with its type's and own generic arguments substituted.
fn substituted(head: &FnSig, subst: &super::ty::Subst) -> FnSig {
    FnSig {
        params: head
            .params
            .iter()
            .map(|param| super::sig::ParamSig {
                ty: subst.ty(&param.ty),
                ..param.clone()
            })
            .collect(),
        result: subst.ty(&head.result),
        fails: head.fails.as_ref().map(|ty| subst.ty(ty)),
        ..head.clone()
    }
}

/// The local at the root of a place.
/// Adds each binding a pattern makes to `found`.
fn bindings_of(pattern: &TPattern, found: &mut Vec<LocalId>) {
    match &pattern.kind {
        TPatternKind::Binding(local) => found.push(*local),
        TPatternKind::Fields(_, fields) => {
            for (_, field) in fields {
                bindings_of(field, found);
            }
        }
        TPatternKind::Wildcard | TPatternKind::Value(..) | TPatternKind::Failed => {}
    }
}

fn root_local(place: &TExpr) -> Option<LocalId> {
    match &place.kind {
        TKind::Local(local) => Some(*local),
        TKind::Field { base, .. } => root_local(base),
        TKind::Index(call) => root_local(&call.args.first()?.expr),
        _ => None,
    }
}

/// Whether an operand of a deferred call has a legal form: a place without index steps, a
/// literal, or a path naming a const item, a unit variant or a function item.
fn defer_operand(expr: &ast::Expr) -> bool {
    match &expr.kind {
        ast::ExprKind::Path { .. } | ast::ExprKind::SelfValue | ast::ExprKind::Literal(_) => true,
        ast::ExprKind::Field { base, .. } => defer_operand(base),
        _ => false,
    }
}

/// Whether an expression holds an `await`, `mut` or `take` marker outside closures.
fn has_effect_marker(expr: &ast::Expr) -> bool {
    use ast::ExprKind;
    match &expr.kind {
        ExprKind::Marked {
            awaited,
            mode,
            inner,
            ..
        } => awaited.is_some() || mode.is_some() || has_effect_marker(inner),
        ExprKind::Paren(inner) | ExprKind::Dbg(inner) => has_effect_marker(inner),
        ExprKind::Field { base, .. } => has_effect_marker(base),
        ExprKind::Call { callee, args, .. } => {
            has_effect_marker(callee) || args.iter().any(|arg| has_effect_marker(&arg.value))
        }
        ExprKind::Index { base, index } => has_effect_marker(base) || has_effect_marker(index),
        ExprKind::Unary { operand, .. } => has_effect_marker(operand),
        ExprKind::Binary { left, right, .. } => has_effect_marker(left) || has_effect_marker(right),
        ExprKind::When { .. } => true,
        _ => false,
    }
}
