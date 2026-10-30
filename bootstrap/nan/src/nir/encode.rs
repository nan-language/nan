//! The canonical NIR encoding of functions, types, literals and const values, naming items by
//! canonical path and generic parameters by their position among the unit's.

use super::{
    Block, Builtin, Call, Callable, Callee, CheckKind, Constant, FnRef, Function, Inst, Jump,
    Literal, NTy, Op, Operand, Payload, Place, Region, Terminator, UnaryOp,
};
use crate::check::db::ItemId;
use crate::check::ty::{Arg, Bound, Const, FnTy, LimitSet, Param, Scalar, Ty};
use crate::encoding::{Record, bytes, list, svar, uvar};
use crate::syntax::ast::{FnKind, Mode};

/// What the encoding writes for the items, members and generic parameters a unit names.
pub trait Names {
    /// An item's canonical path, or a core vocabulary name's.
    fn path(&self, item: ItemId) -> String;
    /// The name of an interface's method, by its index.
    fn method(&self, interface: ItemId, method: u32) -> String;
    /// The name of a register of a registers block, by its index.
    fn register(&self, block: ItemId, register: u32) -> String;
    /// A generic parameter's position among the unit's.
    fn param(&self, param: Param) -> u64;
    /// The types of the const parameters among the generic parameters a function reference or
    /// an item's arguments bind, by position; none for a type parameter.
    fn const_types(&self, callable: &Callable) -> Vec<Option<Scalar>>;
    /// Whether a read parameter of a type is passed by reference: the type is neither Copy nor
    /// a view.
    fn by_reference(&self, ty: &Ty) -> bool;
}

/// A scalar's position in the scalar table.
pub fn scalar_position(scalar: Scalar) -> u64 {
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

fn number(out: &mut Vec<u8>, value: u64) {
    uvar(out, u128::from(value));
}

fn text(out: &mut Vec<u8>, value: &str) {
    bytes(out, value.as_bytes());
}

fn flag(out: &mut Vec<u8>, value: bool) {
    number(out, u64::from(value));
}

/// An integer of a scalar type: an svar for a signed type, a uvar otherwise.
fn integer(out: &mut Vec<u8>, value: i128, scalar: Scalar) {
    match scalar.is_signed() {
        true => svar(out, value),
        false => uvar(out, value as u128),
    }
}

/// A literal: its type, then its value.
pub fn literal(out: &mut Vec<u8>, value: &Literal) {
    match value {
        Literal::Int(_, ty) | Literal::Float(_, ty) => scalar_type(out, *ty),
        Literal::Bool(_) => scalar_type(out, Scalar::Bool),
        Literal::Char(_) => scalar_type(out, Scalar::Char),
        Literal::Text(_) => {
            named_type(out, "core::Str");
            number(out, 0);
        }
        Literal::Bytes(_) => {
            named_type(out, "core::Slice");
            number(out, 1);
            scalar_type(out, Scalar::U8);
        }
        Literal::Unit => number(out, 1),
    }
    literal_value(out, value);
}

/// A literal's value: an integer as an svar for a signed type and a uvar otherwise, a float as
/// its bits least significant byte first, a `bool` or `char` as a uvar, text and bytes as bytes.
pub fn literal_value(out: &mut Vec<u8>, value: &Literal) {
    match value {
        Literal::Int(value, ty) => integer(out, *value, *ty),
        Literal::Float(bits, ty) => {
            let width = if *ty == Scalar::F32 { 4 } else { 8 };
            out.extend_from_slice(&bits.to_le_bytes()[..width]);
        }
        Literal::Bool(value) => flag(out, *value),
        Literal::Char(value) => number(out, u64::from(*value)),
        Literal::Text(value) => text(out, value),
        Literal::Bytes(data) => bytes(out, data),
        Literal::Unit => {}
    }
}

fn scalar_type(out: &mut Vec<u8>, scalar: Scalar) {
    number(out, 0);
    number(out, scalar_position(scalar));
}

fn named_type(out: &mut Vec<u8>, path: &str) {
    number(out, 3);
    text(out, path);
}

/// The position of each limit in the `limit` rule, with the encoding of `stack`'s size or of
/// the class.
fn limit_words(limits: &LimitSet) -> Vec<(u64, Option<Vec<u8>>)> {
    let mut words: Vec<(u64, Option<Vec<u8>>)> = [
        limits.pure,
        limits.notrap,
        limits.terminates,
        limits.noalloc,
    ]
    .into_iter()
    .enumerate()
    .filter(|(_, holds)| *holds)
    .map(|(position, _)| (position as u64, None))
    .collect();
    words.extend(limits.stack.map(|size| {
        let mut content = Vec::new();
        number(&mut content, size);
        (4, Some(content))
    }));
    words.extend(limits.class.map(|class| {
        let mut content = Vec::new();
        class.encode(&mut content);
        (5, Some(content))
    }));
    words
}

/// Writes NIR in the canonical encoding through what the unit names.
pub struct Encoder<'n> {
    pub names: &'n dyn Names,
}

impl Encoder<'_> {
    /// A limits clause: a list of its entries, each a record of its word and `stack`'s size or
    /// the class.
    pub fn limits(&self, limits: &LimitSet) -> Vec<u8> {
        let mut out = Vec::new();
        list(
            &mut out,
            limit_words(limits).into_iter(),
            |out, (word, size)| {
                let mut entry = Record::default();
                let mut content = Vec::new();
                number(&mut content, word);
                entry.field(1, content);
                if let Some(content) = size {
                    entry.field(2, content);
                }
                out.extend(entry.finish());
            },
        );
        out
    }

    /// The head of a function or function type as part 5 writes it, from key 3 on: its
    /// parameters, result, failure type, `uses wait`, limits and convention.
    pub fn fn_head(&self, head: &mut Record, ty: &FnTy) {
        let mut params = Vec::new();
        list(&mut params, ty.params.iter(), |out, param| {
            let mut element = Record::default();
            let mut mode = Vec::new();
            number(&mut mode, mode_position(param.mode));
            element.field(1, mode);
            let mut name = Vec::new();
            text(&mut name, &param.name);
            element.field(2, name);
            let mut ty = Vec::new();
            self.param_ty(&mut ty, param.mode, &param.ty);
            element.field(3, ty);
            out.extend(element.finish());
        });
        if !ty.params.is_empty() {
            head.field(3, params);
        }
        let mut result = Vec::new();
        self.ty(&mut result, &ty.result);
        head.field(4, result);
        if let Some(fails) = &ty.fails {
            let mut failure = Vec::new();
            self.ty(&mut failure, fails);
            head.field(5, failure);
        }
        let mut wait = Vec::new();
        flag(&mut wait, ty.wait);
        head.field(6, wait);
        if !limit_words(&ty.limits).is_empty() {
            head.field(7, self.limits(&ty.limits));
        }
        if let Some(convention) = &ty.convention {
            let mut content = Vec::new();
            text(&mut content, convention);
            head.field(8, content);
        }
    }

    /// A parameter's NIR type: its type, or a reference to it for a `mut` parameter and a read
    /// one of a type that is neither Copy nor a view.
    fn param_ty(&self, out: &mut Vec<u8>, mode: Mode, ty: &Ty) {
        match mode {
            Mode::Mut => number(out, 8),
            Mode::Read if self.names.by_reference(ty) => number(out, 7),
            _ => {}
        }
        self.ty(out, ty);
    }

    pub fn ty(&self, out: &mut Vec<u8>, ty: &Ty) {
        match ty {
            Ty::Scalar(scalar) => {
                number(out, 0);
                number(out, scalar_position(*scalar));
            }
            Ty::Unit => number(out, 1),
            Ty::Never => number(out, 2),
            Ty::Item(item, args) => {
                named_type(out, &self.names.path(*item));
                let scalars = self.names.const_types(&Callable::Item {
                    item: *item,
                    role: super::Role::Body,
                    index: 0,
                });
                self.args(out, args, &scalars);
            }
            Ty::Param(param) => {
                number(out, 4);
                number(out, self.names.param(*param));
            }
            Ty::Fn(fn_ty) => {
                number(out, 6);
                number(
                    out,
                    match fn_ty.kind {
                        FnKind::Plain => 0,
                        FnKind::Interrupt => 1,
                        FnKind::Task => 2,
                    },
                );
                let mut head = Record::default();
                self.fn_head(&mut head, fn_ty);
                out.extend(head.finish());
            }
            Ty::Dyn(bound) => {
                named_type(out, "core::Dyn");
                number(out, 1);
                self.bound(out, bound);
            }
            Ty::Registers(block) => {
                named_type(out, &self.names.path(*block));
                number(out, 0);
            }
            Ty::Error => panic!("an error type in accepted NIR"),
        }
    }

    pub fn nty(&self, out: &mut Vec<u8>, ty: &NTy) {
        match ty {
            NTy::Owned(ty) => self.ty(out, ty),
            NTy::Ref(ty) => {
                number(out, 7);
                self.ty(out, ty);
            }
            NTy::RefMut(ty) => {
                number(out, 8);
                self.ty(out, ty);
            }
        }
    }

    /// An interface with its arguments, as a named type.
    pub fn bound(&self, out: &mut Vec<u8>, bound: &Bound) {
        named_type(out, &self.names.path(bound.interface));
        let scalars = self.names.const_types(&Callable::Item {
            item: bound.interface,
            role: super::Role::Body,
            index: 0,
        });
        self.args(out, &bound.args, &scalars);
    }

    /// Generic arguments: a type, or a const argument as the literal of its parameter's type.
    fn args(&self, out: &mut Vec<u8>, args: &[Arg], scalars: &[Option<Scalar>]) {
        list(
            out,
            args.iter().enumerate(),
            |out, (position, arg)| match arg {
                Arg::Ty(ty) => self.ty(out, ty),
                Arg::Const(constant) => self.const_arg(out, constant, scalars.get(position)),
            },
        );
    }

    fn const_arg(&self, out: &mut Vec<u8>, constant: &Const, scalar: Option<&Option<Scalar>>) {
        match constant {
            Const::Param(param) => {
                number(out, 4);
                number(out, self.names.param(*param));
            }
            Const::Int(value) => {
                let scalar = scalar.copied().flatten().unwrap_or(Scalar::Usize);
                number(out, 5);
                literal(out, &Literal::Int(*value, scalar));
            }
            Const::Bool(value) => {
                number(out, 5);
                literal(out, &Literal::Bool(*value));
            }
            Const::Error => panic!("an error argument in accepted NIR"),
        }
    }

    /// A function reference: its canonical path, role and index, and type arguments.
    pub fn fn_ref(&self, out: &mut Vec<u8>, callee: &FnRef) {
        let (path, role, index) = match &callee.callable {
            Callable::Item { item, role, index } => (self.names.path(*item), *role as u64, *index),
            Callable::Method { interface, method } => (
                format!(
                    "{}.{}",
                    self.names.path(*interface),
                    self.names.method(*interface, *method)
                ),
                0,
                0,
            ),
            Callable::Derived { owner, method } => {
                (format!("{}.{method}", self.names.path(*owner)), 0, 0)
            }
            Callable::Builtin(Builtin::Conversion(conversion, scalar)) => {
                (format!("{}.{}", scalar.name(), conversion.word()), 0, 0)
            }
            Callable::Builtin(Builtin::Bits(conversion, owner)) => {
                let owner = match owner {
                    super::BitsOwner::Scalar(scalar) => scalar.name().to_owned(),
                    super::BitsOwner::Layout(item) => self.names.path(*item),
                };
                (format!("{owner}.{}", conversion.word()), 0, 0)
            }
            Callable::Builtin(Builtin::Intrinsic(path)) => ((*path).to_owned(), 0, 0),
        };
        text(out, &path);
        number(out, role);
        number(out, u64::from(index));
        let scalars = self.names.const_types(&callee.callable);
        self.args(out, &callee.args, &scalars);
    }

    fn place(&self, out: &mut Vec<u8>, place: &Place) {
        number(out, u64::from(place.root.0));
        list(out, place.steps.iter(), |out, step| text(out, step));
    }

    fn operand(&self, out: &mut Vec<u8>, operand: &Operand) {
        match operand {
            Operand::Value(value) => {
                number(out, 0);
                number(out, u64::from(value.0));
            }
            Operand::Literal(value) => {
                number(out, 1);
                literal(out, value);
            }
            Operand::Place(mode, place) => {
                number(out, 2);
                number(out, mode_position(*mode));
                self.place(out, place);
            }
        }
    }

    fn operands(&self, out: &mut Vec<u8>, operands: &[Operand]) {
        list(out, operands.iter(), |out, operand| {
            self.operand(out, operand)
        });
    }

    fn region(&self, out: &mut Vec<u8>, region: &Region) {
        list(out, region.blocks.iter(), |out, block| {
            number(out, u64::from(block.0));
        });
    }

    /// An instruction: its opcode, its premise marker, then its parts in notation order.
    fn inst(&self, out: &mut Vec<u8>, inst: &Inst) {
        let opcode = opcode(&inst.op);
        number(out, opcode);
        number(
            out,
            inst.trusted.map_or(0, |premise| u64::from(premise) + 1),
        );
        match &inst.op {
            Op::Slot { ty, .. } => self.ty(out, ty),
            Op::Load(place) | Op::Move(place) | Op::Release(place) => self.place(out, place),
            Op::Ref(place) | Op::RefMut(place) => {
                flag(out, matches!(inst.op, Op::RefMut(_)));
                self.place(out, place);
            }
            Op::Init(place, operand) | Op::Assign(place, operand) => {
                self.place(out, place);
                self.operand(out, operand);
            }
            Op::FnRef(callee) => self.fn_ref(out, callee),
            Op::Constant(constant) => match constant {
                Constant::Item(item) => {
                    number(out, 0);
                    text(out, &self.names.path(*item));
                }
                Constant::Param(param) => {
                    number(out, 1);
                    number(out, self.names.param(*param));
                }
                Constant::Register(block, register) => {
                    number(out, 0);
                    text(
                        out,
                        &format!(
                            "{}.{}",
                            self.names.path(*block),
                            self.names.register(*block, *register)
                        ),
                    );
                }
            },
            Op::Construct { ty, fields } => {
                self.ty(out, ty);
                list(out, fields.iter(), |out, (_, operand)| {
                    self.operand(out, operand);
                });
            }
            Op::Variant {
                ty,
                variant,
                payload,
            } => {
                self.ty(out, ty);
                text(out, variant);
                self.payload(out, payload);
            }
            Op::Is { place, ty, variant } => {
                self.place(out, place);
                self.ty(out, ty);
                text(out, variant);
            }
            Op::Closure { index, captures } => {
                number(out, u64::from(*index));
                self.operands(out, captures);
            }
            Op::Convert { value, to, via } => {
                self.operand(out, value);
                self.ty(out, to);
                match via {
                    Some(variant) => {
                        number(out, 1);
                        text(out, variant);
                    }
                    None => number(out, 0),
                }
            }
            Op::Unary(_, operand) | Op::Assume(operand) | Op::Dbg(operand) => {
                self.operand(out, operand);
            }
            Op::Binary(_, left, right) => {
                self.operand(out, left);
                self.operand(out, right);
            }
            Op::Call(call) => self.call_parts(out, call),
            Op::Check { kind, operand, .. } => {
                number(out, check_position(*kind));
                self.operand(out, operand);
            }
            Op::Quantifier {
                low,
                high,
                inclusive,
                region,
                ..
            } => {
                self.operand(out, low);
                flag(out, *inclusive);
                self.operand(out, high);
                self.region(out, region);
            }
            Op::When {
                attempt,
                accesses,
                region,
            } => {
                flag(out, *attempt);
                self.operands(out, accesses);
                self.region(out, region);
            }
        }
    }

    fn payload(&self, out: &mut Vec<u8>, payload: &Payload) {
        match payload {
            Payload::None => number(out, 0),
            Payload::Single(operand) => {
                number(out, 1);
                self.operand(out, operand);
            }
            Payload::Labelled(fields) => {
                number(out, 2);
                list(out, fields.iter(), |out, (_, operand)| {
                    self.operand(out, operand);
                });
            }
        }
    }

    /// A call's parts after its opcode: `await`, its callee and its operands.
    fn call_parts(&self, out: &mut Vec<u8>, call: &Call) {
        flag(out, call.awaited);
        match &call.callee {
            Callee::Fn(callee) => self.fn_ref(out, callee),
            Callee::Value(value) => number(out, u64::from(value.0)),
            Callee::Dyn {
                place,
                interface,
                method,
            } => {
                self.place(out, place);
                text(out, &self.names.path(*interface));
                text(out, &self.names.method(*interface, *method));
            }
        }
        self.operands(out, &call.args);
    }

    fn target(&self, out: &mut Vec<u8>, jump: &Jump) {
        number(out, u64::from(jump.block.0));
        self.operands(out, &jump.args);
    }

    fn terminator(&self, out: &mut Vec<u8>, term: &Terminator) {
        match term {
            Terminator::Jump(jump) => {
                number(out, 42);
                self.target(out, jump);
            }
            Terminator::Branch(operand, then, otherwise) => {
                number(out, 43);
                self.operand(out, operand);
                self.target(out, then);
                self.target(out, otherwise);
            }
            Terminator::Call { call, ok, fail } => {
                number(out, 44);
                number(out, call_opcode(call));
                number(out, 0);
                self.call_parts(out, call);
                number(out, u64::from(ok.0));
                number(out, u64::from(fail.0));
            }
            Terminator::Return(operand) => {
                number(out, 45);
                match operand {
                    Some(operand) => {
                        number(out, 1);
                        self.operand(out, operand);
                    }
                    None => number(out, 0),
                }
            }
            Terminator::Fail(operand) => {
                number(out, 46);
                self.operand(out, operand);
            }
            Terminator::Todo(intent) => {
                number(out, 47);
                text(out, intent);
            }
            Terminator::Unreachable => number(out, 48),
            Terminator::Yield(operand) => {
                number(out, 49);
                self.operand(out, operand);
            }
        }
    }

    fn block(&self, out: &mut Vec<u8>, function: &Function, block: &Block) {
        list(out, block.params.iter(), |out, value| {
            self.nty(out, function.ty(*value));
        });
        list(out, block.insts.iter(), |out, inst| self.inst(out, inst));
        self.terminator(out, &block.term);
    }

    /// A function: its role, index, parameters, result, failure type, wait flag and blocks, and
    /// for a closure its kind and limits.
    pub fn function(&self, function: &Function) -> Vec<u8> {
        let mut record = Record::default();
        let mut role = Vec::new();
        number(&mut role, function.role as u64);
        record.field(1, role);
        let mut index = Vec::new();
        number(&mut index, u64::from(function.index));
        record.field(2, index);
        let mut params = Vec::new();
        list(&mut params, function.params.iter(), |out, param| {
            number(out, mode_position(param.mode));
            self.nty(out, function.ty(param.value));
        });
        record.field(3, params);
        let mut result = Vec::new();
        self.ty(&mut result, &function.result);
        record.field(4, result);
        if let Some(fails) = &function.fails {
            let mut failure = Vec::new();
            self.ty(&mut failure, fails);
            record.field(5, failure);
        }
        let mut wait = Vec::new();
        flag(&mut wait, function.wait);
        record.field(6, wait);
        let mut blocks = Vec::new();
        list(&mut blocks, function.blocks.iter(), |out, block| {
            self.block(out, function, block);
        });
        record.field(7, blocks);
        if let Some(closure) = &function.closure {
            let mut head = Record::default();
            let mut kind = Vec::new();
            flag(&mut kind, closure.task);
            head.field(1, kind);
            if !limit_words(&closure.limits).is_empty() {
                head.field(2, self.limits(&closure.limits));
            }
            record.field(8, head.finish());
        }
        record.finish()
    }
}

/// A mode's position among `read`, `mut` and `take`.
fn mode_position(mode: Mode) -> u64 {
    match mode {
        Mode::Read => 0,
        Mode::Mut => 1,
        Mode::Take => 2,
    }
}

fn check_position(kind: CheckKind) -> u64 {
    match kind {
        CheckKind::Requires => 0,
        CheckKind::Ensures => 1,
        CheckKind::Predicate => 2,
        CheckKind::Expect => 3,
        CheckKind::Invariant => 4,
        CheckKind::Decreases => 5,
    }
}

fn call_opcode(call: &Call) -> u64 {
    match call.callee {
        Callee::Fn(_) => 33,
        Callee::Value(_) => 34,
        Callee::Dyn { .. } => 35,
    }
}

/// An instruction's opcode: the names of the instruction list numbered from 0, each `call`
/// entry once.
fn opcode(op: &Op) -> u64 {
    match op {
        Op::Slot { .. } => 0,
        Op::Load(_) => 1,
        Op::Move(_) => 2,
        Op::Ref(_) | Op::RefMut(_) => 3,
        Op::Init(..) => 4,
        Op::Assign(..) => 5,
        Op::Release(_) => 6,
        Op::FnRef(_) => 7,
        Op::Constant(_) => 8,
        Op::Construct { .. } => 9,
        Op::Variant { .. } => 10,
        Op::Is { .. } => 11,
        Op::Closure { .. } => 12,
        Op::Convert { .. } => 13,
        Op::Unary(UnaryOp::Neg, _) => 14,
        Op::Binary(op, ..) => match op {
            super::BinaryOp::Add => 15,
            super::BinaryOp::Sub => 16,
            super::BinaryOp::Mul => 17,
            super::BinaryOp::Div => 18,
            super::BinaryOp::Rem => 19,
            super::BinaryOp::Shl => 20,
            super::BinaryOp::Shr => 21,
            super::BinaryOp::Band => 22,
            super::BinaryOp::Bor => 23,
            super::BinaryOp::Bxor => 24,
            super::BinaryOp::Eq => 26,
            super::BinaryOp::Ne => 27,
            super::BinaryOp::Lt => 28,
            super::BinaryOp::Le => 29,
            super::BinaryOp::Gt => 30,
            super::BinaryOp::Ge => 31,
        },
        Op::Unary(UnaryOp::Inv, _) => 25,
        Op::Unary(UnaryOp::Not, _) => 32,
        Op::Call(call) => call_opcode(call),
        Op::Check { .. } => 36,
        Op::Assume(_) => 37,
        Op::Quantifier { all, .. } => {
            if *all {
                38
            } else {
                39
            }
        }
        Op::When { .. } => 40,
        Op::Dbg(_) => 41,
    }
}

#[cfg(test)]
mod tests;
