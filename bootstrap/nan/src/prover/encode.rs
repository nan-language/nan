//! The canonical encoding of query terms, literals and clauses, whose byte order is the term
//! order.

use std::rc::Rc;

use super::canon::{Atom, Clause, Literal};
use super::term::{Op, Term, TermId, Terms};
use crate::check::Checker;
use crate::check::sig::SigKind;
use crate::check::ty::{Arg, Const, Scalar, Ty};
use crate::encoding::bytes;
use crate::nir::{BinaryOp, Builtin, Callable, Constant, FnRef, Literal as Value, UnaryOp};

/// The opcodes of the query forms, after those of NIR's instructions and terminators.
const ITE: u64 = 49;
const QUOTIENT: u64 = 50;
const REMAINDER: u64 = 51;
const TAG: u64 = 52;
const FRESH: u64 = 53;
const SKOLEM: u64 = 54;
const BOUND: u64 = 55;
/// The connectives a term may hold as an operand, which no instruction names.
const AND: u64 = 56;
const OR: u64 = 57;

/// The encodings of a store's terms, each computed once.
#[derive(Clone, Default)]
pub struct Encodings(Vec<Option<Rc<[u8]>>>);

impl Encodings {
    pub fn term(&mut self, ck: &Checker, terms: &Terms, id: TermId) -> Rc<[u8]> {
        if let Some(Some(done)) = self.0.get(id as usize) {
            return done.clone();
        }
        let mut out = Vec::new();
        self.write_term(ck, terms, id, &mut out);
        let done: Rc<[u8]> = out.into();
        if self.0.len() <= id as usize {
            self.0.resize(id as usize + 1, None);
        }
        self.0[id as usize] = Some(done.clone());
        done
    }

    pub fn literal(&mut self, ck: &Checker, terms: &Terms, literal: &Literal) -> Vec<u8> {
        let mut out = Vec::new();
        uvar(&mut out, u64::from(literal.negated));
        match &literal.atom {
            Atom::Le(linear) | Atom::Eq(linear) => {
                uvar(&mut out, u64::from(matches!(literal.atom, Atom::Eq(_))));
                uvar(&mut out, linear.terms.len() as u64);
                for (coefficient, term) in &linear.terms {
                    svar(&mut out, *coefficient);
                    out.extend_from_slice(&self.term(ck, terms, *term));
                }
                svar(&mut out, linear.constant);
            }
            Atom::Bool(term) => {
                uvar(&mut out, 2u64);
                out.extend_from_slice(&self.term(ck, terms, *term));
            }
            Atom::All(forall) => {
                uvar(&mut out, 3u64);
                out.extend_from_slice(&self.term(ck, terms, forall.low));
                out.extend_from_slice(&self.term(ck, terms, forall.high));
                uvar(&mut out, u64::from(forall.inclusive));
                out.extend_from_slice(&self.term(ck, terms, forall.bound));
                out.extend_from_slice(&self.term(ck, terms, forall.body));
            }
        }
        out
    }

    pub fn clause(&mut self, ck: &Checker, terms: &Terms, clause: &Clause) -> Vec<u8> {
        let mut out = Vec::new();
        uvar(&mut out, clause.len() as u64);
        for literal in clause {
            out.extend(self.literal(ck, terms, literal));
        }
        out
    }

    fn write_term(&mut self, ck: &Checker, terms: &Terms, id: TermId, out: &mut Vec<u8>) {
        let parts = |encodings: &mut Encodings, out: &mut Vec<u8>, parts: &[TermId]| {
            uvar(out, parts.len() as u64);
            for part in parts {
                out.extend_from_slice(&encodings.term(ck, terms, *part));
            }
        };
        match terms.get(id) {
            Term::Param(value) => {
                uvar(out, 0u64);
                uvar(out, *value);
            }
            Term::Entry(value) => {
                uvar(out, 1u64);
                uvar(out, *value);
            }
            Term::Lit(value) => {
                uvar(out, 2u64);
                literal(out, value);
            }
            Term::App(op, operands) => {
                uvar(out, 3u64);
                operation(ck, out, op);
                parts(self, out, operands);
            }
            Term::Call(callee, operands) => {
                uvar(out, 4u64);
                uvar(out, 33u64);
                fn_ref(ck, out, callee);
                parts(self, out, operands);
            }
            Term::Field(part, step) => {
                uvar(out, 5u64);
                out.extend_from_slice(&self.term(ck, terms, *part));
                bytes(out, step.as_bytes());
            }
            Term::Ite(..) | Term::Quot(..) | Term::Rem(..) | Term::Tag(_) => {
                uvar(out, 6u64);
                uvar(
                    out,
                    match terms.get(id) {
                        Term::Ite(..) => ITE,
                        Term::Quot(..) => QUOTIENT,
                        Term::Rem(..) => REMAINDER,
                        _ => TAG,
                    },
                );
                for part in terms.get(id).parts() {
                    out.extend_from_slice(&self.term(ck, terms, part));
                }
            }
            Term::Fresh(number) | Term::Skolem(number) | Term::Bound(number) => {
                uvar(out, 7u64);
                uvar(
                    out,
                    match terms.get(id) {
                        Term::Fresh(_) => FRESH,
                        Term::Skolem(_) => SKOLEM,
                        _ => BOUND,
                    },
                );
                uvar(out, *number);
            }
            Term::And(operands) | Term::Or(operands) => {
                uvar(out, 3u64);
                uvar(
                    out,
                    match terms.get(id) {
                        Term::And(_) => AND,
                        _ => OR,
                    },
                );
                parts(self, out, operands);
            }
            Term::Not(part) => {
                uvar(out, 3u64);
                uvar(out, 32u64);
                parts(self, out, &[*part]);
            }
            Term::Quant {
                all,
                low,
                high,
                inclusive,
                bound,
                body,
            } => {
                uvar(out, 3u64);
                uvar(out, if *all { 38u64 } else { 39 });
                for part in [low, high] {
                    out.extend_from_slice(&self.term(ck, terms, *part));
                }
                uvar(out, u64::from(*inclusive));
                for part in [bound, body] {
                    out.extend_from_slice(&self.term(ck, terms, *part));
                }
            }
        }
    }
}

fn uvar(out: &mut Vec<u8>, value: impl Into<u128>) {
    crate::encoding::uvar(out, value.into());
}

/// A signed integer as the uvar 2n, or −2n − 1 when negative.
fn svar(out: &mut Vec<u8>, value: i128) {
    let folded = match value >= 0 {
        true => (value as u128) << 1,
        false => ((-(value + 1)) as u128) << 1 | 1,
    };
    uvar(out, folded);
}

/// An instruction's opcode and its parts other than operands.
fn operation(ck: &Checker, out: &mut Vec<u8>, op: &Op) {
    match op {
        Op::Unary(op) => uvar(
            out,
            match op {
                UnaryOp::Neg => 14u64,
                UnaryOp::Inv => 25,
                UnaryOp::Not => 32,
            },
        ),
        Op::Binary(op) => uvar(
            out,
            match op {
                BinaryOp::Add => 15u64,
                BinaryOp::Sub => 16,
                BinaryOp::Mul => 17,
                BinaryOp::Div => 18,
                BinaryOp::Rem => 19,
                BinaryOp::Shl => 20,
                BinaryOp::Shr => 21,
                BinaryOp::Band => 22,
                BinaryOp::Bor => 23,
                BinaryOp::Bxor => 24,
                BinaryOp::Eq => 26,
                BinaryOp::Ne => 27,
                BinaryOp::Lt => 28,
                BinaryOp::Le => 29,
                BinaryOp::Gt => 30,
                BinaryOp::Ge => 31,
            },
        ),
        Op::Construct(ty) => {
            uvar(out, 9u64);
            write_ty(ck, out, ty);
        }
        Op::Variant(ty, variant) => {
            uvar(out, 10u64);
            write_ty(ck, out, ty);
            uvar(out, *variant);
        }
        Op::Constant(constant) => {
            uvar(out, 8u64);
            match constant {
                Constant::Item(item) => {
                    uvar(out, 0u64);
                    bytes(out, ck.db.canonical(*item).as_bytes());
                }
                Constant::Param(param) => {
                    uvar(out, 1u64);
                    uvar(out, param.index);
                }
                Constant::Register(block, register) => {
                    uvar(out, 2u64);
                    bytes(out, ck.db.canonical(*block).as_bytes());
                    uvar(out, *register);
                }
            }
        }
        Op::FnRef(callee) => {
            uvar(out, 7u64);
            fn_ref(ck, out, callee);
        }
        Op::Closure(index) => {
            uvar(out, 12u64);
            uvar(out, *index);
        }
        Op::Convert(ty, via) => {
            uvar(out, 13u64);
            write_ty(ck, out, ty);
            match via {
                Some(via) => {
                    uvar(out, 1u64);
                    bytes(out, via.as_bytes());
                }
                None => uvar(out, 0u64),
            }
        }
    }
}

/// A function reference: its path, role and index, and type arguments.
fn fn_ref(ck: &Checker, out: &mut Vec<u8>, callee: &FnRef) {
    let (path, role, index) = match &callee.callable {
        Callable::Item { item, role, index } => (ck.db.canonical(*item), *role as u64, *index),
        Callable::Method { interface, method } => {
            let name = match &ck.sig(*interface).kind {
                SigKind::Interface { methods } => methods[*method as usize].name.clone(),
                _ => method.to_string(),
            };
            (format!("{}.{name}", ck.db.canonical(*interface)), 0, 0)
        }
        Callable::Derived { owner, method } => {
            (format!("{}.{method}", ck.db.canonical(*owner)), 0, 0)
        }
        Callable::Builtin(Builtin::Conversion(conversion, scalar)) => {
            (format!("{}.{}", scalar.name(), conversion.word()), 0, 0)
        }
        Callable::Builtin(Builtin::Bits(conversion, owner)) => {
            let owner = match owner {
                crate::nir::BitsOwner::Scalar(scalar) => scalar.name().to_owned(),
                crate::nir::BitsOwner::Layout(item) => ck.db.canonical(*item),
            };
            (format!("{owner}.{}", conversion.word()), 0, 0)
        }
        Callable::Builtin(Builtin::Intrinsic(path)) => ((*path).to_owned(), 0, 0),
    };
    bytes(out, path.as_bytes());
    uvar(out, role);
    uvar(out, index);
    args(ck, out, &callee.args);
}

fn args(ck: &Checker, out: &mut Vec<u8>, args: &[Arg]) {
    uvar(out, args.len() as u64);
    for arg in args {
        match arg {
            Arg::Ty(ty) => write_ty(ck, out, ty),
            Arg::Const(constant) => {
                uvar(out, 5u64);
                match constant {
                    Const::Int(value) => svar(out, *value),
                    Const::Bool(value) => uvar(out, u64::from(*value)),
                    Const::Param(param) => uvar(out, param.index),
                    Const::Error => {}
                }
            }
        }
    }
}

fn write_ty(ck: &Checker, out: &mut Vec<u8>, ty: &Ty) {
    match ty {
        Ty::Scalar(scalar) => {
            uvar(out, 0u64);
            uvar(out, table_position(*scalar));
        }
        Ty::Unit => uvar(out, 1u64),
        Ty::Never => uvar(out, 2u64),
        Ty::Item(item, type_args) => {
            uvar(out, 3u64);
            bytes(out, ck.db.canonical(*item).as_bytes());
            args(ck, out, type_args);
        }
        Ty::Param(param) => {
            uvar(out, 4u64);
            uvar(out, param.index);
        }
        Ty::Fn(fn_ty) => {
            uvar(out, 6u64);
            uvar(out, fn_ty.params.len() as u64);
            for param in &fn_ty.params {
                write_ty(ck, out, &param.ty);
            }
            write_ty(ck, out, &fn_ty.result);
        }
        Ty::Dyn(bound) => {
            uvar(out, 9u64);
            bytes(out, ck.db.canonical(bound.interface).as_bytes());
            args(ck, out, &bound.args);
        }
        Ty::Registers(item) => {
            uvar(out, 10u64);
            bytes(out, ck.db.canonical(*item).as_bytes());
        }
        Ty::Error => uvar(out, 11u64),
    }
}

/// A literal: its type, then its value.
fn literal(out: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Int(value, scalar) => {
            uvar(out, 0u64);
            uvar(out, table_position(*scalar));
            match scalar.is_signed() {
                true => svar(out, *value),
                false => uvar(out, *value as u128),
            }
        }
        Value::Float(bits, scalar) => {
            uvar(out, 0u64);
            uvar(out, table_position(*scalar));
            let width = if *scalar == Scalar::F32 { 4 } else { 8 };
            out.extend_from_slice(&bits.to_le_bytes()[..width]);
        }
        Value::Bool(value) => {
            uvar(out, 0u64);
            uvar(out, table_position(Scalar::Bool));
            uvar(out, u64::from(*value));
        }
        Value::Char(value) => {
            uvar(out, 0u64);
            uvar(out, table_position(Scalar::Char));
            uvar(out, u32::from(*value));
        }
        Value::Text(text) => {
            uvar(out, 3u64);
            bytes(out, b"core::Str");
            uvar(out, 0u64);
            bytes(out, text.as_bytes());
        }
        Value::Bytes(data) => {
            uvar(out, 3u64);
            bytes(out, b"core::Slice");
            uvar(out, 1u64);
            uvar(out, 0u64);
            uvar(out, table_position(Scalar::U8));
            bytes(out, data);
        }
        Value::Unit => uvar(out, 1u64),
    }
}

/// A scalar's position in the scalar table.
fn table_position(scalar: Scalar) -> u64 {
    match scalar {
        Scalar::U8 => 0,
        Scalar::U16 => 1,
        Scalar::U32 => 2,
        Scalar::U64 => 3,
        Scalar::I8 => 4,
        Scalar::I16 => 5,
        Scalar::I32 => 6,
        Scalar::I64 => 7,
        Scalar::Usize => 8,
        Scalar::Isize => 9,
        Scalar::F32 => 10,
        Scalar::F64 => 11,
        Scalar::Bool => 12,
        Scalar::Char => 13,
    }
}
