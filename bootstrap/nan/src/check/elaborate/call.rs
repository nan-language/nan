//! Elaborating calls: the callee and arguments, the predicate, requires and decreases checks a
//! call carries, failing calls with `try` and `attempt`, and checking entries.

use std::collections::HashMap;
use std::ops::Range;

use super::expr::Out;
use super::{Elab, Elaborated, Fault, Held};
use crate::check::db::ItemId;
use crate::check::sig::{FnSig, SigKind};
use crate::check::tir::{self, ClauseRole, Form, TBody, TClosure, TKind};
use crate::check::ty::{Arg, Args, Scalar, Subst, Ty};
use crate::nir::{
    Builtin, Call, Callable, Callee, CheckKind, FnRef, Function, NTy, Op, Operand, Payload, Place,
    Role, Span, Terminator,
};
use crate::syntax::ast::{FailureMarker, ItemKind, Mode, Tok};

/// What elaborating a rewrite gives: its outermost call with the callee value and arguments
/// evaluated, none when one diverges; or its value.
enum Rewritten<'c> {
    Call(Option<Evaluated<'c>>),
    Value(Out),
}

/// A call with its callee value, for a call through a function value, and its arguments
/// evaluated.
struct Evaluated<'c> {
    call: &'c tir::Call,
    function_value: Option<crate::nir::Value>,
    args: Vec<Operand>,
}

/// The requires clauses a call meets: the unit whose functions compute them, their indexes
/// there, and whether each is disputed.
struct Requires {
    callee: FnRef,
    clauses: Vec<(u32, bool)>,
    /// The positions of the call's arguments the requires functions read.
    params: Range<usize>,
}

impl Elab<'_, '_> {
    pub fn call(&mut self, call: &tir::Call, ty: &Ty, span: Span) -> Elaborated<Out> {
        let function_value = match &call.callee {
            tir::Callee::Value(value) => match self.operand(value)? {
                Some(Operand::Value(value)) => Some(value),
                Some(_) => {
                    return Err(Fault::Unsupported(
                        "a function value that is no value".to_owned(),
                    ));
                }
                None => return Ok(Out::Never),
            },
            _ => None,
        };
        let in_place = match call.callee {
            tir::Callee::Function { item } => call.proj || self.ck.reads_in_place(item),
            _ => call.proj,
        };
        let mut args = Vec::new();
        for (index, arg) in call.args.iter().enumerate() {
            // A projection's result is part of its receiver, so a receiver read from a place is
            // passed as that place, never as a copy the part would point into; so is an argument
            // an operation makes a pointer to.
            let operand = match in_place && index == 0 && arg.mode == Mode::Read {
                true => self.borrowed(&arg.expr)?,
                false => self.arg(&arg.expr, arg.mode)?,
            };
            match operand {
                Some(operand) => args.push(operand),
                None => return Ok(Out::Never),
            }
        }
        if let tir::Callee::Function { item } = &call.callee
            && matches!(self.ck.db.ast(*item).kind, ItemKind::Retired(_))
        {
            return self.forwarded(call, *item, args, ty, span);
        }
        self.finish_call(call, function_value, args, ty, span, None)
    }

    /// A call forwarded through a tombstone's old head: its arguments, `args`, in the old head's
    /// parameters, then the tombstone's rewrite over them, the call's markers on the rewrite's
    /// outermost call, all with the call's span.
    fn forwarded(
        &mut self,
        call: &tir::Call,
        tombstone: ItemId,
        args: Vec<Operand>,
        ty: &Ty,
        span: Span,
    ) -> Elaborated<Out> {
        if self.forwarding.contains(&tombstone) {
            return Err(Fault::Unsupported(
                "a cycle of forwarded rewrites".to_owned(),
            ));
        }
        let (Some(sig), Some(body)) = (self.ck.fn_sig(tombstone), self.ck.typed(tombstone)) else {
            return Err(Fault::Failed);
        };
        let Some(rewrite) = &body.root else {
            return Err(Fault::Failed);
        };
        let subst = Subst::of(sig.generics.iter().map(|def| def.param), &call.type_args);
        let mark = self.b.mark();
        let mut held = HashMap::new();
        for ((local, param), arg) in body.params.iter().zip(&call.head.params).zip(args) {
            let binding = match arg {
                Operand::Literal(literal) => Held::Literal(literal),
                Operand::Place(Mode::Take, place) => {
                    let ty = NTy::Owned(param.ty.clone());
                    let value = self.emit_value(Op::Move(place), ty, span);
                    self.taken_binding(value, &param.ty, call.token)
                }
                Operand::Place(_, place) => Held::Place(place),
                Operand::Value(value) if param.mode == Mode::Take => {
                    self.taken_binding(value, &param.ty, call.token)
                }
                Operand::Value(value) => Held::Value(value),
            };
            held.insert(*local, binding);
        }
        let mut closures: Vec<&TClosure> = Vec::new();
        super::collect_closures(rewrite, &mut closures);
        let count = closures
            .iter()
            .map(|closure| closure.index)
            .max()
            .unwrap_or(0);
        let base = self.unit.inline_closures(self.body, count);
        let (parts, nested) = {
            let builder = std::mem::replace(&mut self.b, super::Builder::new());
            let mut nested = Elab::new(self.ck, &mut *self.unit, body.as_ref(), self.role);
            nested.b = builder;
            nested.held = held;
            nested.trusted = self.trusted;
            nested.closure_base = base;
            nested.forwarding = self.forwarding.clone();
            nested.forwarding.push(tombstone);
            let parts = match &rewrite.kind {
                TKind::Call(inner) => nested.callee_and_args(inner).map(Rewritten::Call),
                _ => nested.expr(rewrite).map(Rewritten::Value),
            };
            let held = nested.scopes.swap_remove(0).slots;
            (
                parts,
                (nested.b, nested.temps, held, nested.moved, nested.wrapped),
            )
        };
        let (builder, temps, slots, moved, wrapped) = nested;
        self.b = builder;
        self.temps.extend(temps);
        self.scopes[self.temp_scope].slots.extend(slots);
        self.moved.extend(moved);
        self.wrapped.extend(wrapped);
        if count > 0 {
            let inlined = super::Inlined {
                base,
                span,
                subst: subst.clone(),
            };
            self.ck
                .closure_functions(&mut *self.unit, &body, Some(&inlined))?;
        }
        let out = match parts? {
            Rewritten::Call(None) | Rewritten::Value(Out::Never) => Out::Never,
            Rewritten::Call(Some(Evaluated {
                call: inner,
                function_value,
                args,
            })) => {
                let mut outer = inner.clone();
                outer.token = call.token;
                outer.failure = call.failure;
                outer.awaited = call.awaited;
                outer.type_args = subst.args(&inner.type_args);
                outer.head = subst.fn_ty(&inner.head);
                if let tir::Callee::Method {
                    interface, self_ty, ..
                } = &mut outer.callee
                {
                    interface.args = subst.args(&interface.args);
                    *self_ty = subst.ty(self_ty);
                }
                if call.failure.is_none() && outer.head.fails.is_some() {
                    return Err(Fault::Unsupported(
                        "a forwarded rewrite failing where its use does not".to_owned(),
                    ));
                }
                let through = call.head.fails.as_ref();
                self.finish_call(&outer, function_value, args, ty, span, through)?
            }
            Rewritten::Value(value) => match call.failure {
                Some((FailureMarker::Attempt, _)) => {
                    let value = match value {
                        Out::Place(place) => self.take_place(place, &call.head.result, span),
                        Out::Operand(operand) => operand,
                        Out::Never => unreachable!("a value"),
                    };
                    let wrapped = self.emit_value(
                        Op::Variant {
                            ty: ty.clone(),
                            variant: "Ok".to_owned(),
                            payload: Payload::Single(value),
                        },
                        NTy::Owned(ty.clone()),
                        span,
                    );
                    Out::Operand(Operand::Value(wrapped))
                }
                _ => value,
            },
        };
        self.b.since(&mark, span, &subst);
        Ok(out)
    }

    /// The binding of a `take` parameter of an inlined rewrite to the value its argument moves:
    /// the value when Copy, else a temporary of the statement holding it.
    fn taken_binding(&mut self, value: crate::nir::Value, ty: &Ty, token: Tok) -> Held {
        let place = self.temporary(Operand::Value(value), ty, token);
        match place.root == value {
            true => Held::Value(value),
            false => Held::Slot(place.root),
        }
    }

    /// The callee value and the arguments of a call, evaluated; none when one diverges.
    fn callee_and_args<'c>(&mut self, call: &'c tir::Call) -> Elaborated<Option<Evaluated<'c>>> {
        let function_value = match &call.callee {
            tir::Callee::Value(value) => match self.operand(value)? {
                Some(Operand::Value(value)) => Some(value),
                Some(_) => {
                    return Err(Fault::Unsupported(
                        "a function value that is no value".to_owned(),
                    ));
                }
                None => return Ok(None),
            },
            _ => None,
        };
        let mut args = Vec::new();
        for arg in &call.args {
            match self.arg(&arg.expr, arg.mode)? {
                Some(operand) => args.push(operand),
                None => return Ok(None),
            }
        }
        Ok(Some(Evaluated {
            call,
            function_value,
            args,
        }))
    }

    /// A call whose callee and arguments are evaluated: its checks, then the call, its failure
    /// converted first to `through` when given.
    pub fn finish_call(
        &mut self,
        call: &tir::Call,
        function_value: Option<crate::nir::Value>,
        mut args: Vec<Operand>,
        ty: &Ty,
        span: Span,
        through: Option<&Ty>,
    ) -> Elaborated<Out> {
        let callee = match (&call.callee, function_value) {
            (_, Some(value)) => Callee::Value(value),
            (
                tir::Callee::Method {
                    interface,
                    method,
                    dynamic: true,
                    ..
                },
                None,
            ) => {
                let place = match args.first() {
                    Some(Operand::Place(_, place)) => place.clone(),
                    Some(Operand::Value(value)) => Place::of(*value),
                    _ => {
                        return Err(Fault::Unsupported(
                            "a dynamic call without receiver".to_owned(),
                        ));
                    }
                };
                Callee::Dyn {
                    place,
                    interface: interface.interface,
                    method: *method as u32,
                }
            }
            _ => Callee::Fn(self.callee(call)?),
        };
        if let Callee::Fn(fn_ref) = &callee {
            self.call_checks(call, fn_ref, &mut args, span)?;
        }
        let nir_call = Call {
            awaited: call.awaited,
            callee,
            args,
        };
        // A failure type of `Never` is uninhabited, so the call cannot fail and is lowered
        // without a failure path, as its calling form carries none.
        if call
            .head
            .fails
            .as_ref()
            .is_some_and(|fails| *fails != Ty::Never)
        {
            return self.failing_call(call, nir_call, ty, span, through);
        }
        if call.head.result == Ty::Never {
            self.emit(Op::Call(nir_call), span);
            self.end(Terminator::Unreachable, span);
            return Ok(Out::Never);
        }
        if call.head.result == Ty::Unit {
            self.emit(Op::Call(nir_call), span);
            return Ok(Out::Operand(Operand::Literal(crate::nir::Literal::Unit)));
        }
        let result_ty = self.result_nty(call);
        let reference = result_ty.is_reference();
        let value = self.emit_value(Op::Call(nir_call), result_ty, span);
        if call.proj && self.ck.projection_form(call) == Form::Wrapped {
            self.wrapped.insert(value);
        }
        Ok(match reference {
            true => Out::Place(Place::of(value)),
            false => Out::Operand(Operand::Value(value)),
        })
    }

    /// The NIR type of a call's result: a projection's part is a reference, writable when the
    /// projection takes its receiver by `mut`.
    fn result_nty(&self, call: &tir::Call) -> NTy {
        let result = call.head.result.clone();
        if !call.proj || self.ck.projection_form(call) != Form::Part {
            return NTy::Owned(result);
        }
        let writable = call
            .head
            .params
            .first()
            .is_some_and(|param| param.mode == Mode::Mut);
        match writable {
            true => NTy::RefMut(result),
            false => NTy::Ref(result),
        }
    }

    /// The function a call names.
    fn callee(&self, call: &tir::Call) -> Elaborated<FnRef> {
        Ok(match &call.callee {
            tir::Callee::Function { item } => FnRef::item(*item, call.type_args.clone()),
            tir::Callee::Method {
                interface,
                method,
                self_ty,
                ..
            } => {
                let name = self.ck.method_name(interface.interface, *method);
                match self_ty {
                    Ty::Param(_) => FnRef {
                        callable: Callable::Method {
                            interface: interface.interface,
                            method: *method as u32,
                        },
                        args: call.type_args.clone(),
                    },
                    _ => {
                        let implementation =
                            self.implementation(self_ty, interface.interface, &name)?;
                        self.ck
                            .with_call_args(implementation, interface.interface, &call.type_args)
                    }
                }
            }
            tir::Callee::Builtin(tir::Builtin::DynView) => FnRef {
                callable: Callable::Builtin(Builtin::Intrinsic("Dyn.view")),
                args: [&call.head.result, &call.args[0].expr.ty]
                    .into_iter()
                    .map(|ty| Arg::Ty(ty.clone()))
                    .collect(),
            },
            tir::Callee::Builtin(tir::Builtin::Template) => FnRef {
                callable: Callable::Builtin(Builtin::Intrinsic("text::fmt")),
                args: call
                    .args
                    .iter()
                    .map(|arg| Arg::Ty(arg.expr.ty.clone()))
                    .collect(),
            },
            tir::Callee::Builtin(tir::Builtin::DmaAccess) => FnRef {
                callable: Callable::Builtin(Builtin::Intrinsic("hw::Dma.access")),
                args: call.type_args.clone(),
            },
            tir::Callee::Builtin(builtin) => FnRef {
                callable: Callable::Builtin(match builtin {
                    tir::Builtin::Conversion(builtin) => *builtin,
                    tir::Builtin::DmaAccess => unreachable!("a DMA access has its own reference"),
                    tir::Builtin::MintRoot => Builtin::Intrinsic("hw::mint_root"),
                    tir::Builtin::DynView | tir::Builtin::Template => {
                        unreachable!("a Dyn view or template has its own reference")
                    }
                }),
                args: Args::from(Vec::new()),
            },
            tir::Callee::Value(_) => unreachable!("an indirect call has a value callee"),
        })
    }

    /// The checks a call carries before it runs: the predicates of its arguments, then each
    /// requires of its callee, then its decreases obligation within a cycle.
    fn call_checks(
        &mut self,
        call: &tir::Call,
        callee: &FnRef,
        args: &mut [Operand],
        span: Span,
    ) -> Elaborated<()> {
        if !self.callee_is_helper(callee) {
            for (arg, operand) in call.args.iter().zip(args.iter()) {
                if let Some(place) = operand_place(operand) {
                    self.predicate_checks(place, &arg.expr.ty, span)?;
                }
            }
        }
        let requires = self.requires_of(call, callee);
        let decreases = self.decreases_of(callee);
        if requires
            .as_ref()
            .is_some_and(|requires| !requires.clauses.is_empty())
            || decreases.is_some()
        {
            self.slot_taken_values(call, args);
        }
        let read: Vec<Operand> = call
            .args
            .iter()
            .zip(args.iter())
            .map(|(arg, operand)| self.read_again(operand, &arg.expr.ty, span))
            .collect();
        if let Some(requires) = requires {
            for (index, disputed) in requires.clauses {
                let fn_ref = FnRef {
                    callable: match &requires.callee.callable {
                        Callable::Item { item, .. } => Callable::Item {
                            item: *item,
                            role: Role::Requires,
                            index,
                        },
                        other => other.clone(),
                    },
                    args: requires.callee.args.clone(),
                };
                let passed = read[requires.params.clone()].to_vec();
                let value = self.call_value(fn_ref, passed, &Ty::Scalar(Scalar::Bool), span);
                self.emit(
                    Op::Check {
                        kind: CheckKind::Requires,
                        operand: Operand::Value(value),
                        open: disputed,
                    },
                    span,
                );
            }
        }
        if let (Some(measure_ty), Some(measure)) = (decreases, self.measure) {
            let fn_ref = FnRef {
                callable: match &callee.callable {
                    Callable::Item { item, .. } => Callable::Item {
                        item: *item,
                        role: Role::Decreases,
                        index: 0,
                    },
                    other => other.clone(),
                },
                args: callee.args.clone(),
            };
            let value = self.call_value(fn_ref, read, &measure_ty, span);
            let own_ty = self.measure_ty();
            let (callee_measure, entry_measure) = if measure_ty == own_ty {
                (Operand::Value(value), Operand::Value(measure))
            } else {
                (
                    self.widened(Operand::Value(value), &measure_ty, span),
                    self.widened(Operand::Value(measure), &own_ty, span),
                )
            };
            let less = self.emit_value(
                Op::Binary(crate::nir::BinaryOp::Lt, callee_measure, entry_measure),
                NTy::Owned(Ty::Scalar(Scalar::Bool)),
                span,
            );
            self.emit(
                Op::Check {
                    kind: CheckKind::Decreases,
                    operand: Operand::Value(less),
                    open: false,
                },
                span,
            );
        }
        Ok(())
    }

    /// A measure widened to `u64` unless it is one.
    fn widened(&mut self, measure: Operand, ty: &Ty, span: Span) -> Operand {
        if *ty == Ty::Scalar(Scalar::U64) {
            return measure;
        }
        let fn_ref = FnRef {
            callable: Callable::Builtin(Builtin::Conversion(
                crate::nir::Conversion::Widen,
                Scalar::U64,
            )),
            args: Args::from(Vec::new()),
        };
        Operand::Value(self.call_value(fn_ref, vec![measure], &Ty::Scalar(Scalar::U64), span))
    }

    /// The type of the function's own decreases measure.
    fn measure_ty(&self) -> Ty {
        self.body
            .clauses
            .iter()
            .find(|clause| clause.role == ClauseRole::Decreases)
            .map_or(Ty::Scalar(Scalar::U64), |clause| clause.expr.ty.clone())
    }

    /// Moves each owned temporary a call takes into a slot, so that its checks read it first.
    fn slot_taken_values(&mut self, call: &tir::Call, args: &mut [Operand]) {
        for (arg, operand) in call.args.iter().zip(args.iter_mut()) {
            let ty = &arg.expr.ty;
            if let Operand::Value(value) = operand
                && !self.ck.nir_copy(ty)
                && !self.ck.nir_view(ty)
            {
                let place =
                    Place::of(self.slot_holding(Operand::Value(*value), ty, arg.expr.token));
                self.moved.insert(place.clone());
                *operand = Operand::Place(Mode::Take, place);
            }
        }
    }

    /// An argument's operand passed again by read, to a contract function.
    fn read_again(&mut self, operand: &Operand, ty: &Ty, span: Span) -> Operand {
        match operand {
            Operand::Place(_, place) => self.read_operand(place.clone(), ty, span),
            other => other.clone(),
        }
    }

    /// Whether a callee is a helper of the module being elaborated: a function of it that is
    /// not a boundary function, so a call to it carries no predicate checks.
    fn callee_is_helper(&self, callee: &FnRef) -> bool {
        match &callee.callable {
            Callable::Item { item, .. } => {
                self.ck.db.module_of(*item) == self.ck.db.module_of(self.item)
                    && !self.ck.is_boundary(*item)
            }
            _ => false,
        }
    }

    /// The requires clauses a call meets: a direct callee's own, or the interface method's for
    /// a call through a bound, through `Dyn` or of an implementing function without its own.
    fn requires_of(&self, call: &tir::Call, callee: &FnRef) -> Option<Requires> {
        if let Callable::Item { item, .. } = &callee.callable {
            let clauses = self.ck.requires_clauses(*item);
            if clauses.iter().any(|(_, disputed)| !disputed) {
                return Some(Requires {
                    callee: callee.clone(),
                    clauses,
                    params: 0..call.args.len(),
                });
            }
        }
        let (interface, method, args) = match (&call.callee, &callee.callable) {
            (
                tir::Callee::Method {
                    interface,
                    method,
                    self_ty,
                    ..
                },
                _,
            ) => {
                let args = self
                    .ck
                    .interface_args(interface.interface, self_ty, &interface.args);
                let given = self.ck.sig(interface.interface).generics.len().max(1);
                let own = call.type_args.get(given..).unwrap_or_default();
                let args = args.iter().chain(own).cloned().collect();
                (interface.interface, *method, args)
            }
            (_, Callable::Item { item, .. }) => {
                let implemented = self.ck.implemented_method(*item)?;
                let generics = &self.ck.sig(*item).generics;
                let subst = Subst::of(generics.iter().map(|def| def.param), &callee.args);
                let own = (generics.iter().zip(callee.args.iter()))
                    .filter(|(def, _)| def.param.owner == *item)
                    .map(|(_, arg)| arg.clone());
                let interface_args = subst.args(&implemented.args);
                let args = interface_args.iter().cloned().chain(own).collect();
                (implemented.interface, implemented.method, args)
            }
            _ => return None,
        };
        Some(Requires {
            callee: FnRef {
                callable: Callable::Item {
                    item: interface,
                    role: Role::Requires,
                    index: 0,
                },
                args,
            },
            clauses: self.ck.method_requires(interface, method),
            params: self.ck.contract_params(interface, method, call.args.len()),
        })
    }

    /// The measure type of a callee in the cycle of the function being elaborated, both with a
    /// decreases clause.
    fn decreases_of(&self, callee: &FnRef) -> Option<Ty> {
        let Callable::Item { item, .. } = &callee.callable else {
            return None;
        };
        if self.role != Role::Body || self.measure.is_none() {
            return None;
        }
        let measure = self
            .ck
            .typed(*item)?
            .clauses
            .iter()
            .find(|clause| clause.role == ClauseRole::Decreases)?
            .expr
            .ty
            .clone();
        self.ck.same_cycle(self.item, *item).then_some(measure)
    }

    /// A call that may fail: its terminator, then `try` converting and exiting with the
    /// failure, or `attempt` building a `Result`.
    fn failing_call(
        &mut self,
        call: &tir::Call,
        nir_call: Call,
        ty: &Ty,
        span: crate::nir::Span,
        through: Option<&Ty>,
    ) -> Elaborated<Out> {
        let ok = self.b.block();
        let fail = self.b.block();
        let value_ty = call.head.result.clone();
        let raised = call.head.fails.clone().unwrap_or(Ty::Never);
        let result = self.b.block_param(ok, self.result_nty(call));
        let failure = self.b.block_param(fail, NTy::Owned(raised.clone()));
        let failure_ty = through.cloned().unwrap_or_else(|| raised.clone());
        self.end(
            Terminator::Call {
                call: nir_call,
                ok,
                fail,
            },
            span,
        );
        match call.failure {
            Some((FailureMarker::Attempt, _)) => {
                let join = self.b.block();
                let joined = self.b.block_param(join, NTy::Owned(ty.clone()));
                let moved = self.moved.clone();
                self.b.at(ok);
                let wrapped = self.emit_value(
                    Op::Variant {
                        ty: ty.clone(),
                        variant: "Ok".to_owned(),
                        payload: Payload::Single(Operand::Value(result)),
                    },
                    NTy::Owned(ty.clone()),
                    span,
                );
                self.b.jump(join, vec![Operand::Value(wrapped)], span);
                self.b.at(fail);
                let failure =
                    self.converted_to(Operand::Value(failure), &raised, &failure_ty, span);
                let wrapped = self.emit_value(
                    Op::Variant {
                        ty: ty.clone(),
                        variant: "Err".to_owned(),
                        payload: Payload::Single(failure),
                    },
                    NTy::Owned(ty.clone()),
                    span,
                );
                self.b.jump(join, vec![Operand::Value(wrapped)], span);
                self.moved = moved;
                self.b.at(join);
                Ok(Out::Operand(Operand::Value(joined)))
            }
            _ => {
                let moved = self.moved.clone();
                self.b.at(fail);
                if raised == Ty::Never {
                    self.end(Terminator::Unreachable, span);
                } else {
                    let failure =
                        self.converted_to(Operand::Value(failure), &raised, &failure_ty, span);
                    let converted = self.converted(failure, &failure_ty, call.token)?;
                    self.exit_fail(converted, call.token)?;
                }
                self.moved = moved;
                self.b.at(ok);
                if value_ty == Ty::Unit {
                    return Ok(Out::Operand(Operand::Literal(crate::nir::Literal::Unit)));
                }
                Ok(match self.b.ty(result).is_reference() {
                    true => Out::Place(Place::of(result)),
                    false => Out::Operand(Operand::Value(result)),
                })
            }
        }
    }

    /// The predicate checks of a closure expression, for the values reachable from its
    /// captures.
    pub fn closure_predicate_checks(&mut self, closure: &TClosure, span: Span) -> Elaborated<()> {
        for capture in super::closure_captures(closure) {
            if matches!(self.held.get(&capture.outer), Some(Held::Literal(_))) {
                continue;
            }
            let ty = self.body.local(capture.outer).ty.clone();
            let place = self.local_place(capture.outer)?;
            self.predicate_checks(place, &ty, span)?;
        }
        Ok(())
    }

    /// The predicate checks at an exit of a boundary function, for the values reachable from
    /// the result or failure it carries, of its type, and from its `mut` parameters.
    pub fn exit_predicate_checks(
        &mut self,
        carried: Option<(Operand, Ty)>,
        span: Span,
    ) -> Elaborated<()> {
        let boundary = match self.role {
            Role::Closure => true,
            Role::Body | Role::Default => self.ck.is_boundary(self.item),
            _ => false,
        };
        if !boundary {
            return Ok(());
        }
        if let Some((operand, ty)) = carried
            && let Some(place) = operand_place(&operand)
        {
            self.predicate_checks(place, &ty, span)?;
        }
        let params: Vec<(tir::LocalId, Ty)> = self
            .own_params
            .iter()
            .filter(|(_, mode)| *mode == Mode::Mut)
            .map(|(local, _)| (*local, self.body.local(*local).ty.clone()))
            .collect();
        for (local, ty) in params {
            let place = self.local_place(local)?;
            self.predicate_checks(place, &ty, span)?;
        }
        Ok(())
    }

    /// The predicate checks of the values reachable from a place whose type's defining module
    /// is the module being elaborated: each predicate's function called on the value, then
    /// `check predicate`, the value before its fields in declaration order.
    pub fn predicate_checks(&mut self, place: Place, ty: &Ty, span: Span) -> Elaborated<()> {
        let Ty::Item(item, args) = ty else {
            return Ok(());
        };
        if self.ck.db.module_of(*item) != self.ck.db.module_of(self.item) {
            return Ok(());
        }
        if let ItemKind::Struct(_) = &self.ck.db.ast(*item).kind {
            let count = self.ck.predicate_count(*item);
            if count > 0 {
                let operand = self.read_operand(place.clone(), ty, span);
                for index in 0..count {
                    let fn_ref = FnRef {
                        callable: Callable::Item {
                            item: *item,
                            role: Role::Predicate,
                            index,
                        },
                        args: args.clone(),
                    };
                    let value = self.call_value(
                        fn_ref,
                        vec![operand.clone()],
                        &Ty::Scalar(Scalar::Bool),
                        span,
                    );
                    self.emit(
                        Op::Check {
                            kind: CheckKind::Predicate,
                            operand: Operand::Value(value),
                            open: false,
                        },
                        span,
                    );
                }
            }
            if let SigKind::Struct { fields } = &self.ck.sig(*item).kind {
                let subst = crate::check::ty::Subst::of(
                    self.ck.sig(*item).generics.iter().map(|def| def.param),
                    args,
                );
                for field in fields {
                    let field_ty = subst.ty(&field.ty);
                    if self.ck.has_predicates_within(&field_ty) {
                        self.predicate_checks(place.step(&field.name), &field_ty, span)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// The checking entry of a function with requires: the requires checks a direct call makes,
    /// then the call of its body, returning its result or failure.
    pub fn entry_function(
        ck: &crate::check::Checker,
        unit: &mut super::UnitDraft,
        body: &TBody,
        sig: &FnSig,
        span: Span,
    ) -> Elaborated<Function> {
        let mut elab = Elab::new(ck, unit, body, Role::Entry);
        elab.declare_params(&body.params, &sig.params_modes());
        let args: Vec<Operand> = elab
            .params
            .iter()
            .map(|param| match param.mode {
                Mode::Read => Operand::Value(param.value),
                mode => Operand::Place(mode, Place::of(param.value)),
            })
            .collect();
        let read: Vec<Operand> = elab
            .params
            .iter()
            .map(|param| match elab.b.ty(param.value) {
                NTy::RefMut(_) => Operand::Place(Mode::Read, Place::of(param.value)),
                _ => Operand::Value(param.value),
            })
            .collect();
        let clauses = body
            .clauses
            .iter()
            .filter(|clause| clause.role == ClauseRole::Requires);
        for (clause, (index, disputed)) in clauses.zip(ck.requires_clauses(elab.item)) {
            let at = Span::at(clause.token);
            let value = elab.call_value(
                elab.own(Role::Requires, index),
                read.clone(),
                &Ty::Scalar(Scalar::Bool),
                at,
            );
            elab.emit(
                Op::Check {
                    kind: CheckKind::Requires,
                    operand: Operand::Value(value),
                    open: disputed,
                },
                at,
            );
        }
        let call = Call {
            awaited: sig.wait,
            callee: Callee::Fn(elab.own(Role::Body, 0)),
            args,
        };
        let fails = sig.fails.clone().filter(|ty| *ty != Ty::Never);
        match &fails {
            Some(failure) => {
                let ok = elab.b.block();
                let fail = elab.b.block();
                let result = elab.b.block_param(ok, NTy::Owned(sig.result.clone()));
                let failure = elab.b.block_param(fail, NTy::Owned(failure.clone()));
                elab.end(Terminator::Call { call, ok, fail }, span);
                elab.b.at(ok);
                elab.end(Terminator::Return(Some(Operand::Value(result))), span);
                elab.b.at(fail);
                elab.end(Terminator::Fail(Operand::Value(failure)), span);
            }
            None if sig.result == Ty::Unit => {
                elab.emit(Op::Call(call), span);
                elab.end(Terminator::Return(None), span);
            }
            None => {
                let result = elab.emit_value(Op::Call(call), NTy::Owned(sig.result.clone()), span);
                elab.end(Terminator::Return(Some(Operand::Value(result))), span);
            }
        }
        Ok(elab.finish(sig.result.clone(), fails, sig.wait, None, span))
    }
}

/// The place an operand passes, when it passes one.
fn operand_place(operand: &Operand) -> Option<Place> {
    match operand {
        Operand::Place(_, place) => Some(place.clone()),
        Operand::Value(value) => Some(Place::of(*value)),
        Operand::Literal(_) => None,
    }
}
