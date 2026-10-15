//! Canonicalisation: the premises and negated goal of a query as clauses of literals over linear
//! integer atoms, boolean atoms and bounded `all`; then the relevant clauses alone, their
//! symbols renumbered, sorted into term order.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::encode::Encodings;
use super::term::{Op, Sort, Term, TermId, Terms};
use crate::check::Checker;
use crate::check::db::{ItemId, PackageId};
use crate::check::sig::SigKind;
use crate::check::ty::{Scalar, Ty};
use crate::nir::{BinaryOp, UnaryOp};
use crate::syntax::ast::TypeModifier;

/// Σ ci × ti and a constant.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Linear {
    pub terms: Vec<(i128, TermId)>,
    pub constant: i128,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Atom {
    /// Σ ci × ti ≤ c.
    Le(Linear),
    /// Σ ci × ti = c.
    Eq(Linear),
    Bool(TermId),
    All(Rc<Forall>),
}

/// `all x in low..<high: body`, or `..=`, the body a formula over the bound symbol x.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Forall {
    pub low: TermId,
    pub high: TermId,
    pub inclusive: bool,
    pub bound: TermId,
    pub body: TermId,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Literal {
    pub negated: bool,
    pub atom: Atom,
}

impl Literal {
    /// The literal holding exactly when this one does not.
    pub fn negation(&self) -> Literal {
        match (&self.atom, self.negated) {
            (Atom::Le(linear), false) => Literal {
                negated: false,
                atom: Atom::Le(Linear {
                    terms: linear.terms.iter().map(|(c, t)| (-c, *t)).collect(),
                    constant: -linear.constant - 1,
                }),
            },
            _ => Literal {
                negated: !self.negated,
                atom: self.atom.clone(),
            },
        }
    }
}

pub type Clause = Vec<Literal>;

/// Where a premise comes from, as statuses read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Fact,
    /// The operand of an `assume`.
    Assume,
    /// The condition of an obligation site, by its index among its function's sites.
    Site(usize),
    Goal,
}

/// A formula in negation normal form, its leaves not yet atoms.
#[derive(Clone, Debug)]
pub enum Formula {
    True,
    False,
    And(Vec<Formula>),
    Or(Vec<Formula>),
    /// A comparison or boolean term, holding or negated.
    Leaf(TermId, bool),
    Lit(Literal),
}

/// A leaf as a literal, or the truth value it folds to.
pub enum Folded {
    True,
    False,
    Lit(Literal),
}

pub struct Canon<'a, 'c> {
    pub ck: &'a Checker<'c>,
    pub terms: &'a mut Terms,
    /// The package of the code judged, beside which another package's open enum may grow.
    package: PackageId,
    normal: HashMap<TermId, TermId>,
    /// The fresh boolean symbol standing for each ite condition that is not an atom.
    conditions: HashMap<TermId, TermId>,
    /// The symbols standing for conditions whose defining clauses are not yet emitted.
    defined: Vec<(TermId, TermId)>,
    /// The divisions whose facts are given, by dividend and divisor.
    divided: HashSet<(TermId, TermId)>,
    pub clauses: Vec<(Clause, Origin)>,
}

impl<'a, 'c> Canon<'a, 'c> {
    pub fn new(ck: &'a Checker<'c>, terms: &'a mut Terms, package: PackageId) -> Canon<'a, 'c> {
        Canon {
            ck,
            terms,
            package,
            divided: HashSet::new(),
            normal: HashMap::new(),
            conditions: HashMap::new(),
            defined: Vec::new(),
            clauses: Vec::new(),
        }
    }

    pub fn premise(&mut self, term: TermId, origin: Origin) {
        let formula = self.formula(term, true);
        self.emit(formula, origin);
    }

    pub fn negated_goal(&mut self, goal: TermId) {
        let formula = self.formula(goal, false);
        self.emit(formula, Origin::Goal);
    }

    /// A formula as negation normal form: guards and boolean ites as disjunctions, `not`
    /// pushed onto atoms, and each `any` within no `all` as its body for a skolem symbol.
    pub fn formula(&mut self, term: TermId, positive: bool) -> Formula {
        match self.terms.get(term).clone() {
            Term::Lit(crate::nir::Literal::Bool(value)) => match value == positive {
                true => Formula::True,
                false => Formula::False,
            },
            Term::And(parts) | Term::Or(parts) => {
                let conjunction = matches!(self.terms.get(term), Term::And(_)) == positive;
                let parts = parts
                    .iter()
                    .map(|part| self.formula(*part, positive))
                    .collect();
                match conjunction {
                    true => Formula::And(parts),
                    false => Formula::Or(parts),
                }
            }
            Term::Not(inner) => self.formula(inner, !positive),
            Term::Ite(condition, then, otherwise) if self.terms.sort(term) == Sort::Bool => {
                Formula::And(vec![
                    Formula::Or(vec![
                        self.formula(condition, false),
                        self.formula(then, positive),
                    ]),
                    Formula::Or(vec![
                        self.formula(condition, true),
                        self.formula(otherwise, positive),
                    ]),
                ])
            }
            Term::Quant {
                all,
                low,
                high,
                inclusive,
                bound,
                body,
            } => {
                let body = match positive {
                    true => body,
                    false => self.terms.not(body),
                };
                let forall = Forall {
                    low,
                    high,
                    inclusive,
                    bound,
                    body,
                };
                match all == positive {
                    true => Formula::Lit(Literal {
                        negated: false,
                        atom: Atom::All(Rc::new(forall)),
                    }),
                    false => self.witness(&forall),
                }
            }
            _ => Formula::Leaf(term, positive),
        }
    }

    /// `any x in low..<high: body` as the body for a skolem symbol k, with low ≤ k and k < high.
    fn witness(&mut self, any: &Forall) -> Formula {
        let ty = self.terms.ty(any.bound).clone();
        let skolem = self.terms.skolem(ty);
        let body = self.terms.substitute(any.body, any.bound, skolem);
        let above = self.terms.compare(BinaryOp::Le, any.low, skolem);
        let op = if any.inclusive {
            BinaryOp::Le
        } else {
            BinaryOp::Lt
        };
        let below = self.terms.compare(op, skolem, any.high);
        Formula::And(vec![
            self.formula(body, true),
            Formula::Leaf(above, true),
            Formula::Leaf(below, true),
        ])
    }

    fn negate(&mut self, formula: Formula) -> Formula {
        match formula {
            Formula::True => Formula::False,
            Formula::False => Formula::True,
            Formula::And(parts) => {
                Formula::Or(parts.into_iter().map(|part| self.negate(part)).collect())
            }
            Formula::Or(parts) => {
                Formula::And(parts.into_iter().map(|part| self.negate(part)).collect())
            }
            Formula::Leaf(term, positive) => Formula::Leaf(term, !positive),
            Formula::Lit(literal) => match &literal.atom {
                Atom::All(forall) if !literal.negated => {
                    let body = self.terms.not(forall.body);
                    self.witness(&Forall {
                        body,
                        ..(**forall).clone()
                    })
                }
                _ => Formula::Lit(literal.negation()),
            },
        }
    }

    /// Adds a formula's clauses: each conjunct its own premise.
    pub fn emit(&mut self, formula: Formula, origin: Origin) {
        match formula {
            Formula::True => {}
            Formula::And(parts) => {
                for part in parts {
                    self.emit(part, origin);
                }
            }
            other => self.clause(other, origin),
        }
    }

    /// Adds a disjunction as a clause, a conjunction within it standing as a fresh boolean
    /// symbol, followed by the clauses defining each symbol it introduces.
    fn clause(&mut self, formula: Formula, origin: Origin) {
        let mut leaves = Vec::new();
        let mut conjunctions = Vec::new();
        self.disjuncts(formula, &mut leaves, &mut conjunctions);
        let mark = self.defined.len();
        let mut literals = Vec::new();
        let mut satisfied = false;
        for leaf in leaves {
            match self.literal(leaf) {
                Folded::True => satisfied = true,
                Folded::False => {}
                Folded::Lit(literal) => literals.push(literal),
            }
        }
        if !satisfied {
            self.clauses.push((literals, origin));
        }
        for (symbol, parts) in conjunctions {
            let mut back = vec![Formula::Leaf(symbol, true)];
            for part in parts {
                self.emit(
                    Formula::Or(vec![Formula::Leaf(symbol, false), part.clone()]),
                    origin,
                );
                back.push(self.negate(part));
            }
            self.emit(Formula::Or(back), origin);
        }
        let defined: Vec<(TermId, TermId)> = self.defined.drain(mark..).collect();
        for (symbol, condition) in defined {
            let forward = self.formula(condition, true);
            self.emit(
                Formula::Or(vec![Formula::Leaf(symbol, false), forward]),
                origin,
            );
            let backward = self.formula(condition, false);
            self.emit(
                Formula::Or(vec![Formula::Leaf(symbol, true), backward]),
                origin,
            );
        }
    }

    fn disjuncts(
        &mut self,
        formula: Formula,
        leaves: &mut Vec<Formula>,
        conjunctions: &mut Vec<(TermId, Vec<Formula>)>,
    ) {
        match formula {
            Formula::Or(parts) => {
                for part in parts {
                    self.disjuncts(part, leaves, conjunctions);
                }
            }
            Formula::And(parts) => {
                let symbol = self.terms.fresh(Ty::Scalar(Scalar::Bool));
                leaves.push(Formula::Leaf(symbol, true));
                conjunctions.push((symbol, parts));
            }
            other => leaves.push(other),
        }
    }

    /// A leaf as a literal: an integer comparison as a linear atom, any other term as a
    /// boolean atom; a literal boolean as its truth value.
    pub fn literal(&mut self, leaf: Formula) -> Folded {
        match leaf {
            Formula::True => Folded::True,
            Formula::False => Folded::False,
            Formula::Lit(literal) => Folded::Lit(literal),
            Formula::Leaf(term, positive) => {
                if let Some(folded) = self.comparison(term, positive) {
                    return folded;
                }
                let atom = self.normal(term);
                match self.terms.as_bool(atom) {
                    Some(value) if value == positive => Folded::True,
                    Some(_) => Folded::False,
                    None => Folded::Lit(Literal {
                        negated: !positive,
                        atom: Atom::Bool(atom),
                    }),
                }
            }
            Formula::And(_) | Formula::Or(_) => {
                unreachable!("a connective among a clause's leaves")
            }
        }
    }

    /// An integer comparison as a linear atom, its coefficients divided by their greatest
    /// common divisor; none for another term, or one whose sums leave the integers the prover
    /// computes with.
    pub fn comparison(&mut self, term: TermId, positive: bool) -> Option<Folded> {
        let Term::App(Op::Binary(op), parts) = self.terms.get(term).clone() else {
            return None;
        };
        if !op.is_comparison() || self.terms.sort(parts[0]) != Sort::Int {
            return None;
        }
        let op = match positive {
            true => op,
            false => complement(op),
        };
        let left = self.linear(parts[0])?;
        let right = self.linear(parts[1])?;
        let mut difference = left;
        for (coefficient, term) in right.terms {
            add_term(&mut difference.terms, coefficient.checked_neg()?, term)?;
        }
        difference.constant = difference.constant.checked_sub(right.constant)?;
        difference
            .terms
            .retain(|(coefficient, _)| *coefficient != 0);
        let (terms, constant) = (difference.terms, difference.constant.checked_neg()?);
        let negated_terms = || terms.iter().map(|(c, t)| (-c, *t)).collect::<Vec<_>>();
        let (atom, negated) = match op {
            BinaryOp::Le => (le(terms.clone(), constant), false),
            BinaryOp::Lt => (le(terms.clone(), constant.checked_sub(1)?), false),
            BinaryOp::Ge => (le(negated_terms(), constant.checked_neg()?), false),
            BinaryOp::Gt => (
                le(negated_terms(), constant.checked_neg()?.checked_sub(1)?),
                false,
            ),
            BinaryOp::Eq => (eq(terms.clone(), constant), false),
            _ => (eq(terms.clone(), constant), true),
        };
        Some(match atom {
            Folded::True if negated => Folded::False,
            Folded::False if negated => Folded::True,
            Folded::Lit(Literal { atom, .. }) => Folded::Lit(Literal { negated, atom }),
            folded => folded,
        })
    }

    /// An integer term as a sum: literals, `add`, `sub`, `neg`, `mul` by a literal and `inv`
    /// interpreted, every other term an atom of the sum.
    fn linear(&mut self, term: TermId) -> Option<Linear> {
        let mut sum = Linear {
            terms: Vec::new(),
            constant: 0,
        };
        self.add_linear(term, 1, &mut sum)?;
        sum.terms.retain(|(coefficient, _)| *coefficient != 0);
        Some(sum)
    }

    fn add_linear(&mut self, term: TermId, factor: i128, sum: &mut Linear) -> Option<()> {
        if let Some(value) = self.terms.as_int(term) {
            sum.constant = sum.constant.checked_add(factor.checked_mul(value)?)?;
            return Some(());
        }
        match self.terms.get(term).clone() {
            Term::App(Op::Binary(BinaryOp::Add), parts) => {
                self.add_linear(parts[0], factor, sum)?;
                self.add_linear(parts[1], factor, sum)
            }
            Term::App(Op::Binary(BinaryOp::Sub), parts) => {
                self.add_linear(parts[0], factor, sum)?;
                self.add_linear(parts[1], factor.checked_neg()?, sum)
            }
            Term::App(Op::Unary(UnaryOp::Neg), parts) => {
                self.add_linear(parts[0], factor.checked_neg()?, sum)
            }
            Term::App(Op::Binary(BinaryOp::Mul), parts)
                if self.terms.as_int(parts[0]).is_some()
                    || self.terms.as_int(parts[1]).is_some() =>
            {
                let (literal, other) = match self.terms.as_int(parts[0]) {
                    Some(value) => (value, parts[1]),
                    None => (self.terms.as_int(parts[1])?, parts[0]),
                };
                self.add_linear(other, factor.checked_mul(literal)?, sum)
            }
            Term::App(Op::Unary(UnaryOp::Inv), parts) => {
                let scalar = self.terms.ty(term).scalar()?;
                let shift = match scalar.is_signed() {
                    true => -1,
                    false => scalar.range().1,
                };
                sum.constant = sum.constant.checked_add(factor.checked_mul(shift)?)?;
                self.add_linear(parts[0], factor.checked_neg()?, sum)
            }
            _ => {
                let atom = self.normal(term);
                add_term(&mut sum.terms, factor, atom)
            }
        }
    }

    /// A term with each ite condition that is not an atom replaced by a fresh boolean symbol.
    pub fn normal(&mut self, term: TermId) -> TermId {
        if let Some(normal) = self.normal.get(&term) {
            return *normal;
        }
        let normal = match self.terms.get(term).clone() {
            Term::Ite(condition, then, otherwise) => {
                let condition = self.condition(condition);
                let then = self.normal(then);
                let otherwise = self.normal(otherwise);
                self.terms.ite(condition, then, otherwise)
            }
            Term::Quant { .. } => term,
            shape => {
                let parts = shape.parts();
                let normals: Vec<TermId> = parts.iter().map(|part| self.normal(*part)).collect();
                match normals == parts {
                    true => term,
                    false => self.terms.rebuild(term, &normals),
                }
            }
        };
        self.normal.insert(term, normal);
        normal
    }

    fn condition(&mut self, condition: TermId) -> TermId {
        let condition = self.normal(condition);
        let atom = !matches!(
            self.terms.get(condition),
            Term::And(_) | Term::Or(_) | Term::Not(_) | Term::Quant { .. } | Term::Ite(..)
        );
        if atom {
            return condition;
        }
        if let Some(symbol) = self.conditions.get(&condition) {
            return *symbol;
        }
        let symbol = self.terms.fresh(Ty::Scalar(Scalar::Bool));
        self.conditions.insert(condition, symbol);
        self.defined.push((symbol, condition));
        symbol
    }

    /// The range premises and division facts of the integer symbols and applications of
    /// clauses, in order of their first occurrence, skipping the terms in `known`.
    pub fn ranges(&mut self, clauses: &[Clause], known: &mut HashSet<TermId>) -> Vec<Clause> {
        let mut order = Vec::new();
        for clause in clauses {
            for literal in clause {
                let terms = match &literal.atom {
                    Atom::All(forall) => vec![forall.low, forall.high],
                    atom => atom_terms(atom),
                };
                for term in terms {
                    self.collect(term, known, &mut order);
                }
            }
        }
        let mut premises = Vec::new();
        let mut index = 0;
        while index < order.len() {
            let term = order[index];
            index += 1;
            for premise in self.range_of(term) {
                for (_, part) in linear_of(&premise) {
                    self.collect(part, known, &mut order);
                }
                premises.push(vec![premise]);
            }
        }
        premises
    }

    /// Adds the integer terms within a term outside quantifiers, each after its parts.
    fn collect(&self, term: TermId, known: &mut HashSet<TermId>, order: &mut Vec<TermId>) {
        if !known.insert(term) || matches!(self.terms.get(term), Term::Quant { .. }) {
            return;
        }
        for part in self.terms.get(term).parts() {
            self.collect(part, known, order);
        }
        if self.terms.sort(term) == Sort::Int {
            order.push(term);
        }
    }

    /// The range premises of an integer term, with the facts of a division, remainder, bitwise
    /// and or shift the prover knows through facts.
    fn range_of(&mut self, term: TermId) -> Vec<Literal> {
        let ty = self.terms.ty(term).clone();
        let scalar = ty.scalar();
        let unsigned = scalar.is_some_and(|scalar| !scalar.is_signed());
        let at_least = |value: i128| Literal {
            negated: false,
            atom: Atom::Le(Linear {
                terms: vec![(-1, term)],
                constant: -value,
            }),
        };
        let at_most = |value: i128| Literal {
            negated: false,
            atom: Atom::Le(Linear {
                terms: vec![(1, term)],
                constant: value,
            }),
        };
        let below = |other: TermId, offset: i128| Literal {
            negated: false,
            atom: Atom::Le(Linear {
                terms: vec![(1, term), (-1, other)],
                constant: offset,
            }),
        };
        let typed = || match scalar {
            Some(Scalar::Char) => vec![at_least(0), at_most(0x10_FFFF)],
            Some(scalar) if scalar.is_integer() => {
                let (low, high) = scalar.range();
                vec![at_least(low), at_most(high)]
            }
            _ => Vec::new(),
        };
        match self.terms.get(term).clone() {
            Term::Lit(_) | Term::Ite(..) | Term::Bound(_) => Vec::new(),
            Term::Tag(value) => {
                let Ty::Item(item, _) = self.terms.ty(value).clone() else {
                    return vec![at_least(0)];
                };
                match &self.ck.sig(item).kind {
                    SigKind::Enum { variants } if !self.foreign_open(item) => {
                        vec![at_least(0), at_most(variants.len() as i128 - 1)]
                    }
                    _ => vec![at_least(0)],
                }
            }
            Term::Quot(dividend, divisor) | Term::Rem(dividend, divisor) => {
                let mut facts = typed();
                if !self.divided.insert((dividend, divisor)) {
                    return facts;
                }
                let quotient = self.terms.intern(Term::Quot(dividend, divisor), ty.clone());
                let remainder = self.terms.intern(Term::Rem(dividend, divisor), ty);
                let divisor = self.terms.as_int(divisor).unwrap_or(1);
                facts.push(Literal {
                    negated: false,
                    atom: Atom::Le(Linear {
                        terms: vec![(1, remainder)],
                        constant: divisor - 1,
                    }),
                });
                if let Some(sum) = self.linear(dividend) {
                    let mut terms = sum.terms;
                    terms.push((-divisor, quotient));
                    terms.push((-1, remainder));
                    facts.push(Literal {
                        negated: false,
                        atom: Atom::Eq(Linear {
                            terms,
                            constant: -sum.constant,
                        }),
                    });
                }
                facts
            }
            Term::App(Op::Binary(BinaryOp::Add | BinaryOp::Sub), _)
            | Term::App(Op::Unary(UnaryOp::Neg | UnaryOp::Inv), _) => self.definition(term),
            Term::App(Op::Binary(BinaryOp::Mul), parts)
                if parts.iter().any(|part| self.terms.as_int(*part).is_some()) =>
            {
                self.definition(term)
            }
            Term::App(Op::Binary(op), parts) => match op {
                BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul => Vec::new(),
                BinaryOp::Shl if scalar.is_some_and(Scalar::is_signed) => Vec::new(),
                BinaryOp::Div | BinaryOp::Shr if unsigned => {
                    vec![at_least(0), below(parts[0], 0)]
                }
                BinaryOp::Rem if unsigned => {
                    vec![at_least(0), below(parts[0], 0), below(parts[1], -1)]
                }
                BinaryOp::Band if unsigned => {
                    vec![at_least(0), below(parts[0], 0), below(parts[1], 0)]
                }
                _ => typed(),
            },
            _ => typed(),
        }
    }

    /// The equality an interpreted arithmetic term holding a place where no sum stands for it,
    /// such as an ite's arm, has with the sum it interprets.
    fn definition(&mut self, term: TermId) -> Vec<Literal> {
        let Some(sum) = self.linear(term) else {
            return Vec::new();
        };
        let mut terms = vec![(1, term)];
        terms.extend(
            sum.terms
                .iter()
                .map(|(coefficient, part)| (-coefficient, *part)),
        );
        match eq(terms, sum.constant) {
            Folded::Lit(literal) => vec![literal],
            _ => Vec::new(),
        }
    }

    /// Whether an enum is open and declared by another package than the code judged.
    fn foreign_open(&self, item: ItemId) -> bool {
        self.ck.db.ast(item).has_modifier(TypeModifier::Open)
            && self.ck.db.package_of(item) != self.package
    }
}

fn complement(op: BinaryOp) -> BinaryOp {
    match op {
        BinaryOp::Lt => BinaryOp::Ge,
        BinaryOp::Le => BinaryOp::Gt,
        BinaryOp::Gt => BinaryOp::Le,
        BinaryOp::Ge => BinaryOp::Lt,
        BinaryOp::Eq => BinaryOp::Ne,
        _ => BinaryOp::Eq,
    }
}

fn add_term(terms: &mut Vec<(i128, TermId)>, coefficient: i128, term: TermId) -> Option<()> {
    match terms.iter_mut().find(|(_, existing)| *existing == term) {
        Some((existing, _)) => *existing = existing.checked_add(coefficient)?,
        None => terms.push((coefficient, term)),
    }
    Some(())
}

fn gcd(a: i128, b: i128) -> i128 {
    let (mut a, mut b) = (a.abs(), b.abs());
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn divisor(terms: &[(i128, TermId)]) -> i128 {
    terms
        .iter()
        .fold(0, |divisor, (coefficient, _)| gcd(divisor, *coefficient))
}

/// Σ ci × ti ≤ c, divided by the coefficients' divisor, c rounded toward negative infinity.
fn le(terms: Vec<(i128, TermId)>, constant: i128) -> Folded {
    if terms.is_empty() {
        return if 0 <= constant {
            Folded::True
        } else {
            Folded::False
        };
    }
    let divisor = divisor(&terms);
    Folded::Lit(Literal {
        negated: false,
        atom: Atom::Le(Linear {
            terms: terms.iter().map(|(c, t)| (c / divisor, *t)).collect(),
            constant: constant.div_euclid(divisor),
        }),
    })
}

/// Σ ci × ti = c, divided by the coefficients' divisor; false when it does not divide c.
fn eq(terms: Vec<(i128, TermId)>, constant: i128) -> Folded {
    if terms.is_empty() {
        return if constant == 0 {
            Folded::True
        } else {
            Folded::False
        };
    }
    let divisor = divisor(&terms);
    if constant % divisor != 0 {
        return Folded::False;
    }
    Folded::Lit(Literal {
        negated: false,
        atom: Atom::Eq(Linear {
            terms: terms.iter().map(|(c, t)| (c / divisor, *t)).collect(),
            constant: constant / divisor,
        }),
    })
}

/// The terms an atom holds directly.
pub fn atom_terms(atom: &Atom) -> Vec<TermId> {
    match atom {
        Atom::Le(linear) | Atom::Eq(linear) => linear.terms.iter().map(|(_, term)| *term).collect(),
        Atom::Bool(term) => vec![*term],
        Atom::All(forall) => vec![forall.low, forall.high, forall.bound, forall.body],
    }
}

fn linear_of(literal: &Literal) -> Vec<(i128, TermId)> {
    match &literal.atom {
        Atom::Le(linear) | Atom::Eq(linear) => linear.terms.clone(),
        _ => Vec::new(),
    }
}

/// A canonical query: its relevant clauses in term order over its own store, each with the
/// origin of the premise it comes from.
pub struct Query {
    pub terms: Terms,
    pub clauses: Vec<Clause>,
    pub origins: Vec<Origin>,
}

/// Canonicalises a query: premises in order, then the range premises of their terms, then the
/// negated goal.
pub fn canonical(
    ck: &Checker,
    terms: &mut Terms,
    package: PackageId,
    premises: &[(TermId, Origin)],
    goal: TermId,
) -> Query {
    let mut canon = Canon::new(ck, terms, package);
    for (premise, origin) in premises {
        canon.premise(*premise, *origin);
    }
    let start = canon.clauses.len();
    canon.negated_goal(goal);
    let clauses: Vec<Clause> = canon
        .clauses
        .iter()
        .map(|(clause, _)| clause.clone())
        .collect();
    let ranges = canon.ranges(&clauses, &mut HashSet::new());
    let mut all = canon.clauses;
    let tail = all.split_off(start);
    all.extend(ranges.into_iter().map(|clause| (clause, Origin::Fact)));
    all.extend(tail);
    let relevant = relevant(terms, &all);
    let kept: Vec<(Clause, Origin)> = all
        .into_iter()
        .zip(relevant)
        .filter(|(_, relevant)| *relevant)
        .map(|(clause, _)| clause)
        .collect();
    let mut transfer = Transfer {
        from: terms,
        to: Terms::default(),
        map: HashMap::new(),
    };
    let moved: Vec<(Clause, Origin)> = kept
        .iter()
        .map(|(clause, origin)| (transfer.clause(clause), *origin))
        .collect();
    let store = transfer.to;
    let mut encodings = Encodings::default();
    let mut sorted: Vec<(Vec<u8>, Clause, Origin)> = moved
        .into_iter()
        .map(|(clause, origin)| {
            let clause = sorted_clause(ck, &store, &mut encodings, clause);
            (encodings.clause(ck, &store, &clause), clause, origin)
        })
        .collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    Query {
        terms: store,
        clauses: sorted.iter().map(|(_, clause, _)| clause.clone()).collect(),
        origins: sorted.iter().map(|(_, _, origin)| *origin).collect(),
    }
}

/// A clause in term order: the terms of each atom, then its literals, an equality whose first
/// coefficient is negative negated.
pub fn sorted_clause(
    ck: &Checker,
    terms: &Terms,
    encodings: &mut Encodings,
    clause: Clause,
) -> Clause {
    let mut literals: Vec<(Vec<u8>, Literal)> = clause
        .into_iter()
        .map(|mut literal| {
            if let Atom::Le(linear) | Atom::Eq(linear) = &mut literal.atom {
                linear
                    .terms
                    .sort_by_cached_key(|(_, term)| encodings.term(ck, terms, *term));
            }
            if let Atom::Eq(linear) = &mut literal.atom
                && linear
                    .terms
                    .first()
                    .is_some_and(|(coefficient, _)| *coefficient < 0)
            {
                for (coefficient, _) in &mut linear.terms {
                    *coefficient = -*coefficient;
                }
                linear.constant = -linear.constant;
            }
            (encodings.literal(ck, terms, &literal), literal)
        })
        .collect();
    literals.sort_by(|a, b| a.0.cmp(&b.0));
    literals.into_iter().map(|(_, literal)| literal).collect()
}

/// Which clauses are relevant: those of the negated goal, and each sharing a symbol or an
/// application with a relevant one; every clause when the negated goal holds no symbol or
/// application, as when asking whether premises entail `false`.
fn relevant(terms: &Terms, clauses: &[(Clause, Origin)]) -> Vec<bool> {
    let keys: Vec<Vec<TermId>> = clauses
        .iter()
        .map(|(clause, _)| {
            let mut seen = HashSet::new();
            for literal in clause {
                for term in atom_terms(&literal.atom) {
                    subterms(terms, term, &mut seen);
                }
            }
            seen.into_iter().collect()
        })
        .collect();
    let mut holding: HashMap<TermId, Vec<usize>> = HashMap::new();
    for (index, keys) in keys.iter().enumerate() {
        for key in keys {
            holding.entry(*key).or_default().push(index);
        }
    }
    let goal_keys = clauses
        .iter()
        .zip(&keys)
        .any(|((_, origin), keys)| *origin == Origin::Goal && !keys.is_empty());
    let mut relevant: Vec<bool> = clauses
        .iter()
        .map(|(_, origin)| *origin == Origin::Goal || !goal_keys)
        .collect();
    let mut pending: Vec<usize> = (0..clauses.len())
        .filter(|index| relevant[*index])
        .collect();
    let mut reached = HashSet::new();
    while let Some(index) = pending.pop() {
        for key in &keys[index] {
            if !reached.insert(*key) {
                continue;
            }
            for other in &holding[key] {
                if !relevant[*other] {
                    relevant[*other] = true;
                    pending.push(*other);
                }
            }
        }
    }
    relevant
}

/// The symbols and applications within a term, itself included.
fn subterms(terms: &Terms, term: TermId, seen: &mut HashSet<TermId>) {
    if matches!(terms.get(term), Term::Lit(_)) || !seen.insert(term) {
        return;
    }
    for part in terms.get(term).parts() {
        subterms(terms, part, seen);
    }
}

/// Copies terms into a new store, numbering fresh, skolem and bound symbols in order of first
/// occurrence.
struct Transfer<'t> {
    from: &'t Terms,
    to: Terms,
    map: HashMap<TermId, TermId>,
}

impl Transfer<'_> {
    fn clause(&mut self, clause: &Clause) -> Clause {
        clause
            .iter()
            .map(|literal| Literal {
                negated: literal.negated,
                atom: match &literal.atom {
                    Atom::Le(linear) => Atom::Le(self.linear(linear)),
                    Atom::Eq(linear) => Atom::Eq(self.linear(linear)),
                    Atom::Bool(term) => Atom::Bool(self.term(*term)),
                    Atom::All(forall) => Atom::All(Rc::new(Forall {
                        low: self.term(forall.low),
                        high: self.term(forall.high),
                        inclusive: forall.inclusive,
                        bound: self.term(forall.bound),
                        body: self.term(forall.body),
                    })),
                },
            })
            .collect()
    }

    fn linear(&mut self, linear: &Linear) -> Linear {
        Linear {
            terms: linear
                .terms
                .iter()
                .map(|(coefficient, term)| (*coefficient, self.term(*term)))
                .collect(),
            constant: linear.constant,
        }
    }

    fn term(&mut self, term: TermId) -> TermId {
        if let Some(moved) = self.map.get(&term) {
            return *moved;
        }
        let ty = self.from.ty(term).clone();
        let moved = match self.from.get(term) {
            Term::Fresh(_) => self.to.fresh(ty),
            Term::Skolem(_) => self.to.skolem(ty),
            Term::Bound(_) => self.to.bound(ty),
            shape => {
                let parts: Vec<TermId> =
                    shape.parts().iter().map(|part| self.term(*part)).collect();
                self.to.intern(shape.with_parts(&parts), ty)
            }
        };
        self.map.insert(term, moved);
        moved
    }
}
