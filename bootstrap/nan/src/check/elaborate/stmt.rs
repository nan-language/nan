//! Elaborating statements: bindings, assignments, loops, deferred calls, conditions, and the
//! exits that run deferred calls, release slots and check contracts before leaving.

use std::collections::BTreeSet;
use std::ops::Range;

use super::expr::Out;
use super::{Deferred, Elab, Elaborated, Exit, Fault, Held, Leave, Loop, Scope};
use crate::check::db::ItemId;
use crate::check::sig::{Payload, SigKind};
use crate::check::tir::{
    LocalId, LocalKind, TBlock, TClosure, TExpr, TKind, TPattern, TPatternKind, TStmt, TStmtKind,
};
use crate::check::ty::{Args, Scalar, Subst, Ty};
use crate::nir::{
    BlockId, Callable, CheckKind, FnRef, Jump, Literal, NTy, Op, Operand, Place, Role, Span,
    Terminator, Value, is_variant_step,
};
use crate::syntax::ast::{self, Mode, Tok};

/// The state of one branch at its end: the open block, the value it yields and what it moved.
pub struct ArmEnd {
    pub block: Option<BlockId>,
    pub value: Option<Yielded>,
    pub moved: BTreeSet<Place>,
}

/// What a branch yields: a value, or a part it names in place.
pub enum Yielded {
    Value(Operand),
    Part(Place),
}

impl Elab<'_, '_> {
    /// The code of a function or test body, ending a `Unit` body with its exit.
    pub fn function_root(&mut self, root: &TExpr) -> Elaborated<()> {
        if self.projection && branches(root) {
            self.expr(&returned_in_branches(root))?;
            return Ok(());
        }
        let out = self.expr(root)?;
        if self.b.open() {
            let value = self.argument(out, root, Mode::Take);
            let end = match &root.kind {
                TKind::Block(block) => block.end,
                _ => root.token,
            };
            let value = value.filter(|operand| *operand != Operand::Literal(Literal::Unit));
            self.exit_return(value, end)?;
        }
        Ok(())
    }

    /// The code of a function returning its root expression's value: a const, a clause, an
    /// example or a transparent body.
    pub fn function_root_value(&mut self, root: &TExpr) -> Elaborated<()> {
        if let Some(value) = self.operand(root)? {
            self.release_temps(0, root.token);
            self.exit_return(Some(value), root.token)?;
        }
        Ok(())
    }

    /// The code of a closure's body, whose loops the typed body numbers after the enclosing
    /// code's.
    pub fn closure_root(&mut self, closure: &TClosure) -> Elaborated<()> {
        self.loop_base = closure.loops;
        match &closure.body.kind {
            TKind::Block(_) => self.function_root(&closure.body),
            _ => self.function_root_value(&closure.body),
        }
    }

    pub fn stmt(&mut self, stmt: &TStmt) -> Elaborated<()> {
        if !self.b.open() {
            return Ok(());
        }
        self.releasing_temps(stmt.token, |elab| elab.stmt_kind(stmt))
    }

    fn stmt_kind(&mut self, stmt: &TStmt) -> Elaborated<()> {
        let span = Self::span(stmt.token);
        match &stmt.kind {
            TStmtKind::Let {
                pattern,
                value,
                otherwise,
            } => self.let_stmt(stmt.token, pattern, value, otherwise.as_ref())?,
            TStmtKind::Var { local, value } if self.views(*local, value) => {
                if let Some(bound) = self.bound_value(value)? {
                    self.held.insert(*local, Held::Value(bound));
                }
            }
            TStmtKind::Var { local, value } if branches(value) => {
                self.branch_binding(*local, value, span)?;
            }
            TStmtKind::Var { local, value } => {
                let ty = self.body.local(*local).ty.clone();
                let slot = self.binding_slot(*local, &ty, span);
                if let Some(operand) = self.arg(value, Mode::Take)? {
                    self.emit(Op::Init(Place::of(slot), operand), span);
                    self.hold(slot);
                }
            }
            TStmtKind::Discard(value) => {
                let operand = match self.expr(value)? {
                    // A place reached through a reference, a part included, ends with its owner.
                    Out::Place(place) if self.b.ty(place.root).is_reference() => None,
                    Out::Place(place) => {
                        Some(self.take_place(place, &value.ty, Self::spanning(value)))
                    }
                    Out::Operand(operand) => Some(operand),
                    Out::Never => None,
                };
                if let Some(operand) = operand
                    && !self.ck.nir_copy(&value.ty)
                    && !self.ck.nir_view(&value.ty)
                    && let Operand::Value(discarded) = operand
                    && !self.wrapped.contains(&discarded)
                {
                    let slot = self.slot_holding(operand, &value.ty, stmt.token);
                    self.emit(Op::Release(Place::of(slot)), span);
                }
            }
            TStmtKind::While {
                condition,
                invariants,
                body,
            } => self.while_stmt(stmt.token, Some(condition), invariants, body)?,
            TStmtKind::Loop { invariants, body } => {
                self.while_stmt(stmt.token, None, invariants, body)?;
            }
            TStmtKind::For {
                pattern,
                mode,
                iterated,
                cursor,
                invariants,
                body,
            } => self.for_stmt(
                stmt.token, pattern, *mode, iterated, cursor, invariants, body,
            )?,
            TStmtKind::Defer(call) => {
                let places = deferred_places(call, self)?;
                let scope = self.scopes.last_mut().expect("a scope");
                scope.deferred.push(Deferred {
                    token: stmt.token,
                    call: call.clone(),
                    places,
                });
            }
            TStmtKind::Undefer(place) => {
                if let Some(place) = self.place(place)? {
                    for scope in &mut self.scopes {
                        scope
                            .deferred
                            .retain(|deferred| !deferred.places.contains(&place));
                    }
                }
            }
            TStmtKind::Expect(condition) => {
                if let Some(operand) = self.operand(condition)? {
                    self.emit(
                        Op::Check {
                            kind: CheckKind::Expect,
                            operand,
                            open: false,
                        },
                        span,
                    );
                }
            }
            TStmtKind::Assume(condition) => {
                let index = self.unit.conditions[&(Role::Assume, condition.token)];
                let args = self.named_operands(condition, span)?;
                let fn_ref = self.own(Role::Assume, index);
                let value = self.call_value(fn_ref, args, &Ty::Scalar(Scalar::Bool), span);
                self.emit(Op::Assume(Operand::Value(value)), span);
            }
            TStmtKind::Expr(value) => {
                self.expr(value)?;
            }
            TStmtKind::Assign { target, op, value } => {
                self.assign(stmt.token, target, *op, value)?;
            }
        }
        Ok(())
    }

    /// Elaborates a statement, or an operand whose temporaries are released once it is
    /// computed, with temporaries of its own, released at its end.
    pub fn releasing_temps<T>(
        &mut self,
        token: Tok,
        elaborate: impl FnOnce(&mut Self) -> Elaborated<T>,
    ) -> Elaborated<T> {
        let outer = std::mem::take(&mut self.temps);
        let scope = std::mem::replace(&mut self.temp_scope, self.scopes.len() - 1);
        let elaborated = elaborate(self)?;
        self.release_temps(0, token);
        self.temps = outer;
        self.temp_scope = scope;
        Ok(elaborated)
    }

    /// Releases what the temporaries created since the first `from` of the statement still
    /// hold, in reverse order of creation; past this point no place within them counts as moved.
    pub fn release_temps(&mut self, from: usize, token: Tok) {
        let temps = self.temps.split_off(from);
        self.scopes[self.temp_scope]
            .slots
            .retain(|slot| !temps.contains(slot));
        for temp in temps.iter().rev() {
            self.release_slot(*temp, Self::span(token));
        }
        self.moved.retain(|place| !temps.contains(&place.root));
    }

    /// A slot for a binding of the current scope, created before its initialiser's code; the
    /// scope releases it once `hold` records that it holds a value.
    pub fn binding_slot(&mut self, local: LocalId, ty: &Ty, span: Span) -> Value {
        let slot = self.named_slot(local, ty, span);
        self.held.insert(local, Held::Slot(slot));
        slot
    }

    /// Records that a slot of the current scope holds a value, which the scope's exits release.
    pub fn hold(&mut self, slot: Value) {
        self.scopes.last_mut().expect("a scope").slots.push(slot);
    }

    /// A slot named after a binding.
    pub fn named_slot(&mut self, local: LocalId, ty: &Ty, span: Span) -> Value {
        let name = self.body.local(local).name.clone();
        self.emit_value(
            Op::Slot {
                name: Some(name),
                ty: ty.clone(),
            },
            NTy::Owned(ty.clone()),
            span,
        )
    }

    /// Whether a `let` binding is a value: Copy, or bound to a part, a wrapped part or a place
    /// reached through a reference.
    fn binds_value(&self, local: LocalId, value: &TExpr) -> bool {
        let ty = &self.body.local(local).ty;
        self.ck.nir_copy(ty) || is_part(value) || self.borrowed_path(value)
    }

    /// Whether a `var` binding is a view binding, naming what its initialiser names: a part, or
    /// a place reached through a reference, that is not Copy.
    fn views(&self, local: LocalId, value: &TExpr) -> bool {
        let ty = &self.body.local(local).ty;
        !self.ck.nir_copy(ty) && (is_part(value) || self.borrowed_path(value))
    }

    /// Whether an expression names a place reached through a reference: a local bound to one,
    /// or a field path from such a local.
    fn borrowed_path(&self, value: &TExpr) -> bool {
        match &value.kind {
            TKind::Local(local) => matches!(self.held.get(local),
                Some(Held::Value(bound)) if self.b.ty(*bound).is_reference()),
            TKind::Field { base, .. } => self.borrowed_path(base),
            _ => false,
        }
    }

    fn let_stmt(
        &mut self,
        token: Tok,
        pattern: &TPattern,
        value: &TExpr,
        otherwise: Option<&TBlock>,
    ) -> Elaborated<()> {
        let span = Self::span(token);
        if otherwise.is_none()
            && let TPatternKind::Binding(local) = pattern.kind
        {
            if self.binds_value(local, value) {
                if let Some(bound) = self.bound_value(value)? {
                    self.held.insert(local, Held::Value(bound));
                }
                return Ok(());
            }
            if branches(value) {
                return self.branch_binding(local, value, span);
            }
            let ty = self.body.local(local).ty.clone();
            let slot = self.binding_slot(local, &ty, span);
            if let Some(operand) = self.arg(value, Mode::Take)? {
                self.emit(Op::Init(Place::of(slot), operand), span);
                self.hold(slot);
            }
            return Ok(());
        }
        let Some(scrutinee) = self.scrutinee(value)? else {
            return Ok(());
        };
        self.let_pattern(token, pattern, scrutinee, otherwise)
    }

    /// The value a `let` binding of a value takes: a Copy value, or a projection's reference.
    fn bound_value(&mut self, value: &TExpr) -> Elaborated<Option<Value>> {
        let span = Self::spanning(value);
        Ok(match self.expr(value)? {
            Out::Never => None,
            Out::Operand(Operand::Value(bound)) => Some(bound),
            Out::Operand(operand) => {
                let ty = NTy::Owned(value.ty.clone());
                let place = self.temporary(operand, &value.ty, value.token);
                Some(self.emit_value(Op::Load(place), ty, span))
            }
            Out::Place(place) if self.ck.nir_copy(&value.ty) => {
                match self.take_place(place, &value.ty, span) {
                    Operand::Value(bound) => Some(bound),
                    _ => None,
                }
            }
            Out::Place(place) => {
                Some(self.emit_value(Op::Ref(place), NTy::Ref(value.ty.clone()), span))
            }
        })
    }

    /// `P = e` and `P op= e`: P's operands, then e, then the `at_mut` call of an indexed P, then
    /// for `op=` the operator on P's value and e's, then `init` where P holds no value and
    /// `assign` where it holds one.
    fn assign(
        &mut self,
        token: Tok,
        target: &TExpr,
        op: Option<ast::BinaryOp>,
        value: &TExpr,
    ) -> Elaborated<()> {
        let span = Span {
            first: token,
            last: value.last,
        };
        let Some(pending) = self.target_operands(target)? else {
            return Ok(());
        };
        let Some(right) = self.arg(value, Mode::Take)? else {
            return Ok(());
        };
        let Some(place) = self.reach_target(pending)? else {
            return Ok(());
        };
        let operand = match op {
            Some(op) => {
                let current = self.read_operand(place.clone(), &target.ty, span);
                let result =
                    self.operator_values(op, current, right, &target.ty, &target.ty, span)?;
                let Some(result) = result else {
                    return Ok(());
                };
                result
            }
            None => right,
        };
        let holds = !self.moved.iter().any(|moved| is_prefix(moved, &place));
        self.moved.retain(|moved| !is_prefix(&place, moved));
        self.emit(
            match holds {
                true => Op::Assign(place, operand),
                false => Op::Init(place, operand),
            },
            span,
        );
        Ok(())
    }

    /// The operands of an assignment target, evaluated before its right side: its place, or the
    /// object and index of an indexed place, whose `at_mut` call follows the right side.
    fn target_operands<'t>(&mut self, target: &'t TExpr) -> Elaborated<Option<Target<'t>>> {
        Ok(match &target.kind {
            TKind::Index(call) => {
                let mut args = Vec::new();
                for arg in &call.args {
                    match self.arg(&arg.expr, arg.mode)? {
                        Some(operand) => args.push(operand),
                        None => return Ok(None),
                    }
                }
                Some(Target::Indexed {
                    call,
                    span: Self::spanning(target),
                    args,
                    steps: Vec::new(),
                })
            }
            TKind::Field { base, name } => match self.target_operands(base)? {
                Some(Target::Place(place)) => Some(Target::Place(place.step(name))),
                Some(Target::Indexed {
                    call,
                    span,
                    args,
                    mut steps,
                }) => {
                    steps.push(name.clone());
                    Some(Target::Indexed {
                        call,
                        span,
                        args,
                        steps,
                    })
                }
                None => None,
            },
            _ => self.place(target)?.map(Target::Place),
        })
    }

    /// The place an assignment target reaches, through its `at_mut` call when indexed.
    fn reach_target(&mut self, target: Target) -> Elaborated<Option<Place>> {
        match target {
            Target::Place(place) => Ok(Some(place)),
            Target::Indexed {
                call,
                span,
                args,
                steps,
            } => match self.finish_call(call, None, args, &call.head.result, span, None)? {
                Out::Place(mut place) => {
                    place.steps.extend(steps);
                    Ok(Some(place))
                }
                Out::Never => Ok(None),
                Out::Operand(_) => Err(Fault::Unsupported(
                    "an indexed target without place".to_owned(),
                )),
            },
        }
    }

    fn while_stmt(
        &mut self,
        token: Tok,
        condition: Option<&TExpr>,
        invariants: &[TExpr],
        body: &TBlock,
    ) -> Elaborated<()> {
        let span = Self::span(token);
        let header = self.b.block();
        let exit = self.b.block();
        self.b.jump(header, Vec::new(), span);
        self.b.at(header);
        if let Some(condition) = condition {
            let Some(test) =
                self.releasing_temps(condition.token, |elab| elab.operand(condition))?
            else {
                return Ok(());
            };
            let start = self.b.block();
            self.end(
                Terminator::Branch(test, Jump::to(start), Jump::to(exit)),
                span,
            );
            self.b.at(start);
        }
        let moved = self.moved.clone();
        self.loops.push(Loop {
            header,
            exit,
            depth: self.scopes.len(),
        });
        self.scopes.push(Scope::default());
        self.invariants(invariants, span)?;
        for stmt in &body.stmts {
            self.stmt(stmt)?;
        }
        self.close_scope(body.end)?;
        if self.b.open() {
            self.b.jump(header, Vec::new(), Self::span(body.end));
        }
        self.loops.pop();
        self.moved = moved;
        self.b.at(exit);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn for_stmt(
        &mut self,
        token: Tok,
        pattern: &TPattern,
        mode: Mode,
        iterated: &TExpr,
        cursor: &Ty,
        invariants: &[TExpr],
        body: &TBlock,
    ) -> Elaborated<()> {
        if cursor.has_error() {
            return Err(Fault::Failed);
        }
        let span = Self::span(token);
        self.scopes.push(Scope::default());
        let slot = self.emit_value(
            Op::Slot {
                name: None,
                ty: cursor.clone(),
            },
            NTy::Owned(cursor.clone()),
            span,
        );
        let Some(obtained) = self.cursor(iterated, mode, cursor)? else {
            return Ok(());
        };
        self.emit(Op::Init(Place::of(slot), obtained), span);
        self.hold(slot);
        let header = self.b.block();
        let exit = self.b.block();
        self.b.jump(header, Vec::new(), span);
        self.b.at(header);
        let (next, element, projects) = self.cursor_next(cursor)?;
        let option = self.ck.option_ty(element.clone());
        let yielded = self.call_value(
            next,
            vec![Operand::Place(Mode::Mut, Place::of(slot))],
            &option,
            span,
        );
        let some = self.emit_value(
            Op::Is {
                place: Place::of(yielded),
                ty: option.clone(),
                variant: "Some".to_owned(),
            },
            NTy::Owned(Ty::Scalar(Scalar::Bool)),
            span,
        );
        let start = self.b.block();
        self.end(
            Terminator::Branch(Operand::Value(some), Jump::to(start), Jump::to(exit)),
            span,
        );
        self.b.at(start);
        let moved = self.moved.clone();
        self.loops.push(Loop {
            header,
            exit,
            depth: self.scopes.len(),
        });
        self.scopes.push(Scope::default());
        let part = Place::of(yielded).step("Some");
        self.bind_element(pattern, part, &element, mode, projects, span)?;
        self.invariants(invariants, span)?;
        for stmt in &body.stmts {
            self.stmt(stmt)?;
        }
        self.close_scope(body.end)?;
        if self.b.open() {
            self.b.jump(header, Vec::new(), Self::span(body.end));
        }
        self.loops.pop();
        self.moved = moved;
        self.b.at(exit);
        self.close_scope(token)?;
        Ok(())
    }

    /// The calls and checks of a loop's invariants, at the start of its body.
    fn invariants(&mut self, invariants: &[TExpr], span: Span) -> Elaborated<()> {
        for invariant in invariants {
            let index = self.unit.conditions[&(Role::Invariant, invariant.token)];
            let args = self.named_operands(invariant, span)?;
            let fn_ref = self.own(Role::Invariant, index);
            let value = self.call_value(
                fn_ref,
                args,
                &Ty::Scalar(Scalar::Bool),
                Self::span(invariant.token),
            );
            self.emit(
                Op::Check {
                    kind: CheckKind::Invariant,
                    operand: Operand::Value(value),
                    open: false,
                },
                Self::span(invariant.token),
            );
        }
        Ok(())
    }

    /// The operands passing the bindings a condition names, by read, by first mention.
    fn named_operands(&mut self, condition: &TExpr, span: Span) -> Elaborated<Vec<Operand>> {
        let mut operands = Vec::new();
        for local in super::named_bindings(condition) {
            let ty = self.body.local(local).ty.clone();
            let place = self.local_place(local)?;
            operands.push(self.read_operand(place, &ty, span));
        }
        Ok(operands)
    }

    /// Closes the innermost scope at its end: its deferred calls, then its slots' releases; the
    /// places of its slots no longer count as moved, as no later code reaches them.
    pub fn close_scope(&mut self, end: Tok) -> Elaborated<()> {
        let depth = self.scopes.len() - 1;
        if self.b.open() {
            self.leave_scopes(depth, end)?;
        }
        let scope = self.scopes.pop().expect("the scope closed");
        self.moved
            .retain(|place| !scope.slots.contains(&place.root));
        Ok(())
    }

    /// The deferred calls pending in the scopes from `depth` inward, innermost first in reverse
    /// order of registration, then the releases of their slots still holding values.
    fn leave_scopes(&mut self, depth: usize, token: Tok) -> Elaborated<()> {
        let span = Self::span(token);
        let mut deferred: Vec<Deferred> = Vec::new();
        for scope in self.scopes[depth..].iter().rev() {
            deferred.extend(scope.deferred.iter().rev().cloned());
        }
        for call in deferred {
            self.releasing_temps(call.token, |elab| elab.expr(&call.call))?;
        }
        let mut slots: Vec<Value> = Vec::new();
        for scope in self.scopes[depth..].iter().rev() {
            slots.extend(scope.slots.iter().rev().copied());
        }
        for slot in slots {
            self.release_slot(slot, span);
        }
        Ok(())
    }

    /// The type of the value at a place of struct fields.
    fn place_ty(&self, place: &Place) -> Ty {
        place
            .steps
            .iter()
            .fold(self.b.ty(place.root).ty().clone(), |ty, step| {
                self.ck.field_type(&ty, step).expect("a struct field")
            })
    }

    /// Releases a slot unless it holds nothing or a Copy or view value; of a slot with parts
    /// moved out, only the parts left.
    fn release_slot(&mut self, slot: Value, span: Span) {
        let ty = self.b.ty(slot).ty().clone();
        self.release_left(Place::of(slot), &ty, span);
    }

    /// Releases what a place still holds unless that is nothing or a Copy or view value: of a
    /// struct or variant payload with parts moved out, the parts left.
    fn release_left(&mut self, place: Place, ty: &Ty, span: Span) {
        if self.moved.iter().any(|moved| is_prefix(moved, &place))
            || self.ck.nir_copy(ty)
            || self.ck.nir_view(ty)
        {
            return;
        }
        let moved = self
            .moved
            .iter()
            .find(|moved| is_prefix(&place, moved))
            .cloned();
        let parts = match (moved, ty) {
            (Some(moved), Ty::Item(item, args)) => self.parts(&place, &moved, *item, args),
            _ => None,
        };
        match parts {
            Some(parts) => {
                for (part, part_ty) in parts {
                    self.release_left(part, &part_ty, span);
                }
            }
            None => self.emit(Op::Release(place), span),
        }
    }

    /// The parts of a struct value at a place, or of the payload of the variant a moved part
    /// lies in, with their types.
    fn parts(
        &self,
        place: &Place,
        moved: &Place,
        item: ItemId,
        args: &Args,
    ) -> Option<Vec<(Place, Ty)>> {
        let sig = self.ck.sig(item);
        let subst = Subst::of(sig.generics.iter().map(|def| def.param), args);
        match &sig.kind {
            SigKind::Struct { fields } => Some(
                fields
                    .iter()
                    .map(|field| (place.step(&field.name), subst.ty(&field.ty)))
                    .collect(),
            ),
            SigKind::Enum { variants } => {
                let step = &moved.steps[place.steps.len()];
                let variant = variants.iter().find(|variant| variant.name == *step)?;
                let payload = place.step(step);
                Some(match &variant.payload {
                    Payload::None => Vec::new(),
                    Payload::Single(ty) => vec![(payload, subst.ty(ty))],
                    Payload::Labelled(fields) => fields
                        .iter()
                        .map(|(name, ty)| (payload.step(name), subst.ty(ty)))
                        .collect(),
                })
            }
            _ => None,
        }
    }

    /// `return e`: its value, the exit of every scope, then the function's return exit.
    pub fn return_expr(&mut self, value: Option<&TExpr>, token: Tok) -> Elaborated<()> {
        if self.projection
            && let Some(value) = value.filter(|value| branches(value))
        {
            self.expr(&returned_in_branches(value))?;
            return Ok(());
        }
        let operand = match value {
            Some(value) => match self.arg(value, Mode::Take)? {
                Some(operand) => Some(operand),
                None => return Ok(()),
            },
            None => None,
        };
        let operand = operand.filter(|operand| *operand != Operand::Literal(Literal::Unit));
        self.exit_return(operand, token)
    }

    /// The return exit: deferred calls and releases of every scope, the exit's predicate checks,
    /// the ensures checks, the release of `take` parameter slots, and the terminator.
    pub fn exit_return(&mut self, value: Option<Operand>, token: Tok) -> Elaborated<()> {
        let carried = value.clone().zip(self.exit_ty.clone());
        if self.leave_when(Exit::Return, carried.clone(), token)? {
            return Ok(());
        }
        let span = Self::span(token);
        self.leave_scopes(0, token)?;
        self.exit_predicate_checks(carried, span)?;
        let ensures = std::mem::take(&mut self.exits.ensures);
        for (index, olds) in &ensures {
            let mut args = self.param_operands_except_taken(0..self.body.params.len())?;
            args.push(value.clone().unwrap_or(Operand::Literal(Literal::Unit)));
            args.extend(olds.iter().map(|old| Operand::Value(self.old_values[old])));
            let fn_ref = self.own(Role::Ensures, *index);
            let checked = self.call_value(fn_ref, args, &Ty::Scalar(Scalar::Bool), span);
            self.emit(
                Op::Check {
                    kind: CheckKind::Ensures,
                    operand: Operand::Value(checked),
                    open: false,
                },
                span,
            );
        }
        self.exits.ensures = ensures;
        self.inherited_checks(value.as_ref(), span)?;
        self.release_take_slots(span);
        self.end(Terminator::Return(value), span);
        Ok(())
    }

    /// The checks of the ensures a function inherits from the interface method it implements,
    /// reached when each requires of that method held at entry.
    fn inherited_checks(&mut self, value: Option<&Operand>, span: Span) -> Elaborated<()> {
        let Some(inherited) = self.exits.inherited.take() else {
            return Ok(());
        };
        let join = self.b.block();
        for held in &inherited.requires_values {
            let next = self.b.block();
            self.end(
                Terminator::Branch(Operand::Value(*held), Jump::to(next), Jump::to(join)),
                span,
            );
            self.b.at(next);
        }
        for clause in &inherited.ensures {
            let mut args =
                self.param_operands_except_taken(inherited.implemented.params.clone())?;
            args.push(value.cloned().unwrap_or(Operand::Literal(Literal::Unit)));
            args.extend(
                clause
                    .olds
                    .iter()
                    .map(|old| Operand::Value(inherited.old_values[old])),
            );
            let fn_ref = FnRef {
                callable: Callable::Item {
                    item: inherited.implemented.interface,
                    role: Role::Ensures,
                    index: clause.index,
                },
                args: inherited.implemented.args.clone(),
            };
            let checked = self.call_value(fn_ref, args, &Ty::Scalar(Scalar::Bool), span);
            self.emit(
                Op::Check {
                    kind: CheckKind::Ensures,
                    operand: Operand::Value(checked),
                    open: false,
                },
                span,
            );
        }
        self.end(Terminator::Jump(Jump::to(join)), span);
        self.b.at(join);
        self.exits.inherited = Some(inherited);
        Ok(())
    }

    /// The operands passing the parameters other than `take` ones by read, as ensures take them.
    /// The operands of the parameters at `positions` the function does not take.
    fn param_operands_except_taken(&mut self, positions: Range<usize>) -> Elaborated<Vec<Operand>> {
        let locals: Vec<LocalId> = self.body.params[positions]
            .iter()
            .copied()
            .filter(|local| !matches!(self.body.local(*local).kind, LocalKind::Param(Mode::Take)))
            .collect();
        let mut operands = Vec::new();
        for local in locals {
            let ty = self.body.local(local).ty.clone();
            let place = self.local_place(local)?;
            operands.push(self.read_operand(place, &ty, Self::span(self.body.local(local).token)));
        }
        Ok(operands)
    }

    fn release_take_slots(&mut self, span: Span) {
        let slots = self.take_slots.clone();
        for slot in slots.into_iter().rev() {
            self.release_slot(slot, span);
        }
    }

    /// `fail e`: its value converted to the failure type, then the failure exit.
    pub fn fail_expr(&mut self, value: &TExpr, token: Tok) -> Elaborated<()> {
        let Some(operand) = self.arg(value, Mode::Take)? else {
            return Ok(());
        };
        let failure = self.converted(operand, &value.ty, token)?;
        self.exit_fail(failure, token)
    }

    /// A failure converted into the function's failure type when its type differs.
    pub fn converted(&mut self, failure: Operand, ty: &Ty, token: Tok) -> Elaborated<Operand> {
        Ok(match self.failure_ty.clone() {
            Some(target) => self.converted_to(failure, ty, &target, Self::span(token)),
            None => failure,
        })
    }

    /// A failure of type `from` converted into `to`.
    pub fn converted_to(&mut self, failure: Operand, from: &Ty, to: &Ty, span: Span) -> Operand {
        if from == to {
            return failure;
        }
        let via = self.ck.conversion_variant(from, to);
        let value = self.emit_value(
            Op::Convert {
                value: failure,
                to: to.clone(),
                via,
            },
            NTy::Owned(to.clone()),
            span,
        );
        Operand::Value(value)
    }

    /// The failure exit: deferred calls and releases of every scope, the exit's predicate
    /// checks, the release of `take` parameter slots, and the terminator.
    pub fn exit_fail(&mut self, failure: Operand, token: Tok) -> Elaborated<()> {
        let carried = self.failure_ty.clone().map(|ty| (failure.clone(), ty));
        if self.leave_when(Exit::Fail, carried.clone(), token)? {
            return Ok(());
        }
        let span = Self::span(token);
        self.leave_scopes(0, token)?;
        self.exit_predicate_checks(carried, span)?;
        self.release_take_slots(span);
        self.end(Terminator::Fail(failure), span);
        Ok(())
    }

    /// `break` or `continue`: the exits of the scopes left, then the jump.
    pub fn leave_loop(&mut self, index: usize, breaks: bool, token: Tok) -> Elaborated<()> {
        let Some(target) = index
            .checked_sub(self.loop_base)
            .and_then(|position| self.loops.get(position))
        else {
            return Err(Fault::Unsupported("a jump to no loop".to_owned()));
        };
        let (block, depth) = match breaks {
            true => (target.exit, target.depth),
            false => (target.header, target.depth),
        };
        let outside = self.when.as_ref().is_some_and(|when| depth < when.depth);
        if outside && self.leave_when(Exit::Loop { index, breaks }, None, token)? {
            return Ok(());
        }
        self.leave_scopes(depth, token)?;
        self.b.jump(block, Vec::new(), Self::span(token));
        Ok(())
    }

    /// Within an `await when` body, an exit's part inside its region: the exits of the body's
    /// scopes, then the jump to a block of the enclosing code that continues the exit with the
    /// value it carries, of its type. Whether the exit leaves such a body.
    fn leave_when(
        &mut self,
        exit: Exit,
        carried: Option<(Operand, Ty)>,
        token: Tok,
    ) -> Elaborated<bool> {
        let Some(depth) = self.when.as_ref().map(|when| when.depth) else {
            return Ok(false);
        };
        self.leave_scopes(depth, token)?;
        let block = self.b.outer_block();
        let value = carried
            .as_ref()
            .map(|(_, ty)| self.b.block_param(block, NTy::Owned(ty.clone())));
        let args = carried.into_iter().map(|(operand, _)| operand).collect();
        self.b.jump(block, args, Self::span(token));
        let leave = Leave {
            block,
            value,
            moved: self.moved.clone(),
            exit,
            token,
        };
        self.when.as_mut().expect("a when body").leaves.push(leave);
        Ok(true)
    }

    /// Continues the exits that left an `await when` body, in the enclosing code.
    pub fn continue_leaves(&mut self, leaves: Vec<Leave>) -> Elaborated<()> {
        let (current, moved) = (self.b.current(), self.moved.clone());
        for leave in leaves {
            self.b.at(leave.block);
            self.moved = leave.moved;
            let value = leave.value.map(Operand::Value);
            match leave.exit {
                Exit::Return => self.exit_return(value, leave.token)?,
                Exit::Fail => {
                    let failure = value.expect("a failure value");
                    self.exit_fail(failure, leave.token)?;
                }
                Exit::Loop { index, breaks } => self.leave_loop(index, breaks, leave.token)?,
            }
        }
        self.moved = moved;
        if let Some(current) = current {
            self.b.at(current);
        }
        Ok(())
    }

    /// The end of a branch: its value, the release of the temporaries created since the first
    /// `temps`, its open block and moved places.
    pub fn arm_end(&mut self, out: Out, ty: &Ty, token: Tok, temps: usize) -> Elaborated<ArmEnd> {
        let value = match out {
            Out::Operand(operand) => Some(Yielded::Value(operand)),
            Out::Place(place) if self.borrowed_part(&place, ty) => Some(Yielded::Part(place)),
            Out::Place(place) => Some(Yielded::Value(self.take_place(
                place,
                ty,
                Self::span(token),
            ))),
            Out::Never => None,
        };
        self.release_temps(temps, token);
        Ok(ArmEnd {
            block: self.b.current(),
            value,
            moved: self.moved.clone(),
        })
    }

    /// Joins branches: each releases what it still holds of the parts of slots another moved,
    /// of the whole value of a variant payload part, then jumps to the join, passing its value
    /// when the branches yield one: a reference when each yields a part, which the join then
    /// names in place.
    pub fn join(&mut self, arms: Vec<ArmEnd>, ty: &Ty, span: Span) -> Elaborated<Out> {
        let live: Vec<ArmEnd> = arms.into_iter().filter(|arm| arm.block.is_some()).collect();
        if live.is_empty() {
            self.b.end(Terminator::Unreachable, span);
            return Ok(Out::Never);
        }
        let mut union: BTreeSet<Place> = BTreeSet::new();
        for arm in &live {
            union.extend(arm.moved.iter().cloned());
        }
        let released: BTreeSet<Place> = union
            .iter()
            .filter(|place| {
                self.b.is_slot(place.root)
                    && live
                        .iter()
                        .any(|arm| !arm.moved.iter().any(|moved| is_prefix(moved, place)))
            })
            .map(before_variant)
            .collect();
        let join = self.b.block();
        let yields = !matches!(ty, Ty::Unit | Ty::Never);
        let parts = yields
            && live
                .iter()
                .all(|arm| matches!(arm.value, Some(Yielded::Part(_))));
        let held = match parts {
            true => NTy::Ref(ty.clone()),
            false => NTy::Owned(ty.clone()),
        };
        let param = yields.then(|| self.b.block_param(join, held));
        for arm in live {
            self.b.at(arm.block.expect("a live branch"));
            self.moved = arm.moved.clone();
            for place in &released {
                let place_ty = self.place_ty(place);
                self.release_left(place.clone(), &place_ty, span);
                self.moved.insert(place.clone());
            }
            let args = match (yields, arm.value) {
                (true, Some(Yielded::Value(value))) => vec![value],
                (true, Some(Yielded::Part(place))) if parts => {
                    let reference = NTy::Ref(ty.clone());
                    vec![Operand::Value(self.emit_value(
                        Op::Ref(place),
                        reference,
                        span,
                    ))]
                }
                (true, Some(Yielded::Part(place))) => vec![self.take_place(place, ty, span)],
                _ => Vec::new(),
            };
            self.b.jump(join, args, span);
        }
        self.moved = union;
        self.moved.extend(released);
        self.b.at(join);
        Ok(match param {
            Some(param) if parts => Out::Place(Place::of(param)),
            Some(param) => Out::Operand(Operand::Value(param)),
            None => Out::Operand(Operand::Literal(Literal::Unit)),
        })
    }

    /// Whether a place a branch yields is a part it names in place: one that is not Copy, reached
    /// through a reference.
    pub fn borrowed_part(&self, place: &Place, ty: &Ty) -> bool {
        !self.ck.nir_copy(ty) && self.b.ty(place.root).is_reference()
    }

    /// A `let` or `var` binding of an `if` or `match` value: a view binding of the part its
    /// branches name, or else a binding owning the value they move in.
    fn branch_binding(&mut self, local: LocalId, value: &TExpr, span: Span) -> Elaborated<()> {
        match self.expr(value)? {
            Out::Never => {}
            Out::Place(place) if self.borrowed_part(&place, &value.ty) => {
                let reference = NTy::Ref(value.ty.clone());
                let bound = self.emit_value(Op::Ref(place), reference, span);
                self.held.insert(local, Held::Value(bound));
            }
            out => {
                let ty = self.body.local(local).ty.clone();
                let slot = self.binding_slot(local, &ty, span);
                if let Some(operand) = self.argument(out, value, Mode::Take) {
                    self.emit(Op::Init(Place::of(slot), operand), span);
                    self.hold(slot);
                }
            }
        }
        Ok(())
    }
}

/// An assignment target with its operands evaluated.
enum Target<'t> {
    Place(Place),
    Indexed {
        call: &'t crate::check::tir::Call,
        span: Span,
        args: Vec<Operand>,
        steps: Vec<String>,
    },
}

/// Whether `prefix` is a place `place` lies within.
pub fn is_prefix(prefix: &Place, place: &Place) -> bool {
    prefix.root == place.root && place.steps.starts_with(&prefix.steps)
}

/// A place up to its first variant step: the enum value holding a payload part.
fn before_variant(place: &Place) -> Place {
    let kept = place.steps.iter().take_while(|step| !is_variant_step(step));
    Place {
        root: place.root,
        steps: kept.cloned().collect(),
    }
}

/// Whether an expression is a part or wrapped part: a projection's result, a path of fields
/// from one, or a construction of a wrapped part.
pub fn is_part(expr: &TExpr) -> bool {
    match &expr.kind {
        TKind::Index(_) => true,
        TKind::Call(call) => call.proj,
        TKind::Construct { wrapped, .. } => *wrapped,
        TKind::Field { base, .. } => is_part(base),
        _ => false,
    }
}

/// Whether a returned expression yields its value through branches: an `if` or a `match`.
fn branches(value: &TExpr) -> bool {
    matches!(value.kind, TKind::If { .. } | TKind::Match { .. })
}

/// A returned `if` or `match` whose branches each return their value, as a projection returns
/// its result in the form its head gives.
fn returned_in_branches(value: &TExpr) -> TExpr {
    let mut returned = value.clone();
    return_branches(&mut returned);
    returned
}

/// Makes each branch of a value-yielding expression return its value.
fn return_branches(expr: &mut TExpr) {
    match &mut expr.kind {
        TKind::If {
            then, otherwise, ..
        } => {
            return_tail(then);
            if let Some(otherwise) = otherwise {
                return_branches(otherwise);
            }
        }
        TKind::Match { arms, .. } => {
            for arm in arms {
                return_branches(&mut arm.body);
            }
        }
        TKind::Block(block) => return_tail(block),
        _ if expr.ty.is_never() => return,
        _ => {
            let value = std::mem::replace(expr, TExpr::failed(expr.token));
            expr.last = value.last;
            expr.kind = TKind::Return(Some(Box::new(value)));
        }
    }
    expr.ty = Ty::Never;
}

/// Makes the value a block yields, its last statement's, return.
fn return_tail(block: &mut TBlock) {
    if let Some(TStmt {
        kind: TStmtKind::Expr(tail),
        ..
    }) = block.stmts.last_mut()
    {
        return_branches(tail);
    }
}

/// The places a deferred call passes as its receiver and arguments.
fn deferred_places(call: &TExpr, elab: &mut Elab) -> Elaborated<Vec<Place>> {
    let TKind::Call(call) = &call.kind else {
        return Ok(Vec::new());
    };
    let mut places = Vec::new();
    for arg in &call.args {
        if let Some(place) = place_path(&arg.expr, elab)? {
            places.push(place);
        }
    }
    Ok(places)
}

/// The place a place expression names, without elaborating code.
fn place_path(expr: &TExpr, elab: &mut Elab) -> Elaborated<Option<Place>> {
    Ok(match &expr.kind {
        TKind::Local(local) => Some(elab.local_place(*local)?),
        TKind::Field { base, name } => place_path(base, elab)?.map(|place| place.step(name)),
        _ => None,
    })
}
