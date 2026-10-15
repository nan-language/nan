//! The procedure: propagation over equivalence classes of terms, intervals, congruences, a
//! difference graph, clauses and quantifier instances, then case splits to depth 2, all
//! within one query's fuel.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::canon::{
    Atom, Canon, Clause, Folded, Forall, Formula, Linear, Literal, Origin, Query, sorted_clause,
};
use super::encode::Encodings;
use super::term::{Op, Sort, Term, TermId, Terms};
use crate::check::Checker;
use crate::check::db::PackageId;
use crate::nir::BinaryOp;

/// The steps a query may take, shared by all its branches and its candidate.
pub const FUEL: u32 = 10_000;

/// The depth below which an open state is split.
const DEPTH: u32 = 2;

/// The difference graph's zero node.
const ZERO: TermId = TermId::MAX;

/// Why propagation stops early.
#[derive(Debug, PartialEq, Eq)]
pub enum Halt {
    Contradiction,
    Fuel,
}

pub type Step<T> = Result<T, Halt>;

/// The interval and congruence of an integer class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Class {
    pub low: Option<i128>,
    pub high: Option<i128>,
    pub residue: i128,
    pub modulus: i128,
}

impl Class {
    const ANY: Class = Class {
        low: None,
        high: None,
        residue: 0,
        modulus: 1,
    };

    pub fn single(&self) -> Option<i128> {
        match (self.low, self.high) {
            (Some(low), Some(high)) if low == high => Some(low),
            _ => None,
        }
    }

    /// Its bounds moved inward to the nearest values its congruence allows.
    fn fitted(mut self) -> Class {
        if self.modulus >= 2 {
            if let Some(low) = self.low {
                self.low = low.checked_add((self.residue - low).rem_euclid(self.modulus));
            }
            if let Some(high) = self.high {
                self.high = high.checked_sub((high - self.residue).rem_euclid(self.modulus));
            }
        }
        self
    }

    fn empty(&self) -> bool {
        matches!((self.low, self.high), (Some(low), Some(high)) if low > high)
    }
}

/// A clause of the state with its encoding, by which clauses are in term order.
#[derive(Clone)]
struct Entry {
    id: u32,
    key: Rc<[u8]>,
    literals: Clause,
}

#[derive(Clone)]
pub struct State {
    pub terms: Terms,
    pub encodings: Encodings,
    parent: Vec<TermId>,
    size: Vec<u32>,
    classes: Vec<Class>,
    registered: Vec<bool>,
    /// The applications, ite terms, `mul` applications and integer terms, each in term order.
    apps: Vec<TermId>,
    ites: Vec<TermId>,
    muls: Vec<TermId>,
    pub integers: Vec<TermId>,
    /// The linear literal of each integer comparison term.
    comparisons: HashMap<TermId, Literal>,
    clauses: Vec<Entry>,
    asserted: HashSet<Literal>,
    /// The asserted linear atoms in term order.
    atoms: Vec<(Rc<[u8]>, Linear, bool)>,
    disequalities: Vec<Linear>,
    /// The pairs of terms an asserted disequality keeps apart.
    separated: Vec<(TermId, TermId)>,
    /// The difference graph: the bound w of each edge (from, to), meaning to − from ≤ w, over
    /// class roots and the zero node.
    graph: HashMap<(TermId, TermId), i128>,
    /// The clauses instantiated, with the term instantiated at.
    instances: Vec<(u32, TermId)>,
    pub truth: TermId,
    pub falsity: TermId,
    next_clause: u32,
}

/// A query's procedure: its fuel, shared by every branch and the candidate.
pub struct Solver<'a, 'c> {
    pub ck: &'a Checker<'c>,
    pub package: PackageId,
    pub steps: u32,
}

/// What the procedure ends with.
pub enum Decision {
    Proved,
    /// A branch ended without contradiction, at depth 2 or with no atom to split on.
    Open(Box<State>, bool),
    Exhausted,
}

/// What a case split asserts in its first branch and negates in its second.
enum Split {
    Term(TermId),
    Literal(Literal),
}

impl<'a, 'c> Solver<'a, 'c> {
    pub fn new(ck: &'a Checker<'c>, package: PackageId) -> Solver<'a, 'c> {
        Solver {
            ck,
            package,
            steps: 0,
        }
    }

    pub fn step(&mut self) -> Step<()> {
        if self.steps == FUEL {
            return Err(Halt::Fuel);
        }
        self.steps += 1;
        Ok(())
    }

    pub fn decide(&mut self, query: Query) -> Decision {
        let mut state = State {
            terms: query.terms,
            encodings: Encodings::default(),
            parent: Vec::new(),
            size: Vec::new(),
            classes: Vec::new(),
            registered: Vec::new(),
            apps: Vec::new(),
            ites: Vec::new(),
            muls: Vec::new(),
            integers: Vec::new(),
            comparisons: HashMap::new(),
            clauses: Vec::new(),
            asserted: HashSet::new(),
            atoms: Vec::new(),
            disequalities: Vec::new(),
            separated: Vec::new(),
            graph: HashMap::new(),
            instances: Vec::new(),
            truth: 0,
            falsity: 0,
            next_clause: 0,
        };
        state.truth = state.terms.bool(true);
        state.falsity = state.terms.bool(false);
        let (truth, falsity) = (state.truth, state.falsity);
        self.register(&mut state, truth);
        self.register(&mut state, falsity);
        for clause in query.clauses {
            self.add_clause(&mut state, clause);
        }
        match self.search(state, 0) {
            Ok(None) => Decision::Proved,
            Ok(Some((state, depth))) => Decision::Open(Box::new(state), depth),
            Err(_) => Decision::Exhausted,
        }
    }

    /// Propagates, then splits below depth 2; the open state, and whether it is at depth 2,
    /// or none when every branch ends in a contradiction.
    fn search(&mut self, mut state: State, depth: u32) -> Step<Option<(State, bool)>> {
        match self.propagate(&mut state) {
            Err(Halt::Contradiction) => return Ok(None),
            Err(Halt::Fuel) => return Err(Halt::Fuel),
            Ok(()) => {}
        }
        if depth == DEPTH {
            return Ok(Some((state, true)));
        }
        let split = match self.split(&mut state) {
            Ok(Some(split)) => split,
            Ok(None) => return Ok(Some((state, false))),
            Err(Halt::Contradiction) => return Ok(None),
            Err(Halt::Fuel) => return Err(Halt::Fuel),
        };
        for value in [true, false] {
            self.step()?;
            let mut branch = state.clone();
            let asserted = match &split {
                Split::Term(term) => self.assert_term(&mut branch, *term, value),
                Split::Literal(literal) => match value {
                    true => self.assert(&mut branch, literal),
                    false => self.assert(&mut branch, &literal.negation()),
                },
            };
            match asserted {
                Err(Halt::Contradiction) => continue,
                Err(Halt::Fuel) => return Err(Halt::Fuel),
                Ok(_) => {}
            }
            if let Some(open) = self.search(branch, depth + 1)? {
                return Ok(Some(open));
            }
        }
        Ok(None)
    }

    /// The atom to split on: the condition of the first ite term whose condition is undecided,
    /// else the first undecided literal, other than an `all`, of the first clause neither
    /// satisfied nor asserting.
    fn split(&mut self, state: &mut State) -> Step<Option<Split>> {
        for ite in state.ites.clone() {
            let Term::Ite(condition, ..) = state.terms.get(ite).clone() else {
                continue;
            };
            if self.decided(state, condition)?.is_none() {
                return Ok(Some(Split::Term(condition)));
            }
        }
        for index in 0..state.clauses.len() {
            let literals = state.clauses[index].literals.clone();
            let mut open = Vec::new();
            let mut satisfied = false;
            for literal in &literals {
                match self.evaluate(state, literal)? {
                    Some(true) => satisfied = true,
                    Some(false) => {}
                    None => open.push(literal.clone()),
                }
            }
            if satisfied || open.len() < 2 {
                continue;
            }
            if let Some(literal) = open
                .into_iter()
                .find(|literal| !matches!(literal.atom, Atom::All(_)))
            {
                let positive = match literal.negated {
                    true => literal.negation(),
                    false => literal,
                };
                return Ok(Some(Split::Literal(positive)));
            }
        }
        Ok(None)
    }

    /// Runs rounds of the rules until a contradiction or a round that changes nothing.
    pub fn propagate(&mut self, state: &mut State) -> Step<()> {
        loop {
            let mut changed = self.congruence(state)?;
            changed |= self.ite_terms(state)?;
            changed |= self.intervals(state)?;
            changed |= self.congruences(state)?;
            changed |= self.differences(state)?;
            changed |= self.clauses(state)?;
            changed |= self.quantifiers(state)?;
            if !changed {
                return Ok(());
            }
        }
    }

    /// Rule 1: applications of one function to arguments in the same classes merge, and so do
    /// integer classes holding the same single value.
    fn congruence(&mut self, state: &mut State) -> Step<bool> {
        let mut changed = false;
        let mut seen: HashMap<Term, TermId> = HashMap::new();
        for app in state.apps.clone() {
            let shape = state.terms.get(app).clone();
            let roots: Vec<TermId> = shape.parts().iter().map(|part| state.find(*part)).collect();
            match seen.get(&shape.with_parts(&roots)) {
                Some(other) => changed |= self.merge(state, app, *other)?,
                None => {
                    seen.insert(shape.with_parts(&roots), app);
                }
            }
        }
        let mut values: HashMap<i128, TermId> = HashMap::new();
        for term in state.integers.clone() {
            let root = state.find(term);
            if let Some(value) = state.classes[root as usize].single() {
                match values.get(&value) {
                    Some(other) => changed |= self.merge(state, term, *other)?,
                    None => {
                        values.insert(value, term);
                    }
                }
            }
        }
        Ok(changed)
    }

    /// Rule 2: an ite term merges with the arm its decided condition selects; otherwise it lies
    /// within its arms' hull, and a class excluding one arm decides the condition.
    fn ite_terms(&mut self, state: &mut State) -> Step<bool> {
        let mut changed = false;
        for ite in state.ites.clone() {
            let Term::Ite(condition, then, otherwise) = state.terms.get(ite).clone() else {
                continue;
            };
            match self.decided(state, condition)? {
                Some(true) => changed |= self.merge(state, ite, then)?,
                Some(false) => changed |= self.merge(state, ite, otherwise)?,
                None => {
                    if state.terms.sort(ite) == Sort::Int {
                        let (a, b) = (state.class(then), state.class(otherwise));
                        let low = a.low.zip(b.low).map(|(a, b)| a.min(b));
                        let high = a.high.zip(b.high).map(|(a, b)| a.max(b));
                        changed |= self.narrow(state, ite, low, high)?;
                    }
                    if state.excludes(ite, then) {
                        changed |= self.assert_term(state, condition, false)?;
                    } else if state.excludes(ite, otherwise) {
                        changed |= self.assert_term(state, condition, true)?;
                    }
                }
            }
        }
        Ok(changed)
    }

    /// Rule 3: each asserted atom narrows its terms' intervals from the others', each
    /// disequality excludes a bound of one term once its other terms are single, and each `mul`
    /// application lies within its operands' products.
    fn intervals(&mut self, state: &mut State) -> Step<bool> {
        let mut changed = false;
        for (_, linear, equality) in state.atoms.clone() {
            for index in 0..linear.terms.len() {
                let (coefficient, term) = linear.terms[index];
                let (others_low, others_high) = state.sum(&linear, Some(index));
                let rest_high = others_low.and_then(|low| linear.constant.checked_sub(low));
                let rest_low = match equality {
                    true => others_high.and_then(|high| linear.constant.checked_sub(high)),
                    false => None,
                };
                let (low, high) = match coefficient > 0 {
                    true => (
                        rest_low.map(|value| ceil_div(value, coefficient)),
                        rest_high.map(|value| floor_div(value, coefficient)),
                    ),
                    false => (
                        rest_high.map(|value| ceil_div(value, coefficient)),
                        rest_low.map(|value| floor_div(value, coefficient)),
                    ),
                };
                changed |= self.narrow(state, term, low, high)?;
            }
        }
        for linear in state.disequalities.clone() {
            let open: Vec<usize> = (0..linear.terms.len())
                .filter(|index| state.class(linear.terms[*index].1).single().is_none())
                .collect();
            let index = match open[..] {
                [index] => index,
                [] if !linear.terms.is_empty() => 0,
                _ => continue,
            };
            let (others, _) = state.sum(&linear, Some(index));
            let (coefficient, term) = linear.terms[index];
            let Some(rest) = others.and_then(|others| linear.constant.checked_sub(others)) else {
                continue;
            };
            if rest % coefficient != 0 {
                continue;
            }
            let excluded = rest / coefficient;
            let class = state.class(term);
            if class.low == Some(excluded) {
                changed |= self.narrow(state, term, excluded.checked_add(1), None)?;
            } else if class.high == Some(excluded) {
                changed |= self.narrow(state, term, None, excluded.checked_sub(1))?;
            }
        }
        for mul in state.muls.clone() {
            let parts = state.terms.get(mul).parts();
            let (a, b) = (state.class(parts[0]), state.class(parts[1]));
            let (Some(a_low), Some(a_high), Some(b_low), Some(b_high)) =
                (a.low, a.high, b.low, b.high)
            else {
                continue;
            };
            let products: Option<Vec<i128>> = [
                a_low.checked_mul(b_low),
                a_low.checked_mul(b_high),
                a_high.checked_mul(b_low),
                a_high.checked_mul(b_high),
            ]
            .into_iter()
            .collect();
            if let Some(products) = products {
                let low = products.iter().min().copied();
                let high = products.iter().max().copied();
                changed |= self.narrow(state, mul, low, high)?;
            }
        }
        Ok(changed)
    }

    /// Rule 4: each asserted equality gives each of its terms of coefficient ±1 the congruence
    /// its other terms' residues and moduli imply.
    fn congruences(&mut self, state: &mut State) -> Step<bool> {
        let mut changed = false;
        for (_, linear, equality) in state.atoms.clone() {
            if !equality {
                continue;
            }
            for index in 0..linear.terms.len() {
                let (coefficient, term) = linear.terms[index];
                if coefficient.abs() != 1 {
                    continue;
                }
                let mut modulus = 0;
                let mut sum: i128 = 0;
                let mut overflow = false;
                for (other, (c, t)) in linear.terms.iter().enumerate() {
                    if other == index {
                        continue;
                    }
                    let class = state.class(*t);
                    let (residue, m) = match class.single() {
                        Some(value) => (value, 0),
                        None => (class.residue, class.modulus),
                    };
                    match c.checked_mul(m).zip(c.checked_mul(residue)) {
                        Some((cm, cr)) => {
                            modulus = gcd(modulus, cm);
                            sum = match sum.checked_add(cr) {
                                Some(sum) => sum,
                                None => {
                                    overflow = true;
                                    break;
                                }
                            };
                        }
                        None => {
                            overflow = true;
                            break;
                        }
                    }
                }
                if overflow || modulus < 2 {
                    continue;
                }
                let Some(rest) = linear.constant.checked_sub(sum) else {
                    continue;
                };
                let residue = (coefficient * rest).rem_euclid(modulus);
                changed |= self.congruent(state, term, residue, modulus)?;
            }
        }
        Ok(changed)
    }

    /// Rule 5: the asserted atoms of two terms of coefficients 1 and −1, and the intervals of
    /// their classes, as edges of the difference graph, which is then closed by shortest paths.
    fn differences(&mut self, state: &mut State) -> Step<bool> {
        let mut changed = false;
        let mut edges: HashMap<(TermId, TermId), i128> = HashMap::new();
        let lower =
            |edges: &mut HashMap<(TermId, TermId), i128>, from: TermId, to: TermId, w: i128| {
                let entry = edges.entry((from, to)).or_insert(w);
                *entry = (*entry).min(w);
            };
        for ((from, to), weight) in state.graph.clone() {
            let from = if from == ZERO { ZERO } else { state.find(from) };
            let to = if to == ZERO { ZERO } else { state.find(to) };
            lower(&mut edges, from, to, weight);
        }
        for (_, linear, equality) in state.atoms.clone() {
            let Some((positive, negative)) = difference(&linear) else {
                continue;
            };
            let (p, n) = (state.find(positive), state.find(negative));
            lower(&mut edges, n, p, linear.constant);
            if equality {
                lower(&mut edges, p, n, -linear.constant);
            }
        }
        let mut nodes: Vec<TermId> = edges
            .keys()
            .flat_map(|(from, to)| [*from, *to])
            .filter(|node| *node != ZERO)
            .collect();
        nodes.sort_by_key(|node| state.encodings.term(self.ck, &state.terms, *node));
        nodes.dedup();
        if nodes.is_empty() {
            return Ok(false);
        }
        for node in &nodes {
            let class = state.classes[*node as usize];
            if let Some(high) = class.high {
                lower(&mut edges, ZERO, *node, high);
            }
            if let Some(low) = class.low
                && let Some(weight) = low.checked_neg()
            {
                lower(&mut edges, *node, ZERO, weight);
            }
        }
        nodes.push(ZERO);
        for k in &nodes {
            for i in &nodes {
                let Some(ik) = edges.get(&(*i, *k)).copied() else {
                    continue;
                };
                for j in &nodes {
                    let Some(kj) = edges.get(&(*k, *j)).copied() else {
                        continue;
                    };
                    let Some(through) = ik.checked_add(kj) else {
                        continue;
                    };
                    if i == j {
                        if through < 0 {
                            return Err(Halt::Contradiction);
                        }
                        continue;
                    }
                    if edges.get(&(*i, *j)).is_none_or(|direct| through < *direct) {
                        self.step()?;
                        edges.insert((*i, *j), through);
                        changed = true;
                    }
                }
            }
        }
        state.graph = edges.clone();
        for node in &nodes {
            if *node == ZERO {
                continue;
            }
            let high = edges.get(&(ZERO, *node)).copied();
            let low = edges
                .get(&(*node, ZERO))
                .and_then(|weight| weight.checked_neg());
            changed |= self.narrow(state, *node, low, high)?;
        }
        for (index, a) in nodes.iter().enumerate() {
            for b in &nodes[index + 1..] {
                if *a == ZERO || *b == ZERO {
                    continue;
                }
                if edges.get(&(*a, *b)).is_some_and(|w| *w <= 0)
                    && edges.get(&(*b, *a)).is_some_and(|w| *w <= 0)
                {
                    changed |= self.merge(state, *a, *b)?;
                }
            }
        }
        Ok(changed)
    }

    /// Rule 6: a clause whose literals are all false is a contradiction, and one with exactly
    /// one literal that is not false, and none true, asserts it.
    fn clauses(&mut self, state: &mut State) -> Step<bool> {
        let mut changed = false;
        let mut index = 0;
        while index < state.clauses.len() {
            let literals = state.clauses[index].literals.clone();
            index += 1;
            let mut open = Vec::new();
            let mut satisfied = false;
            for literal in &literals {
                match self.evaluate(state, literal)? {
                    Some(true) => {
                        satisfied = true;
                        break;
                    }
                    Some(false) => {}
                    None => open.push(literal),
                }
            }
            if satisfied {
                continue;
            }
            match open[..] {
                [] => return Err(Halt::Contradiction),
                [literal] => changed |= self.assert(state, literal)?,
                _ => {}
            }
        }
        Ok(changed)
    }

    /// Rule 7: each clause holding an `all` literal is instantiated at each term that applies
    /// one of its triggers' functions, once per class of that term.
    fn quantifiers(&mut self, state: &mut State) -> Step<bool> {
        let mut changed = false;
        let entries: Vec<Entry> = state
            .clauses
            .iter()
            .filter(|entry| {
                entry
                    .literals
                    .iter()
                    .any(|literal| matches!(literal.atom, Atom::All(_)))
            })
            .cloned()
            .collect();
        for entry in entries {
            for (position, literal) in entry.literals.iter().enumerate() {
                let Atom::All(forall) = &literal.atom else {
                    continue;
                };
                for (trigger, index) in triggers(&state.terms, forall) {
                    let shape = state.terms.get(trigger).clone();
                    let parts = shape.parts();
                    for app in state.apps.clone() {
                        let candidate = state.terms.get(app).clone();
                        let candidate_parts = candidate.parts();
                        if candidate_parts.len() != parts.len()
                            || candidate.with_parts(&vec![0; parts.len()])
                                != shape.with_parts(&vec![0; parts.len()])
                        {
                            continue;
                        }
                        let matches = parts.iter().zip(&candidate_parts).enumerate().all(
                            |(at, (part, candidate_part))| {
                                at == index
                                    || state.is_registered(*part)
                                        && state.find(*part) == state.find(*candidate_part)
                            },
                        );
                        if !matches {
                            continue;
                        }
                        let instance = candidate_parts[index];
                        let root = state.find(instance);
                        if state
                            .instances
                            .iter()
                            .any(|(id, at)| *id == entry.id && state.find(*at) == root)
                        {
                            continue;
                        }
                        self.step()?;
                        state.instances.push((entry.id, instance));
                        self.instantiate(state, &entry.literals, position, forall, instance);
                        changed = true;
                    }
                }
            }
        }
        Ok(changed)
    }

    /// Adds the clause with its `all` literal at `position` replaced by its body at `instance`,
    /// guarded by the range, canonicalised.
    fn instantiate(
        &mut self,
        state: &mut State,
        literals: &Clause,
        position: usize,
        forall: &Forall,
        instance: TermId,
    ) {
        let mut canon = Canon::new(self.ck, &mut state.terms, self.package);
        let mut parts: Vec<Formula> = literals
            .iter()
            .enumerate()
            .filter(|(at, _)| *at != position)
            .map(|(_, literal)| Formula::Lit(literal.clone()))
            .collect();
        let above = canon.terms.compare(BinaryOp::Le, forall.low, instance);
        let op = if forall.inclusive {
            BinaryOp::Le
        } else {
            BinaryOp::Lt
        };
        let below = canon.terms.compare(op, instance, forall.high);
        let body = canon.terms.substitute(forall.body, forall.bound, instance);
        parts.push(Formula::Leaf(above, false));
        parts.push(Formula::Leaf(below, false));
        parts.push(canon.formula(body, true));
        canon.emit(Formula::Or(parts), Origin::Fact);
        let clauses: Vec<Clause> = canon
            .clauses
            .iter()
            .map(|(clause, _)| clause.clone())
            .collect();
        let mut known: HashSet<TermId> = (0..state.registered.len() as TermId)
            .filter(|term| state.registered[*term as usize])
            .collect();
        let ranges = canon.ranges(&clauses, &mut known);
        for clause in ranges.into_iter().chain(clauses) {
            self.add_clause(state, clause);
        }
    }

    /// Adds a clause in term order, registering its terms.
    fn add_clause(&mut self, state: &mut State, clause: Clause) {
        for literal in &clause {
            match &literal.atom {
                Atom::All(forall) => {
                    for term in [forall.low, forall.high] {
                        self.register(state, term);
                    }
                    for term in closed_parts(&state.terms, forall) {
                        self.register(state, term);
                    }
                }
                atom => {
                    for term in super::canon::atom_terms(atom) {
                        self.register(state, term);
                    }
                }
            }
        }
        let clause = sorted_clause(self.ck, &state.terms, &mut state.encodings, clause);
        let key: Rc<[u8]> = state
            .encodings
            .clause(self.ck, &state.terms, &clause)
            .into();
        let at = state
            .clauses
            .partition_point(|entry| entry.key.as_ref() <= key.as_ref());
        state.clauses.insert(
            at,
            Entry {
                id: state.next_clause,
                key,
                literals: clause,
            },
        );
        state.next_clause += 1;
    }

    /// Gives a term and its parts their own classes, and files it among the state's terms.
    fn register(&mut self, state: &mut State, term: TermId) {
        if state.is_registered(term) {
            return;
        }
        let shape = state.terms.get(term).clone();
        if !matches!(shape, Term::Quant { .. }) {
            for part in shape.parts() {
                self.register(state, part);
            }
        }
        let index = term as usize;
        if state.parent.len() <= index {
            state.parent.resize(index + 1, 0);
            state.size.resize(index + 1, 0);
            state.classes.resize(index + 1, Class::ANY);
            state.registered.resize(index + 1, false);
        }
        state.parent[index] = term;
        state.size[index] = 1;
        state.registered[index] = true;
        let sort = state.terms.sort(term);
        if let Some(value) = state.terms.as_int(term) {
            state.classes[index] = Class {
                low: Some(value),
                high: Some(value),
                ..Class::ANY
            };
        }
        let key = state.encodings.term(self.ck, &state.terms, term);
        let insert = |list: &mut Vec<TermId>, state_encodings: &mut Encodings, terms: &Terms| {
            let at = list.partition_point(|other| {
                state_encodings.term(self.ck, terms, *other).as_ref() <= key.as_ref()
            });
            list.insert(at, term);
        };
        if !shape.parts().is_empty() && !matches!(shape, Term::Quant { .. }) {
            insert(&mut state.apps, &mut state.encodings, &state.terms);
        }
        if matches!(shape, Term::Ite(..)) {
            insert(&mut state.ites, &mut state.encodings, &state.terms);
        }
        if sort == Sort::Int {
            insert(&mut state.integers, &mut state.encodings, &state.terms);
            if let Term::App(Op::Binary(BinaryOp::Mul), parts) = &shape
                && parts.iter().all(|part| state.terms.as_int(*part).is_none())
            {
                insert(&mut state.muls, &mut state.encodings, &state.terms);
            }
        }
        let mut canon = Canon::new(self.ck, &mut state.terms, self.package);
        match canon.comparison(term, true) {
            Some(Folded::Lit(literal)) => {
                state.comparisons.insert(term, literal);
            }
            Some(Folded::True) => {
                state.comparisons.insert(term, tautology(true));
            }
            Some(Folded::False) => {
                state.comparisons.insert(term, tautology(false));
            }
            None => {}
        }
    }

    /// Merges two classes: their intervals and congruences intersect.
    pub fn merge(&mut self, state: &mut State, a: TermId, b: TermId) -> Step<bool> {
        let (a, b) = (state.find(a), state.find(b));
        if a == b {
            return Ok(false);
        }
        self.step()?;
        let (truth, falsity) = (state.find(state.truth), state.find(state.falsity));
        if (a == truth && b == falsity) || (a == falsity && b == truth) {
            return Err(Halt::Contradiction);
        }
        if state.separated.iter().any(|(x, y)| {
            let (x, y) = (state.find(*x), state.find(*y));
            (x == a && y == b) || (x == b && y == a)
        }) {
            return Err(Halt::Contradiction);
        }
        let (root, child) = match state.size[a as usize] >= state.size[b as usize] {
            true => (a, b),
            false => (b, a),
        };
        state.parent[child as usize] = root;
        state.size[root as usize] += state.size[child as usize];
        let merged = intersect(state.classes[root as usize], state.classes[child as usize])
            .ok_or(Halt::Contradiction)?;
        state.classes[root as usize] = merged;
        Ok(true)
    }

    /// Narrows a class's interval, inward to its congruence.
    pub fn narrow(
        &mut self,
        state: &mut State,
        term: TermId,
        low: Option<i128>,
        high: Option<i128>,
    ) -> Step<bool> {
        let root = state.find(term) as usize;
        let class = state.classes[root];
        let mut next = class;
        if let Some(low) = low {
            next.low = Some(next.low.map_or(low, |old| old.max(low)));
        }
        if let Some(high) = high {
            next.high = Some(next.high.map_or(high, |old| old.min(high)));
        }
        let next = next.fitted();
        if next == class {
            return Ok(false);
        }
        self.step()?;
        if next.empty() {
            return Err(Halt::Contradiction);
        }
        state.classes[root] = next;
        Ok(true)
    }

    fn congruent(
        &mut self,
        state: &mut State,
        term: TermId,
        residue: i128,
        modulus: i128,
    ) -> Step<bool> {
        let root = state.find(term) as usize;
        let class = state.classes[root];
        let Some((residue, modulus)) = crt(class.residue, class.modulus, residue, modulus)? else {
            return Ok(false);
        };
        let next = Class {
            residue,
            modulus,
            ..class
        }
        .fitted();
        if next == class {
            return Ok(false);
        }
        self.step()?;
        if next.empty() {
            return Err(Halt::Contradiction);
        }
        state.classes[root] = next;
        Ok(true)
    }

    /// Asserts a literal: a boolean atom merges with `true` or `false`, a linear atom joins
    /// the asserted atoms, and a negated equality records a disequality.
    pub fn assert(&mut self, state: &mut State, literal: &Literal) -> Step<bool> {
        if state.asserted.contains(literal) {
            return Ok(false);
        }
        if let (Atom::Le(_), true) = (&literal.atom, literal.negated) {
            return self.assert(state, &literal.negation());
        }
        self.step()?;
        state.asserted.insert(literal.clone());
        match (&literal.atom, literal.negated) {
            (Atom::Bool(term), negated) => {
                let target = if negated { state.falsity } else { state.truth };
                self.merge(state, *term, target)?;
                if let Some(linear) = state.comparisons.get(term).cloned() {
                    let linear = if negated { linear.negation() } else { linear };
                    self.assert(state, &linear)?;
                }
            }
            (Atom::Le(linear), _) | (Atom::Eq(linear), false) => {
                let key: Rc<[u8]> = state
                    .encodings
                    .literal(self.ck, &state.terms, literal)
                    .into();
                let at = state
                    .atoms
                    .partition_point(|(other, ..)| other.as_ref() <= key.as_ref());
                let equality = matches!(literal.atom, Atom::Eq(_));
                state.atoms.insert(at, (key, linear.clone(), equality));
            }
            (Atom::Eq(linear), true) => {
                state.disequalities.push(linear.clone());
                if let Some((positive, negative)) = difference(linear)
                    && linear.constant == 0
                {
                    if state.find(positive) == state.find(negative) {
                        return Err(Halt::Contradiction);
                    }
                    state.separated.push((positive, negative));
                }
            }
            (Atom::All(_), _) => {}
        }
        Ok(true)
    }

    /// Asserts that a boolean term is `true` or `false`, and its linear literal for an integer
    /// comparison.
    pub fn assert_term(&mut self, state: &mut State, term: TermId, value: bool) -> Step<bool> {
        self.assert(
            state,
            &Literal {
                negated: !value,
                atom: Atom::Bool(term),
            },
        )
    }

    /// Whether a boolean term's class is `true` or `false`, or its linear literal is decided;
    /// a decided comparison merges with its value.
    fn decided(&mut self, state: &mut State, term: TermId) -> Step<Option<bool>> {
        let root = state.find(term);
        if root == state.find(state.truth) {
            return Ok(Some(true));
        }
        if root == state.find(state.falsity) {
            return Ok(Some(false));
        }
        let Some(literal) = state.comparisons.get(&term).cloned() else {
            return Ok(None);
        };
        let value = state.entails(&literal);
        if let Some(value) = value {
            let target = if value { state.truth } else { state.falsity };
            self.merge(state, term, target)?;
        }
        Ok(value)
    }

    /// Whether a literal is true or false in the state, if it decides it.
    pub fn evaluate(&mut self, state: &mut State, literal: &Literal) -> Step<Option<bool>> {
        if state.asserted.contains(literal) {
            return Ok(Some(true));
        }
        if state.asserted.contains(&literal.negation()) {
            return Ok(Some(false));
        }
        let value = match &literal.atom {
            Atom::Bool(term) => self.decided(state, *term)?,
            Atom::All(_) => None,
            _ => state.entails(&Literal {
                negated: false,
                atom: literal.atom.clone(),
            }),
        };
        Ok(value.map(|value| value != literal.negated))
    }
}

impl State {
    pub fn find(&self, mut term: TermId) -> TermId {
        while self.parent[term as usize] != term {
            term = self.parent[term as usize];
        }
        term
    }

    pub fn is_registered(&self, term: TermId) -> bool {
        self.registered.get(term as usize).copied().unwrap_or(false)
    }

    /// Whether a disequality excludes a value for a term: one keeping its class apart from a
    /// class of that single value, or one whose other terms all have single values.
    pub fn excluded(&self, term: TermId, value: i128) -> bool {
        let root = self.find(term);
        let apart = self.separated.iter().any(|(p, q)| {
            let (p, q) = (self.find(*p), self.find(*q));
            (p == root && self.classes[q as usize].single() == Some(value))
                || (q == root && self.classes[p as usize].single() == Some(value))
        });
        apart
            || self.disequalities.iter().any(|linear| {
                let mut sum: Option<i128> = Some(0);
                let mut own = 0;
                for (coefficient, other) in &linear.terms {
                    match self.find(*other) == root {
                        true => own += coefficient,
                        false => {
                            sum = sum
                                .zip(self.classes[self.find(*other) as usize].single())
                                .and_then(|(sum, single)| {
                                    sum.checked_add(coefficient.checked_mul(single)?)
                                });
                        }
                    }
                }
                own != 0
                    && sum
                        .and_then(|sum| own.checked_mul(value).and_then(|own| own.checked_add(sum)))
                        == Some(linear.constant)
            })
    }

    pub fn class(&self, term: TermId) -> Class {
        self.classes[self.find(term) as usize]
    }

    /// Whether a term is registered and its class is `true`, `false`, or neither.
    pub fn truth_of(&self, term: TermId) -> Option<bool> {
        if !self.is_registered(term) {
            return None;
        }
        let root = self.find(term);
        match root {
            _ if root == self.find(self.truth) => Some(true),
            _ if root == self.find(self.falsity) => Some(false),
            _ => None,
        }
    }

    /// The bounds of Σ ci × ti over the terms' intervals, without the term at `skip`.
    fn sum(&self, linear: &Linear, skip: Option<usize>) -> (Option<i128>, Option<i128>) {
        let mut low = Some(0i128);
        let mut high = Some(0i128);
        for (index, (coefficient, term)) in linear.terms.iter().enumerate() {
            if Some(index) == skip {
                continue;
            }
            let class = self.class(*term);
            let (a, b) = (
                class.low.and_then(|value| value.checked_mul(*coefficient)),
                class.high.and_then(|value| value.checked_mul(*coefficient)),
            );
            let (least, most) = if *coefficient > 0 { (a, b) } else { (b, a) };
            low = low
                .zip(least)
                .and_then(|(sum, value)| sum.checked_add(value));
            high = high
                .zip(most)
                .and_then(|(sum, value)| sum.checked_add(value));
        }
        (low, high)
    }

    /// Whether the state entails a positive linear literal or its negation.
    fn entails(&self, literal: &Literal) -> Option<bool> {
        let (linear, equality) = match &literal.atom {
            Atom::Le(linear) => (linear, false),
            Atom::Eq(linear) => (linear, true),
            _ => return None,
        };
        let value = self.entails_atom(linear, equality)?;
        Some(value != literal.negated)
    }

    fn entails_atom(&self, linear: &Linear, equality: bool) -> Option<bool> {
        let mut by_class: Vec<(TermId, i128)> = Vec::new();
        for (coefficient, term) in &linear.terms {
            let root = self.find(*term);
            match by_class.iter_mut().find(|(class, _)| *class == root) {
                Some((_, sum)) => *sum += coefficient,
                None => by_class.push((root, *coefficient)),
            }
        }
        if by_class.iter().all(|(_, coefficient)| *coefficient == 0) {
            return Some(match equality {
                true => linear.constant == 0,
                false => 0 <= linear.constant,
            });
        }
        let (low, high) = self.sum(linear, None);
        if equality {
            if low.is_some_and(|low| low > linear.constant)
                || high.is_some_and(|high| high < linear.constant)
            {
                return Some(false);
            }
            if low == Some(linear.constant) && high == Some(linear.constant) {
                return Some(true);
            }
            if self.disequalities.contains(linear) {
                return Some(false);
            }
        } else {
            if high.is_some_and(|high| high <= linear.constant) {
                return Some(true);
            }
            if low.is_some_and(|low| low > linear.constant) {
                return Some(false);
            }
        }
        let (positive, negative) = difference(linear)?;
        let (p, n) = (self.find(positive), self.find(negative));
        let up = self.graph.get(&(n, p)).copied();
        let down = self.graph.get(&(p, n)).copied();
        if down.is_some_and(|down| down < -linear.constant) {
            return Some(false);
        }
        if equality {
            if up.is_some_and(|up| up < linear.constant) {
                return Some(false);
            }
            if up == Some(linear.constant) && down == Some(-linear.constant) {
                return Some(true);
            }
        } else if up.is_some_and(|up| up <= linear.constant) {
            return Some(true);
        }
        None
    }

    /// Whether a term's class excludes another's: disjoint intervals, a disequality, or
    /// opposite truth values.
    fn excludes(&self, a: TermId, b: TermId) -> bool {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return false;
        }
        if let (Some(x), Some(y)) = (self.truth_of(a), self.truth_of(b)) {
            return x != y;
        }
        let (x, y) = (self.classes[ra as usize], self.classes[rb as usize]);
        let disjoint = x.high.zip(y.low).is_some_and(|(high, low)| high < low)
            || y.high.zip(x.low).is_some_and(|(high, low)| high < low);
        disjoint
            || self.separated.iter().any(|(p, q)| {
                let (p, q) = (self.find(*p), self.find(*q));
                (p == ra && q == rb) || (p == rb && q == ra)
            })
    }
}

/// The two terms of a linear atom p − n, when its coefficients are 1 and −1.
fn difference(linear: &Linear) -> Option<(TermId, TermId)> {
    match linear.terms[..] {
        [(1, p), (-1, n)] | [(-1, n), (1, p)] => Some((p, n)),
        _ => None,
    }
}

/// A literal that is always true or always false.
fn tautology(value: bool) -> Literal {
    Literal {
        negated: !value,
        atom: Atom::Le(Linear {
            terms: Vec::new(),
            constant: 0,
        }),
    }
}

/// The applications in an `all` body with its bound symbol as exactly one argument and no
/// bound symbol among the others, each with that argument's position.
fn triggers(terms: &Terms, forall: &Forall) -> Vec<(TermId, usize)> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    let mut pending = vec![forall.body];
    while let Some(term) = pending.pop() {
        if !seen.insert(term) {
            continue;
        }
        let parts = terms.get(term).parts();
        let at: Vec<usize> = (0..parts.len())
            .filter(|index| parts[*index] == forall.bound)
            .collect();
        if let [index] = at[..]
            && !matches!(
                terms.get(term),
                Term::Quant { .. } | Term::And(_) | Term::Or(_) | Term::Not(_)
            )
            && parts
                .iter()
                .enumerate()
                .all(|(other, part)| other == index || !has_bound(terms, *part))
        {
            found.push((term, index));
        }
        pending.extend(parts);
    }
    found.sort();
    found
}

fn has_bound(terms: &Terms, term: TermId) -> bool {
    match terms.get(term) {
        Term::Bound(_) => true,
        shape => shape.parts().iter().any(|part| has_bound(terms, *part)),
    }
}

/// The subterms of an `all` body without bound symbols, whose classes its triggers compare.
fn closed_parts(terms: &Terms, forall: &Forall) -> Vec<TermId> {
    triggers(terms, forall)
        .into_iter()
        .flat_map(|(trigger, index)| {
            let parts = terms.get(trigger).parts();
            parts
                .into_iter()
                .enumerate()
                .filter(move |(at, _)| *at != index)
                .map(|(_, part)| part)
        })
        .collect()
}

fn gcd(a: i128, b: i128) -> i128 {
    let (mut a, mut b) = (a.abs(), b.abs());
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// x ≡ r1 (mod m1) and x ≡ r2 (mod m2) as one congruence; a contradiction when they have no
/// common solution, none when the result leaves the integers computed with.
fn crt(r1: i128, m1: i128, r2: i128, m2: i128) -> Step<Option<(i128, i128)>> {
    let g = gcd(m1, m2);
    if (r2 - r1).rem_euclid(g) != 0 {
        return Err(Halt::Contradiction);
    }
    let Some(lcm) = (m1 / g).checked_mul(m2) else {
        return Ok(None);
    };
    let (m1g, m2g) = (m1 / g, m2 / g);
    let Some(inverse) = inverse(m1g.rem_euclid(m2g), m2g) else {
        return Ok(None);
    };
    let k = ((r2 - r1) / g)
        .rem_euclid(m2g)
        .checked_mul(inverse)
        .map(|k| k.rem_euclid(m2g));
    let Some(x) = k
        .and_then(|k| m1.checked_mul(k))
        .and_then(|x| x.checked_add(r1))
    else {
        return Ok(None);
    };
    Ok(Some((x.rem_euclid(lcm), lcm)))
}

/// The inverse of a modulo m, for a and m coprime.
fn inverse(a: i128, m: i128) -> Option<i128> {
    if m == 1 {
        return Some(0);
    }
    let (mut old_r, mut r) = (a, m);
    let (mut old_s, mut s) = (1i128, 0i128);
    while r != 0 {
        let quotient = old_r / r;
        (old_r, r) = (r, old_r - quotient * r);
        (old_s, s) = (s, old_s.checked_sub(quotient.checked_mul(s)?)?);
    }
    (old_r == 1).then(|| old_s.rem_euclid(m))
}

/// The intersection of two classes' intervals and congruences; none when empty.
fn intersect(a: Class, b: Class) -> Option<Class> {
    let low = match (a.low, b.low) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, y) => x.or(y),
    };
    let high = match (a.high, b.high) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, y) => x.or(y),
    };
    let (residue, modulus) = match crt(a.residue, a.modulus, b.residue, b.modulus) {
        Err(_) => return None,
        Ok(Some(congruence)) => congruence,
        Ok(None) => (a.residue, a.modulus),
    };
    let class = Class {
        low,
        high,
        residue,
        modulus,
    }
    .fitted();
    (!class.empty()).then_some(class)
}

fn floor_div(value: i128, divisor: i128) -> i128 {
    let quotient = value / divisor;
    if (value % divisor != 0) && ((value < 0) != (divisor < 0)) {
        quotient - 1
    } else {
        quotient
    }
}

fn ceil_div(value: i128, divisor: i128) -> i128 {
    let quotient = value / divisor;
    if (value % divisor != 0) && ((value < 0) == (divisor < 0)) {
        quotient + 1
    } else {
        quotient
    }
}
