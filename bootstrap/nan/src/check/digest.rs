//! Digests: the facts the checker computes from the code of a function or closure and from the
//! digests of what it reaches, and the limits that demand them, judged at each site of the code
//! that brings a fact.

use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use super::Checker;
use super::db::ItemId;
use super::sig::SigKind;
use super::tir::{
    Builtin, Call, Callee, LocalKind, TBlock, TBody, TClosure, TExpr, TKind, TStmtKind,
};
use super::ty::{LimitSet, Ty};
use crate::diagnostic::{Code, Reason, Role};
use crate::syntax::ast::{ClauseKind, FnKind, ItemKind, Tok};

/// The facts code holds, or what it reaches holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Digest {
    pub wait: bool,
    pub alloc: bool,
    pub device: bool,
    pub privileged: bool,
    pub sync: bool,
    pub terminates: bool,
    pub float: bool,
    /// The primitive capabilities whose functions it calls; none for the unbounded set.
    pub capabilities: Option<BTreeSet<ItemId>>,
    /// The validators and decoders it calls; none for the unbounded set.
    pub validators: Option<BTreeSet<ItemId>>,
}

impl Digest {
    /// The digest of code that does nothing.
    pub fn none() -> Digest {
        Digest {
            wait: false,
            alloc: false,
            device: false,
            privileged: false,
            sync: false,
            terminates: true,
            float: false,
            capabilities: Some(BTreeSet::new()),
            validators: Some(BTreeSet::new()),
        }
    }

    /// The declared digest of a function type or head: the worst digest, waiting as it states
    /// and with the facts its limits set.
    pub fn declared(wait: bool, limits: &LimitSet) -> Digest {
        let mut digest = Digest {
            wait,
            alloc: !limits.pure && !limits.noalloc,
            device: !limits.pure,
            privileged: !limits.pure,
            sync: !limits.pure,
            terminates: limits.terminates,
            float: false,
            capabilities: None,
            validators: None,
        };
        if limits.pure {
            digest.capabilities = Some(BTreeSet::new());
            digest.validators = Some(BTreeSet::new());
        }
        digest
    }

    pub fn join(&mut self, other: &Digest) {
        self.wait |= other.wait;
        self.alloc |= other.alloc;
        self.device |= other.device;
        self.privileged |= other.privileged;
        self.sync |= other.sync;
        self.terminates &= other.terminates;
        self.float |= other.float;
        join_set(&mut self.capabilities, &other.capabilities);
        join_set(&mut self.validators, &other.validators);
    }

    /// Whether it holds no effect fact, the wait fact excepted, and empty sets.
    pub fn pure(&self) -> bool {
        !self.alloc
            && !self.device
            && !self.privileged
            && !self.sync
            && self.capabilities.as_ref().is_some_and(BTreeSet::is_empty)
            && self.validators.as_ref().is_some_and(BTreeSet::is_empty)
    }

    /// The code of the rule a limit demanding facts this digest lacks breaks.
    fn broken(&self, limits: &LimitSet) -> Option<Code> {
        if limits.pure && !self.pure() {
            return Some(Code::LimitImpure);
        }
        if limits.terminates && !self.terminates {
            return Some(Code::LimitMayNotTerminate);
        }
        (limits.noalloc && self.alloc).then_some(Code::LimitAllocates)
    }
}

/// Joins a set into another, an absent set holding every element.
fn join_set(into: &mut Option<BTreeSet<ItemId>>, other: &Option<BTreeSet<ItemId>>) {
    match (into.as_mut(), other) {
        (Some(set), Some(other)) => set.extend(other.iter().copied()),
        (Some(_), None) => *into = None,
        (None, _) => {}
    }
}

/// A site of code: where it brings facts, and what brings them.
#[derive(Clone, Debug)]
pub struct Site {
    pub token: Tok,
    pub facts: Digest,
    pub brought: Brought,
}

/// What brings the facts of a site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Brought {
    /// An operation of the code itself.
    Operation,
    /// A call, or a function supplied as an argument: the function with a body it names, if any.
    Call(Option<ItemId>),
    /// A closure supplied as a function argument, whose own sites are judged.
    Charged,
}

/// The digests computed in one check, by item.
#[derive(Default)]
pub struct Digests {
    done: HashMap<ItemId, Rc<Digest>>,
    /// The items whose digests are being computed, outermost first.
    computing: Vec<ItemId>,
}

impl Checker<'_> {
    /// The digest of a function item: computed from its body, supplied for a std function
    /// without one, and otherwise declared.
    pub fn digest(&self, item: ItemId) -> Rc<Digest> {
        self.db.note(item);
        if let Some(digest) = self.digests.borrow().done.get(&item) {
            return digest.clone();
        }
        self.db.about(Some(item), || self.compute_digest(item))
    }

    fn compute_digest(&self, item: ItemId) -> Rc<Digest> {
        let head = self.fn_sig(item);
        let declared = || {
            Rc::new(head.as_ref().map_or_else(
                || Digest::declared(false, &LimitSet::default()),
                |head| Digest::declared(head.wait, &head.limits),
            ))
        };
        if self.digests.borrow().computing.contains(&item) {
            return Rc::new(self.cycle_digest(item));
        }
        let Some(body) = self.typed_body(item) else {
            let digest = match self.db.is_std(item) {
                true => Rc::new(self.supplied(item)),
                false => declared(),
            };
            self.digests.borrow_mut().done.insert(item, digest.clone());
            return digest;
        };
        self.digests.borrow_mut().computing.push(item);
        let mut digest = Digest::none();
        for site in self.sites(item, &body, None) {
            digest.join(&site.facts);
        }
        let wait = head.as_ref().is_some_and(|head| head.wait);
        digest.wait = wait;
        let cyclic = self.in_cycle(item);
        digest.terminates &= !wait && (!cyclic || self.has_decreases(item));
        self.digests.borrow_mut().computing.pop();
        let digest = Rc::new(digest);
        self.digests.borrow_mut().done.insert(item, digest.clone());
        digest
    }

    /// The digest of a body, closure or default function of an item's unit: the item's own for
    /// its body; for a closure or a default body, the join of its own code's sites, waiting as
    /// its head states.
    pub fn function_digest(&self, item: ItemId, role: crate::nir::Role, index: u32) -> Digest {
        let own = |body: &TBody, closure: Option<&TClosure>, wait: bool| {
            let mut digest = Digest::none();
            for site in self.sites(item, body, closure) {
                digest.join(&site.facts);
            }
            digest.wait = wait;
            digest.terminates &= !wait;
            digest
        };
        match role {
            crate::nir::Role::Closure => {
                let Some(body) = self.typed(item) else {
                    return Digest::none();
                };
                let mut closures = Vec::new();
                if let Some(root) = &body.root {
                    nested_closures(root, &mut closures);
                }
                match closures.into_iter().find(|closure| closure.index == index) {
                    Some(closure) => own(&body, Some(closure), closure.ty.wait),
                    None => Digest::none(),
                }
            }
            crate::nir::Role::Default => {
                let SigKind::Interface { methods } = &self.sig(item).kind else {
                    return Digest::none();
                };
                let Some(method) = methods
                    .iter()
                    .enumerate()
                    .filter(|(_, method)| method.default)
                    .nth(index as usize)
                    .map(|(position, _)| position)
                else {
                    return Digest::none();
                };
                let body = self.method_bodies.borrow().get(&(item, method)).cloned();
                match body {
                    Some(body) => own(&body, None, methods[method].sig.wait),
                    None => Digest::none(),
                }
            }
            _ => (*self.digest(item)).clone(),
        }
    }

    /// The facts a function reaching itself is taken to hold while its digest is computed: no
    /// fact, the others of its cycle joining theirs.
    fn cycle_digest(&self, item: ItemId) -> Digest {
        let mut digest = Digest::none();
        digest.terminates = self.has_decreases(item);
        digest
    }

    /// Whether two functions belong to one cycle: each reaches the other through dependencies.
    pub fn same_cycle(&self, first: ItemId, second: ItemId) -> bool {
        self.reaches(first, second, &mut Vec::new()) && self.reaches(second, first, &mut Vec::new())
    }

    /// Whether a function belongs to a cycle: it reaches itself through its dependencies.
    pub fn in_cycle(&self, item: ItemId) -> bool {
        self.reaches(item, item, &mut Vec::new())
    }

    /// Whether a function reaches `target` through its dependencies: the functions it calls
    /// directly and those it supplies as function arguments.
    fn reaches(&self, from: ItemId, target: ItemId, seen: &mut Vec<ItemId>) -> bool {
        if seen.contains(&from) {
            return false;
        }
        seen.push(from);
        let Some(body) = self.typed_body(from) else {
            return false;
        };
        let mut dependencies = Vec::new();
        if let Some(root) = &body.root {
            collect_dependencies(root, &mut dependencies);
        }
        dependencies
            .into_iter()
            .any(|dependency| dependency == target || self.reaches(dependency, target, seen))
    }

    fn has_decreases(&self, item: ItemId) -> bool {
        matches!(&self.db.ast(item).kind, ItemKind::Fn(function)
            if function.clauses.iter().any(|clause| matches!(clause.kind, ClauseKind::Decreases(_))))
    }

    /// The sites of a body's code outside the closures it holds, `closure` naming the closure
    /// whose body is walked when it is one; a closure's code is charged to the call it is
    /// supplied to.
    pub fn sites(&self, item: ItemId, body: &TBody, closure: Option<&TClosure>) -> Vec<Site> {
        self.walk(item, body, closure).sites
    }

    /// The walk of a body's own code, or of a closure's.
    fn walk<'c>(
        &self,
        item: ItemId,
        body: &'c TBody,
        closure: Option<&TClosure>,
    ) -> Walk<'_, 'c, '_> {
        let mut walk = Walk {
            ck: self,
            item,
            body,
            closure: closure.map_or(0, |closure| closure.index),
            sites: Vec::new(),
            expects: Vec::new(),
        };
        match closure {
            Some(closure) => walk.expr(&closure.body),
            None => {
                if let Some(root) = &body.root {
                    walk.expr(root);
                }
            }
        }
        walk
    }

    /// Reports each `expect` of a body, its closures' included, whose condition is not pure.
    pub fn check_expect_purity(&self, item: ItemId, body: &TBody) {
        let mut pending = vec![None];
        while let Some(closure) = pending.pop() {
            for (token, sites) in self.walk(item, body, closure).expects {
                if sites.iter().any(|site| !site.facts.pure()) {
                    self.report(item, token, Code::ContractImpureCondition);
                }
            }
            pending.extend(closures_of(body, closure).into_iter().map(Some));
        }
    }

    /// The sites of a body's whole text: its own code and that of each closure it holds.
    pub fn text_sites(&self, item: ItemId, body: &TBody) -> Vec<Site> {
        let mut sites = self.sites(item, body, None);
        let mut pending = closures_of(body, None);
        while let Some(closure) = pending.pop() {
            sites.extend(self.sites(item, body, Some(closure)));
            pending.extend(closures_of(body, Some(closure)));
        }
        sites
    }

    /// The limits a function's code meets: its own and those of the interface methods it
    /// implements, judged at each site of its text, closure bodies included, that breaks one.
    pub fn check_limits(&self, item: ItemId, body: &TBody) {
        let Some(head) = self.fn_sig(item) else {
            return;
        };
        let ItemKind::Fn(function) = &self.db.ast(item).kind else {
            return;
        };
        let mut limits = head.limits;
        if matches!(function.body, crate::syntax::ast::FnBody::Expr(_)) {
            limits.pure = true;
            limits.notrap = true;
            limits.terminates = true;
        }
        for line in self.implemented_lines(item) {
            if let SigKind::Impl {
                interface: Some(bound),
                ..
            } = &self.sig(line).kind
                && let Some(method) =
                    self.interface_method(bound.interface, &function.label.name.text)
                && let SigKind::Interface { methods } = &self.sig(bound.interface).kind
            {
                let demanded = &methods[method].sig.limits;
                limits.pure |= demanded.pure;
                limits.terminates |= demanded.terminates;
                limits.noalloc |= demanded.noalloc;
            }
        }
        let mut reported = Vec::new();
        self.judge(item, body, None, &limits, &mut reported);
        if let Some(root) = &body.root {
            self.check_fit_limits(item, body, root);
        }
    }

    /// The limits a function type demands of each named function or function value supplied
    /// for it, reported at the use; a closure is judged as if they were its own.
    fn check_fit_limits(&self, item: ItemId, body: &TBody, expr: &TExpr) {
        if let TKind::Call(call) | TKind::Index(call) = &expr.kind {
            for (arg, param) in call.args.iter().zip(&call.head.params) {
                let Ty::Fn(wanted) = &param.ty else {
                    continue;
                };
                let facts = match &arg.expr.kind {
                    TKind::FnValue { item: function, .. } => (*self.digest(*function)).clone(),
                    TKind::Local(local)
                        if !matches!(body.local(*local).kind, LocalKind::Param(_))
                            && matches!(&arg.expr.ty, Ty::Fn(_)) =>
                    {
                        let Ty::Fn(found) = &arg.expr.ty else {
                            continue;
                        };
                        Digest::declared(found.wait, &found.limits)
                    }
                    _ => continue,
                };
                if let Some(code) = facts.broken(&wanted.limits) {
                    self.report(item, arg.expr.token, code);
                }
            }
        }
        expr.each_child(&mut |child| self.check_fit_limits(item, body, child));
    }

    /// Judges the sites of a body, or of a closure of it, against limits; the closures it holds
    /// are judged against their own limits first, each site reported once, for the innermost.
    fn judge(
        &self,
        item: ItemId,
        body: &TBody,
        closure: Option<&TClosure>,
        limits: &LimitSet,
        reported: &mut Vec<Tok>,
    ) {
        for inner in closures_of(body, closure) {
            let mut own = inner.ty.limits;
            own.pure |= inner.demanded.pure;
            own.terminates |= inner.demanded.terminates;
            own.noalloc |= inner.demanded.noalloc;
            self.judge(item, body, Some(inner), &own, reported);
            if limits.pure || limits.terminates || limits.noalloc {
                self.judge(item, body, Some(inner), limits, reported);
            }
        }
        for site in self.sites(item, body, closure) {
            if site.brought == Brought::Charged || reported.contains(&site.token) {
                continue;
            }
            if let Some(code) = site.facts.broken(limits) {
                reported.push(site.token);
                let because = self.trace(item, &site, code);
                self.report_because(item, site.token, code, because);
            }
        }
    }

    /// The because-chain of a site breaking a limit: a `called` reason per call down to the
    /// operation carrying the fact, and a `raised` reason for that operation.
    fn trace(&self, item: ItemId, site: &Site, code: Code) -> Vec<Reason> {
        let mut because = Vec::new();
        let mut current = (item, site.clone());
        let mut seen = Vec::new();
        while let Brought::Call(Some(callee)) = current.1.brought {
            because.push(self.reason(
                current.0,
                current.1.token,
                Role::Called,
                self.db.canonical(callee),
            ));
            if seen.contains(&callee) {
                return because;
            }
            seen.push(callee);
            let Some(body) = self.typed_body(callee) else {
                return because;
            };
            let next = self
                .sites(callee, &body, None)
                .into_iter()
                .find(|inner| inner.facts.broken(&limits_of(code)).is_some());
            match next {
                Some(next) => current = (callee, next),
                None => return because,
            }
        }
        because.push(self.reason(
            current.0,
            current.1.token,
            Role::Raised,
            raised(code).to_owned(),
        ));
        because
    }
}

/// The limits demanding the fact a code reports missing.
fn limits_of(code: Code) -> LimitSet {
    LimitSet {
        pure: code == Code::LimitImpure,
        terminates: code == Code::LimitMayNotTerminate,
        noalloc: code == Code::LimitAllocates,
        ..LimitSet::default()
    }
}

/// The fact an operation raising a limit's code carries.
fn raised(code: Code) -> &'static str {
    match code {
        Code::LimitImpure => "effect",
        Code::LimitMayNotTerminate => "may-not-terminate",
        _ => "alloc",
    }
}

/// The closures a body holds directly, or those a closure of it holds.
fn closures_of<'b>(body: &'b TBody, within: Option<&'b TClosure>) -> Vec<&'b TClosure> {
    let mut found = Vec::new();
    let root = match within {
        Some(closure) => Some(&closure.body),
        None => body.root.as_ref(),
    };
    if let Some(root) = root {
        collect_closures(root, &mut found);
    }
    found
}

/// The functions an expression calls directly or supplies as function arguments, at any depth.
fn collect_dependencies(expr: &TExpr, found: &mut Vec<ItemId>) {
    match &expr.kind {
        TKind::Call(call) | TKind::Index(call) => {
            if let Callee::Function { item, .. } = &call.callee {
                found.push(*item);
            }
        }
        TKind::FnValue { item, .. } => found.push(*item),
        _ => {}
    }
    expr.each_child(&mut |child| collect_dependencies(child, found));
}

/// The closures an expression holds at any depth, those inside closures included.
fn nested_closures<'b>(expr: &'b TExpr, found: &mut Vec<&'b TClosure>) {
    if let TKind::Closure(closure) = &expr.kind {
        found.push(closure);
        nested_closures(&closure.body, found);
        return;
    }
    expr.each_child(&mut |child| nested_closures(child, found));
}

fn collect_closures<'b>(expr: &'b TExpr, found: &mut Vec<&'b TClosure>) {
    if let TKind::Closure(closure) = &expr.kind {
        found.push(closure);
        return;
    }
    expr.each_child(&mut |child| collect_closures(child, found));
}

/// The walk collecting the sites of one body's code.
struct Walk<'w, 'c, 'a> {
    ck: &'w Checker<'a>,
    item: ItemId,
    body: &'c TBody,
    /// The closure whose code is walked, 0 for the item's own body.
    closure: u32,
    sites: Vec<Site>,
    /// The sites of each `expect` condition, by the condition's first token.
    expects: Vec<(Tok, Vec<Site>)>,
}

impl Walk<'_, '_, '_> {
    fn site(&mut self, token: Tok, facts: Digest, brought: Brought) {
        self.sites.push(Site {
            token,
            facts,
            brought,
        });
    }

    fn expr(&mut self, expr: &TExpr) {
        match &expr.kind {
            TKind::Call(call) | TKind::Index(call) => self.call(call),
            TKind::Closure(_) => return,
            TKind::When { .. } => {
                let mut facts = Digest::none();
                facts.wait = true;
                facts.sync = true;
                self.site(expr.token, facts, Brought::Operation);
            }
            TKind::Unary { operand, .. } => self.float(expr.token, &operand.ty),
            TKind::Binary { left, .. } => self.float(expr.token, &left.ty),
            TKind::Block(block) => {
                self.block(block);
                return;
            }
            TKind::If {
                condition,
                then,
                otherwise,
            } => {
                self.expr(condition);
                self.block(then);
                if let Some(otherwise) = otherwise {
                    self.expr(otherwise);
                }
                return;
            }
            _ => {}
        }
        expr.each_child(&mut |child| self.expr(child));
    }

    fn block(&mut self, block: &TBlock) {
        for stmt in &block.stmts {
            match &stmt.kind {
                TStmtKind::While { .. } | TStmtKind::Loop { .. } => {
                    let mut facts = Digest::none();
                    facts.terminates = false;
                    self.site(stmt.token, facts, Brought::Operation);
                }
                TStmtKind::For { cursor, .. } => {
                    let finite = matches!(cursor, Ty::Item(item, _) if self.ck.db.is_std(*item))
                        || cursor.scalar().is_some();
                    if !finite {
                        let mut facts = Digest::none();
                        facts.terminates = false;
                        self.site(stmt.token, facts, Brought::Operation);
                    }
                }
                TStmtKind::Assign {
                    target,
                    op: Some(_),
                    ..
                } => self.float(stmt.token, &target.ty),
                TStmtKind::Expect(condition) => {
                    let mut condition_walk = Walk {
                        ck: self.ck,
                        item: self.item,
                        body: self.body,
                        closure: self.closure,
                        sites: Vec::new(),
                        expects: Vec::new(),
                    };
                    condition_walk.expr(condition);
                    self.expects.push((condition.token, condition_walk.sites));
                    continue;
                }
                TStmtKind::Assume(_) => continue,
                _ => {}
            }
            match &stmt.kind {
                TStmtKind::While {
                    condition,
                    invariants,
                    body,
                } => {
                    self.expr(condition);
                    invariants.iter().for_each(|invariant| self.expr(invariant));
                    self.block(body);
                }
                TStmtKind::For {
                    iterated,
                    invariants,
                    body,
                    ..
                } => {
                    self.expr(iterated);
                    invariants.iter().for_each(|invariant| self.expr(invariant));
                    self.block(body);
                }
                TStmtKind::Loop { invariants, body } => {
                    invariants.iter().for_each(|invariant| self.expr(invariant));
                    self.block(body);
                }
                TStmtKind::Let {
                    value, otherwise, ..
                } => {
                    self.expr(value);
                    if let Some(otherwise) = otherwise {
                        self.block(otherwise);
                    }
                }
                _ => stmt.each_child(&mut |child| self.expr(child)),
            }
        }
    }

    /// The float use of an operation on a value of `ty`.
    fn float(&mut self, token: Tok, ty: &Ty) {
        if ty.scalar().is_some_and(super::ty::Scalar::is_float) {
            let mut facts = Digest::none();
            facts.float = true;
            self.site(token, facts, Brought::Operation);
        }
    }

    /// A call: the facts of what it calls, and of each function argument it supplies.
    fn call(&mut self, call: &Call) {
        let token = call.token;
        match &call.callee {
            Callee::Function { item, .. } => {
                let mut facts = (*self.ck.digest(*item)).clone();
                if self.ck.db.is_std(*item) && self.ck.is_primitive_capability_member(*item) {
                    let mut capabilities = facts.capabilities.clone().unwrap_or_default();
                    capabilities.insert(self.ck.owner_item(*item));
                    facts.capabilities = Some(capabilities);
                }
                if self.ck.is_validator(*item) {
                    let mut validators = facts.validators.clone().unwrap_or_default();
                    validators.insert(*item);
                    facts.validators = Some(validators);
                }
                let body_owned = self.ck.db.ast(*item).kind.is_fn_with_body();
                self.site(token, facts, Brought::Call(body_owned.then_some(*item)));
            }
            Callee::Method {
                interface, method, ..
            } => {
                if let SigKind::Interface { methods } = &self.ck.sig(interface.interface).kind {
                    let head = &methods[*method].sig;
                    self.site(
                        token,
                        Digest::declared(head.wait, &head.limits),
                        Brought::Call(None),
                    );
                }
            }
            Callee::Value(value) => {
                if !self.own_parameter(value)
                    && let Ty::Fn(fn_ty) = &value.ty
                {
                    self.site(
                        token,
                        Digest::declared(fn_ty.wait, &fn_ty.limits),
                        Brought::Call(None),
                    );
                }
            }
            Callee::Builtin(builtin) => {
                if let Builtin::Conversion(_) = builtin {
                    let source = call.args.first().map(|arg| &arg.expr.ty);
                    let floats = source
                        .into_iter()
                        .chain([&call.head.result])
                        .any(|ty| ty.scalar().is_some_and(super::ty::Scalar::is_float));
                    if floats {
                        let mut facts = Digest::none();
                        facts.float = true;
                        self.site(token, facts, Brought::Operation);
                    }
                }
                if *builtin == Builtin::DmaAccess {
                    let mut facts = Digest::none();
                    facts.device = true;
                    self.site(token, facts, Brought::Operation);
                }
            }
        }
        for arg in &call.args {
            self.argument(token, &arg.expr);
        }
    }

    /// A function argument supplied at a call: a closure's digest, a named function's, or the
    /// declared digest of a function value's type, a parameter of the code walked adding none;
    /// supplied to a `task fn` parameter, without its waiting.
    fn argument(&mut self, token: Tok, arg: &TExpr) {
        let Ty::Fn(fn_ty) = &arg.ty else {
            return;
        };
        let mut facts = match &arg.kind {
            TKind::Closure(closure) => {
                let mut facts = Digest::none();
                for site in self.ck.sites(self.item, self.body, Some(closure)) {
                    facts.join(&site.facts);
                }
                facts.wait = closure.ty.wait;
                facts
            }
            TKind::FnValue { item: function, .. } => {
                let facts = (*self.ck.digest(*function)).clone();
                self.site(token, facts, Brought::Call(Some(*function)));
                return;
            }
            _ if self.own_parameter(arg) => return,
            _ => Digest::declared(fn_ty.wait, &fn_ty.limits),
        };
        if fn_ty.kind == FnKind::Task {
            facts.wait = false;
        }
        let brought = match arg.kind {
            TKind::Closure(_) => Brought::Charged,
            _ => Brought::Call(None),
        };
        self.site(token, facts, brought);
    }

    /// Whether an expression is a function-typed parameter of the code walked.
    fn own_parameter(&self, value: &TExpr) -> bool {
        matches!(value.kind, TKind::Local(local)
            if matches!(self.body.local(local).kind, LocalKind::Param(_))
                && self.body.local(local).closure == self.closure)
    }
}

impl Checker<'_> {
    /// The typed body of a function item with a body.
    pub fn typed_body(&self, item: ItemId) -> Option<Rc<TBody>> {
        self.db.note(item);
        let kind = &self.db.ast(item).kind;
        if !kind.is_fn_with_body() && !matches!(kind, ItemKind::Retired(_)) {
            return None;
        }
        self.typed(item)
    }

    /// The functions of primitive capabilities a function calls, directly or through the
    /// functions with bodies it calls.
    pub fn methods_reached(&self, item: ItemId) -> Vec<ItemId> {
        let mut reached = Vec::new();
        let mut seen = vec![item];
        let mut pending = vec![item];
        while let Some(current) = pending.pop() {
            let Some(body) = self.typed_body(current) else {
                continue;
            };
            let mut called = Vec::new();
            if let Some(root) = &body.root {
                collect_dependencies(root, &mut called);
            }
            for callee in called {
                if self.is_primitive_capability_member(callee) {
                    reached.push(callee);
                } else if !seen.contains(&callee) {
                    seen.push(callee);
                    pending.push(callee);
                }
            }
        }
        reached
    }

    /// Whether a std function is a member of a primitive capability.
    pub(super) fn is_primitive_capability_member(&self, function: ItemId) -> bool {
        matches!(self.db.owners.get(&function), Some(super::db::TypeKey::Item(owner)) if self.is_primitive_capability(*owner))
            && !self.fn_sig(function).is_some_and(|sig| sig.limits.pure)
    }

    /// The type a member function is named after.
    fn owner_item(&self, function: ItemId) -> ItemId {
        match self.db.owners.get(&function) {
            Some(super::db::TypeKey::Item(owner)) => *owner,
            _ => function,
        }
    }
}
