//! Elaborating expressions: values, places and the control flow of conditionals, logical
//! operators and quantifiers.

use super::{Elab, Elaborated, Fault, Held, WhenBody};
use crate::check::db::ItemId;
use crate::check::sig::SigKind;
use crate::check::tir::{Lit, LocalId, TBlock, TExpr, TKind, TStmtKind};
use crate::check::ty::{Scalar, Ty};
use crate::nir::{
    BinaryOp, Constant, FnRef, Jump, Literal, NTy, Op, Operand, Payload, Place, Span, Terminator,
    UnaryOp, Value,
};
use crate::syntax::ast::{self, Mode, Tok};

/// What an elaborated expression yields.
pub enum Out {
    /// A value or a literal.
    Operand(Operand),
    /// A place: a binding's slot or reference, a field path from one, or a projection's result.
    Place(Place),
    /// Nothing: control left the expression.
    Never,
}

/// A NIR literal of a typed literal's value.
pub fn literal(lit: &Lit, ty: &Ty) -> Elaborated<Literal> {
    Ok(match (lit, ty) {
        (Lit::Int(value), Ty::Scalar(scalar)) => Literal::Int(*value, *scalar),
        (Lit::Float(bits), Ty::Scalar(scalar)) => Literal::Float(*bits, *scalar),
        (Lit::Bool(value), _) => Literal::Bool(*value),
        (Lit::Char(c), _) => Literal::Char(*c),
        (Lit::Text(text), _) => Literal::Text(text.clone()),
        (Lit::Bytes(bytes), _) => Literal::Bytes(bytes.clone()),
        _ => return Err(Fault::Unsupported(format!("a literal of type {ty:?}"))),
    })
}

impl Elab<'_, '_> {
    pub fn span(token: Tok) -> Span {
        Span::at(token)
    }

    /// The span of a typed expression, from its first token to its last.
    pub fn spanning(expr: &TExpr) -> Span {
        Span {
            first: expr.token,
            last: expr.last,
        }
    }

    /// The place a local names.
    pub fn local_place(&mut self, local: LocalId) -> Elaborated<Place> {
        if let Some((value, field)) = self.fields.get(&local) {
            return Ok(Place::of(*value).step(field));
        }
        match self.held.get(&local) {
            Some(Held::Value(value) | Held::Slot(value)) => Ok(Place::of(*value)),
            Some(Held::Place(place)) => Ok(place.clone()),
            Some(Held::Literal(_)) | None => Err(Fault::Unsupported(format!(
                "the binding {} has no place in {:?}",
                self.body.local(local).name,
                self.role
            ))),
        }
    }

    /// Whether a place is its root alone, an owned value that is not a slot: the value itself.
    fn owned_root(&self, place: &Place) -> bool {
        place.steps.is_empty()
            && !self.b.is_slot(place.root)
            && !self.b.ty(place.root).is_reference()
    }

    /// The value at a place, copied when Copy and moved otherwise.
    pub fn take_place(&mut self, place: Place, ty: &Ty, span: Span) -> Operand {
        if self.owned_root(&place) {
            return Operand::Value(place.root);
        }
        if self.ck.nir_copy(ty) {
            return Operand::Value(self.emit_value(Op::Load(place), NTy::Owned(ty.clone()), span));
        }
        let value = self.emit_value(Op::Move(place.clone()), NTy::Owned(ty.clone()), span);
        self.moved.insert(place);
        Operand::Value(value)
    }

    /// The operand passing a place by read: its value when Copy or a view, the place otherwise.
    pub fn read_operand(&mut self, place: Place, ty: &Ty, span: Span) -> Operand {
        if self.owned_root(&place) {
            return Operand::Value(place.root);
        }
        if self.ck.nir_copy(ty) || self.ck.nir_view(ty) {
            return Operand::Value(self.emit_value(Op::Load(place), NTy::Owned(ty.clone()), span));
        }
        Operand::Place(Mode::Read, place)
    }

    /// An expression read in place: a place is passed as itself, however small its type, and any
    /// other expression as a read argument.
    pub fn borrowed(&mut self, expr: &TExpr) -> Elaborated<Option<Operand>> {
        Ok(match self.expr(expr)? {
            Out::Place(place) if self.owned_root(&place) => Some(Operand::Value(place.root)),
            Out::Place(place) => Some(Operand::Place(Mode::Read, place)),
            out => self.argument(out, expr, Mode::Read),
        })
    }

    /// An expression as a value: places read, copied or moved.
    pub fn operand(&mut self, expr: &TExpr) -> Elaborated<Option<Operand>> {
        Ok(match self.expr(expr)? {
            Out::Operand(operand) => Some(operand),
            Out::Place(place) => Some(self.take_place(place, &expr.ty, Self::spanning(expr))),
            Out::Never => None,
        })
    }

    /// An expression as a place: a temporary that is not Copy goes into a slot released at the
    /// statement's end, and a Copy temporary is its own root.
    pub fn place(&mut self, expr: &TExpr) -> Elaborated<Option<Place>> {
        Ok(match self.expr(expr)? {
            Out::Place(place) => Some(place),
            Out::Operand(operand) => Some(self.temporary(operand, &expr.ty, expr.token)),
            Out::Never => None,
        })
    }

    /// A place holding a value: the value itself when Copy, a view or a wrapped part, else a
    /// temporary of the statement being elaborated.
    pub fn temporary(&mut self, operand: Operand, ty: &Ty, token: Tok) -> Place {
        if let Operand::Value(value) = &operand
            && (self.ck.nir_copy(ty) || self.ck.nir_view(ty) || self.wrapped.contains(value))
        {
            return Place::of(*value);
        }
        let slot = self.slot_holding(operand, ty, token);
        self.temps.push(slot);
        self.scopes[self.temp_scope].slots.push(slot);
        Place::of(slot)
    }

    /// An unnamed slot initialised with a value.
    pub fn slot_holding(&mut self, operand: Operand, ty: &Ty, token: Tok) -> Value {
        let span = Self::span(token);
        let slot = self.emit_value(
            Op::Slot {
                name: None,
                ty: ty.clone(),
            },
            NTy::Owned(ty.clone()),
            span,
        );
        self.emit(Op::Init(Place::of(slot), operand), span);
        slot
    }

    pub fn expr(&mut self, expr: &TExpr) -> Elaborated<Out> {
        if !self.b.open() {
            return Ok(Out::Never);
        }
        let span = Self::spanning(expr);
        Ok(match &expr.kind {
            TKind::Failed => return Err(Fault::Failed),
            TKind::Literal(lit) => Out::Operand(Operand::Literal(literal(lit, &expr.ty)?)),
            TKind::Unit => Out::Operand(Operand::Literal(Literal::Unit)),
            TKind::Result => match self.result {
                Some(result) => Out::Place(Place::of(result)),
                None => {
                    return Err(Fault::Unsupported(
                        "`result` outside an ensures clause".to_owned(),
                    ));
                }
            },
            TKind::Old(_) => match self.olds.get(&expr.token) {
                Some(value) => Out::Place(Place::of(*value)),
                None => {
                    return Err(Fault::Unsupported(
                        "`old` outside an ensures clause".to_owned(),
                    ));
                }
            },
            TKind::Local(local) => match self.held.get(local) {
                Some(Held::Literal(literal)) => Out::Operand(Operand::Literal(literal.clone())),
                _ => Out::Place(self.local_place(*local)?),
            },
            TKind::Value(item) => {
                let value = self.emit_value(
                    Op::Constant(Constant::Item(*item)),
                    NTy::Owned(expr.ty.clone()),
                    span,
                );
                Out::Operand(Operand::Value(value))
            }
            TKind::FnValue { item, type_args } => {
                let value = self.emit_value(
                    Op::FnRef(FnRef::item(*item, type_args.clone())),
                    NTy::Owned(expr.ty.clone()),
                    span,
                );
                Out::Operand(Operand::Value(value))
            }
            TKind::ConstParam(param) => match self.const_params.get(param) {
                Some(value) => Out::Operand(Operand::Value(*value)),
                None => {
                    let value = self.emit_value(
                        Op::Constant(Constant::Param(*param)),
                        NTy::Owned(expr.ty.clone()),
                        span,
                    );
                    Out::Operand(Operand::Value(value))
                }
            },
            TKind::Register { block, index } => {
                let value = self.emit_value(
                    Op::Constant(Constant::Register(*block, *index as u32)),
                    NTy::Owned(expr.ty.clone()),
                    span,
                );
                Out::Operand(Operand::Value(value))
            }
            TKind::Field { base, name } => match self.place(base)? {
                Some(place) => Out::Place(place.step(name)),
                None => Out::Never,
            },
            TKind::Call(call) | TKind::Index(call) => {
                self.call(call, &expr.ty, Self::spanning(expr))?
            }
            TKind::Construct { variant, values } => self.construct(expr, *variant, values)?,
            TKind::Unary { op, operand } => self.unary(expr, *op, operand)?,
            TKind::Binary { op, left, right } => self.binary(expr, *op, left, right)?,
            TKind::If {
                condition,
                then,
                otherwise,
            } => self.if_expr(expr, condition, then, otherwise.as_deref())?,
            TKind::Block(block) => self.block_expr(block, &expr.ty)?,
            TKind::Match { scrutinee, arms } => self.match_expr(expr, scrutinee, arms)?,
            TKind::Closure(closure) => self.closure_value(expr, closure)?,
            TKind::When {
                attempt,
                accesses,
                body,
            } => self.when_expr(expr, *attempt, accesses, body)?,
            TKind::Trusted(body) => {
                let premise = self.unit.premise(expr.token);
                let outer = self.trusted.replace(premise);
                let out = self.expr(body);
                self.trusted = outer;
                out?
            }
            TKind::Todo(intent) => {
                self.end(Terminator::Todo(intent.clone()), span);
                Out::Never
            }
            TKind::Dbg(inner) => {
                let Some(operand) = self.arg(inner, Mode::Take)? else {
                    return Ok(Out::Never);
                };
                let ty = NTy::Owned(expr.ty.clone());
                Out::Operand(Operand::Value(self.emit_value(Op::Dbg(operand), ty, span)))
            }
            TKind::Return(value) => {
                self.return_expr(value.as_deref(), expr.token)?;
                Out::Never
            }
            TKind::Break(index) => {
                self.leave_loop(*index, true, expr.token)?;
                Out::Never
            }
            TKind::Continue(index) => {
                self.leave_loop(*index, false, expr.token)?;
                Out::Never
            }
            TKind::Fail(value) => {
                self.fail_expr(value, expr.token)?;
                Out::Never
            }
            TKind::Quantifier {
                all,
                local,
                range,
                body,
            } => self.quantifier(expr, *all, *local, range, body)?,
        })
    }

    /// The value a block yields: its one expression statement's, or nothing.
    pub fn block_expr(&mut self, block: &TBlock, ty: &Ty) -> Elaborated<Out> {
        let yields = !matches!(ty, Ty::Unit | Ty::Never);
        self.scopes.push(super::Scope::default());
        let mut out = Out::Operand(Operand::Literal(Literal::Unit));
        let last = block.stmts.len().saturating_sub(1);
        for (index, stmt) in block.stmts.iter().enumerate() {
            if !self.b.open() {
                break;
            }
            match (&stmt.kind, yields && index == last) {
                (TStmtKind::Expr(value), true) => {
                    out = match self.operand(value)? {
                        Some(operand) => Out::Operand(operand),
                        None => Out::Never,
                    };
                }
                _ => self.stmt(stmt)?,
            }
        }
        self.close_scope(block.end)?;
        if !self.b.open() {
            return Ok(Out::Never);
        }
        Ok(out)
    }

    fn construct(
        &mut self,
        expr: &TExpr,
        variant: Option<usize>,
        values: &[(String, TExpr)],
    ) -> Elaborated<Out> {
        let span = Self::spanning(expr);
        let mut operands: Vec<(String, Operand)> = Vec::new();
        for (name, value) in values {
            let Some(operand) = self.arg(value, Mode::Take)? else {
                return Ok(Out::Never);
            };
            operands.push((name.clone(), operand));
        }
        let Ty::Item(item, _) = &expr.ty else {
            return Err(Fault::Unsupported(
                "a construction of no item type".to_owned(),
            ));
        };
        let sig = self.ck.sig(*item);
        let op = match (&sig.kind, variant) {
            (SigKind::Struct { fields } | SigKind::Layout { fields, .. }, None) => {
                let ordered = fields
                    .iter()
                    .filter_map(|field| {
                        operands
                            .iter()
                            .position(|(name, _)| *name == field.name)
                            .map(|at| operands[at].clone())
                    })
                    .collect();
                Op::Construct {
                    ty: expr.ty.clone(),
                    fields: ordered,
                }
            }
            (SigKind::Enum { variants }, Some(index)) => {
                let variant = &variants[index];
                let payload = match &variant.payload {
                    crate::check::sig::Payload::None => Payload::None,
                    crate::check::sig::Payload::Single(_) => {
                        Payload::Single(operands.pop().map(|(_, operand)| operand).ok_or_else(
                            || Fault::Unsupported("a payload without value".to_owned()),
                        )?)
                    }
                    crate::check::sig::Payload::Labelled(labels) => Payload::Labelled(
                        labels
                            .iter()
                            .filter_map(|(label, _)| {
                                operands.iter().find(|(name, _)| name == label).cloned()
                            })
                            .collect(),
                    ),
                };
                Op::Variant {
                    ty: expr.ty.clone(),
                    variant: variant.name.clone(),
                    payload,
                }
            }
            (SigKind::Layout { tags, .. }, Some(index)) => Op::Variant {
                ty: expr.ty.clone(),
                variant: tags[index].clone(),
                payload: Payload::None,
            },
            _ => {
                return Err(Fault::Unsupported(
                    "a construction of an unknown shape".to_owned(),
                ));
            }
        };
        let value = self.emit_value(op, NTy::Owned(expr.ty.clone()), span);
        if variant.is_none() {
            self.predicate_checks(Place::of(value), &expr.ty, span)?;
        }
        Ok(Out::Operand(Operand::Value(value)))
    }

    fn unary(&mut self, expr: &TExpr, op: ast::UnaryOp, operand: &TExpr) -> Elaborated<Out> {
        let span = Self::spanning(expr);
        let Some(value) = self.operand(operand)? else {
            return Ok(Out::Never);
        };
        let nir_op = match (op, operand.ty.scalar()) {
            (ast::UnaryOp::Not, _) => UnaryOp::Not,
            (ast::UnaryOp::Neg, Some(_)) => UnaryOp::Neg,
            (ast::UnaryOp::BitNot, Some(_)) => UnaryOp::Inv,
            (ast::UnaryOp::Neg, None) => {
                return self.operator_call(expr, "Neg", "neg", vec![value]);
            }
            (ast::UnaryOp::BitNot, None) => {
                return self.operator_call(expr, "BitNot", "bit_not", vec![value]);
            }
        };
        let result = self.emit_value(Op::Unary(nir_op, value), NTy::Owned(expr.ty.clone()), span);
        Ok(Out::Operand(Operand::Value(result)))
    }

    fn binary(
        &mut self,
        expr: &TExpr,
        op: ast::BinaryOp,
        left: &TExpr,
        right: &TExpr,
    ) -> Elaborated<Out> {
        let span = Self::spanning(expr);
        match op {
            ast::BinaryOp::And | ast::BinaryOp::Or => {
                return self.logical(op == ast::BinaryOp::And, left, right, span);
            }
            ast::BinaryOp::RangeExclusive | ast::BinaryOp::RangeInclusive => {
                let Some(low) = self.operand(left)? else {
                    return Ok(Out::Never);
                };
                let Some(high) = self.operand(right)? else {
                    return Ok(Out::Never);
                };
                let inclusive = op == ast::BinaryOp::RangeInclusive;
                let value = self.emit_value(
                    Op::Construct {
                        ty: expr.ty.clone(),
                        fields: vec![
                            ("low".to_owned(), low),
                            ("high".to_owned(), high),
                            (
                                "inclusive".to_owned(),
                                Operand::Literal(Literal::Bool(inclusive)),
                            ),
                        ],
                    },
                    NTy::Owned(expr.ty.clone()),
                    span,
                );
                return Ok(Out::Operand(Operand::Value(value)));
            }
            _ => {}
        }
        let by_instruction = self.by_instruction(op, &left.ty);
        let mode = Mode::Read;
        let left_value = match by_instruction {
            true => self.operand(left)?,
            false => self.arg(left, mode)?,
        };
        let Some(left_value) = left_value else {
            return Ok(Out::Never);
        };
        let right_value = match by_instruction {
            true => self.operand(right)?,
            false => self.arg(right, mode)?,
        };
        let Some(right_value) = right_value else {
            return Ok(Out::Never);
        };
        Ok(
            match self.operator_values(op, left_value, right_value, &left.ty, &expr.ty, span)? {
                Some(result) => Out::Operand(result),
                None => Out::Never,
            },
        )
    }

    /// Whether an operator on operands of a type is an instruction: on scalars, and `==` and
    /// `!=` on `Str` and `Slice[u8]`; otherwise it calls its operator interface.
    fn by_instruction(&self, op: ast::BinaryOp, ty: &Ty) -> bool {
        let text =
            self.ck.is_std(ty, "core::Str") || *ty == self.ck.slice_ty(Ty::Scalar(Scalar::U8));
        ty.scalar().is_some() || (text && matches!(op, ast::BinaryOp::Eq | ast::BinaryOp::Ne))
    }

    /// An operator applied to evaluated operands of `operand_ty`: its instruction, or the call of
    /// its operator interface's method, an ordering comparison testing the `core::Ordering` it
    /// returns.
    pub fn operator_values(
        &mut self,
        op: ast::BinaryOp,
        left: Operand,
        right: Operand,
        operand_ty: &Ty,
        result_ty: &Ty,
        span: Span,
    ) -> Elaborated<Option<Operand>> {
        let instruction = match op {
            ast::BinaryOp::Add => BinaryOp::Add,
            ast::BinaryOp::Sub => BinaryOp::Sub,
            ast::BinaryOp::Mul => BinaryOp::Mul,
            ast::BinaryOp::Div => BinaryOp::Div,
            ast::BinaryOp::Rem => BinaryOp::Rem,
            ast::BinaryOp::BitAnd => BinaryOp::Band,
            ast::BinaryOp::BitOr => BinaryOp::Bor,
            ast::BinaryOp::BitXor => BinaryOp::Bxor,
            ast::BinaryOp::Shl => BinaryOp::Shl,
            ast::BinaryOp::Shr => BinaryOp::Shr,
            ast::BinaryOp::Eq => BinaryOp::Eq,
            ast::BinaryOp::Ne => BinaryOp::Ne,
            ast::BinaryOp::Lt => BinaryOp::Lt,
            ast::BinaryOp::Le => BinaryOp::Le,
            ast::BinaryOp::Gt => BinaryOp::Gt,
            ast::BinaryOp::Ge => BinaryOp::Ge,
            _ => {
                return Err(Fault::Unsupported(
                    "a logical or range operator on values".to_owned(),
                ));
            }
        };
        if self.by_instruction(op, operand_ty) {
            let result = self.emit_value(
                Op::Binary(instruction, left, right),
                NTy::Owned(result_ty.clone()),
                span,
            );
            return Ok(Some(Operand::Value(result)));
        }
        let (interface, method) = match instruction {
            BinaryOp::Eq | BinaryOp::Ne => ("Eq", "eq"),
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => ("Ord", "cmp"),
            BinaryOp::Add => ("Add", "add"),
            BinaryOp::Sub => ("Sub", "sub"),
            BinaryOp::Mul => ("Mul", "mul"),
            BinaryOp::Div => ("Div", "div"),
            BinaryOp::Rem => ("Rem", "rem"),
            BinaryOp::Band => ("BitAnd", "bit_and"),
            BinaryOp::Bor => ("BitOr", "bit_or"),
            BinaryOp::Bxor => ("BitXor", "bit_xor"),
            BinaryOp::Shl => ("Shl", "shl"),
            BinaryOp::Shr => ("Shr", "shr"),
        };
        let interface_item = self
            .ck
            .db
            .core_item(interface)
            .or_else(|| self.ck.db.std_item("core", interface))
            .ok_or_else(|| Fault::Unsupported(format!("the core interface {interface}")))?;
        let callee = self.implementation(operand_ty, interface_item, method)?;
        let args = vec![left, right];
        let bool_ty = Ty::Scalar(Scalar::Bool);
        let value = match instruction {
            BinaryOp::Eq | BinaryOp::Ne => {
                let equal = self.call_value(callee, args, &bool_ty, span);
                match instruction {
                    BinaryOp::Eq => equal,
                    _ => self.emit_value(
                        Op::Unary(UnaryOp::Not, Operand::Value(equal)),
                        NTy::Owned(bool_ty),
                        span,
                    ),
                }
            }
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
                let ordering = self
                    .ck
                    .db
                    .std_item("core", "Ordering")
                    .map(|item| Ty::item(item, Vec::new()))
                    .ok_or_else(|| Fault::Unsupported("core::Ordering".to_owned()))?;
                let order = self.call_value(callee, args, &ordering, span);
                let (variant, negate) = match instruction {
                    BinaryOp::Lt => ("Less", false),
                    BinaryOp::Ge => ("Less", true),
                    BinaryOp::Gt => ("Greater", false),
                    _ => ("Greater", true),
                };
                let test = self.emit_value(
                    Op::Is {
                        place: Place::of(order),
                        ty: ordering,
                        variant: variant.to_owned(),
                    },
                    NTy::Owned(bool_ty.clone()),
                    span,
                );
                match negate {
                    false => test,
                    true => self.emit_value(
                        Op::Unary(UnaryOp::Not, Operand::Value(test)),
                        NTy::Owned(bool_ty),
                        span,
                    ),
                }
            }
            _ => self.call_value(callee, args, result_ty, span),
        };
        Ok(Some(Operand::Value(value)))
    }

    /// `a and b`, `a or b`: a branch on `a` to the block computing `b`, or to the join with the
    /// value `a` decides.
    fn logical(&mut self, and: bool, left: &TExpr, right: &TExpr, span: Span) -> Elaborated<Out> {
        let Some(left_value) = self.operand(left)? else {
            return Ok(Out::Never);
        };
        let compute = self.b.block();
        let join = self.b.block();
        let result = self
            .b
            .block_param(join, NTy::Owned(Ty::Scalar(Scalar::Bool)));
        let decided = Jump {
            block: join,
            args: vec![Operand::Literal(Literal::Bool(!and))],
        };
        let (then, otherwise) = match and {
            true => (Jump::to(compute), decided),
            false => (decided, Jump::to(compute)),
        };
        self.end(Terminator::Branch(left_value, then, otherwise), span);
        self.b.at(compute);
        if let Some(right_value) = self.releasing_temps(right.token, |elab| elab.operand(right))? {
            self.b.jump(join, vec![right_value], span);
        }
        self.b.at(join);
        Ok(Out::Operand(Operand::Value(result)))
    }

    /// A value-yielding or statement `if`: a branch, a block per branch and a join, holding the
    /// value as its parameter when the `if` yields one; a false `if` without `else` branches to
    /// the join unless the other branch moved what it must release.
    fn if_expr(
        &mut self,
        expr: &TExpr,
        condition: &TExpr,
        then: &TBlock,
        otherwise: Option<&TExpr>,
    ) -> Elaborated<Out> {
        let span = Self::spanning(expr);
        let Some(test) = self.operand(condition)? else {
            return Ok(Out::Never);
        };
        let entry = self.b.current().expect("an open block");
        let entry_moved = self.moved.clone();
        let temps = self.temps.len();
        let then_block = self.b.block();
        self.b.at(then_block);
        let out = self.block_expr(then, &expr.ty)?;
        let then_end = self.arm_end(out, &expr.ty, then.end, temps)?;
        let unchanged = then_end.block.is_none() || then_end.moved == entry_moved;
        if otherwise.is_none() && unchanged {
            let join = self.b.block();
            if let Some(block) = then_end.block {
                self.b.at(block);
                self.b.jump(join, Vec::new(), span);
            }
            self.b.at(entry);
            self.end(
                Terminator::Branch(test, Jump::to(then_block), Jump::to(join)),
                span,
            );
            self.moved = entry_moved;
            self.b.at(join);
            return Ok(Out::Operand(Operand::Literal(Literal::Unit)));
        }
        let else_block = self.b.block();
        self.b.at(entry);
        self.end(
            Terminator::Branch(test, Jump::to(then_block), Jump::to(else_block)),
            span,
        );
        self.b.at(else_block);
        self.moved = entry_moved;
        let out = match otherwise {
            Some(otherwise) => self.expr(otherwise)?,
            None => Out::Operand(Operand::Literal(Literal::Unit)),
        };
        let else_end = self.arm_end(out, &expr.ty, expr.token, temps)?;
        self.join(vec![then_end, else_end], &expr.ty, span)
    }

    /// `await when`: the cown handles by their access modes, and the region that receives a
    /// reference to each cown's value and yields the body's value.
    fn when_expr(
        &mut self,
        expr: &TExpr,
        attempt: bool,
        accesses: &[(Mode, TExpr, LocalId)],
        body: &TExpr,
    ) -> Elaborated<Out> {
        let span = Self::spanning(expr);
        let mut operands = Vec::new();
        for (mode, cown, _) in accesses {
            let Some(place) = self.place(cown)? else {
                return Ok(Out::Never);
            };
            operands.push(Operand::Place(*mode, place));
        }
        let outer = self.b.current().expect("an open block");
        self.b.open_region();
        let entry = self.b.block();
        for (mode, _, local) in accesses {
            let ty = self.body.local(*local).ty.clone();
            let reference = match mode {
                Mode::Mut => NTy::RefMut(ty),
                _ => NTy::Ref(ty),
            };
            let value = self.b.block_param(entry, reference);
            self.held.insert(*local, Held::Value(value));
        }
        self.b.at(entry);
        let body_of = WhenBody {
            depth: self.scopes.len(),
            leaves: Vec::new(),
        };
        let enclosing = self.when.replace(body_of);
        let yielded = self.releasing_temps(body.token, |elab| elab.operand(body))?;
        if let Some(value) = yielded {
            self.end(Terminator::Yield(value), span);
        }
        let leaves = std::mem::replace(&mut self.when, enclosing)
            .expect("the when body")
            .leaves;
        let region = self.b.close_region();
        self.b.at(outer);
        let op = Op::When {
            attempt,
            accesses: operands,
            region,
        };
        let out = match expr.ty == Ty::Unit {
            true => {
                self.emit(op, span);
                Out::Operand(Operand::Literal(Literal::Unit))
            }
            false => {
                let value = self.emit_value(op, NTy::Owned(expr.ty.clone()), span);
                Out::Operand(Operand::Value(value))
            }
        };
        self.continue_leaves(leaves)?;
        Ok(out)
    }

    /// A quantifier: `all` or `any` over its range, its region binding the element.
    fn quantifier(
        &mut self,
        expr: &TExpr,
        all: bool,
        local: LocalId,
        range: &TExpr,
        body: &TExpr,
    ) -> Elaborated<Out> {
        let span = Self::spanning(expr);
        let TKind::Binary {
            op,
            left: low,
            right: high,
        } = &range.kind
        else {
            return Err(Fault::Unsupported(
                "a quantifier over no range expression".to_owned(),
            ));
        };
        let Some(low) = self.operand(low)? else {
            return Ok(Out::Never);
        };
        let Some(high) = self.operand(high)? else {
            return Ok(Out::Never);
        };
        let element = self.body.local(local).ty.clone();
        let outer = self.b.current().expect("an open block");
        self.b.open_region();
        let entry = self.b.block();
        let bound = self.b.block_param(entry, NTy::Owned(element));
        self.held.insert(local, Held::Value(bound));
        self.b.at(entry);
        let value = self.operand(body)?;
        if let Some(value) = value {
            self.end(Terminator::Yield(value), span);
        }
        let region = self.b.close_region();
        self.b.at(outer);
        let result = self.emit_value(
            Op::Quantifier {
                all,
                low,
                high,
                inclusive: *op == ast::BinaryOp::RangeInclusive,
                region,
            },
            NTy::Owned(Ty::Scalar(Scalar::Bool)),
            span,
        );
        Ok(Out::Operand(Operand::Value(result)))
    }

    /// The call of an operator interface's method on a non-scalar: the implementing function
    /// the left operand's type gives.
    fn operator_call(
        &mut self,
        expr: &TExpr,
        interface: &str,
        method: &str,
        args: Vec<Operand>,
    ) -> Elaborated<Out> {
        let span = Self::spanning(expr);
        let operand_ty = expr.ty.clone();
        let Some(interface) = self.ck.db.std_item("core", interface) else {
            return Err(Fault::Unsupported(format!(
                "the core interface {interface}"
            )));
        };
        let callee = self.implementation(&operand_ty, interface, method)?;
        let value = self.call_value(callee, args, &expr.ty, span);
        Ok(Out::Operand(Operand::Value(value)))
    }

    /// The function implementing an interface's method for a type.
    pub fn implementation(&self, ty: &Ty, interface: ItemId, method: &str) -> Elaborated<FnRef> {
        self.ck
            .implementation(ty, interface, method)
            .ok_or_else(|| Fault::Unsupported(format!("the implementation of {method}")))
    }

    /// A closure expression: its value holding one operand per capture, in its mode.
    fn closure_value(
        &mut self,
        expr: &TExpr,
        closure: &crate::check::tir::TClosure,
    ) -> Elaborated<Out> {
        let span = Self::spanning(expr);
        let mut operands = Vec::new();
        for capture in super::closure_captures(closure) {
            if let Some(Held::Literal(literal)) = self.held.get(&capture.outer) {
                operands.push(Operand::Literal(literal.clone()));
                continue;
            }
            let ty = self.body.local(capture.outer).ty.clone();
            let place = self.local_place(capture.outer)?;
            let operand = match capture.mode {
                Mode::Read => self.read_operand(place, &ty, span),
                Mode::Mut => Operand::Place(Mode::Mut, place),
                Mode::Take => {
                    self.moved.insert(place.clone());
                    Operand::Place(Mode::Take, place)
                }
            };
            operands.push(operand);
        }
        self.closure_predicate_checks(closure, span)?;
        let value = self.emit_value(
            Op::Closure {
                index: self.closure_base + closure.index - 1,
                captures: operands,
            },
            NTy::Owned(expr.ty.clone()),
            span,
        );
        Ok(Out::Operand(Operand::Value(value)))
    }

    /// The operand an argument passes for a parameter in a mode.
    pub fn arg(&mut self, expr: &TExpr, mode: Mode) -> Elaborated<Option<Operand>> {
        let out = self.expr(expr)?;
        Ok(self.argument(out, expr, mode))
    }

    /// The operand passing an elaborated argument expression in a mode.
    fn argument(&mut self, out: Out, expr: &TExpr, mode: Mode) -> Option<Operand> {
        let span = Self::spanning(expr);
        match (out, mode) {
            (Out::Never, _) => None,
            (Out::Operand(operand), Mode::Read) => match operand {
                Operand::Value(value)
                    if !self.ck.nir_copy(&expr.ty) && !self.ck.nir_view(&expr.ty) =>
                {
                    let place = self.temporary(Operand::Value(value), &expr.ty, expr.token);
                    Some(self.read_operand(place, &expr.ty, span))
                }
                other => Some(other),
            },
            (Out::Operand(operand), Mode::Mut) => {
                let slot = self.slot_holding(operand, &expr.ty, expr.token);
                self.temps.push(slot);
                self.scopes[self.temp_scope].slots.push(slot);
                Some(Operand::Place(Mode::Mut, Place::of(slot)))
            }
            (Out::Operand(operand), _) => Some(operand),
            (Out::Place(place), Mode::Read) => Some(self.read_operand(place, &expr.ty, span)),
            (Out::Place(place), Mode::Mut) => Some(Operand::Place(Mode::Mut, place)),
            (Out::Place(place), Mode::Take) => {
                if self.ck.nir_copy(&expr.ty) || self.owned_root(&place) {
                    Some(self.take_place(place, &expr.ty, span))
                } else {
                    self.moved.insert(place.clone());
                    Some(Operand::Place(Mode::Take, place))
                }
            }
        }
    }

    /// A call returning a value of `ty`.
    pub fn call_value(&mut self, callee: FnRef, args: Vec<Operand>, ty: &Ty, span: Span) -> Value {
        self.emit_value(
            Op::Call(crate::nir::Call {
                awaited: false,
                callee: crate::nir::Callee::Fn(callee),
                args,
            }),
            NTy::Owned(ty.clone()),
            span,
        )
    }
}
