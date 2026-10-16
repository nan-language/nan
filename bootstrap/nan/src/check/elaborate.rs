//! Elaboration: the translation of an accepted item's checked source into its NIR unit. Each
//! typed body, clause and predicate becomes a NIR function; bindings become values or slots,
//! control flow becomes blocks, and calls carry the requires, bounds and predicate checks their
//! callees and operations state.

mod build;
mod call;
mod expr;
mod pattern;
mod stmt;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use build::Builder;

use super::Checker;
use super::db::{ItemId, TypeKey};
use super::props::Property;
use super::sig::{FnSig, SigKind};
use super::tir::{ClauseRole, LocalId, LocalKind, TBody, TClosure, TExpr, TKind, TStmtKind};
use super::ty::{Arg, Args, Const, Param as GenericParam, Scalar, Subst, Ty};
use crate::nir::{
    Callable, ClosureHead, FnRef, Function, NTy, Op, Operand, Param, Place, Role, Span, Unit, Value,
};
use crate::syntax::ast::{self, ItemKind, Mode, Tok, Visibility};

/// Why elaboration stops: a construct that failed through an error on another item, so the item
/// has no unit, or a construct it cannot translate, which is an internal error.
pub enum Fault {
    Failed,
    Unsupported(String),
}

pub type Elaborated<T> = Result<T, Fault>;

impl Checker<'_> {
    /// The NIR unit of an item the checker accepts; none for a rejected item, a package, a
    /// tombstone, or an item whose elaboration faulted, which reports an internal error.
    pub fn unit(&self, item: ItemId) -> Option<Rc<Unit>> {
        self.db.note(item);
        if let Some(unit) = self.units.borrow().get(&item) {
            return unit.clone();
        }
        let unit = match self.rejected(item) {
            true => None,
            false => match self.db.about(Some(item), || self.elaborate(item)) {
                Ok(unit) => unit.map(Rc::new),
                Err(Fault::Failed) => None,
                Err(Fault::Unsupported(fault)) => {
                    panic!("elaborating {}: {fault}", self.db.canonical(item))
                }
            },
        };
        self.units.borrow_mut().insert(item, unit.clone());
        unit
    }

    fn elaborate(&self, item: ItemId) -> Elaborated<Option<Unit>> {
        self.type_parts(item);
        let mut unit = UnitDraft::new(item);
        match &self.db.ast(item).kind {
            ItemKind::Fn(function) => {
                let (Some(sig), Some(body)) = (self.fn_sig(item), self.typed(item)) else {
                    return Ok(None);
                };
                unit.number_conditions(&body);
                let has_body = !matches!(function.body, ast::FnBody::None);
                self.function_unit(&mut unit, &sig, &body, has_body, Role::Body)?;
            }
            ItemKind::Interface(_) => {
                let SigKind::Interface { methods } = &self.sig(item).kind else {
                    return Ok(None);
                };
                let bodies: Vec<Rc<TBody>> = (0..methods.len())
                    .filter_map(|index| self.method_bodies.borrow().get(&(item, index)).cloned())
                    .collect();
                for body in &bodies {
                    unit.number_conditions(body);
                }
                for (method, body) in methods.iter().zip(&bodies) {
                    self.function_unit(
                        &mut unit,
                        &method.sig,
                        body,
                        method.default,
                        Role::Default,
                    )?;
                }
            }
            ItemKind::Struct(_) => self.predicate_functions(&mut unit)?,
            ItemKind::Const(_) | ItemKind::ConstAssert(_) | ItemKind::Contribute(_) => {
                let Some(body) = self.typed(item) else {
                    return Ok(None);
                };
                unit.number_conditions(&body);
                self.value_unit(&mut unit, &body)?;
            }
            ItemKind::Test(_) => {
                let Some(body) = self.typed(item) else {
                    return Ok(None);
                };
                unit.number_conditions(&body);
                let root = body.root.as_ref().expect("a test has a body");
                let mut elab = Elab::new(self, &mut unit, &body, Role::Body);
                elab.declare_params(&body.params, &[]);
                elab.function_root(root)?;
                let span = Span::at(self.db.ast(item).token);
                let function = elab.finish(Ty::Unit, None, false, None, span);
                unit.add(function);
                self.closure_functions(&mut unit, &body)?;
                self.statement_functions(&mut unit, &body)?;
            }
            ItemKind::Package | ItemKind::Retired(_) => return Ok(None),
            _ => {}
        }
        self.where_functions(&mut unit)?;
        Ok(Some(unit.finish()))
    }

    /// Types without reporting what checking an item types beside its body, when its check has
    /// not: a struct's predicates, an interface's method bodies and the `where` conditions of
    /// its const parameters.
    fn type_parts(&self, item: ItemId) {
        let ast = self.db.ast(item);
        let typed = match &ast.kind {
            ItemKind::Struct(_) => self.typed_predicates.borrow().contains_key(&item),
            ItemKind::Interface(_) => self.method_bodies.borrow().contains_key(&(item, 0)),
            _ => true,
        };
        if !typed {
            self.typed(item);
        }
        if let Some(generics) = super::decl::generic_params(ast)
            && !self.typed_wheres.borrow().contains_key(&item)
        {
            self.muted.set(self.muted.get() + 1);
            self.where_conditions(item, generics);
            self.muted.set(self.muted.get() - 1);
        }
    }

    /// The typed body of an item, its clauses included: the one its check produced, or one typed
    /// without reporting.
    pub fn typed(&self, item: ItemId) -> Option<Rc<TBody>> {
        self.db.note(item);
        if let Some(body) = self.bodies.borrow().get(&item) {
            return Some(body.clone());
        }
        self.muted.set(self.muted.get() + 1);
        let body = self
            .db
            .about(Some(item), || self.check_bodies(item))
            .map(Rc::new);
        self.muted.set(self.muted.get() - 1);
        if let Some(body) = &body {
            self.bodies.borrow_mut().insert(item, body.clone());
        }
        body
    }

    /// The functions of a function or interface method: its body (or default body), checking
    /// entry, closures and contract functions, numbered after those of earlier methods.
    fn function_unit(
        &self,
        unit: &mut UnitDraft,
        sig: &FnSig,
        body: &Rc<TBody>,
        has_body: bool,
        role: Role,
    ) -> Elaborated<()> {
        let span = Span::at(self.db.ast(unit.item).token);
        let fails = sig.fails.clone().filter(|ty| *ty != Ty::Never);
        let olds = unit.number_olds(body);
        if has_body && let Some(root) = &body.root {
            let index = unit.next(role);
            let first = unit.count(Role::Ensures);
            let inherited = match role {
                Role::Body => self.inherited(unit.item),
                _ => None,
            };
            let mut elab = Elab::new(self, unit, body, role);
            elab.exits = Exits::of(body, &olds, first);
            elab.exits.inherited = inherited;
            elab.failure_ty = fails.clone();
            elab.exit_ty = Some(sig.result.clone());
            elab.declare_params(&body.params, &sig.params_modes());
            elab.enter(&olds)?;
            elab.function_root(root)?;
            let function = elab.finish(sig.result.clone(), fails, sig.wait, None, span);
            unit.add(Function { index, ..function });
            let checked = body
                .clauses
                .iter()
                .any(|clause| clause.role == ClauseRole::Requires && !clause.disputed);
            if role == Role::Body && checked {
                let entry = Elab::entry_function(self, unit, body, sig, span)?;
                unit.add(entry);
            }
            self.closure_functions(unit, body)?;
        }
        self.contract_functions(unit, sig, body, &olds)?;
        self.statement_functions(unit, body)
    }

    /// The body function of a const, const assert or contribution, without parameters.
    fn value_unit(&self, unit: &mut UnitDraft, body: &Rc<TBody>) -> Elaborated<()> {
        let root = body.root.as_ref().expect("a value has an initializer");
        let span = Span::at(self.db.ast(unit.item).token);
        let mut elab = Elab::new(self, unit, body, Role::Body);
        elab.function_root_value(root)?;
        let function = elab.finish(root.ty.clone(), None, false, None, span);
        unit.add(function);
        self.closure_functions(unit, body)?;
        self.statement_functions(unit, body)
    }

    /// The closure functions of a body, each taking its captures, then its own parameters.
    fn closure_functions(&self, unit: &mut UnitDraft, body: &Rc<TBody>) -> Elaborated<()> {
        let mut closures: Vec<&TClosure> = Vec::new();
        if let Some(root) = &body.root {
            collect_closures(root, &mut closures);
        }
        closures.sort_by_key(|closure| closure.index);
        for closure in closures {
            let mut elab = Elab::new(self, unit, body, Role::Closure);
            for capture in closure_captures(closure) {
                let local = body.local(capture.local);
                let value = elab.b.value(elab.param_ty(capture.mode, &local.ty));
                elab.params.push(Param {
                    mode: capture.mode,
                    value,
                    name: local.name.clone(),
                    span: Span::at(local.token),
                });
                elab.held.insert(capture.local, Held::Value(value));
                elab.own_params.push((capture.local, capture.mode));
                if capture.mode == Mode::Take {
                    elab.take_params.push((capture.local, value));
                }
            }
            let modes: Vec<Mode> = closure.ty.params.iter().map(|param| param.mode).collect();
            elab.declare_params(&closure.params, &modes);
            let fails = closure.ty.fails.clone().filter(|ty| *ty != Ty::Never);
            elab.failure_ty = fails.clone();
            elab.exit_ty = Some(closure.ty.result.clone());
            elab.enter(&HashMap::new())?;
            elab.closure_root(closure)?;
            let head = ClosureHead {
                task: closure.task,
                limits: closure.ty.limits,
            };
            let function = elab.finish(
                closure.ty.result.clone(),
                fails,
                closure.ty.wait,
                Some(head),
                Span::at(closure.body.token),
            );
            unit.add(Function {
                index: closure.index - 1,
                ..function
            });
        }
        Ok(())
    }

    /// The requires, ensures, decreases, old and example functions of a function's clauses.
    fn contract_functions(
        &self,
        unit: &mut UnitDraft,
        sig: &FnSig,
        body: &Rc<TBody>,
        olds: &HashMap<Tok, u32>,
    ) -> Elaborated<()> {
        let all_read: Vec<LocalId> = body.params.clone();
        let kept: Vec<LocalId> = body
            .params
            .iter()
            .zip(sig.params_modes())
            .filter(|(_, mode)| *mode != Mode::Take)
            .map(|(local, _)| *local)
            .collect();
        for clause in &body.clauses {
            let role = match clause.role {
                ClauseRole::Requires => Role::Requires,
                ClauseRole::Ensures => Role::Ensures,
                ClauseRole::Decreases => Role::Decreases,
            };
            let index = unit.next(role);
            let span = Span::at(clause.token);
            let mut elab = Elab::new(self, unit, body, role);
            if clause.role == ClauseRole::Ensures {
                elab.declare_read_params(&kept);
                let result = elab.read_param("result", &sig.result, span);
                elab.result = Some(result);
                let mut terms: Vec<(Tok, Ty)> = Vec::new();
                collect_olds(&clause.expr, &mut |token, ty| {
                    terms.push((token, ty.clone()))
                });
                terms.sort_by_key(|(token, _)| *token);
                for (token, ty) in terms {
                    let value = elab.read_param("old", &ty, Span::at(token));
                    elab.olds.insert(token, value);
                }
            } else {
                elab.declare_read_params(&all_read);
            }
            elab.function_root_value(&clause.expr)?;
            let function = elab.finish(clause.expr.ty.clone(), None, false, None, span);
            unit.add(Function { index, ..function });
        }
        let mut terms: Vec<(Tok, &TExpr)> = Vec::new();
        for clause in body
            .clauses
            .iter()
            .filter(|clause| clause.role == ClauseRole::Ensures)
        {
            collect_old_terms(&clause.expr, &mut terms);
        }
        for (token, term) in terms {
            let mut elab = Elab::new(self, unit, body, Role::Old);
            elab.declare_read_params(&all_read);
            elab.function_root_value(term)?;
            let function = elab.finish(term.ty.clone(), None, false, None, Span::at(token));
            unit.add(Function {
                index: olds[&token],
                ..function
            });
        }
        for example in &body.examples {
            let index = unit.next(Role::Example);
            let root = example.body.root.as_ref().expect("an example expression");
            let mut elab = Elab::new(self, unit, &example.body, Role::Example);
            elab.function_root_value(root)?;
            let function = elab.finish(root.ty.clone(), None, false, None, Span::at(example.token));
            unit.add(Function { index, ..function });
            self.closure_functions(unit, &Rc::new(example.body.clone()))?;
        }
        Ok(())
    }

    /// The invariant and assume functions of a body's loop invariants and `assume` statements,
    /// each taking the bindings it names by first mention.
    fn statement_functions(&self, unit: &mut UnitDraft, body: &Rc<TBody>) -> Elaborated<()> {
        let mut conditions: Vec<(Role, &TExpr)> = Vec::new();
        if let Some(root) = &body.root {
            collect_conditions(root, &mut conditions);
        }
        for (role, condition) in conditions {
            let index = unit.conditions[&(role, condition.token)];
            let mut elab = Elab::new(self, unit, body, role);
            elab.declare_read_params(&named_bindings(condition));
            elab.function_root_value(condition)?;
            let function = elab.finish(
                condition.ty.clone(),
                None,
                false,
                None,
                Span::at(condition.token),
            );
            unit.add(Function { index, ..function });
        }
        Ok(())
    }

    /// The predicate functions of a struct, one per `where` and `invariant` line, each taking
    /// the struct value.
    fn predicate_functions(&self, unit: &mut UnitDraft) -> Elaborated<()> {
        let item = unit.item;
        let Some(predicates) = self.typed_predicates.borrow().get(&item).cloned() else {
            return Ok(());
        };
        let args: Vec<Arg> = self
            .sig(item)
            .generics
            .iter()
            .map(|def| match def.constant {
                Some(_) => Arg::Const(Const::Param(def.param)),
                None => Arg::Ty(Ty::Param(def.param)),
            })
            .collect();
        let struct_ty = Ty::item(item, args);
        for (index, condition) in predicates.conditions.iter().enumerate() {
            let span = Span::at(condition.token);
            let mut elab = Elab::new(self, unit, &predicates.body, Role::Predicate);
            let value = elab.read_param("self", &struct_ty, span);
            for (local, field) in &predicates.fields {
                elab.fields.insert(*local, (value, field.clone()));
            }
            elab.function_root_value(condition)?;
            let function = elab.finish(condition.ty.clone(), None, false, None, span);
            unit.add(Function {
                index: index as u32,
                ..function
            });
        }
        Ok(())
    }

    /// The `where` functions of an item's const parameters, each taking the const parameters.
    fn where_functions(&self, unit: &mut UnitDraft) -> Elaborated<()> {
        let item = unit.item;
        let Some(wheres) = self.typed_wheres.borrow().get(&item).cloned() else {
            return Ok(());
        };
        for (index, condition) in wheres.conditions.iter().enumerate() {
            let span = Span::at(condition.token);
            let mut elab = Elab::new(self, unit, &wheres.body, Role::Where);
            for (param, scalar) in &wheres.params {
                let value = elab.read_param(&self.generic_name(*param), &Ty::Scalar(*scalar), span);
                elab.const_params.insert(*param, value);
            }
            elab.function_root_value(condition)?;
            let function = elab.finish(condition.ty.clone(), None, false, None, span);
            unit.add(Function {
                index: index as u32,
                ..function
            });
        }
        Ok(())
    }

    /// The name of a generic parameter, as its item declares it.
    pub fn generic_name(&self, param: GenericParam) -> String {
        self.sig(param.owner)
            .generics
            .iter()
            .find(|def| def.param == param)
            .map_or_else(|| "Self".to_owned(), |def| def.name.clone())
    }

    /// Whether NIR copies a value of the type: a type parameter is Copy exactly when bounded by
    /// `Copy`.
    pub fn nir_copy(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Param(param) => self
                .param_bounds(*param)
                .iter()
                .any(|bound| self.db.canonical(bound.interface) == "core::Copy"),
            _ => self.has(ty, Property::Copy),
        }
    }

    /// Whether NIR treats a value of the type as a view: a type parameter never is.
    pub fn nir_view(&self, ty: &Ty) -> bool {
        !matches!(ty, Ty::Param(_)) && self.has(ty, Property::View)
    }

    /// Whether a function is a boundary function of its module: `pub` or `pkg`, or implementing
    /// an interface method.
    pub fn is_boundary(&self, function: ItemId) -> bool {
        self.db.ast(function).visibility != Visibility::Private
            || !self.implemented_lines(function).is_empty()
    }

    /// The requires clauses of a function item, by index, with whether each is disputed.
    pub fn requires_clauses(&self, function: ItemId) -> Vec<(u32, bool)> {
        self.typed(function)
            .map(|body| clause_indexes(&body, ClauseRole::Requires, 0))
            .unwrap_or_default()
    }

    /// The requires a function's body meets at entry, as the functions computing them: its own
    /// clauses that are not disputed, or, for an implementing function with none, those of the
    /// interface method it implements.
    pub fn entry_requires(&self, function: ItemId) -> Vec<FnRef> {
        let own = self.requires_clauses(function);
        if own.iter().any(|(_, disputed)| !disputed) {
            let args: Args = self
                .sig(function)
                .generics
                .iter()
                .map(super::sig::param_arg)
                .collect();
            return own
                .into_iter()
                .filter(|(_, disputed)| !disputed)
                .map(|(index, _)| FnRef {
                    callable: Callable::Item {
                        item: function,
                        role: Role::Requires,
                        index,
                    },
                    args: args.clone(),
                })
                .collect();
        }
        let Some(implemented) = self.implemented_method(function) else {
            return Vec::new();
        };
        self.method_requires(implemented.interface, implemented.method)
            .into_iter()
            .filter(|(_, disputed)| !disputed)
            .map(|(index, _)| FnRef {
                callable: Callable::Item {
                    item: implemented.interface,
                    role: Role::Requires,
                    index,
                },
                args: implemented.args.clone(),
            })
            .collect()
    }

    /// The typed bodies of an interface's methods, in method order.
    pub fn method_typed(&self, interface: ItemId) -> Vec<Rc<TBody>> {
        let count = match &self.sig(interface).kind {
            SigKind::Interface { methods } => methods.len(),
            _ => 0,
        };
        (0..count)
            .filter_map(|method| {
                self.method_bodies
                    .borrow()
                    .get(&(interface, method))
                    .cloned()
            })
            .collect()
    }

    /// The requires clauses of an interface method, by their index in the interface's unit,
    /// with whether each is disputed.
    pub fn method_requires(&self, interface: ItemId, method: usize) -> Vec<(u32, bool)> {
        let mut first = 0;
        for index in 0..method {
            if let Some(body) = self.method_bodies.borrow().get(&(interface, index)) {
                first += clause_indexes(body, ClauseRole::Requires, 0).len() as u32;
            }
        }
        self.method_bodies
            .borrow()
            .get(&(interface, method))
            .map(|body| clause_indexes(body, ClauseRole::Requires, first))
            .unwrap_or_default()
    }

    /// The ensures clauses of a function item, or of one of an interface's methods, as their
    /// unit's functions compute them.
    pub fn ensures_clauses(&self, item: ItemId, method: Option<usize>) -> Vec<EnsuresClause> {
        let bodies: Vec<Rc<TBody>> = match method {
            None => self.typed(item).into_iter().collect(),
            Some(method) => (0..=method)
                .filter_map(|index| self.method_bodies.borrow().get(&(item, index)).cloned())
                .collect(),
        };
        let mut clauses = Vec::new();
        let (mut index, mut old) = (0, 0);
        for (position, body) in bodies.iter().enumerate() {
            let ensures = body
                .clauses
                .iter()
                .filter(|clause| clause.role == ClauseRole::Ensures);
            for clause in ensures {
                let mut count = 0;
                collect_olds(&clause.expr, &mut |_, _| count += 1);
                if method.is_none_or(|method| position == method) {
                    clauses.push(EnsuresClause {
                        index,
                        olds: (old..old + count).collect(),
                        disputed: clause.disputed,
                    });
                }
                index += 1;
                old += count;
            }
        }
        clauses
    }

    /// The function implementing an interface's method for a type: the function its impl line's
    /// module declares or the interface's default body, a derived method, or the method through a
    /// generic parameter's bound or a conformance the toolchain supplies.
    pub fn implementation(&self, ty: &Ty, interface: ItemId, method: &str) -> Option<FnRef> {
        let index = self.interface_method(interface, method)?;
        let through_bound = FnRef {
            callable: Callable::Method {
                interface,
                method: index as u32,
            },
            args: std::iter::once(Arg::Ty(ty.clone())).collect(),
        };
        let Some(key) = crate::check::conform::type_key(ty).filter(|_| !matches!(ty, Ty::Param(_)))
        else {
            return Some(through_bound);
        };
        for line in self.impl_lines(key) {
            let SigKind::Impl {
                interface: Some(bound),
                ..
            } = &self.sig(line).kind
            else {
                continue;
            };
            if bound.interface != interface {
                continue;
            }
            let subst = self.type_subst(key, ty);
            if let Some(function) = self.implementing_function(line, method) {
                let args = self
                    .sig(function)
                    .generics
                    .iter()
                    .map(|def| match subst.get(def.param) {
                        Some(arg) => arg.clone(),
                        None => Arg::Ty(Ty::Param(def.param)),
                    })
                    .collect();
                return Some(FnRef::item(function, args));
            }
            let args = bound
                .args
                .iter()
                .map(|arg| subst.arg(arg))
                .chain(std::iter::once(Arg::Ty(ty.clone())))
                .collect();
            return Some(FnRef {
                callable: Callable::Item {
                    item: interface,
                    role: Role::Default,
                    index: self.default_index(interface, index),
                },
                args,
            });
        }
        if let TypeKey::Item(owner) = key
            && self.derives(owner, interface)
        {
            let method: &'static str = match method {
                "eq" => "eq",
                "cmp" => "cmp",
                "hash" => "hash",
                "default" => "default",
                "render" => "render",
                "write" => "write",
                "generate" => "generate",
                _ => return None,
            };
            return Some(FnRef {
                callable: Callable::Derived { owner, method },
                args: ty.args().iter().cloned().collect(),
            });
        }
        Some(through_bound)
    }

    /// The implementation of a method for a call giving the method `args`: those of `Self` and
    /// the interface, then the method's own. An implementing function receives the own ones for
    /// its own generic parameters, a derived method or default body after the others, and a call
    /// through a bound or a supplied conformance all of them.
    pub fn with_call_args(&self, implementation: FnRef, interface: ItemId, args: &[Arg]) -> FnRef {
        let interface_args = (self.sig(interface).generics.iter())
            .filter(|def| !def.param.is_self())
            .count();
        let own = args.get(1 + interface_args..).unwrap_or_default();
        let args: Args = match implementation.callable {
            Callable::Method { .. } => args.into(),
            Callable::Derived { .. }
            | Callable::Item {
                role: Role::Default,
                ..
            } => implementation.args.iter().chain(own).cloned().collect(),
            Callable::Item {
                item,
                role: Role::Body,
                ..
            } => {
                let mut own = own.iter();
                self.sig(item)
                    .generics
                    .iter()
                    .zip(implementation.args.iter())
                    .map(|(def, arg)| match def.param.owner == item {
                        true => own.next().cloned().unwrap_or_else(|| arg.clone()),
                        false => arg.clone(),
                    })
                    .collect()
            }
            Callable::Item { .. } | Callable::Builtin(_) => return implementation,
        };
        FnRef {
            args,
            ..implementation
        }
    }

    /// The substitution of the generic parameters of an item's unit from the type arguments of a
    /// reference to one of its functions, in the item's order. A reference into an interface's
    /// unit then gives the referenced method's own, which stand for the own parameters of every
    /// method by position, as each function of the unit belongs to one method.
    pub fn unit_subst(&self, item: ItemId, args: &[Arg]) -> Subst {
        let sig = self.sig(item);
        let mut subst = Subst::of(sig.generics.iter().map(|def| def.param), args);
        if let SigKind::Interface { methods } = &sig.kind {
            let own = args.get(sig.generics.len()..).unwrap_or_default();
            for method in methods {
                for (def, arg) in method.sig.own_generics().iter().zip(own) {
                    subst.insert(def.param, arg.clone());
                }
            }
        }
        subst
    }

    /// Whether a function is a foreign fn.
    pub fn is_foreign(&self, function: ItemId) -> bool {
        matches!(&self.db.ast(function).kind, ItemKind::Fn(item)
            if item.modifier.is_some_and(|(modifier, _)| modifier == ast::FnModifier::Foreign))
    }

    /// The index among an interface's default bodies of a method's default body.
    pub fn default_index(&self, interface: ItemId, method: usize) -> u32 {
        match &self.sig(interface).kind {
            SigKind::Interface { methods } => methods[..method]
                .iter()
                .filter(|method| method.default)
                .count() as u32,
            _ => 0,
        }
    }

    /// The interface method a function implements through an impl line naming its type.
    pub fn implemented_method(&self, function: ItemId) -> Option<Implemented> {
        let ItemKind::Fn(item) = &self.db.ast(function).kind else {
            return None;
        };
        let self_ty = match self.db.owners.get(&function)? {
            TypeKey::Item(owner) => self.own_type(*owner),
            TypeKey::Scalar(scalar) => Ty::Scalar(*scalar),
            TypeKey::Unit => Ty::Unit,
        };
        self.implemented_lines(function)
            .into_iter()
            .find_map(|line| {
                let SigKind::Impl {
                    interface: Some(bound),
                    ..
                } = &self.sig(line).kind
                else {
                    return None;
                };
                let method = self.interface_method(bound.interface, &item.label.name.text)?;
                Some(Implemented {
                    interface: bound.interface,
                    method,
                    args: self.interface_args(bound.interface, &self_ty, &bound.args),
                })
            })
    }

    /// The ensures an implementing function inherits from the interface method it implements:
    /// none when that method has no ensures clause that is not disputed.
    fn inherited(&self, function: ItemId) -> Option<Inherited> {
        let implemented = self.implemented_method(function)?;
        let ensures: Vec<EnsuresClause> = self
            .ensures_clauses(implemented.interface, Some(implemented.method))
            .into_iter()
            .filter(|clause| !clause.disputed)
            .collect();
        if ensures.is_empty() {
            return None;
        }
        let requires = self
            .method_requires(implemented.interface, implemented.method)
            .into_iter()
            .filter(|(_, disputed)| !disputed)
            .map(|(index, _)| index)
            .collect();
        Some(Inherited {
            implemented,
            ensures,
            requires,
            requires_values: Vec::new(),
            old_values: HashMap::new(),
        })
    }

    /// The type arguments of an interface's unit for a `Self` type and the arguments of a bound
    /// naming the interface: the interface's own parameters', then `Self`.
    pub fn interface_args(&self, interface: ItemId, self_ty: &Ty, args: &[Arg]) -> Args {
        let own = self.sig(interface).generics.len().saturating_sub(1);
        args.iter()
            .take(own)
            .cloned()
            .chain(std::iter::once(Arg::Ty(self_ty.clone())))
            .collect()
    }

    /// The number of predicates of a struct.
    pub fn predicate_count(&self, item: ItemId) -> u32 {
        match &self.db.ast(item).kind {
            ItemKind::Struct(structure) => structure
                .members
                .iter()
                .filter(|member| match member {
                    ast::StructMember::Field(field) => field.where_clause.is_some(),
                    ast::StructMember::Invariant(_) => true,
                })
                .count() as u32,
            _ => 0,
        }
    }

    /// Whether a value of the type reaches a struct with predicates through its fields.
    pub fn has_predicates_within(&self, ty: &Ty) -> bool {
        let mut seen = Vec::new();
        self.reaches_predicates(ty, &mut seen)
    }

    fn reaches_predicates(&self, ty: &Ty, seen: &mut Vec<ItemId>) -> bool {
        let Ty::Item(item, args) = ty else {
            return false;
        };
        if seen.contains(item) {
            return false;
        }
        seen.push(*item);
        if self.predicate_count(*item) > 0 {
            return true;
        }
        let sig = self.sig(*item);
        let SigKind::Struct { fields } = &sig.kind else {
            return false;
        };
        let subst = super::ty::Subst::of(sig.generics.iter().map(|def| def.param), args);
        fields
            .iter()
            .any(|field| self.reaches_predicates(&subst.ty(&field.ty), seen))
    }
}

/// The clauses of one role of a typed body, by index from `first`, with whether each is
/// disputed.
fn clause_indexes(body: &TBody, role: ClauseRole, first: u32) -> Vec<(u32, bool)> {
    body.clauses
        .iter()
        .filter(|clause| clause.role == role)
        .enumerate()
        .map(|(index, clause)| (first + index as u32, clause.disputed))
        .collect()
}

/// The interface method a function implements: the interface, the method's index, and the
/// interface's unit's type arguments with the function's type as `Self`.
pub struct Implemented {
    pub interface: ItemId,
    pub method: usize,
    pub args: Args,
}

/// An ensures clause as its unit's functions compute it: its ensures function's index, the
/// `old` functions it reads, in order, and whether it is disputed.
pub struct EnsuresClause {
    pub index: u32,
    pub olds: Vec<u32>,
    pub disputed: bool,
}

/// The typed predicates of a struct: its fields as the locals the conditions name, with their
/// names, and its `where` and `invariant` conditions in member order.
pub struct TypedPredicates {
    pub body: TBody,
    pub fields: Vec<(LocalId, String)>,
    pub conditions: Vec<TExpr>,
}

/// The typed `where` conditions of an item's const parameters, in parameter order, with the
/// const parameters and their types.
pub struct TypedWheres {
    pub body: TBody,
    pub params: Vec<(GenericParam, Scalar)>,
    pub conditions: Vec<TExpr>,
}

/// The functions of a unit as elaboration produces them, and its numbering of indexed roles.
pub struct UnitDraft {
    pub item: ItemId,
    functions: Vec<Function>,
    counts: HashMap<Role, u32>,
    /// The index of each invariant line and assume statement, by its condition's first token.
    conditions: HashMap<(Role, Tok), u32>,
    /// The first token of each trusted block or expression, by premise index.
    premises: Vec<Tok>,
}

impl UnitDraft {
    fn new(item: ItemId) -> UnitDraft {
        UnitDraft {
            item,
            functions: Vec::new(),
            counts: HashMap::new(),
            conditions: HashMap::new(),
            premises: Vec::new(),
        }
    }

    /// The number of functions of a role so far.
    fn count(&self, role: Role) -> u32 {
        self.counts.get(&role).copied().unwrap_or(0)
    }

    /// The next index of a role.
    fn next(&mut self, role: Role) -> u32 {
        let count = self.counts.entry(role).or_default();
        *count += 1;
        *count - 1
    }

    /// Numbers the old terms of a function's ensures clauses in source order, after earlier
    /// methods' terms; returns each one's index by its first token.
    fn number_olds(&mut self, body: &TBody) -> HashMap<Tok, u32> {
        let mut tokens: Vec<Tok> = Vec::new();
        for clause in body
            .clauses
            .iter()
            .filter(|clause| clause.role == ClauseRole::Ensures)
        {
            collect_olds(&clause.expr, &mut |token, _| tokens.push(token));
        }
        tokens.sort();
        tokens
            .into_iter()
            .map(|token| (token, self.next(Role::Old)))
            .collect()
    }

    /// Numbers the loop invariants and `assume` conditions of a body in source order, after
    /// earlier methods'.
    fn number_conditions(&mut self, body: &TBody) {
        let mut conditions: Vec<(Role, &TExpr)> = Vec::new();
        if let Some(root) = &body.root {
            collect_conditions(root, &mut conditions);
        }
        for (role, condition) in conditions {
            let index = self.next(role);
            self.conditions.insert((role, condition.token), index);
        }
    }

    /// The premise index of a trusted block or expression, by its first token, numbered in
    /// order of first request.
    fn premise(&mut self, token: Tok) -> u32 {
        match self.premises.iter().position(|known| *known == token) {
            Some(index) => index as u32,
            None => {
                self.premises.push(token);
                self.premises.len() as u32 - 1
            }
        }
    }

    fn add(&mut self, function: Function) {
        self.functions.push(function);
    }

    fn finish(mut self) -> Unit {
        self.functions
            .sort_by_key(|function| (function.role, function.index));
        Unit {
            item: self.item,
            functions: self.functions,
        }
    }
}

/// How a local of the typed body is held in the function being elaborated.
#[derive(Clone, Copy, Debug)]
pub enum Held {
    /// A value: a Copy or view binding, a parameter received by value or reference, or a name a
    /// pattern binds by `load` or reference.
    Value(Value),
    /// A slot holding the binding.
    Slot(Value),
}

/// The ensures clauses a function checks at each return: its own, by index from `first`, with
/// the indexes of the old terms each reads, and those it inherits.
#[derive(Default)]
pub struct Exits {
    pub ensures: Vec<(u32, Vec<u32>)>,
    pub inherited: Option<Inherited>,
}

impl Exits {
    fn of(body: &TBody, olds: &HashMap<Tok, u32>, first: u32) -> Exits {
        let ensures = body
            .clauses
            .iter()
            .filter(|clause| clause.role == ClauseRole::Ensures)
            .enumerate()
            .filter(|(_, clause)| !clause.disputed)
            .map(|(index, clause)| {
                let mut read: Vec<u32> = Vec::new();
                collect_olds(&clause.expr, &mut |token, _| read.push(olds[&token]));
                read.sort();
                (first + index as u32, read)
            })
            .collect();
        Exits {
            ensures,
            inherited: None,
        }
    }
}

/// The ensures an implementing function meets for the interface method it implements, where
/// that method's requires held at entry: the interface's unit with its type arguments, the
/// clauses that are not disputed and the requires that are not disputed, with the values entry
/// computes of each requires and old term.
pub struct Inherited {
    pub implemented: Implemented,
    pub ensures: Vec<EnsuresClause>,
    pub requires: Vec<u32>,
    pub requires_values: Vec<Value>,
    pub old_values: HashMap<u32, Value>,
}

/// A scope of the code being elaborated: the slots it created and the deferred calls registered
/// with it.
#[derive(Default)]
pub struct Scope {
    pub slots: Vec<Value>,
    pub deferred: Vec<Deferred>,
}

/// A deferred call: its call expression, and the places it names.
#[derive(Clone)]
pub struct Deferred {
    pub token: Tok,
    pub call: TExpr,
    pub places: Vec<Place>,
}

/// The targets of a loop being elaborated: its header and exit blocks and the scope depth
/// outside its body.
pub struct Loop {
    pub header: crate::nir::BlockId,
    pub exit: crate::nir::BlockId,
    pub depth: usize,
}

/// An `await when` body being elaborated: the scope depth outside it and the exits leaving it.
pub struct WhenBody {
    pub depth: usize,
    pub leaves: Vec<Leave>,
}

/// An exit leaving an `await when` body: the block of the enclosing code its region jumps to,
/// that block's parameter holding the value the exit carries, the places moved on its path,
/// and the exit it continues there.
pub struct Leave {
    pub block: crate::nir::BlockId,
    pub value: Option<Value>,
    pub moved: BTreeSet<Place>,
    pub exit: Exit,
    pub token: Tok,
}

pub enum Exit {
    Return,
    Fail,
    Loop { index: usize, breaks: bool },
}

/// The elaboration of one NIR function.
pub struct Elab<'e, 'a> {
    pub ck: &'e Checker<'a>,
    pub item: ItemId,
    pub unit: &'e mut UnitDraft,
    pub body: &'e TBody,
    pub role: Role,
    pub b: Builder,
    pub params: Vec<Param>,
    pub held: HashMap<LocalId, Held>,
    /// The places moved on the path being elaborated.
    pub moved: BTreeSet<Place>,
    pub scopes: Vec<Scope>,
    pub loops: Vec<Loop>,
    /// The loops of the enclosing code a closure lies in, which the typed body numbers first.
    pub loop_base: usize,
    pub exits: Exits,
    /// The function's failure type, into which failures convert.
    pub failure_ty: Option<Ty>,
    /// The function's result type, whose values the exit's predicate checks cover.
    pub exit_ty: Option<Ty>,
    /// The function's own parameters and captures with their modes.
    pub own_params: Vec<(LocalId, Mode)>,
    /// The `take` parameters, which entry stores into slots.
    pub take_params: Vec<(LocalId, Value)>,
    /// The slots of `take` parameters, released after the ensures checks.
    pub take_slots: Vec<Value>,
    /// The value of each old term at entry, by index.
    pub old_values: HashMap<u32, Value>,
    /// The old parameter of an ensures function, by its term's first token.
    pub olds: HashMap<Tok, Value>,
    /// The `result` parameter of an ensures function.
    pub result: Option<Value>,
    /// The decreases measure at entry.
    pub measure: Option<Value>,
    /// The struct value and field of each field local of a predicate function.
    pub fields: HashMap<LocalId, (Value, String)>,
    /// The parameter of each const parameter of a `where` function.
    pub const_params: HashMap<GenericParam, Value>,
    /// The premise index of the trusted code being elaborated.
    pub trusted: Option<u32>,
    /// The temporaries of the statement being elaborated still holding values on the path being
    /// elaborated, released at its end.
    pub temps: Vec<Value>,
    /// The wrapped parts projection calls give, which own nothing.
    pub wrapped: HashSet<Value>,
    /// The `await when` body being elaborated.
    pub when: Option<WhenBody>,
    /// The scope whose slots the temporaries of the statement being elaborated are.
    pub temp_scope: usize,
}

impl<'e, 'a> Elab<'e, 'a> {
    fn new(
        ck: &'e Checker<'a>,
        unit: &'e mut UnitDraft,
        body: &'e TBody,
        role: Role,
    ) -> Elab<'e, 'a> {
        Elab {
            ck,
            item: unit.item,
            unit,
            body,
            role,
            b: Builder::new(),
            params: Vec::new(),
            held: HashMap::new(),
            moved: BTreeSet::new(),
            scopes: vec![Scope::default()],
            loops: Vec::new(),
            loop_base: 0,
            exits: Exits::default(),
            failure_ty: None,
            exit_ty: None,
            own_params: Vec::new(),
            take_params: Vec::new(),
            take_slots: Vec::new(),
            old_values: HashMap::new(),
            olds: HashMap::new(),
            result: None,
            measure: None,
            fields: HashMap::new(),
            const_params: HashMap::new(),
            trusted: None,
            temps: Vec::new(),
            wrapped: HashSet::new(),
            temp_scope: 0,
            when: None,
        }
    }

    /// The NIR type of a parameter received in a mode: `take` by value, `mut` by writable
    /// reference, read by value when Copy or a view and by reference otherwise.
    pub fn param_ty(&self, mode: Mode, ty: &Ty) -> NTy {
        match mode {
            Mode::Take => NTy::Owned(ty.clone()),
            Mode::Mut => NTy::RefMut(ty.clone()),
            Mode::Read if self.ck.nir_copy(ty) || self.ck.nir_view(ty) => NTy::Owned(ty.clone()),
            Mode::Read => NTy::Ref(ty.clone()),
        }
    }

    /// Declares the parameters of a body or closure in their modes; a missing mode is the
    /// local's own.
    fn declare_params(&mut self, locals: &[LocalId], modes: &[Mode]) {
        for (index, local) in locals.iter().enumerate() {
            let data = self.body.local(*local);
            let mode = modes.get(index).copied().unwrap_or(match data.kind {
                LocalKind::Param(mode) => mode,
                _ => Mode::Read,
            });
            let value = self.b.value(self.param_ty(mode, &data.ty));
            self.params.push(Param {
                mode,
                value,
                name: data.name.clone(),
                span: Span::at(data.token),
            });
            self.held.insert(*local, Held::Value(value));
            self.own_params.push((*local, mode));
            if mode == Mode::Take {
                self.take_params.push((*local, value));
            }
        }
    }

    /// Declares read parameters for locals of the typed body, as contract functions take them.
    fn declare_read_params(&mut self, locals: &[LocalId]) {
        for local in locals {
            let data = self.body.local(*local);
            let value = self.read_param(&data.name.clone(), &data.ty.clone(), Span::at(data.token));
            self.held.insert(*local, Held::Value(value));
        }
    }

    /// A read parameter of a contract function.
    fn read_param(&mut self, name: &str, ty: &Ty, span: Span) -> Value {
        let value = self.b.value(self.param_ty(Mode::Read, ty));
        self.params.push(Param {
            mode: Mode::Read,
            value,
            name: name.to_owned(),
            span,
        });
        value
    }

    /// The entry code: a slot per `take` parameter, then a call of each old function of the
    /// ensures clauses in index order, then of the decreases function.
    fn enter(&mut self, olds: &HashMap<Tok, u32>) -> Elaborated<()> {
        for (local, value) in std::mem::take(&mut self.take_params) {
            let data = self.body.local(local);
            let span = Span::at(data.token);
            let ty = data.ty.clone();
            let slot = self.emit_value(
                Op::Slot {
                    name: Some(data.name.clone()),
                    ty: ty.clone(),
                },
                NTy::Owned(ty),
                span,
            );
            self.emit(Op::Init(Place::of(slot), Operand::Value(value)), span);
            self.held.insert(local, Held::Slot(slot));
            self.take_slots.push(slot);
        }
        let mut terms: Vec<(Tok, u32)> =
            olds.iter().map(|(token, index)| (*token, *index)).collect();
        terms.sort_by_key(|(_, index)| *index);
        let decreases = self
            .body
            .clauses
            .iter()
            .filter(|_| matches!(self.role, Role::Body | Role::Default))
            .find(|clause| clause.role == ClauseRole::Decreases)
            .map(|clause| (clause.token, clause.expr.ty.clone()));
        if terms.is_empty() && decreases.is_none() && self.exits.inherited.is_none() {
            return Ok(());
        }
        let args = self.param_operands()?;
        if let Some(mut inherited) = self.exits.inherited.take() {
            let span = Span::at(self.ck.db.ast(self.item).token);
            let interface = |role, index| FnRef {
                callable: Callable::Item {
                    item: inherited.implemented.interface,
                    role,
                    index,
                },
                args: inherited.implemented.args.clone(),
            };
            for index in inherited.requires.clone() {
                let fn_ref = interface(Role::Requires, index);
                let value = self.call_value(fn_ref, args.clone(), &Ty::Scalar(Scalar::Bool), span);
                inherited.requires_values.push(value);
            }
            let olds: Vec<u32> = inherited
                .ensures
                .iter()
                .flat_map(|clause| clause.olds.clone())
                .collect();
            for old in olds {
                let ty = self.interface_old_ty(&inherited.implemented, old);
                let value = self.call_value(interface(Role::Old, old), args.clone(), &ty, span);
                inherited.old_values.insert(old, value);
            }
            self.exits.inherited = Some(inherited);
        }
        for (token, index) in terms {
            let ty = self.old_term_ty(token);
            let value = self.call_value(
                self.own(Role::Old, index),
                args.clone(),
                &ty,
                Span::at(token),
            );
            self.old_values.insert(index, value);
        }
        if let Some((token, ty)) = decreases {
            let value = self.call_value(self.own(Role::Decreases, 0), args, &ty, Span::at(token));
            self.measure = Some(value);
        }
        Ok(())
    }

    /// The type an interface's old function returns, its `Self` the implementing type.
    fn interface_old_ty(&self, implemented: &Implemented, old: u32) -> Ty {
        let interface = implemented.interface;
        let result = self
            .ck
            .unit(interface)
            .and_then(|unit| {
                unit.function(Role::Old, old)
                    .map(|function| function.result.clone())
            })
            .unwrap_or(Ty::Error);
        let generics = &self.ck.sig(interface).generics;
        super::ty::Subst::of(generics.iter().map(|def| def.param), &implemented.args).ty(&result)
    }

    /// The type of the old term starting at a token.
    fn old_term_ty(&self, token: Tok) -> Ty {
        let mut found = Ty::Error;
        for clause in &self.body.clauses {
            collect_olds(&clause.expr, &mut |at, ty| {
                if at == token {
                    found = ty.clone();
                }
            });
        }
        found
    }

    /// A reference to a NIR function of this unit.
    pub fn own(&self, role: Role, index: u32) -> FnRef {
        FnRef {
            callable: Callable::Item {
                item: self.item,
                role,
                index,
            },
            args: self.own_args(),
        }
    }

    /// The unit's generic parameters as type arguments, as its own functions are called.
    fn own_args(&self) -> Args {
        self.ck
            .sig(self.item)
            .generics
            .iter()
            .map(|def| match def.constant {
                Some(_) => Arg::Const(Const::Param(def.param)),
                None => Arg::Ty(Ty::Param(def.param)),
            })
            .collect()
    }

    /// The operands passing the function's parameters by read, `take` ones from their slots.
    pub fn param_operands(&mut self) -> Elaborated<Vec<Operand>> {
        let mut operands = Vec::new();
        for local in self.body.params.clone() {
            let data = self.body.local(local);
            let (ty, span) = (data.ty.clone(), Span::at(data.token));
            let place = self.local_place(local)?;
            operands.push(self.read_operand(place, &ty, span));
        }
        Ok(operands)
    }

    pub fn emit(&mut self, op: Op, span: Span) {
        self.b.emit(op, None, span, self.trusted);
    }

    pub fn emit_value(&mut self, op: Op, ty: NTy, span: Span) -> Value {
        self.b
            .emit(op, Some(ty), span, self.trusted)
            .expect("an open block")
    }

    pub fn end(&mut self, term: crate::nir::Terminator, span: Span) {
        self.b.end(term, span);
    }

    /// The finished function.
    fn finish(
        self,
        result: Ty,
        fails: Option<Ty>,
        wait: bool,
        closure: Option<ClosureHead>,
        span: Span,
    ) -> Function {
        let values: Vec<Value> = self.params.iter().map(|param| param.value).collect();
        let (blocks, types, renumbered) = self.b.finish(&values);
        let params = self
            .params
            .into_iter()
            .zip(renumbered)
            .map(|(param, value)| Param { value, ..param })
            .collect();
        Function {
            role: self.role,
            index: 0,
            params,
            result,
            fails,
            wait,
            closure,
            blocks,
            types,
            span,
        }
    }
}

impl FnSig {
    /// The modes of its parameters, in order.
    pub fn params_modes(&self) -> Vec<Mode> {
        self.params.iter().map(|param| param.mode).collect()
    }
}

/// A capture of a closure as its function receives it.
pub struct Capture {
    pub local: LocalId,
    pub outer: LocalId,
    pub mode: Mode,
}

/// The captures of a closure in its function's order: its capture list, then its unlisted read
/// captures in order of first use.
pub fn closure_captures(closure: &TClosure) -> Vec<Capture> {
    let mut listed: Vec<(Tok, Capture)> = closure
        .captures
        .iter()
        .filter_map(|capture| {
            let (mode, token) = capture.listed?;
            Some((
                token,
                Capture {
                    local: capture.local,
                    outer: capture.outer,
                    mode,
                },
            ))
        })
        .collect();
    listed.sort_by_key(|(token, _)| *token);
    let unlisted = closure
        .captures
        .iter()
        .filter(|capture| capture.listed.is_none())
        .map(|capture| Capture {
            local: capture.local,
            outer: capture.outer,
            mode: Mode::Read,
        });
    listed
        .into_iter()
        .map(|(_, capture)| capture)
        .chain(unlisted)
        .collect()
}

/// The closures an expression holds, at any depth.
fn collect_closures<'b>(expr: &'b TExpr, found: &mut Vec<&'b TClosure>) {
    if let TKind::Closure(closure) = &expr.kind {
        found.push(closure);
    }
    expr.each_child(&mut |child| collect_closures(child, found));
}

/// Visits each `old(e)` term of an ensures clause with its first token and the type of `e`.
fn collect_olds(expr: &TExpr, visit: &mut dyn FnMut(Tok, &Ty)) {
    if let TKind::Old(inner) = &expr.kind {
        visit(expr.token, &inner.ty);
        return;
    }
    expr.each_child(&mut |child| collect_olds(child, visit));
}

/// The `e` of each `old(e)` term, with the term's first token.
fn collect_old_terms<'b>(expr: &'b TExpr, found: &mut Vec<(Tok, &'b TExpr)>) {
    if let TKind::Old(inner) = &expr.kind {
        found.push((expr.token, inner));
        return;
    }
    expr.each_child(&mut |child| collect_old_terms(child, found));
}

/// The loop invariants and `assume` conditions of a body, closures included, in source order.
fn collect_conditions<'b>(expr: &'b TExpr, found: &mut Vec<(Role, &'b TExpr)>) {
    walk_conditions(expr, found);
    found.sort_by_key(|(_, condition)| condition.token);
}

fn walk_conditions<'b>(expr: &'b TExpr, found: &mut Vec<(Role, &'b TExpr)>) {
    if let TKind::Block(block) = &expr.kind {
        for stmt in &block.stmts {
            match &stmt.kind {
                TStmtKind::While { invariants, .. }
                | TStmtKind::For { invariants, .. }
                | TStmtKind::Loop { invariants, .. } => {
                    found.extend(
                        invariants
                            .iter()
                            .map(|invariant| (Role::Invariant, invariant)),
                    );
                }
                TStmtKind::Assume(condition) => found.push((Role::Assume, condition)),
                _ => {}
            }
        }
    }
    expr.each_child(&mut |child| walk_conditions(child, found));
}

/// The bindings a condition names, by first mention, its quantifiers' own excepted.
pub fn named_bindings(expr: &TExpr) -> Vec<LocalId> {
    let mut named: Vec<LocalId> = Vec::new();
    let mut quantified: HashSet<LocalId> = HashSet::new();
    collect_locals(expr, &mut named, &mut quantified);
    named
}

fn collect_locals(expr: &TExpr, named: &mut Vec<LocalId>, quantified: &mut HashSet<LocalId>) {
    match &expr.kind {
        TKind::Local(local) if !quantified.contains(local) && !named.contains(local) => {
            named.push(*local);
        }
        TKind::Quantifier { local, .. } => {
            quantified.insert(*local);
        }
        _ => {}
    }
    expr.each_child(&mut |child| collect_locals(child, named, quantified));
}

#[cfg(test)]
mod tests;
