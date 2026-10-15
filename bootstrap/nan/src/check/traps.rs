//! The may-trap digest fact, which the statuses of a function's obligations decide, and the
//! rules it breaks: limits and fits demanding notrap, and the callees of contract terms.

use std::collections::HashMap;
use std::rc::Rc;

use super::Checker;
use super::db::ItemId;
use super::sig::SigKind;
use super::tir::{LocalKind, TBody, TExpr, TKind};
use super::ty::{LimitSet, Ty};
use crate::diagnostic::{Code, Role as Because};
use crate::nir::{Callable, Callee, Function, Op, Operand, Role, Span, Terminator, Unit, Value};
use crate::prover::Status;
use crate::syntax::ast::{self, ItemKind, Tok};

/// What brings the may-trap fact into a function's code.
#[derive(Clone, Copy, Debug)]
enum Source {
    /// An obligation keeping a run-time check, or an operation trapping without an obligation.
    Own(Span, bool),
    /// A direct call, or a named function supplied as an argument, reached through its
    /// checking entry when `entry` holds.
    Callee(ItemId, Span, bool),
    /// A call through, or a value supplied of, a declared digest that lacks notrap.
    Declared(Span),
}

impl Checker<'_> {
    /// Whether a function's digest has may-trap: its own code holds a trap site, or a function
    /// it calls directly, a declared digest it calls through, or a function argument it
    /// supplies has may-trap; the members of a cycle share it. A std function without a body
    /// has its supplied digest's; any other function without a body or a unit has its declared
    /// digest's.
    pub fn may_trap(&self, item: ItemId) -> bool {
        self.db.note(item);
        if let Some(known) = self.may_traps.borrow().get(&item) {
            return *known;
        }
        self.db.about(Some(item), || self.compute_may_trap(item))
    }

    fn compute_may_trap(&self, item: ItemId) -> bool {
        let members = self.cycle_of(item);
        let mut traps = false;
        for member in &members {
            let bodiless = matches!(&self.db.ast(*member).kind,
                ItemKind::Fn(function) if matches!(function.body, ast::FnBody::None));
            if bodiless && self.db.is_std(*member) {
                traps |= self.supplied_traps(*member);
                continue;
            }
            let Some(unit) = self.unit(*member).filter(|_| !bodiless) else {
                traps |= self.fn_sig(*member).is_none_or(|sig| !sig.limits.notrap);
                continue;
            };
            for source in self.trap_sources(*member, &unit, Role::Body, 0) {
                traps |= match source {
                    Source::Own(..) | Source::Declared(_) => true,
                    Source::Callee(callee, _, entry) => {
                        (entry && self.entry_traps(callee))
                            || (!members.contains(&callee) && self.may_trap(callee))
                    }
                };
            }
        }
        for member in members {
            self.may_traps.borrow_mut().insert(member, traps);
        }
        traps
    }

    /// Whether a function's checking entry keeps a check.
    fn entry_traps(&self, item: ItemId) -> bool {
        self.obligations(item)
            .iter()
            .any(|site| site.role == Role::Entry && site.status == Status::Checked)
    }

    /// Whether a named function, reached through its checking entry, has may-trap.
    fn traps_through_entry(&self, item: ItemId) -> bool {
        self.entry_traps(item) || self.may_trap(item)
    }

    /// The functions of a function's cycle, itself included.
    fn cycle_of(&self, item: ItemId) -> Vec<ItemId> {
        let mut reachable = vec![item];
        let mut index = 0;
        while index < reachable.len() {
            for dependency in self.dependencies(reachable[index]) {
                if !reachable.contains(&dependency) {
                    reachable.push(dependency);
                }
            }
            index += 1;
        }
        reachable
            .into_iter()
            .filter(|member| *member == item || self.same_cycle(item, *member))
            .collect()
    }

    /// What brings may-trap into a body or closure function of a unit: its checked obligations
    /// and `when` operations, its calls, and the functions and closures it supplies as
    /// arguments, a closure's own sources included.
    fn trap_sources(&self, item: ItemId, unit: &Rc<Unit>, role: Role, index: u32) -> Vec<Source> {
        let Some(function) = unit.function(role, index) else {
            return Vec::new();
        };
        let mut sources: Vec<Source> = self
            .obligations(item)
            .iter()
            .filter(|site| {
                site.role == role && site.index == index && site.status == Status::Checked
            })
            .map(|site| Source::Own(site.span, true))
            .collect();
        let defined = definitions(function);
        let params: Vec<Value> = function.params.iter().map(|param| param.value).collect();
        for block in &function.blocks {
            let mut calls: Vec<(&crate::nir::Call, Span)> = Vec::new();
            for inst in &block.insts {
                match &inst.op {
                    Op::Call(call) => calls.push((call, inst.span)),
                    Op::When { attempt: false, .. } => sources.push(Source::Own(inst.span, false)),
                    _ => {}
                }
            }
            if let Terminator::Call { call, .. } = &block.term {
                calls.push((call, block.term_span));
            }
            for (call, span) in calls {
                match &call.callee {
                    Callee::Fn(fn_ref) => match &fn_ref.callable {
                        Callable::Item {
                            item: callee,
                            role: Role::Body,
                            ..
                        } => sources.push(Source::Callee(*callee, span, false)),
                        Callable::Method { interface, method }
                            if !self.method_limits(*interface, *method).notrap =>
                        {
                            sources.push(Source::Declared(span));
                        }
                        _ => {}
                    },
                    Callee::Value(value) => {
                        let own = params.contains(value);
                        let notrap = matches!(function.ty(*value).ty(), Ty::Fn(fn_ty) if fn_ty.limits.notrap);
                        if !own && !notrap {
                            sources.push(Source::Declared(span));
                        }
                    }
                    Callee::Dyn {
                        interface, method, ..
                    } => {
                        if !self.method_limits(*interface, *method).notrap {
                            sources.push(Source::Declared(span));
                        }
                    }
                }
                for arg in &call.args {
                    let Operand::Value(value) = arg else {
                        continue;
                    };
                    match defined.get(value) {
                        Some(Op::FnRef(fn_ref)) => {
                            if let Callable::Item {
                                item: supplied,
                                role: Role::Body,
                                ..
                            } = &fn_ref.callable
                            {
                                sources.push(Source::Callee(*supplied, span, true));
                            }
                        }
                        Some(Op::Closure { index, .. }) => {
                            sources.extend(self.trap_sources(item, unit, Role::Closure, *index));
                        }
                        _ => {}
                    }
                }
            }
        }
        sources
    }

    fn method_limits(&self, interface: ItemId, method: u32) -> LimitSet {
        match &self.sig(interface).kind {
            SigKind::Interface { methods } => methods[method as usize].sig.limits,
            _ => LimitSet::default(),
        }
    }

    /// The rules may-trap breaks in an item: each site of the text of a function or closure
    /// demanding notrap that brings may-trap, other than an obligation, which the prover's
    /// outcome reports (`limit.may-trap`); each named function or function value with may-trap
    /// supplied where a function type demands notrap (`limit.may-trap`); and each callee of a
    /// contract term with an effect, without terminates or with may-trap
    /// (`contract.ill-defined`).
    pub fn check_traps(&self, item: ItemId) {
        let Some(unit) = self.unit(item) else {
            return;
        };
        let mut reported: Vec<Tok> = Vec::new();
        if let ItemKind::Fn(_) = &self.db.ast(item).kind {
            let body = self.typed_body(item);
            let closures: Vec<u32> = unit
                .functions
                .iter()
                .filter(|function| function.role == Role::Closure)
                .map(|function| function.index)
                .collect();
            for index in closures {
                if body
                    .as_ref()
                    .is_some_and(|body| closure_demands_notrap(body, index))
                {
                    self.report_traps(item, &unit, Role::Closure, index, &mut reported);
                }
            }
            if self.demands_notrap(item) {
                self.report_traps(item, &unit, Role::Body, 0, &mut reported);
                for function in unit
                    .functions
                    .iter()
                    .filter(|function| function.role == Role::Closure)
                {
                    self.report_traps(item, &unit, Role::Closure, function.index, &mut reported);
                }
            }
        }
        for function in unit
            .functions
            .iter()
            .filter(|function| function.role.is_contract())
        {
            self.check_contract_callees(item, function);
        }
        let body = self.typed(item);
        if let Some(body) = body
            && let Some(root) = &body.root
        {
            self.check_fit_traps(item, &body, root);
        }
    }

    fn report_traps(
        &self,
        item: ItemId,
        unit: &Rc<Unit>,
        role: Role,
        index: u32,
        reported: &mut Vec<Tok>,
    ) {
        for source in self.trap_sources(item, unit, role, index) {
            let (span, called) = match source {
                Source::Own(_, true) => continue,
                Source::Own(span, false) | Source::Declared(span) => (span, None),
                Source::Callee(callee, span, entry) => {
                    let traps = match entry {
                        true => self.traps_through_entry(callee),
                        false => self.may_trap(callee),
                    };
                    if !traps {
                        continue;
                    }
                    (span, Some(callee))
                }
            };
            if reported.contains(&span.first) {
                continue;
            }
            reported.push(span.first);
            let mut because = Vec::new();
            if let Some(callee) = called {
                because.push(self.reason(
                    item,
                    span.first,
                    Because::Called,
                    self.db.canonical(callee),
                ));
            }
            because.push(self.reason(item, span.first, Because::Raised, "may-trap".to_owned()));
            self.report_because(item, span.first, Code::LimitMayTrap, because);
        }
    }

    /// Reports each call of a contract function to a callee whose digest is not pure, lacks
    /// terminates or has may-trap.
    fn check_contract_callees(&self, item: ItemId, function: &Function) {
        for block in &function.blocks {
            for inst in &block.insts {
                let Op::Call(call) = &inst.op else {
                    continue;
                };
                let Callee::Fn(fn_ref) = &call.callee else {
                    continue;
                };
                let Callable::Item {
                    item: callee,
                    role: Role::Body,
                    ..
                } = &fn_ref.callable
                else {
                    continue;
                };
                let digest = self.digest(*callee);
                if !digest.pure() || !digest.terminates || self.may_trap(*callee) {
                    self.report(item, inst.span.first, Code::ContractIllDefined);
                }
            }
        }
    }

    /// Reports each named function or function value with may-trap that becomes a value of a
    /// function type demanding notrap: as an argument or a construction's value.
    fn check_fit_traps(&self, item: ItemId, body: &TBody, expr: &TExpr) {
        let mut supplied: Vec<(&TExpr, Ty)> = Vec::new();
        match &expr.kind {
            TKind::Call(call) | TKind::Index(call) => {
                for (arg, param) in call.args.iter().zip(&call.head.params) {
                    supplied.push((&arg.expr, param.ty.clone()));
                }
            }
            TKind::Construct { variant, values } => {
                let fields = self.construct_fields(&expr.ty, *variant);
                for (name, value) in values {
                    if let Some(ty) = fields.get(name) {
                        supplied.push((value, ty.clone()));
                    }
                }
            }
            _ => {}
        }
        for (value, wanted) in supplied {
            let Ty::Fn(wanted) = wanted else {
                continue;
            };
            if !wanted.limits.notrap {
                continue;
            }
            let traps = match &value.kind {
                TKind::FnValue { item: function, .. } => self.traps_through_entry(*function),
                TKind::Local(local) if !matches!(body.local(*local).kind, LocalKind::Param(_)) => {
                    matches!(&value.ty, Ty::Fn(found) if !found.limits.notrap)
                }
                _ => false,
            };
            if traps {
                self.report(item, value.token, Code::LimitMayTrap);
            }
        }
        expr.each_child(&mut |child| self.check_fit_traps(item, body, child));
    }

    /// The types of the fields or payload fields a construction of a type names, by name.
    fn construct_fields(&self, ty: &Ty, variant: Option<usize>) -> HashMap<String, Ty> {
        let Ty::Item(owner, args) = ty else {
            return HashMap::new();
        };
        let sig = self.sig(*owner);
        let subst = super::ty::Subst::of(sig.generics.iter().map(|def| def.param), args);
        match (&sig.kind, variant) {
            (SigKind::Struct { fields }, None) => fields
                .iter()
                .map(|field| (field.name.clone(), subst.ty(&field.ty)))
                .collect(),
            (SigKind::Enum { variants }, Some(index)) => match &variants[index].payload {
                super::sig::Payload::Labelled(fields) => fields
                    .iter()
                    .map(|(name, ty)| (name.clone(), subst.ty(ty)))
                    .collect(),
                _ => HashMap::new(),
            },
            _ => HashMap::new(),
        }
    }

    /// Whether a function's limits demand notrap: through its own `limits` clause, the
    /// interface method it implements, or its body being transparent.
    pub fn demands_notrap(&self, item: ItemId) -> bool {
        let ItemKind::Fn(function) = &self.db.ast(item).kind else {
            return false;
        };
        if matches!(function.body, ast::FnBody::Expr(_)) {
            return true;
        }
        self.fn_sig(item).is_some_and(|sig| sig.limits.notrap)
            || self.implemented_method(item).is_some_and(|implemented| {
                self.method_limits(implemented.interface, implemented.method as u32)
                    .notrap
            })
    }

    /// The functions a function calls directly or supplies as function arguments.
    fn dependencies(&self, item: ItemId) -> Vec<ItemId> {
        let Some(unit) = self.unit(item) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for function in unit
            .functions
            .iter()
            .filter(|function| matches!(function.role, Role::Body | Role::Closure))
        {
            for block in &function.blocks {
                for inst in &block.insts {
                    let callable = match &inst.op {
                        Op::Call(call) => match &call.callee {
                            Callee::Fn(fn_ref) => Some(&fn_ref.callable),
                            _ => None,
                        },
                        Op::FnRef(fn_ref) => Some(&fn_ref.callable),
                        _ => None,
                    };
                    if let Some(Callable::Item {
                        item: callee,
                        role: Role::Body,
                        ..
                    }) = callable
                        && !found.contains(callee)
                    {
                        found.push(*callee);
                    }
                }
                if let Terminator::Call { call, .. } = &block.term
                    && let Callee::Fn(fn_ref) = &call.callee
                    && let Callable::Item {
                        item: callee,
                        role: Role::Body,
                        ..
                    } = &fn_ref.callable
                    && !found.contains(callee)
                {
                    found.push(*callee);
                }
            }
        }
        found
    }
}

/// The instruction defining each value of a function.
fn definitions(function: &Function) -> HashMap<Value, &Op> {
    function
        .blocks
        .iter()
        .flat_map(|block| block.insts.iter())
        .filter_map(|inst| inst.result.map(|result| (result, &inst.op)))
        .collect()
}

/// Whether the closure of a body numbered `index` is written for a function type demanding
/// notrap, or demands it itself.
pub fn closure_demands_notrap(body: &TBody, index: u32) -> bool {
    let mut demands = false;
    if let Some(root) = &body.root {
        find_closure(root, index, &mut demands);
    }
    demands
}

fn find_closure(expr: &TExpr, index: u32, demands: &mut bool) {
    if let TKind::Closure(closure) = &expr.kind
        && closure.index == index
    {
        *demands = closure.demanded.notrap || closure.ty.limits.notrap;
        return;
    }
    expr.each_child(&mut |child| find_closure(child, index, demands));
}
