//! The normative prover: decides each obligation site of an item's NIR functions through a
//! query over its weakest precondition, gives each site its status, and reports the sites
//! whose proof is demanded and that it does not prove.

mod canon;
mod encode;
mod query;
mod refute;
mod solve;
mod term;
#[cfg(test)]
mod tests;

use std::rc::Rc;

use crate::check::Checker;
use crate::check::db::ItemId;
use crate::check::sig::param_arg;
use crate::check::tir::ClauseRole;
use crate::check::ty::{Arg, Const, Scalar, Subst, Ty};
use crate::diagnostic::{Code, Reason};
use crate::interp::{Input, Interpreter, Site as RunSite};
use crate::nir::{BlockId, Callable, CheckKind, Function, Literal, Role, Span, Unit};
use crate::syntax::ast::{self, ItemKind, Mode};
use canon::Origin;
use query::{Query, Site, Walk};
use solve::{Decision, Solver};
use term::{Term, TermId, Terms};

pub use query::Obligation;

/// How an obligation holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Proven,
    Assumed,
    Checked,
    Open,
}

/// The prover's outcome for an obligation it does not prove: refuted with the labelled values
/// of the counterexample's parameters, or unknown with the reasons.
#[derive(Clone, Debug)]
pub enum Unproved {
    Refuted(Vec<String>),
    Unknown(Vec<String>),
}

/// An obligation site of a unit, with its status and, where proof is demanded and it is not
/// proved, the prover's outcome.
#[derive(Clone, Debug)]
pub struct Obligated {
    pub role: Role,
    pub index: u32,
    pub block: BlockId,
    pub position: usize,
    pub obligation: Obligation,
    pub span: Span,
    pub status: Status,
    /// The index of the ensures clause a `check ensures` checks.
    pub clause: Option<u32>,
    pub unproved: Option<Unproved>,
}

impl Checker<'_> {
    /// The obligation sites of an item's unit with their statuses, by function and site order;
    /// examples have none.
    pub fn obligations(&self, item: ItemId) -> Rc<Vec<Obligated>> {
        self.db.note(item);
        if let Some(done) = self.obligations.borrow().get(&item) {
            return done.clone();
        }
        let sites = self.db.about(Some(item), || match self.unit(item) {
            Some(unit) if !self.db.is_std(item) => self.decide_unit(item, &unit),
            _ => Vec::new(),
        });
        let sites = Rc::new(sites);
        self.obligations.borrow_mut().insert(item, sites.clone());
        sites
    }

    /// Reports each obligation of an item whose proof is demanded and that the prover does not
    /// prove: a `decreases` one as unproven, any other with its outcome.
    pub fn check_proofs(&self, item: ItemId) {
        for site in self.obligations(item).iter() {
            let Some(unproved) = &site.unproved else {
                continue;
            };
            let code = match (site.obligation, unproved) {
                (Obligation::Check(CheckKind::Decreases), _) => Code::ContractDecreasesUnproven,
                (_, Unproved::Refuted(_)) => Code::ProofRefuted,
                (_, Unproved::Unknown(_)) => Code::ProofUnknown,
            };
            let notes = match unproved {
                Unproved::Refuted(notes) | Unproved::Unknown(notes) => notes,
            };
            let because = notes.iter().map(|note| Reason::note(note)).collect();
            self.report_because(item, site.span.first, code, because);
        }
    }

    /// The rules entailment decides on a struct's predicates: predicates that no value
    /// satisfies (`contract.vacuous`), and, for a struct deriving `Default`, a default value
    /// the prover does not prove to meet them (`predicate.default-unproven`); each reported
    /// on the struct's first line.
    pub fn check_predicates(&self, item: ItemId) {
        let ItemKind::Struct(structure) = &self.db.ast(item).kind else {
            return;
        };
        let count = self.predicate_count(item);
        let Some(unit) = self.unit(item).filter(|_| count > 0) else {
            return;
        };
        let Some(function) = unit
            .functions
            .iter()
            .position(|function| function.role == Role::Predicate)
        else {
            return;
        };
        let package = self.db.package_of(item);
        let type_args: Vec<Arg> = self.sig(item).generics.iter().map(param_arg).collect();
        let ty = self.own_type(item);
        let mut terms = Terms::default();
        let value = terms.fresh(ty.clone());
        let default = self.default_term(&mut terms, &ty);
        let mut walk = Walk::new(
            self,
            item,
            &mut terms,
            unit.clone(),
            function,
            &type_args,
            Vec::new(),
            Vec::new(),
        );
        let predicates: Vec<TermId> = (0..count)
            .filter_map(|index| {
                walk.returned(item, Role::Predicate, index, &type_args, vec![value])
            })
            .collect();
        let defaults: Vec<TermId> = (0..count)
            .filter_map(|index| {
                walk.returned(item, Role::Predicate, index, &type_args, vec![default])
            })
            .collect();
        let falsity = terms.bool(false);
        if self.entails(&mut terms, package, &predicates, falsity) {
            self.report(item, self.db.ast(item).token, Code::ContractVacuous);
        } else if structure.derives.iter().any(|name| name.text == "Default")
            && !defaults
                .into_iter()
                .all(|goal| self.entails(&mut terms, package, &[], goal))
        {
            self.report(
                item,
                self.db.ast(item).token,
                Code::PredicateDefaultUnproven,
            );
        }
    }

    /// The rules entailment decides on a function's contract: requires that no input
    /// satisfies, or ensures that no result does (`contract.vacuous`, on the last clause), and
    /// each requires of an implementing function that the interface method's requires do not
    /// entail (`contract.impl-requires`).
    pub fn check_contracts(&self, item: ItemId) {
        let (Some(unit), Some(body), Some(sig)) =
            (self.unit(item), self.typed(item), self.fn_sig(item))
        else {
            return;
        };
        let package = self.db.package_of(item);
        let type_args: Vec<Arg> = self.sig(item).generics.iter().map(param_arg).collect();
        let subst = Subst::of(
            self.sig(item).generics.iter().map(|def| def.param),
            &type_args,
        );
        let clauses = |role: ClauseRole| -> Vec<(u32, ast::Tok)> {
            body.clauses
                .iter()
                .filter(|clause| clause.role == role)
                .enumerate()
                .filter(|(_, clause)| !clause.disputed)
                .map(|(index, clause)| (index as u32, clause.token))
                .collect()
        };
        let requires = clauses(ClauseRole::Requires);
        if let Some(host) = unit
            .functions
            .iter()
            .position(|function| function.role == Role::Requires)
        {
            let mut terms = Terms::default();
            let params = symbols(&unit.functions[host], &subst, &mut terms);
            let mut walk = Walk::new(
                self,
                item,
                &mut terms,
                unit.clone(),
                host,
                &type_args,
                Vec::new(),
                Vec::new(),
            );
            let mut facts = Vec::new();
            for (term, param) in params.iter().zip(&sig.params) {
                walk.predicate_facts(
                    *term,
                    &subst.ty(&param.ty),
                    self.is_boundary(item),
                    &mut facts,
                );
            }
            let own: Vec<(TermId, ast::Tok)> = requires
                .iter()
                .filter_map(|(index, token)| {
                    walk.returned(item, Role::Requires, *index, &type_args, params.clone())
                        .map(|term| (term, *token))
                })
                .collect();
            let mut inherited = facts.clone();
            if let Some(implemented) = self.implemented_method(item) {
                let requires = self.method_requires(implemented.interface, implemented.method);
                for (index, disputed) in requires {
                    if !disputed
                        && let Some(term) = walk.returned(
                            implemented.interface,
                            Role::Requires,
                            index,
                            &implemented.args,
                            params.clone(),
                        )
                    {
                        inherited.push(term);
                    }
                }
                for (goal, token) in &own {
                    if !self.entails(&mut terms, package, &inherited, *goal) {
                        self.report(item, *token, Code::ContractImplRequires);
                    }
                }
            }
            if let Some((_, last)) = own.last() {
                facts.extend(own.iter().map(|(term, _)| *term));
                let falsity = terms.bool(false);
                if self.entails(&mut terms, package, &facts, falsity) {
                    self.report(item, *last, Code::ContractVacuous);
                }
            }
        }
        if let Some(&(index, last)) = clauses(ClauseRole::Ensures).last()
            && let Some(position) = unit
                .functions
                .iter()
                .position(|function| function.role == Role::Ensures && function.index == index)
        {
            let function = &unit.functions[position];
            let mut terms = Terms::default();
            let mut params = symbols(function, &subst, &mut terms);
            let mut walk = Walk::new(
                self,
                item,
                &mut terms,
                unit.clone(),
                position,
                &type_args,
                Vec::new(),
                Vec::new(),
            );
            let mut facts = self.ensures_premises(item, function, &mut walk, &mut params, &subst);
            facts.extend(walk.returned(item, Role::Ensures, index, &type_args, params));
            let falsity = terms.bool(false);
            if self.entails(&mut terms, package, &facts, falsity) {
                self.report(item, last, Code::ContractVacuous);
            }
        }
    }

    /// Whether the `where` conditions of the item writing an instantiation entail those of the
    /// instantiated item for its const arguments.
    pub fn wheres_entailed(
        &self,
        holder: ItemId,
        target: ItemId,
        unit: &Rc<Unit>,
        conditions: &[u32],
        args: &[Arg],
        consts: &[(Scalar, &Arg)],
    ) -> bool {
        let Some(host) = unit
            .functions
            .iter()
            .position(|function| function.role == Role::Where)
        else {
            return true;
        };
        let package = self.db.package_of(holder);
        let mut terms = Terms::default();
        let constant = |terms: &mut Terms, scalar: Scalar, arg: &Arg| match arg {
            Arg::Const(Const::Int(value)) => terms.int(*value, scalar),
            Arg::Const(Const::Bool(value)) => terms.bool(*value),
            Arg::Const(Const::Param(param)) => terms.intern(
                Term::App(
                    term::Op::Constant(crate::nir::Constant::Param(*param)),
                    Vec::new(),
                ),
                Ty::Scalar(scalar),
            ),
            _ => terms.fresh(Ty::Scalar(scalar)),
        };
        let params: Vec<TermId> = consts
            .iter()
            .map(|(scalar, arg)| constant(&mut terms, *scalar, arg))
            .collect();
        let own_args: Vec<Arg> = self.sig(holder).generics.iter().map(param_arg).collect();
        let own_params: Vec<TermId> = self
            .sig(holder)
            .generics
            .iter()
            .filter_map(|def| {
                def.constant
                    .map(|scalar| constant(&mut terms, scalar, &param_arg(def)))
            })
            .collect();
        let mut walk = Walk::new(
            self,
            holder,
            &mut terms,
            unit.clone(),
            host,
            args,
            Vec::new(),
            Vec::new(),
        );
        let premises: Vec<TermId> = self
            .unit(holder)
            .iter()
            .flat_map(|own| {
                own.functions
                    .iter()
                    .filter(|function| function.role == Role::Where)
            })
            .filter_map(|function| {
                walk.returned(
                    holder,
                    Role::Where,
                    function.index,
                    &own_args,
                    own_params.clone(),
                )
            })
            .collect();
        let goals: Vec<TermId> = conditions
            .iter()
            .filter_map(|index| walk.returned(target, Role::Where, *index, args, params.clone()))
            .collect();
        goals
            .into_iter()
            .all(|goal| self.entails(&mut terms, package, &premises, goal))
    }

    /// Whether premises entail a goal: the procedure proves the query of the premises and goal.
    fn entails(
        &self,
        terms: &mut Terms,
        package: crate::check::db::PackageId,
        premises: &[TermId],
        goal: TermId,
    ) -> bool {
        let premises: Vec<(TermId, Origin)> = premises
            .iter()
            .map(|premise| (*premise, Origin::Fact))
            .collect();
        let canonical = canon::canonical(self, terms, package, &premises, goal);
        matches!(
            Solver::new(self, package).decide(canonical),
            Decision::Proved
        )
    }

    /// The default of a type as a term: a scalar's zero, `false` or the `char` of value 0, the
    /// construction of the defaults of a struct's fields when it derives `Default`, and a fresh
    /// symbol for any other type.
    fn default_term(&self, terms: &mut Terms, ty: &Ty) -> TermId {
        match ty {
            Ty::Scalar(Scalar::Bool) => terms.bool(false),
            Ty::Scalar(Scalar::Char) => terms.literal(Literal::Char('\0')),
            Ty::Scalar(scalar) if scalar.is_float() => terms.literal(Literal::Float(0, *scalar)),
            Ty::Scalar(scalar) => terms.int(0, *scalar),
            Ty::Item(item, args) => {
                let derives = matches!(&self.db.ast(*item).kind,
                    ItemKind::Struct(structure) if structure.derives.iter().any(|name| name.text == "Default"));
                let fields = match &self.sig(*item).kind {
                    crate::check::sig::SigKind::Struct { fields } if derives => fields.clone(),
                    _ => return terms.fresh(ty.clone()),
                };
                let subst = Subst::of(self.sig(*item).generics.iter().map(|def| def.param), args);
                let parts = fields
                    .iter()
                    .map(|field| self.default_term(terms, &subst.ty(&field.ty)))
                    .collect();
                terms.intern(
                    Term::App(term::Op::Construct(ty.clone()), parts),
                    ty.clone(),
                )
            }
            _ => terms.fresh(ty.clone()),
        }
    }

    fn decide_unit(&self, item: ItemId, unit: &Rc<Unit>) -> Vec<Obligated> {
        let mut decided = Vec::new();
        for (index, function) in unit.functions.iter().enumerate() {
            if matches!(
                function.role,
                Role::Example | Role::Invariant | Role::Assume
            ) || self.clause_disputed(item, function.role, function.index)
            {
                continue;
            }
            let mut terms = Terms::default();
            let (sites, statements) = self.walk_function(item, unit, index, &mut terms);
            let demanded = self.demands_proof(item, function);
            decided.extend(self.decide_function(
                item,
                function.role,
                function.index,
                sites,
                demanded,
                &mut terms,
            ));
            for statement in statements {
                decided.extend(self.decide_function(
                    item,
                    statement.role,
                    statement.index,
                    statement.sites,
                    true,
                    &mut terms,
                ));
            }
        }
        decided
    }

    /// Walks a function of a unit with its entry facts, returning its sites with their queries
    /// and those of the statement functions it calls.
    fn walk_function(
        &self,
        item: ItemId,
        unit: &Rc<Unit>,
        index: usize,
        terms: &mut Terms,
    ) -> (Vec<(Site, Query)>, Vec<query::Statement>) {
        let function = &unit.functions[index];
        let generics = self.sig(item).generics.clone();
        let type_args: Vec<Arg> = generics.iter().map(param_arg).collect();
        let subst = Subst::of(generics.iter().map(|def| def.param), &type_args);
        let mut params = symbols(function, &subst, terms);
        let item_params = self.fn_sig(item).map(|sig| sig.params).unwrap_or_default();
        let mut facts: Vec<TermId> = Vec::new();
        let mut walk = Walk::new(
            self,
            item,
            terms,
            unit.clone(),
            index,
            &type_args,
            Vec::new(),
            Vec::new(),
        );
        walk.opaquely(|walk| match function.role {
            Role::Body
            | Role::Default
            | Role::Entry
            | Role::Requires
            | Role::Old
            | Role::Decreases => {
                let boundary = self.is_boundary(item);
                for (term, param) in params.iter().zip(&item_params) {
                    walk.predicate_facts(*term, &subst.ty(&param.ty), boundary, &mut facts);
                }
                if function.role != Role::Entry {
                    for requires in self.entry_requires(item) {
                        let Callable::Item {
                            item: holder,
                            index,
                            ..
                        } = requires.callable
                        else {
                            continue;
                        };
                        if function.role == Role::Requires
                            && holder == item
                            && index >= function.index
                        {
                            continue;
                        }
                        let args = subst.args(&requires.args);
                        if let Some(fact) =
                            walk.returned(holder, Role::Requires, index, &args, params.clone())
                        {
                            facts.push(fact);
                        }
                    }
                }
            }
            Role::Ensures => {
                let ensures = self.ensures_premises(item, function, walk, &mut params, &subst);
                facts.extend(ensures);
            }
            Role::Closure => {
                for (term, param) in params.iter().zip(&function.params) {
                    let ty = subst.ty(function.ty(param.value).ty());
                    walk.predicate_facts(*term, &ty, false, &mut facts);
                }
            }
            _ => {}
        });
        walk.restart(
            params,
            facts.into_iter().map(|fact| (fact, Origin::Fact)).collect(),
        );
        walk.walk();
        let sites = walk.queries();
        (sites, std::mem::take(&mut walk.statements))
    }

    /// The parameters and entry facts of an ensures function's walk: its kept parameters at
    /// return and `result` as symbols, its `old` values over the parameters at entry; the
    /// facts the requires clauses, the predicates at entry and exit and the earlier ensures
    /// clauses give.
    fn ensures_premises(
        &self,
        item: ItemId,
        function: &Function,
        walk: &mut Walk,
        params: &mut [TermId],
        subst: &Subst,
    ) -> Vec<TermId> {
        let Some(sig) = self.fn_sig(item) else {
            return Vec::new();
        };
        let mut kept = params.iter().copied();
        let entry: Vec<TermId> = sig
            .params
            .iter()
            .map(|param| match param.mode {
                Mode::Read => kept.next().expect("a kept parameter"),
                Mode::Mut => {
                    kept.next();
                    walk.terms.fresh(subst.ty(&param.ty))
                }
                Mode::Take => walk.terms.fresh(subst.ty(&param.ty)),
            })
            .collect();
        let kept_count = sig
            .params
            .iter()
            .filter(|param| param.mode != Mode::Take)
            .count();
        let type_args: Vec<Arg> = self.sig(item).generics.iter().map(param_arg).collect();
        let clauses = self.ensures_clauses(item, None);
        let mut facts = Vec::new();
        for (term, param) in entry.iter().zip(&sig.params) {
            walk.predicate_facts(
                *term,
                &subst.ty(&param.ty),
                self.is_boundary(item),
                &mut facts,
            );
        }
        for requires in self.entry_requires(item) {
            let Callable::Item {
                item: holder,
                index,
                ..
            } = requires.callable
            else {
                continue;
            };
            let args = subst.args(&requires.args);
            if let Some(fact) = walk.returned(holder, Role::Requires, index, &args, entry.clone()) {
                facts.push(fact);
            }
        }
        let result = params[kept_count];
        let at_return: Vec<TermId> = params[..kept_count].to_vec();
        let own = clauses.iter().find(|clause| clause.index == function.index);
        let olds = own.map(|clause| clause.olds.clone()).unwrap_or_default();
        for (old, slot) in olds.into_iter().zip(kept_count + 1..) {
            if let Some(term) = walk.returned(item, Role::Old, old, &type_args, entry.clone()) {
                params[slot] = term;
            }
        }
        walk.predicate_facts(
            result,
            &subst.ty(&sig.result),
            self.is_boundary(item),
            &mut facts,
        );
        let mut returned = at_return.iter();
        for param in &sig.params {
            if param.mode == Mode::Take {
                continue;
            }
            let term = *returned.next().expect("a kept parameter");
            if param.mode == Mode::Mut {
                walk.predicate_facts(
                    term,
                    &subst.ty(&param.ty),
                    self.is_boundary(item),
                    &mut facts,
                );
            }
        }
        let earlier = clauses
            .iter()
            .filter(|clause| clause.index < function.index && !clause.disputed);
        for clause in earlier {
            let mut earlier_params = at_return.clone();
            earlier_params.push(result);
            for old in &clause.olds {
                let term = walk
                    .returned(item, Role::Old, *old, &type_args, entry.clone())
                    .unwrap_or_else(|| walk.terms.fresh(Ty::Unit));
                earlier_params.push(term);
            }
            if let Some(fact) = walk.returned(
                item,
                Role::Ensures,
                clause.index,
                &type_args,
                earlier_params,
            ) {
                facts.push(fact);
            }
        }
        facts
    }

    /// Whether a contract function belongs to a disputed clause, which has no well-definedness
    /// obligations.
    fn clause_disputed(&self, item: ItemId, role: Role, index: u32) -> bool {
        let clause_role = match role {
            Role::Requires => ClauseRole::Requires,
            Role::Ensures => ClauseRole::Ensures,
            _ => return false,
        };
        let bodies: Vec<Rc<crate::check::tir::TBody>> = match &self.db.ast(item).kind {
            ItemKind::Fn(_) => self.typed(item).into_iter().collect(),
            ItemKind::Interface(_) => self.method_typed(item),
            _ => Vec::new(),
        };
        bodies
            .iter()
            .flat_map(|body| body.clauses.iter())
            .filter(|clause| clause.role == clause_role)
            .nth(index as usize)
            .is_some_and(|clause| clause.disputed)
    }

    /// Whether proof is demanded for the obligations of a function of an item's unit: those
    /// of a body, default body or closure whose limits demand notrap or of a transparent
    /// function, and the well-definedness obligations of contract terms.
    fn demands_proof(&self, item: ItemId, function: &Function) -> bool {
        match function.role {
            Role::Body | Role::Default => self.demands_notrap(item),
            Role::Closure => {
                self.demands_notrap(item)
                    || self.typed_body(item).is_some_and(|body| {
                        crate::check::closure_demands_notrap(&body, function.index)
                    })
            }
            Role::Entry | Role::Example => false,
            _ => true,
        }
    }

    /// Decides the sites of one function in order, giving each its status, and, where proof
    /// is demanded and the procedure does not prove it, its outcome.
    fn decide_function(
        &self,
        item: ItemId,
        role: Role,
        index: u32,
        sites: Vec<(Site, Query)>,
        demanded: bool,
        terms: &mut Terms,
    ) -> Vec<Obligated> {
        let package = self.db.package_of(item);
        let mut statuses: Vec<Status> = Vec::new();
        let mut decided = Vec::new();
        for (site, query) in sites {
            let demanded = demanded || site.obligation == Obligation::Check(CheckKind::Decreases);
            let mut unproved = None;
            let status = match site.open {
                true => Status::Open,
                false => {
                    let canonical =
                        canon::canonical(self, terms, package, &query.premises, query.goal);
                    let origins = canonical.origins.clone();
                    let mut solver = Solver::new(self, package);
                    match solver.decide(canonical) {
                        Decision::Proved => {
                            let assumed = origins.iter().any(|origin| match origin {
                                Origin::Assume => true,
                                Origin::Site(earlier) => statuses[*earlier] == Status::Assumed,
                                _ => false,
                            });
                            match assumed {
                                true => Status::Assumed,
                                false => Status::Proven,
                            }
                        }
                        decision => {
                            if demanded {
                                unproved = Some(self.unproved(
                                    item,
                                    role,
                                    index,
                                    &site,
                                    decision,
                                    &mut solver,
                                ));
                            }
                            Status::Checked
                        }
                    }
                }
            };
            statuses.push(status);
            decided.push(Obligated {
                role,
                index,
                block: site.block,
                position: site.position,
                obligation: site.obligation,
                span: site.span,
                status,
                clause: site.clause,
                unproved,
            });
        }
        decided
    }

    /// The outcome of a site the procedure does not prove: refuted when the open state gives a
    /// candidate the interpreter confirms, unknown otherwise.
    fn unproved(
        &self,
        item: ItemId,
        role: Role,
        index: u32,
        site: &Site,
        decision: Decision,
        solver: &mut Solver,
    ) -> Unproved {
        let (mut state, depth) = match decision {
            Decision::Open(state, depth) => (state, depth),
            _ => return Unproved::Unknown(vec!["the fuel ran out".to_owned()]),
        };
        let reason = match depth {
            true => "the open state at depth 2 gave",
            false => "the open state with nothing left to split on gave",
        };
        let candidate = self.candidate(item, role, index, solver, &mut state);
        let Some((inputs, type_args)) = candidate else {
            return Unproved::Unknown(vec![format!("{reason} no candidate")]);
        };
        let notes: Vec<String> = match self.unit(item) {
            Some(unit) => unit
                .function(role, index)
                .map(|function| {
                    function
                        .params
                        .iter()
                        .zip(&inputs)
                        .map(|(param, input)| {
                            format!("{}: {}", param.name, refute::literal(self, input))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let run_site = RunSite {
            role,
            index,
            block: site.block,
            position: site.position,
        };
        let mut interpreter = Interpreter::new(self);
        match interpreter.confirm(item, run_site, inputs, &type_args) {
            true => Unproved::Refuted(notes),
            false => Unproved::Unknown(vec![format!("{reason} a candidate that is not confirmed")]),
        }
    }

    /// The candidate of an open state: the values of the function's parameters and const
    /// parameters once its entry terms are fixed; none for a function of a role other than
    /// `body`, `requires`, `old` and `decreases`, an item with generic type parameters, a
    /// parameter of a type no candidate holds, or a fixing that contradicts.
    fn candidate(
        &self,
        item: ItemId,
        role: Role,
        index: u32,
        solver: &mut Solver,
        state: &mut solve::State,
    ) -> Option<(Vec<Input>, Vec<Arg>)> {
        if !matches!(
            role,
            Role::Body | Role::Requires | Role::Old | Role::Decreases
        ) {
            return None;
        }
        let generics = self.sig(item).generics.clone();
        if generics.iter().any(|def| def.constant.is_none()) {
            return None;
        }
        if !refute::fix(solver, state) {
            return None;
        }
        let unit = self.unit(item)?;
        let function = unit.function(role, index)?;
        let type_args: Vec<Arg> = generics
            .iter()
            .map(|def| {
                let constant = Term::App(
                    term::Op::Constant(crate::nir::Constant::Param(def.param)),
                    Vec::new(),
                );
                let value = state
                    .terms
                    .lookup(&constant)
                    .filter(|term| state.is_registered(*term))
                    .and_then(|term| state.class(term).single())
                    .unwrap_or(0);
                Arg::Const(Const::Int(value))
            })
            .collect();
        let subst = Subst::of(generics.iter().map(|def| def.param), &type_args);
        let builder = refute::Builder::new(self, state, self.db.module_of(item));
        let inputs = function
            .params
            .iter()
            .map(|param| {
                let symbol = match function.ty(param.value).is_reference() {
                    true => Term::Entry(param.value.0),
                    false => Term::Param(param.value.0),
                };
                builder.value(
                    state.terms.lookup(&symbol),
                    &subst.ty(function.ty(param.value).ty()),
                )
            })
            .collect::<Option<Vec<Input>>>()?;
        Some((inputs, type_args))
    }
}

/// The symbols a function's parameters stand for: each one's own, or for a reference the
/// content at entry of the place it designates.
fn symbols(function: &Function, subst: &Subst, terms: &mut Terms) -> Vec<TermId> {
    function
        .params
        .iter()
        .map(|param| {
            let ty = subst.ty(function.ty(param.value).ty());
            let symbol = match function.ty(param.value).is_reference() {
                true => Term::Entry(param.value.0),
                false => Term::Param(param.value.0),
            };
            terms.intern(symbol, ty)
        })
        .collect()
}
