//! Terms: the values of a query, each interned once with its type, so that identical terms are
//! one term.

use std::collections::HashMap;

use crate::check::ty::{Scalar, Ty};
use crate::nir::{BinaryOp, Constant, FnRef, Literal, UnaryOp};

pub type TermId = u32;

/// An instruction a term applies: an operator, or an instruction whose result the prover knows
/// only as its application.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Op {
    Unary(UnaryOp),
    Binary(BinaryOp),
    /// A struct value from its fields in declaration order.
    Construct(Ty),
    /// An enum value of a variant, by its index, from its payload fields.
    Variant(Ty, u32),
    Constant(Constant),
    FnRef(FnRef),
    Closure(u32),
    Convert(Ty, Option<String>),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Term {
    /// A parameter of the function, by its value number.
    Param(u32),
    /// The content at entry of the place a reference parameter designates, by the parameter's
    /// value number.
    Entry(u32),
    Lit(Literal),
    App(Op, Vec<TermId>),
    Call(FnRef, Vec<TermId>),
    Field(TermId, String),
    Ite(TermId, TermId, TermId),
    Quot(TermId, TermId),
    Rem(TermId, TermId),
    Tag(TermId),
    Fresh(u32),
    Skolem(u32),
    Bound(u32),
    /// The connectives of formulas, which canonicalisation removes.
    And(Vec<TermId>),
    Or(Vec<TermId>),
    Not(TermId),
    /// `all` or `any` over a range, its body the formula its region yields for the bound symbol.
    Quant {
        all: bool,
        low: TermId,
        high: TermId,
        inclusive: bool,
        bound: TermId,
        body: TermId,
    },
}

impl Term {
    /// The terms it applies its operation to, in written order.
    pub fn parts(&self) -> Vec<TermId> {
        match self {
            Term::App(_, parts) | Term::Call(_, parts) | Term::And(parts) | Term::Or(parts) => {
                parts.clone()
            }
            Term::Field(part, _) | Term::Tag(part) | Term::Not(part) => vec![*part],
            Term::Ite(condition, then, otherwise) => vec![*condition, *then, *otherwise],
            Term::Quot(left, right) | Term::Rem(left, right) => vec![*left, *right],
            Term::Quant {
                low,
                high,
                bound,
                body,
                ..
            } => vec![*low, *high, *bound, *body],
            _ => Vec::new(),
        }
    }

    /// The term of the same form over other parts, given in the order `parts` lists them.
    pub fn with_parts(&self, parts: &[TermId]) -> Term {
        match self {
            Term::App(op, _) => Term::App(op.clone(), parts.to_vec()),
            Term::Call(callee, _) => Term::Call(callee.clone(), parts.to_vec()),
            Term::And(_) => Term::And(parts.to_vec()),
            Term::Or(_) => Term::Or(parts.to_vec()),
            Term::Field(_, step) => Term::Field(parts[0], step.clone()),
            Term::Tag(_) => Term::Tag(parts[0]),
            Term::Not(_) => Term::Not(parts[0]),
            Term::Ite(..) => Term::Ite(parts[0], parts[1], parts[2]),
            Term::Quot(..) => Term::Quot(parts[0], parts[1]),
            Term::Rem(..) => Term::Rem(parts[0], parts[1]),
            Term::Quant { all, inclusive, .. } => Term::Quant {
                all: *all,
                low: parts[0],
                high: parts[1],
                inclusive: *inclusive,
                bound: parts[2],
                body: parts[3],
            },
            other => other.clone(),
        }
    }
}

/// The sort of a term.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    Int,
    Bool,
    Other,
}

#[derive(Clone, Default)]
pub struct Terms {
    terms: Vec<Term>,
    types: Vec<Ty>,
    index: HashMap<Term, TermId>,
    /// The next fresh, skolem and bound symbol number.
    symbols: u32,
}

impl Terms {
    pub fn intern(&mut self, term: Term, ty: Ty) -> TermId {
        if let Some(id) = self.index.get(&term) {
            return *id;
        }
        self.terms.push(term.clone());
        self.types.push(ty);
        let id = self.terms.len() as TermId - 1;
        self.index.insert(term, id);
        id
    }

    /// The term of this form, if the store holds it.
    pub fn lookup(&self, term: &Term) -> Option<TermId> {
        self.index.get(term).copied()
    }

    pub fn get(&self, id: TermId) -> &Term {
        &self.terms[id as usize]
    }

    pub fn ty(&self, id: TermId) -> &Ty {
        &self.types[id as usize]
    }

    pub fn len(&self) -> usize {
        self.terms.len()
    }

    pub fn sort(&self, id: TermId) -> Sort {
        match self.get(id) {
            Term::Tag(_) | Term::Quot(..) | Term::Rem(..) => Sort::Int,
            Term::Quant { .. } | Term::And(_) | Term::Or(_) | Term::Not(_) => Sort::Bool,
            _ => sort_of(self.ty(id)),
        }
    }

    pub fn fresh(&mut self, ty: Ty) -> TermId {
        self.symbols += 1;
        self.intern(Term::Fresh(self.symbols - 1), ty)
    }

    pub fn skolem(&mut self, ty: Ty) -> TermId {
        self.symbols += 1;
        self.intern(Term::Skolem(self.symbols - 1), ty)
    }

    pub fn bound(&mut self, ty: Ty) -> TermId {
        self.symbols += 1;
        self.intern(Term::Bound(self.symbols - 1), ty)
    }

    pub fn literal(&mut self, literal: Literal) -> TermId {
        let ty = match &literal {
            Literal::Int(_, scalar) | Literal::Float(_, scalar) => Ty::Scalar(*scalar),
            Literal::Bool(_) => Ty::Scalar(Scalar::Bool),
            Literal::Char(_) => Ty::Scalar(Scalar::Char),
            _ => Ty::Unit,
        };
        self.intern(Term::Lit(literal), ty)
    }

    pub fn bool(&mut self, value: bool) -> TermId {
        self.literal(Literal::Bool(value))
    }

    pub fn int(&mut self, value: i128, scalar: Scalar) -> TermId {
        self.literal(Literal::Int(value, scalar))
    }

    pub fn compare(&mut self, op: BinaryOp, left: TermId, right: TermId) -> TermId {
        self.intern(
            Term::App(Op::Binary(op), vec![left, right]),
            Ty::Scalar(Scalar::Bool),
        )
    }

    /// The conjunction of formulas, without their `true` operands.
    pub fn and(&mut self, parts: Vec<TermId>) -> TermId {
        let parts: Vec<TermId> = parts
            .into_iter()
            .filter(|part| self.as_bool(*part) != Some(true))
            .collect();
        if parts.iter().any(|part| self.as_bool(*part) == Some(false)) {
            return self.bool(false);
        }
        match parts.len() {
            0 => self.bool(true),
            1 => parts[0],
            _ => self.intern(Term::And(parts), Ty::Scalar(Scalar::Bool)),
        }
    }

    /// The disjunction of formulas, without their `false` operands.
    pub fn or(&mut self, parts: Vec<TermId>) -> TermId {
        let parts: Vec<TermId> = parts
            .into_iter()
            .filter(|part| self.as_bool(*part) != Some(false))
            .collect();
        if parts.iter().any(|part| self.as_bool(*part) == Some(true)) {
            return self.bool(true);
        }
        match parts.len() {
            0 => self.bool(false),
            1 => parts[0],
            _ => self.intern(Term::Or(parts), Ty::Scalar(Scalar::Bool)),
        }
    }

    pub fn not(&mut self, part: TermId) -> TermId {
        match (self.as_bool(part), self.get(part)) {
            (Some(value), _) => self.bool(!value),
            (None, Term::Not(inner)) => *inner,
            _ => self.intern(Term::Not(part), Ty::Scalar(Scalar::Bool)),
        }
    }

    /// `guard ⇒ part`.
    pub fn implies(&mut self, guard: TermId, part: TermId) -> TermId {
        let negated = self.not(guard);
        self.or(vec![negated, part])
    }

    /// `ite(c, a, b)`, or the arm a literal condition selects, or the one term both arms are.
    pub fn ite(&mut self, condition: TermId, then: TermId, otherwise: TermId) -> TermId {
        match self.as_bool(condition) {
            Some(true) => then,
            Some(false) => otherwise,
            None if then == otherwise => then,
            None => {
                let ty = self.ty(then).clone();
                self.intern(Term::Ite(condition, then, otherwise), ty)
            }
        }
    }

    /// A term of the same form as `id` over other parts.
    pub fn rebuild(&mut self, id: TermId, parts: &[TermId]) -> TermId {
        let term = self.get(id).clone();
        match term {
            Term::And(_) => self.and(parts.to_vec()),
            Term::Or(_) => self.or(parts.to_vec()),
            Term::Not(_) => self.not(parts[0]),
            Term::Ite(..) => self.ite(parts[0], parts[1], parts[2]),
            _ => {
                let ty = self.ty(id).clone();
                self.intern(term.with_parts(parts), ty)
            }
        }
    }

    /// The term with `to` in place of each occurrence of the symbol `from`.
    pub fn substitute(&mut self, id: TermId, from: TermId, to: TermId) -> TermId {
        let mut done = HashMap::from([(from, to)]);
        self.replaced(id, &mut done)
    }

    fn replaced(&mut self, id: TermId, done: &mut HashMap<TermId, TermId>) -> TermId {
        if let Some(replaced) = done.get(&id) {
            return *replaced;
        }
        let parts = self.get(id).parts();
        let replaced = match parts.is_empty() {
            true => id,
            false => {
                let new: Vec<TermId> = parts
                    .iter()
                    .map(|part| self.replaced(*part, done))
                    .collect();
                match new == parts {
                    true => id,
                    false => self.rebuild(id, &new),
                }
            }
        };
        done.insert(id, replaced);
        replaced
    }

    /// The literal boolean a term is, if it is one.
    pub fn as_bool(&self, id: TermId) -> Option<bool> {
        match self.get(id) {
            Term::Lit(Literal::Bool(value)) => Some(*value),
            _ => None,
        }
    }

    /// The literal integer a term is, if it is one.
    pub fn as_int(&self, id: TermId) -> Option<i128> {
        match self.get(id) {
            Term::Lit(Literal::Int(value, _)) => Some(*value),
            Term::Lit(Literal::Char(c)) => Some(i128::from(u32::from(*c))),
            _ => None,
        }
    }
}

/// The sort of a type's values: integers for integer scalars and `char`, booleans for `bool`.
pub fn sort_of(ty: &Ty) -> Sort {
    match ty {
        Ty::Scalar(Scalar::Bool) => Sort::Bool,
        Ty::Scalar(scalar) if scalar.is_integer() || *scalar == Scalar::Char => Sort::Int,
        _ => Sort::Other,
    }
}
