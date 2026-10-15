//! Elaborating patterns: the tests of match arms and `let ... else`, the bindings a matched
//! pattern makes, and the cursor and element bindings of `for` loops.

use std::collections::HashMap;

use super::expr::{Out, literal};
use super::stmt::ArmEnd;
use super::{Elab, Elaborated, Fault, Held, Scope};
use crate::check::conform::Candidate;
use crate::check::sig::SigKind;
use crate::check::tir::{
    Compared, LocalId, TArm, TBlock, TExpr, TPattern, TPatternKind, ValueTest,
};
use crate::check::ty::{Arg, Scalar, Ty};
use crate::nir::{
    BinaryOp, BlockId, Constant, FnRef, Jump, NTy, Op, Operand, Place, Span, Terminator, Value,
};
use crate::syntax::ast::{Mode, Tok};

/// A place being matched and whether its parts are owned, so that bindings move them.
#[derive(Clone)]
pub struct Scrutinee {
    pub place: Place,
    pub owned: bool,
}

impl Elab<'_, '_> {
    /// The scrutinee of a match or `let`: a place, a Copy value or part as its own root, or a
    /// temporary in a slot.
    pub fn scrutinee(&mut self, value: &TExpr) -> Elaborated<Option<Scrutinee>> {
        Ok(match self.expr(value)? {
            Out::Never => None,
            Out::Place(place) => {
                let owned = self.b.is_slot(place.root);
                Some(Scrutinee { place, owned })
            }
            Out::Operand(operand) => {
                let owned = !self.ck.nir_copy(&value.ty);
                let place = match operand {
                    Operand::Value(root) if !owned => Place::of(root),
                    Operand::Value(root) if self.b.ty(root).is_reference() => Place::of(root),
                    other => self.temporary(other, &value.ty, value.token),
                };
                let owned = owned && self.b.is_slot(place.root);
                Some(Scrutinee { place, owned })
            }
        })
    }

    pub fn match_expr(
        &mut self,
        expr: &TExpr,
        scrutinee: &TExpr,
        arms: &[TArm],
    ) -> Elaborated<Out> {
        let span = Self::span(expr.token);
        let Some(matched) = self.scrutinee(scrutinee)? else {
            return Ok(Out::Never);
        };
        let slots = arms
            .iter()
            .map(|arm| self.alternative_slots(arm, &matched, span))
            .collect::<Elaborated<Vec<_>>>()?;
        let entry_moved = self.moved.clone();
        let mut ends: Vec<ArmEnd> = Vec::new();
        let last = arms.len().saturating_sub(1);
        for (index, arm) in arms.iter().enumerate() {
            if !self.b.open() {
                break;
            }
            self.moved = entry_moved.clone();
            let temps = self.temps.len();
            let next = (index < last).then(|| self.b.block());
            self.scopes.push(Scope::default());
            if binds_in_alternatives(arm) {
                self.alternatives(arm, &matched, &slots[index], next, span)?;
            } else {
                if let Some(next) = next.filter(|_| !arm.patterns.is_empty()) {
                    self.arm_tests(arm, &matched, next, span)?;
                }
                if let Some(pattern) = arm.patterns.first() {
                    self.bind_arm(pattern, &matched, arm.guard.as_ref(), next, span)?;
                }
            }
            let out = self.expr(&arm.body)?;
            let value = match out {
                Out::Place(place) => Out::Operand(self.take_place(place, &expr.ty, span)),
                other => other,
            };
            self.close_scope(arm.token)?;
            ends.push(self.arm_end(value, &expr.ty, arm.token, temps)?);
            if let Some(next) = next {
                self.b.at(next);
            }
        }
        self.join(ends, &expr.ty, span)
    }

    /// The tests of an arm's patterns, continuing when one matches, at a block joining the
    /// alternatives when there are several, and at `next` when none does.
    fn arm_tests(
        &mut self,
        arm: &TArm,
        matched: &Scrutinee,
        next: BlockId,
        span: Span,
    ) -> Elaborated<()> {
        if let [pattern] = &arm.patterns[..] {
            return self.tests(pattern, &matched.place, next, span);
        }
        let pass = self.b.block();
        let count = arm.patterns.len();
        for (index, pattern) in arm.patterns.iter().enumerate() {
            let otherwise = match index + 1 < count {
                true => self.b.block(),
                false => next,
            };
            self.tests(pattern, &matched.place, otherwise, span)?;
            if self.b.open() {
                self.b.jump(pass, Vec::new(), span);
            }
            self.b.at(otherwise);
            if index + 1 == count {
                self.b.at(pass);
            }
        }
        Ok(())
    }

    /// The slots of an arm whose alternatives bind names, one per name the first moves.
    fn alternative_slots(
        &mut self,
        arm: &TArm,
        matched: &Scrutinee,
        span: Span,
    ) -> Elaborated<Vec<(LocalId, Value)>> {
        if !binds_in_alternatives(arm) {
            return Ok(Vec::new());
        }
        let mut bound = Vec::new();
        self.bound(&arm.patterns[0], matched, &mut bound)?;
        Ok(bound
            .into_iter()
            .filter(|name| name.moved)
            .map(|name| {
                let ty = self.body.local(name.local).ty.clone();
                (name.local, self.named_slot(name.local, &ty, span))
            })
            .collect())
    }

    /// The tests, bindings and guard of an arm whose alternatives bind names, in the arm's scope.
    /// Each alternative that matches binds its names, tests the guard, if any, with the names it
    /// moves bound by `ref`, moves them into `slots`, and passes its other bindings to the arm's
    /// block in the first alternative's order. A failed test continues with the next
    /// alternative, the last one's and a false guard at `next`; without `next`, the last
    /// alternative is untested.
    fn alternatives(
        &mut self,
        arm: &TArm,
        matched: &Scrutinee,
        slots: &[(LocalId, Value)],
        next: Option<BlockId>,
        span: Span,
    ) -> Elaborated<()> {
        let body = self.body;
        let scope = self.scopes.last_mut().expect("the arm's scope");
        scope.slots.extend(slots.iter().map(|(_, slot)| *slot));
        let mut first = Vec::new();
        self.bound(&arm.patterns[0], matched, &mut first)?;
        let local_named: HashMap<&str, LocalId> = first
            .iter()
            .map(|name| (body.local(name.local).name.as_str(), name.local))
            .collect();
        let slot_of = |local: LocalId| {
            slots
                .iter()
                .find(|(moved, _)| *moved == local)
                .map(|(_, slot)| *slot)
                .expect("a slot for each name moved")
        };
        let pass = self.b.block();
        let mut params: Option<Vec<(LocalId, Value)>> = None;
        let count = arm.patterns.len();
        for (index, pattern) in arm.patterns.iter().enumerate() {
            let otherwise = match index + 1 < count {
                true => Some(self.b.block()),
                false => next,
            };
            if let Some(otherwise) = otherwise {
                self.tests(pattern, &matched.place, otherwise, span)?;
            }
            let mut bound = Vec::new();
            self.bound(pattern, matched, &mut bound)?;
            let mut values = Vec::new();
            for name in &bound {
                let local = local_named[body.local(name.local).name.as_str()];
                if name.moved && arm.guard.is_none() {
                    self.move_bound(name, slot_of(local), span);
                    continue;
                }
                let value = self.binding_value(name, span)?;
                self.held.insert(local, Held::Value(value));
                if !name.moved {
                    values.push((local, value));
                }
            }
            if let Some(guard) = &arm.guard
                && self.guard(guard, next, span)?
            {
                for name in bound.iter().filter(|name| name.moved) {
                    let local = local_named[body.local(name.local).name.as_str()];
                    self.move_bound(name, slot_of(local), span);
                }
            }
            if self.b.open() {
                let params = params.get_or_insert_with(|| {
                    values
                        .iter()
                        .map(|(local, value)| {
                            (*local, self.b.block_param(pass, self.b.ty(*value).clone()))
                        })
                        .collect()
                });
                let args = params
                    .iter()
                    .map(|(local, _)| {
                        let (_, value) = values
                            .iter()
                            .find(|(bound, _)| bound == local)
                            .expect("each alternative binds the names");
                        Operand::Value(*value)
                    })
                    .collect();
                self.b.jump(pass, args, span);
            }
            if index + 1 < count
                && let Some(otherwise) = otherwise
            {
                self.b.at(otherwise);
            }
        }
        let Some(params) = params else {
            return Ok(());
        };
        self.b.at(pass);
        for (local, param) in params {
            self.held.insert(local, Held::Value(param));
        }
        for (local, slot) in slots {
            self.held.insert(*local, Held::Slot(*slot));
        }
        Ok(())
    }

    /// Binds an arm's matched pattern and tests its guard, if any, continuing at `next` when it
    /// is false; the names the pattern moves are bound by `ref` while the guard runs and moved
    /// after it.
    fn bind_arm(
        &mut self,
        pattern: &TPattern,
        matched: &Scrutinee,
        guard: Option<&TExpr>,
        next: Option<BlockId>,
        span: Span,
    ) -> Elaborated<()> {
        let Some(guard) = guard else {
            return self.bind(pattern, matched, span);
        };
        let mut bound = Vec::new();
        self.bound(pattern, matched, &mut bound)?;
        for name in &bound {
            let value = self.binding_value(name, span)?;
            self.held.insert(name.local, Held::Value(value));
        }
        if self.guard(guard, next, span)? {
            for name in bound.iter().filter(|name| name.moved) {
                let ty = self.body.local(name.local).ty.clone();
                let slot = self.binding_slot(name.local, &ty, span);
                self.move_bound(name, slot, span);
            }
        }
        Ok(())
    }

    /// Tests a guard, continuing when it is true and at `next` when false; whether execution
    /// continues.
    fn guard(&mut self, guard: &TExpr, next: Option<BlockId>, span: Span) -> Elaborated<bool> {
        let Some(test) = self.releasing_temps(guard.token, |elab| elab.operand(guard))? else {
            return Ok(false);
        };
        let next = next.ok_or_else(|| Fault::Unsupported("a guard on the last arm".to_owned()))?;
        let pass = self.b.block();
        self.end(
            Terminator::Branch(test, Jump::to(pass), Jump::to(next)),
            span,
        );
        self.b.at(pass);
        Ok(true)
    }

    /// The tests of one pattern at a place, depth-first and left to right, branching to
    /// `otherwise` at the first that fails.
    fn tests(
        &mut self,
        pattern: &TPattern,
        place: &Place,
        otherwise: BlockId,
        span: Span,
    ) -> Elaborated<()> {
        match &pattern.kind {
            TPatternKind::Wildcard | TPatternKind::Binding(_) => {}
            TPatternKind::Failed => return Err(Fault::Failed),
            TPatternKind::Value(_, test) => {
                let value = self.read_operand(place.clone(), &pattern.ty, span);
                match test {
                    ValueTest::Eq(compared) => {
                        let compared = self.compared(compared, &pattern.ty, span)?;
                        self.test(BinaryOp::Eq, value, compared, otherwise, span);
                    }
                    ValueTest::Range(low, high, exclusive) => {
                        let low = self.compared(low, &pattern.ty, span)?;
                        self.test(BinaryOp::Ge, value.clone(), low, otherwise, span);
                        let high = self.compared(high, &pattern.ty, span)?;
                        let op = match exclusive {
                            true => BinaryOp::Lt,
                            false => BinaryOp::Le,
                        };
                        self.test(op, value, high, otherwise, span);
                    }
                }
            }
            TPatternKind::Fields(variant, fields) => {
                let mut base = place.clone();
                if let Some(index) = variant {
                    let name = self.variant_name(&pattern.ty, *index)?;
                    let test = self.emit_value(
                        Op::Is {
                            place: place.clone(),
                            ty: pattern.ty.clone(),
                            variant: name.clone(),
                        },
                        NTy::Owned(Ty::Scalar(Scalar::Bool)),
                        span,
                    );
                    let pass = self.b.block();
                    self.end(
                        Terminator::Branch(
                            Operand::Value(test),
                            Jump::to(pass),
                            Jump::to(otherwise),
                        ),
                        span,
                    );
                    self.b.at(pass);
                    base = base.step(&name);
                }
                for (name, field) in fields {
                    let at = match name.is_empty() {
                        true => base.clone(),
                        false => base.step(name),
                    };
                    self.tests(field, &at, otherwise, span)?;
                }
            }
        }
        Ok(())
    }

    /// The operand of a value a pattern compares with: a literal, or a const's `constant`.
    fn compared(&mut self, compared: &Compared, ty: &Ty, span: Span) -> Elaborated<Operand> {
        Ok(match compared {
            Compared::Literal(lit) => Operand::Literal(literal(lit, ty)?),
            Compared::Const(item) => Operand::Value(self.emit_value(
                Op::Constant(Constant::Item(*item)),
                NTy::Owned(ty.clone()),
                span,
            )),
        })
    }

    /// A comparison of a value with another, continuing when true and at `otherwise` when
    /// false.
    fn test(
        &mut self,
        op: BinaryOp,
        value: Operand,
        literal: Operand,
        otherwise: BlockId,
        span: Span,
    ) {
        let test = self.emit_value(
            Op::Binary(op, value, literal),
            NTy::Owned(Ty::Scalar(Scalar::Bool)),
            span,
        );
        let pass = self.b.block();
        self.end(
            Terminator::Branch(Operand::Value(test), Jump::to(pass), Jump::to(otherwise)),
            span,
        );
        self.b.at(pass);
    }

    fn variant_name(&self, ty: &Ty, index: usize) -> Elaborated<String> {
        let Ty::Item(item, _) = ty else {
            return Err(Fault::Unsupported(
                "a variant pattern of no enum".to_owned(),
            ));
        };
        match &self.ck.sig(*item).kind {
            SigKind::Enum { variants } => Ok(variants[index].name.clone()),
            SigKind::Layout { tags, .. } => Ok(tags[index].clone()),
            _ => Err(Fault::Unsupported(
                "a variant pattern of no enum".to_owned(),
            )),
        }
    }

    /// The bindings a matched pattern makes in written order: a Copy part by `load`, a part of
    /// an owned scrutinee by `move` into a slot, any other part by `ref`.
    pub fn bind(&mut self, pattern: &TPattern, matched: &Scrutinee, span: Span) -> Elaborated<()> {
        let mut bound = Vec::new();
        self.bound(pattern, matched, &mut bound)?;
        for name in &bound {
            if name.moved {
                let ty = self.body.local(name.local).ty.clone();
                let slot = self.binding_slot(name.local, &ty, span);
                self.move_bound(name, slot, span);
            } else {
                let value = self.binding_value(name, span)?;
                self.held.insert(name.local, Held::Value(value));
            }
        }
        Ok(())
    }

    /// The names a matched pattern binds, in written order.
    fn bound(
        &self,
        pattern: &TPattern,
        matched: &Scrutinee,
        found: &mut Vec<Bound>,
    ) -> Elaborated<()> {
        match &pattern.kind {
            TPatternKind::Binding(local) => found.push(Bound {
                local: *local,
                place: matched.place.clone(),
                moved: matched.owned && !self.ck.nir_copy(&self.body.local(*local).ty),
            }),
            TPatternKind::Fields(variant, fields) => {
                let mut base = matched.place.clone();
                if let Some(index) = variant {
                    base = base.step(&self.variant_name(&pattern.ty, *index)?);
                }
                for (name, field) in fields {
                    let place = match name.is_empty() {
                        true => base.clone(),
                        false => base.step(name),
                    };
                    let part = Scrutinee {
                        place,
                        owned: matched.owned,
                    };
                    self.bound(field, &part, found)?;
                }
            }
            TPatternKind::Wildcard | TPatternKind::Value(..) | TPatternKind::Failed => {}
        }
        Ok(())
    }

    /// The value binding a name without moving its part: a Copy part's `load`, a reference
    /// scrutinee itself, or the part's `ref`.
    fn binding_value(&mut self, name: &Bound, span: Span) -> Elaborated<Value> {
        let ty = self.body.local(name.local).ty.clone();
        let place = name.place.clone();
        if self.ck.nir_copy(&ty) {
            let Operand::Value(value) = self.read_operand(place, &ty, span) else {
                return Err(Fault::Unsupported("a Copy part read as a place".to_owned()));
            };
            return Ok(value);
        }
        if place.steps.is_empty() && !self.b.is_slot(place.root) {
            return Ok(place.root);
        }
        Ok(self.emit_value(Op::Ref(place), NTy::Ref(ty), span))
    }

    /// Moves a bound part into a slot.
    fn move_bound(&mut self, name: &Bound, slot: Value, span: Span) {
        let ty = self.body.local(name.local).ty.clone();
        let value = self.take_place(name.place.clone(), &ty, span);
        self.emit(Op::Init(Place::of(slot), value), span);
    }

    /// `let P = e`, or `let P = e else { B }` as a match with one arm and B.
    pub fn let_pattern(
        &mut self,
        token: Tok,
        pattern: &TPattern,
        matched: Scrutinee,
        otherwise: Option<&TBlock>,
    ) -> Elaborated<()> {
        let span = Self::span(token);
        if let Some(block) = otherwise {
            let fail = self.b.block();
            let moved = self.moved.clone();
            self.tests(pattern, &matched.place, fail, span)?;
            let pass = self.b.current();
            self.b.at(fail);
            self.block_expr(block, &Ty::Never)?;
            if self.b.open() {
                self.end(Terminator::Unreachable, span);
            }
            self.moved = moved;
            match pass {
                Some(pass) => self.b.at(pass),
                None => return Ok(()),
            }
        }
        self.bind(pattern, &matched, span)
    }

    /// The cursor a `for` loop obtains from its iterated expression in its form.
    pub fn cursor(
        &mut self,
        iterated: &TExpr,
        mode: Mode,
        cursor: &Ty,
    ) -> Elaborated<Option<Operand>> {
        let span = Self::span(iterated.token);
        if mode == Mode::Read && iterated.ty == *cursor {
            return self.arg(iterated, Mode::Take);
        }
        let name = match mode {
            Mode::Read => "cursor",
            Mode::Mut => "cursor_mut",
            Mode::Take => "into_cursor",
        };
        let fn_ref = self.member_function(&iterated.ty, name)?;
        let Some(receiver) = self.arg(iterated, mode)? else {
            return Ok(None);
        };
        Ok(Some(Operand::Value(self.call_value(
            fn_ref,
            vec![receiver],
            cursor,
            span,
        ))))
    }

    /// The `next` of a cursor type, its element type and whether it is a projection.
    pub fn cursor_next(&self, cursor: &Ty) -> Elaborated<(FnRef, Ty, bool)> {
        let fn_ref = self.member_function(cursor, "next")?;
        let crate::nir::Callable::Item { item, .. } = fn_ref.callable else {
            return Err(Fault::Unsupported("a cursor without next".to_owned()));
        };
        let sig = self
            .ck
            .fn_sig(item)
            .ok_or_else(|| Fault::Unsupported("a cursor's next without head".to_owned()))?;
        let subst =
            crate::check::ty::Subst::of(sig.generics.iter().map(|def| def.param), &fn_ref.args);
        let option = subst.ty(&sig.result);
        let element = option.type_arg(0).cloned().ok_or_else(|| {
            Fault::Unsupported("a cursor's next not returning an Option".to_owned())
        })?;
        Ok((fn_ref, element, sig.is_proj()))
    }

    /// The function named after a type that method resolution finds for a member name.
    fn member_function(&self, ty: &Ty, name: &str) -> Elaborated<FnRef> {
        let candidates = self.ck.method_candidates(ty, name);
        for candidate in candidates {
            if let Candidate::Function { item, subst }
            | Candidate::Method {
                function: Some((item, subst)),
                ..
            } = candidate
            {
                let key = crate::check::conform::type_key(ty);
                let type_subst = key
                    .map(|key| self.ck.type_subst(key, ty))
                    .unwrap_or_default();
                let args = self
                    .ck
                    .sig(item)
                    .generics
                    .iter()
                    .map(|def| {
                        subst
                            .get(def.param)
                            .or_else(|| type_subst.get(def.param))
                            .cloned()
                            .unwrap_or(Arg::Ty(Ty::Param(def.param)))
                    })
                    .collect();
                return Ok(FnRef::item(item, args));
            }
        }
        Err(Fault::Unsupported(format!(
            "no function {name} for a cursor"
        )))
    }

    /// Binds a `for` pattern to the payload of the element `next` yielded: writable places in
    /// the `mut` form; copies of Copy parts and read-only view bindings of other parts when
    /// `next` is a projection; and owned parts moved into slots otherwise.
    pub fn bind_element(
        &mut self,
        pattern: &TPattern,
        part: Place,
        element: &Ty,
        mode: Mode,
        projects: bool,
        span: Span,
    ) -> Elaborated<()> {
        if let TPatternKind::Binding(local) = pattern.kind
            && mode == Mode::Mut
        {
            let value = self.emit_value(Op::RefMut(part), NTy::RefMut(element.clone()), span);
            self.held.insert(local, Held::Value(value));
            return Ok(());
        }
        let matched = Scrutinee {
            place: part,
            owned: !projects,
        };
        if !projects
            && let TPatternKind::Binding(local) = pattern.kind
            && !self.ck.nir_copy(element)
        {
            let slot = self.binding_slot(local, element, span);
            let value = self.take_place(matched.place, element, span);
            self.emit(Op::Init(Place::of(slot), value), span);
            return Ok(());
        }
        self.bind(pattern, &matched, span)
    }
}

/// A name a matched pattern binds: its local, its part, and whether the part is moved out of an
/// owned scrutinee.
struct Bound {
    local: LocalId,
    place: Place,
    moved: bool,
}

/// Whether an arm has several alternatives binding names.
fn binds_in_alternatives(arm: &TArm) -> bool {
    arm.patterns.len() > 1 && arm.patterns.iter().any(binds_names)
}

fn binds_names(pattern: &TPattern) -> bool {
    match &pattern.kind {
        TPatternKind::Binding(_) => true,
        TPatternKind::Fields(_, fields) => fields.iter().any(|(_, field)| binds_names(field)),
        _ => false,
    }
}
