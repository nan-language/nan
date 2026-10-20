//! Refutation: the candidate an open state gives, its entry terms fixed in term order to the
//! least values their classes allow, and the parameter values it builds for the interpreter's
//! confirmation run.

use std::collections::HashMap;

use super::canon::{Atom, Linear, Literal};
use super::solve::{Solver, State};
use super::term::{Sort, Term, TermId};
use crate::check::Checker;
use crate::check::db::ModuleId;
use crate::check::sig::{Payload, SigKind};
use crate::check::ty::{Arg, Const, Scalar, Subst, Ty};
use crate::interp::{Input, Val};
use crate::nir::{Callable, FnRef};

/// The `len` and `at` functions whose applications to inputs are inputs.
const LENGTHS: [&str; 3] = ["core::Slice.len", "core::SliceMut.len", "core::Array.len"];
const ELEMENTS: [&str; 3] = ["core::Slice.at", "core::SliceMut.at", "core::Array.at"];

/// Fixes the open state's entry terms, the integer and boolean inputs among its terms, each
/// in term order to the least value its class allows, propagating after each; false when a
/// fixing ends in a contradiction or the fuel runs out.
pub fn fix(solver: &mut Solver, state: &mut State) -> bool {
    let mut inputs: HashMap<TermId, bool> = HashMap::new();
    let mut entries: Vec<TermId> = (0..state.terms.len() as TermId)
        .filter(|term| state.is_registered(*term))
        .filter(|term| matches!(state.terms.sort(*term), Sort::Int | Sort::Bool))
        .filter(|term| is_input(solver.ck, state, *term, &mut inputs))
        .collect();
    entries.sort_by_cached_key(|term| state.encodings.term(solver.ck, &state.terms, *term));
    for term in entries {
        if solver.step().is_err() {
            return false;
        }
        let fixed = match state.terms.sort(term) {
            Sort::Bool => {
                let value = state.truth_of(term) == Some(true);
                solver.assert_term(state, term, value)
            }
            _ => {
                let Some(value) = least(state, term) else {
                    return false;
                };
                solver.assert(
                    state,
                    &Literal {
                        negated: false,
                        atom: Atom::Eq(Linear {
                            terms: vec![(1, term)],
                            constant: value,
                        }),
                    },
                )
            }
        };
        if fixed.is_err() || solver.propagate(state).is_err() {
            return false;
        }
    }
    true
}

/// The least value of a term's interval that satisfies its congruence and that no
/// disequality excludes.
fn least(state: &State, term: TermId) -> Option<i128> {
    let class = state.class(term);
    let floor = match state.terms.ty(term) {
        Ty::Scalar(scalar) if scalar.is_integer() => scalar.range().0,
        _ => 0,
    };
    let mut value = class.low.unwrap_or(floor);
    if class.modulus >= 2 {
        value += (class.residue - value).rem_euclid(class.modulus);
    }
    while state.excluded(term, value) {
        value = value.checked_add(class.modulus)?;
    }
    (class.high.is_none_or(|high| value <= high)).then_some(value)
}

/// Whether a term is an input: a parameter, the content of a parameter's place at entry, a
/// const parameter, or a field, payload field, tag, length or element of an input.
fn is_input(ck: &Checker, state: &State, term: TermId, known: &mut HashMap<TermId, bool>) -> bool {
    if let Some(input) = known.get(&term) {
        return *input;
    }
    let input = match state.terms.get(term).clone() {
        Term::Param(_) | Term::Entry(_) => true,
        Term::App(super::term::Op::Constant(crate::nir::Constant::Param(_)), _) => true,
        Term::Field(part, _) | Term::Tag(part) => is_input(ck, state, part, known),
        Term::Call(callee, parts) => match named(ck, &callee) {
            Some(path) if LENGTHS.contains(&path.as_str()) => is_input(ck, state, parts[0], known),
            Some(path) if ELEMENTS.contains(&path.as_str()) => {
                is_input(ck, state, parts[0], known)
                    && (state.terms.as_int(parts[1]).is_some()
                        || is_input(ck, state, parts[1], known))
            }
            _ => false,
        },
        _ => false,
    };
    known.insert(term, input);
    input
}

fn named(ck: &Checker, callee: &FnRef) -> Option<String> {
    match &callee.callable {
        Callable::Item { item, .. } => Some(ck.db.canonical(*item)),
        _ => None,
    }
}

/// Builds the values of a candidate from a fixed state.
pub struct Builder<'b, 'c> {
    pub ck: &'b Checker<'c>,
    pub state: &'b State,
    /// The module of the code judged, which must be able to construct each struct built.
    pub module: ModuleId,
    lengths: HashMap<TermId, TermId>,
    elements: HashMap<TermId, Vec<(TermId, TermId)>>,
}

impl<'b, 'c> Builder<'b, 'c> {
    pub fn new(ck: &'b Checker<'c>, state: &'b State, module: ModuleId) -> Builder<'b, 'c> {
        let mut lengths = HashMap::new();
        let mut elements: HashMap<TermId, Vec<(TermId, TermId)>> = HashMap::new();
        for term in 0..state.terms.len() as TermId {
            if !state.is_registered(term) {
                continue;
            }
            if let Term::Call(callee, parts) = state.terms.get(term)
                && let Some(path) = named(ck, callee)
            {
                if LENGTHS.contains(&path.as_str()) {
                    lengths.insert(parts[0], term);
                } else if ELEMENTS.contains(&path.as_str()) {
                    elements.entry(parts[0]).or_default().push((parts[1], term));
                }
            }
        }
        Builder {
            ck,
            state,
            module,
            lengths,
            elements,
        }
    }

    /// The value of a term of a type: from its fixed class and those of its parts, a part no
    /// fixed term determines taking the least value of its type; none for a type a candidate
    /// cannot hold.
    pub fn value(&self, term: Option<TermId>, ty: &Ty) -> Option<Input> {
        let term = term.filter(|term| self.state.is_registered(*term));
        match ty {
            Ty::Scalar(scalar) => Some(Input::Value(self.scalar(term, *scalar))),
            Ty::Unit => Some(Input::Value(Val::Unit)),
            Ty::Item(item, args) => {
                let path = self.ck.db.canonical(*item);
                let element = || args.first().and_then(Arg::ty).cloned();
                match path.as_str() {
                    "core::Slice" | "core::SliceMut" => {
                        let len = term
                            .and_then(|term| self.lengths.get(&term))
                            .and_then(|len| self.state.class(*len).single())
                            .unwrap_or(0)
                            .max(0) as usize;
                        let elements = self.elements(term, len, &element()?)?;
                        Some(Input::Slice(elements, path == "core::SliceMut"))
                    }
                    "core::Array" => {
                        let len = match args.get(1) {
                            Some(Arg::Const(Const::Int(len))) => *len as usize,
                            _ => return None,
                        };
                        Some(Input::Array(self.elements(term, len, &element()?)?))
                    }
                    _ => self.composite(term, *item, args),
                }
            }
            _ => None,
        }
    }

    fn scalar(&self, term: Option<TermId>, scalar: Scalar) -> Val {
        let fixed = term.and_then(|term| self.state.class(term).single());
        match scalar {
            Scalar::Bool => {
                Val::Bool(term.and_then(|term| self.state.truth_of(term)) == Some(true))
            }
            Scalar::Char => Val::Char(
                fixed
                    .and_then(|value| char::from_u32(value as u32))
                    .unwrap_or('\0'),
            ),
            Scalar::F32 => Val::F32(0.0),
            Scalar::F64 => Val::F64(0.0),
            _ => Val::Int(fixed.unwrap_or(0)),
        }
    }

    /// The elements below `len` of a slice or array term, each from the `at` application at
    /// its index.
    fn elements(&self, term: Option<TermId>, len: usize, element: &Ty) -> Option<Vec<Input>> {
        let applications = term.and_then(|term| self.elements.get(&term));
        (0..len)
            .map(|index| {
                let at = applications.and_then(|applications| {
                    applications
                        .iter()
                        .find(|(position, _)| {
                            self.state.class(*position).single() == Some(index as i128)
                        })
                        .map(|(_, at)| *at)
                });
                self.value(at, element)
            })
            .collect()
    }

    /// A struct the judged module may construct, from its fields, or an enum value, its
    /// variant from its tag and its payload from its payload fields; none for a struct that is a
    /// base type, whose representation no field shows.
    fn composite(
        &self,
        term: Option<TermId>,
        item: crate::check::db::ItemId,
        args: &[Arg],
    ) -> Option<Input> {
        let sig = self.ck.sig(item);
        let subst = Subst::of(sig.generics.iter().map(|def| def.param), args);
        let field = |name: &str| {
            term.and_then(|term| self.state.terms.lookup(&Term::Field(term, name.to_owned())))
        };
        match &sig.kind {
            SigKind::Struct { .. } if self.ck.is_base_type(item) => None,
            SigKind::Struct { fields } => {
                if !fields.iter().all(|field| {
                    self.ck
                        .db
                        .field_visible(self.module, item, field.visibility)
                }) {
                    return None;
                }
                let values = fields
                    .iter()
                    .map(|declared| self.value(field(&declared.name), &subst.ty(&declared.ty)))
                    .collect::<Option<Vec<Input>>>()?;
                Some(Input::Struct(item, values))
            }
            SigKind::Enum { variants } => {
                let tag = term
                    .and_then(|term| self.state.terms.lookup(&Term::Tag(term)))
                    .and_then(|tag| self.state.class(tag).single())
                    .filter(|tag| (0..variants.len() as i128).contains(tag))
                    .unwrap_or(0) as usize;
                let variant = &variants[tag];
                let payload_term = field(&variant.name);
                let payload = match &variant.payload {
                    Payload::None => Vec::new(),
                    Payload::Single(ty) => vec![self.value(payload_term, &subst.ty(ty))?],
                    Payload::Labelled(fields) => fields
                        .iter()
                        .map(|(name, ty)| {
                            let part = payload_term.and_then(|payload| {
                                self.state.terms.lookup(&Term::Field(payload, name.clone()))
                            });
                            self.value(part, &subst.ty(ty))
                        })
                        .collect::<Option<Vec<Input>>>()?,
                };
                Some(Input::Variant(item, tag as u32, payload))
            }
            _ => None,
        }
    }
}

/// A candidate value as a labelled NaN literal.
pub fn literal(ck: &Checker, input: &Input) -> String {
    match input {
        Input::Value(value) => match value {
            Val::Int(value) => value.to_string(),
            Val::Bool(value) => value.to_string(),
            Val::Char(value) => crate::format::normal::char_literal(*value),
            Val::F32(value) => format!("{value:?}"),
            Val::F64(value) => format!("{value:?}"),
            Val::Unit => "Unit".to_owned(),
            other => format!("{other:?}"),
        },
        Input::Struct(item, fields) => {
            let SigKind::Struct { fields: declared } = &ck.sig(*item).kind else {
                return String::new();
            };
            let parts: Vec<String> = declared
                .iter()
                .zip(fields)
                .map(|(field, value)| format!("{}: {}", field.name, literal(ck, value)))
                .collect();
            format!("{}({})", type_name(ck, *item), parts.join(", "))
        }
        Input::Variant(item, index, payload) => {
            let SigKind::Enum { variants } = &ck.sig(*item).kind else {
                return String::new();
            };
            let variant = &variants[*index as usize];
            let name = match ck.db.canonical(*item).as_str() {
                "core::Option" | "core::Result" => variant.name.clone(),
                _ => format!("{}.{}", type_name(ck, *item), variant.name),
            };
            let parts: Vec<String> = match &variant.payload {
                Payload::Labelled(fields) => fields
                    .iter()
                    .zip(payload)
                    .map(|((label, _), value)| format!("{label}: {}", literal(ck, value)))
                    .collect(),
                _ => payload.iter().map(|value| literal(ck, value)).collect(),
            };
            match parts.is_empty() {
                true => name,
                false => format!("{name}({})", parts.join(", ")),
            }
        }
        Input::Array(elements) | Input::Slice(elements, _) => {
            let parts: Vec<String> = elements.iter().map(|value| literal(ck, value)).collect();
            format!("[{}]", parts.join(", "))
        }
    }
}

fn type_name(ck: &Checker, item: crate::check::db::ItemId) -> String {
    let path = ck.db.canonical(item);
    path.rsplit("::").next().unwrap_or(&path).to_owned()
}
