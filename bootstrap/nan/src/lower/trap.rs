//! Trap sites: the call of the trap routine that each failing check and partial operation
//! makes, with its kind and the operands its report shows; and the lines `dbg` writes to the
//! test trace, which show their operand as reports do.
//!
//! An operand record is 40 bytes: the address and length of the operand's source text, the
//! bits of a scalar value or the address of another, how the report writes the bits, as
//! `Written` numbers it, and the address of the glue writing another value, or zero.

use cranelift_codegen::ir::{InstBuilder, TrapCode, Value as IrValue, types};
use cranelift_module::FuncId;

use super::Lowering;
use super::body::{Body, Held};
use super::fx::{Fx, PTR};
use super::runtime::Routine;
use super::runtime::report::{REPORT, Report, values};
use crate::check::db::ItemId;
use crate::check::ty::{Scalar, Subst, Ty};
use crate::nir::{
    BinaryOp, Call, Callable, Callee, CheckKind, Function, Literal, NTy, Op, Operand, Role, Span,
    Terminator, UnaryOp, Value,
};
use crate::syntax::file::SourceFile;
use crate::syntax::grammar::{Keyword, Punct};
use crate::syntax::token::TokenKind;

/// The trap kinds, numbered by their position, as reports and `sys::TrapKind` order them.
pub const KINDS: [&str; 17] = [
    "trap.bounds",
    "trap.overflow",
    "trap.division-by-zero",
    "trap.shift",
    "trap.conversion",
    "trap.stale-handle",
    "trap.exhausted",
    "trap.predicate",
    "trap.requires",
    "trap.ensures",
    "trap.expect",
    "trap.todo",
    "trap.stack-overflow",
    "trap.duplicate-access",
    "trap.poisoned",
    "trap.exception",
    "trap.abandoned",
];

/// How a report writes an operand's value.
#[derive(Clone, Copy)]
pub enum Written {
    Unsigned = 0,
    Signed = 1,
    Bool = 2,
    Char = 3,
    F32 = 4,
    F64 = 5,
    /// A value no report writes: `_`.
    Hidden = 6,
}

pub const OPERAND_RECORD: u64 = 40;

/// The operands a trap site's report shows.
#[derive(Clone)]
pub enum Operands {
    None,
    /// The operands of the trapping operation, each with its value, and how its source shows
    /// them.
    Values(Form, Vec<(Operand, Held, NTy)>),
    /// The two sides of a failing check's condition when it is a comparison.
    Condition(Option<Value>),
    /// The index of an `at` or `at_mut` call and the length it lies outside.
    Bounds(Operand, IrValue, IrValue),
}

/// How the source text of a trapping operation shows its operands.
#[derive(Clone)]
pub enum Form {
    /// The sides of a binary operator, or of the compound assignment applying it.
    Binary(BinaryOp),
    /// What follows a unary operator.
    Unary,
    /// The arguments in the last parentheses or brackets, after the receiver when the
    /// operation has one more operand than them.
    Call,
    /// The value stored into a field: what follows `=`, or the argument with its label.
    Field(String),
}

pub fn kind_number(kind: &str) -> i64 {
    KINDS
        .iter()
        .position(|known| *known == kind)
        .unwrap_or_else(|| panic!("the trap kind {kind}")) as i64
}

/// The trap kind of a kept check.
pub fn check_kind(kind: CheckKind) -> &'static str {
    match kind {
        CheckKind::Requires => "trap.requires",
        CheckKind::Ensures => "trap.ensures",
        CheckKind::Predicate => "trap.predicate",
        CheckKind::Expect | CheckKind::Invariant | CheckKind::Decreases => "trap.expect",
    }
}

/// Traps with `kind` at the current source location, or for a failed requires, ensures or
/// invariant check at its clause or line in the item declaring it unless that is std, a
/// requires checked at a call naming the call as the first caller: the trap routine never
/// returns.
pub fn raise(body: &mut Body, kind: &str, operands: Operands) {
    let clause = match &operands {
        Operands::Condition(Some(condition)) => clause_of(body, *condition),
        _ => None,
    };
    let caller = match clause {
        Some((item, Role::Requires, _))
            if item != body.item || body.function().role != Role::Entry =>
        {
            Some(body.lo.lines.location(body.lo.ck, body.item, body.current))
        }
        _ => None,
    };
    let records = records(body, operands);
    let count = records.len() as u64;
    let array = body.fx.slot((count * OPERAND_RECORD).max(8), 8);
    for (index, (text, shown)) in records.into_iter().enumerate() {
        let at = index as u64 * OPERAND_RECORD;
        let data = body.lo.bytes_data(text.as_bytes());
        let global = body.lo.data_ref(body.fx, data);
        let address = body.fx.b.ins().symbol_value(PTR, global);
        body.fx.store(address, array, at);
        let len = body.fx.ptr(text.len() as u64);
        body.fx.store(len, array, at + 8);
        body.fx.store(shown.bits, array, at + 16);
        let how = body.fx.ptr(shown.written as u64);
        body.fx.store(how, array, at + 24);
        let writer = match shown.writer {
            Some(writer) => {
                let reference = body.lo.func_ref(body.fx, writer);
                body.fx.b.ins().func_addr(PTR, reference)
            }
            None => body.fx.ptr(0),
        };
        body.fx.store(writer, array, at + 32);
    }
    let kind = body.fx.int(types::I64, kind_number(kind));
    let count = body.fx.ptr(count);
    let caller = body.fx.ptr(u64::from(caller.unwrap_or(0)));
    if let Some((item, _, span)) = clause {
        body.lo.locate(body.fx, item, span);
    }
    body.lo
        .call_routine(body.fx, Routine::TrapRaise, &[kind, array, count, caller]);
    body.fx
        .b
        .ins()
        .trap(TrapCode::user(1).expect("a trap code"));
    body.lo.locate(body.fx, body.item, body.current);
}

/// The item, role and span of the requires, ensures or invariant clause whose function a call
/// defining a condition calls, when that item is not std.
fn clause_of(body: &Body, condition: Value) -> Option<(ItemId, Role, Span)> {
    let call = condition_call(body, condition)?;
    let Callee::Fn(fn_ref) = &call.callee else {
        return None;
    };
    let Callable::Item { item, role, index } = fn_ref.callable else {
        return None;
    };
    if !matches!(role, Role::Requires | Role::Ensures | Role::Invariant)
        || body.lo.ck.db.is_std(item)
    {
        return None;
    }
    let span = body.lo.ck.unit(item)?.function(role, index)?.span;
    Some((item, role, span))
}

/// The call defining a value of the function, when a call does.
fn condition_call(body: &Body, condition: Value) -> Option<Call> {
    body.function()
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .find_map(|inst| match &inst.op {
            Op::Call(call) if inst.result == Some(condition) => Some(call.clone()),
            _ => None,
        })
}

/// What a report writes for a value: a scalar's bits and their writing, or the address of a
/// writable value with its type's glue; any other value hidden.
pub struct Shown {
    bits: IrValue,
    written: Written,
    writer: Option<FuncId>,
}

impl Shown {
    fn hidden(fx: &mut Fx) -> Shown {
        Shown {
            bits: fx.ptr(0),
            written: Written::Hidden,
            writer: None,
        }
    }

    /// A writable value at an address.
    fn at(lo: &mut Lowering, ty: &Ty, address: IrValue) -> Shown {
        Shown {
            bits: address,
            written: Written::Hidden,
            writer: Some(values::writer(lo, ty)),
        }
    }

    /// Appends the value to a report.
    fn write(&self, lo: &mut Lowering, fx: &mut Fx, report: Report) {
        match self.writer {
            Some(writer) => {
                let writer = lo.func_ref(fx, writer);
                fx.b.ins().call(writer, &[report.out, self.bits]);
            }
            None => {
                let how = fx.ptr(self.written as u64);
                lo.call_routine(fx, Routine::ReportScalar, &[report.out, self.bits, how]);
            }
        }
    }
}

/// Writes the line of the `dbg` being lowered to the test trace: its path and line, the source
/// text of its operand, ` = ` and the operand's value.
pub fn dbg_line(body: &mut Body, held: Held, nty: &NTy) {
    let span = body.current;
    let text = {
        let source = Source::of(&body.lo.ck.db.item(body.item).file.syntax, span);
        let last = source.tokens.len().saturating_sub(2);
        source.text(2, last).unwrap_or_default()
    };
    let line = body.lo.ck.db.item(body.item).line(span.first);
    let path = super::lines::path(body.lo.ck, body.item);
    let shown = shown(body, held, nty);
    let out = body.fx.slot(8 + REPORT, 8);
    let zero = body.fx.ptr(0);
    body.fx.store(zero, out, 0);
    let report = Report { out };
    report.text(body.fx, body.lo, &format!("{path}:{line} {text} = "));
    shown.write(body.lo, body.fx, report);
    report.text(body.fx, body.lo, "\n");
    let bytes = body.fx.b.ins().iadd_imm_s(out, 8);
    let len = body.fx.load(PTR, out, 0);
    super::supplied::sims::trace(body.lo, body.fx, &[(bytes, len)]);
}

/// The source text and shown value of each operand a report shows.
fn records(body: &mut Body, operands: Operands) -> Vec<(String, Shown)> {
    match operands {
        Operands::None | Operands::Condition(None) => Vec::new(),
        Operands::Values(form, values) => {
            let texts = source_texts(body, body.item, body.current, &form, values.len());
            values
                .into_iter()
                .enumerate()
                .map(|(index, (operand, held, nty))| {
                    let text = match &texts {
                        Some(texts) => texts[index].clone(),
                        None => operand_text(body, &operand),
                    };
                    (text, shown(body, held, &nty))
                })
                .collect()
        }
        Operands::Bounds(index, at, len) => {
            let text = match source_texts(body, body.item, body.current, &Form::Call, 2) {
                Some(mut texts) => texts.remove(1),
                None => operand_text(body, &index),
            };
            let unsigned = |bits| Shown {
                bits,
                written: Written::Unsigned,
                writer: None,
            };
            vec![(text, unsigned(at)), ("len".to_owned(), unsigned(len))]
        }
        Operands::Condition(Some(condition)) => own_sides(body, condition)
            .or_else(|| called_sides(body, condition))
            .unwrap_or_default(),
    }
}

/// The sides of the comparison defining a condition in this function, when its source text
/// is one.
fn own_sides(body: &mut Body, condition: Value) -> Option<Vec<(String, Shown)>> {
    let (op, sides, span) = comparison(body.function(), condition)?;
    let texts = source_texts(body, body.item, span, &Form::Binary(op), 2)?;
    Some(
        texts
            .into_iter()
            .zip(sides)
            .map(|(text, side)| {
                let (held, nty) = body.operand(&side);
                (text, shown(body, held, &nty))
            })
            .collect(),
    )
}

/// The comparison defining a value of a function, when one may: its operator, sides and span.
/// A comparison of values no instruction compares is a call of their `eq`, or of their `cmp`
/// tested for a variant, negated for `!=`, `<=` and `>=`.
pub fn comparison(function: &Function, value: Value) -> Option<(BinaryOp, [Operand; 2], Span)> {
    let inst = function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .find(|inst| inst.result == Some(value))?;
    let (op, sides) = match &inst.op {
        Op::Binary(op, left, right) if op.is_comparison() => (*op, [left.clone(), right.clone()]),
        Op::Call(call) => match call.args.as_slice() {
            [left, right] => (BinaryOp::Eq, [left.clone(), right.clone()]),
            _ => return None,
        },
        Op::Is { place, variant, .. } if place.steps.is_empty() => {
            let (_, sides, _) = comparison(function, place.root)?;
            match variant.as_str() {
                "Less" => (BinaryOp::Lt, sides),
                "Greater" => (BinaryOp::Gt, sides),
                _ => return None,
            }
        }
        Op::Unary(UnaryOp::Not, Operand::Value(inner)) => {
            let (op, sides, _) = comparison(function, *inner)?;
            match op {
                BinaryOp::Eq => (BinaryOp::Ne, sides),
                BinaryOp::Lt => (BinaryOp::Ge, sides),
                BinaryOp::Gt => (BinaryOp::Le, sides),
                _ => return None,
            }
        }
        _ => return None,
    };
    Some((op, sides, inst.span))
}

/// The sides of the comparison that the contract function a call defining a condition calls
/// returns, computed again from the call's operands.
fn called_sides(body: &mut Body, condition: Value) -> Option<Vec<(String, Shown)>> {
    let call = condition_call(body, condition)?;
    let Callee::Fn(fn_ref) = &call.callee else {
        return None;
    };
    let Callable::Item { item, role, index } = fn_ref.callable else {
        return None;
    };
    if !role.is_contract() {
        return None;
    }
    let unit = body.lo.ck.unit(item)?;
    let function = unit.function(role, index)?;
    let returned = function.blocks.iter().find_map(|block| match &block.term {
        Terminator::Return(Some(Operand::Value(value))) => Some(*value),
        _ => None,
    })?;
    let (op, sides, span) = comparison(function, returned)?;
    let texts = source_texts(body, item, span, &Form::Binary(op), 2)?;
    let subst = body.lo.subst(item, &body.subst.args(&fn_ref.args));
    let types = side_types(body.lo, function, &subst, &sides);
    let (offsets, size) = sides_layout(body.lo, &types);
    let record = body.call_sides(fn_ref, &call.args, size);
    let mut shown = Vec::new();
    for ((text, ty), offset) in texts.into_iter().zip(types).zip(offsets) {
        let at = body.fx.offset(record, offset);
        let side = match ty.scalar() {
            Some(_) => Shown {
                bits: body.fx.load(PTR, at, 0),
                written: written(&ty),
                writer: None,
            },
            None if values::writable(body.lo, &ty) => Shown::at(body.lo, &ty, at),
            None => Shown::hidden(body.fx),
        };
        shown.push((text, side));
    }
    Some(shown)
}

/// The types of the sides of a comparison in a function whose types `subst` makes concrete,
/// `Unit` for a place.
pub fn side_types(
    lo: &Lowering,
    function: &Function,
    subst: &Subst,
    sides: &[Operand; 2],
) -> [Ty; 2] {
    sides.each_ref().map(|side| match side {
        Operand::Value(value) => subst.ty(function.ty(*value).ty()),
        Operand::Literal(literal) => literal_ty(lo, literal),
        Operand::Place(..) => Ty::Unit,
    })
}

/// The offsets of the two sides of a comparison in the record its `Sides` function writes, a
/// scalar side taking 8 bytes and any other the bytes of its value, and the record's size.
pub fn sides_layout(lo: &mut Lowering, types: &[Ty; 2]) -> ([u64; 2], u64) {
    let mut size = |ty: &Ty| match ty.scalar() {
        Some(_) => 8,
        None => lo.layout(ty).size.div_ceil(8) * 8,
    };
    let first = size(&types[0]);
    let second = size(&types[1]);
    ([0, first], (first + second).max(8))
}

/// Keeps a side of a comparison at an offset of the record of its `Sides` function: a
/// scalar's bits, or the bytes of a writable value.
pub fn keep_side(body: &mut Body, held: Held, nty: &NTy, ty: &Ty, record: IrValue, offset: u64) {
    if ty.scalar().is_some() {
        let shown = shown(body, held, nty);
        body.fx.store(shown.bits, record, offset);
    } else if values::writable(body.lo, ty) {
        let address = address_of(body, held, ty);
        let target = body.fx.offset(record, offset);
        let size = body.fx.ptr(body.lo.layout(ty).size);
        body.lo
            .call_routine(body.fx, Routine::Copy, &[target, address, size]);
    }
}

/// The source texts of the `count` operands of the operation a span of an item covers, as its
/// form shows them; none when its text does not have that form.
fn source_texts(
    body: &Body,
    item: ItemId,
    span: Span,
    form: &Form,
    count: usize,
) -> Option<Vec<String>> {
    let syntax = &body.lo.ck.db.item(item).file.syntax;
    let source = Source::of(syntax, span);
    let texts = match form {
        Form::Binary(op) => source.binary(op_symbol(*op))?,
        Form::Unary => vec![source.text(1, source.tokens.len().checked_sub(1)?)?],
        Form::Call => source.call(count)?,
        Form::Field(name) => vec![source.field(name)?],
    };
    (texts.len() == count).then_some(texts)
}

/// The code tokens a span covers, and the depth of brackets each lies in.
struct Source<'s> {
    syntax: &'s SourceFile,
    tokens: Vec<usize>,
    depths: Vec<u32>,
}

impl<'s> Source<'s> {
    fn of(syntax: &'s SourceFile, span: Span) -> Source<'s> {
        let tokens: Vec<usize> = (span.first as usize..=span.last as usize)
            .filter(|token| !syntax.tokens[*token].kind.is_virtual())
            .collect();
        let mut depths = Vec::with_capacity(tokens.len());
        let mut depth = 0u32;
        for token in &tokens {
            match syntax.text(*token) {
                "(" | "[" | "{" => {
                    depths.push(depth);
                    depth += 1;
                }
                ")" | "]" | "}" => {
                    depth = depth.saturating_sub(1);
                    depths.push(depth);
                }
                _ => depths.push(depth),
            }
        }
        Source {
            syntax,
            tokens,
            depths,
        }
    }

    fn word(&self, at: usize) -> &str {
        self.syntax.text(self.tokens[at])
    }

    /// The source text from the code token at `first` to the one at `last`, each line break
    /// with its indentation written as one space; none for an empty range.
    fn text(&self, first: usize, last: usize) -> Option<String> {
        if first > last || last >= self.tokens.len() {
            return None;
        }
        let tokens = &self.syntax.tokens;
        let start = tokens[self.tokens[first]].start as usize;
        let end = tokens[self.tokens[last]].end as usize;
        let text = String::from_utf8_lossy(&self.syntax.source[start..end]);
        Some(text.lines().map(str::trim).collect::<Vec<_>>().join(" "))
    }

    /// The sides of the outermost binary operator `symbol`, or of the compound assignment
    /// `symbol=`: a binary operator of no higher precedence than the operators at depth 0
    /// beside it is the rightmost there, as operators of one precedence group to the left.
    fn binary(&self, symbol: &str) -> Option<Vec<String>> {
        let last = self.tokens.len().checked_sub(1)?;
        let compound = format!("{symbol}=");
        let at = (0..=last)
            .find(|at| self.depths[*at] == 0 && self.word(*at) == compound)
            .or_else(|| {
                (1..last).rev().find(|at| {
                    self.depths[*at] == 0 && self.word(*at) == symbol && self.ends_operand(at - 1)
                })
            })?;
        Some(vec![
            self.text(0, at.checked_sub(1)?)?,
            self.text(at + 1, last)?,
        ])
    }

    /// Whether the code token at `at` can end an operand, so that an operator after it is
    /// binary.
    fn ends_operand(&self, at: usize) -> bool {
        match self.syntax.tokens[self.tokens[at]].kind {
            TokenKind::Ident
            | TokenKind::Int
            | TokenKind::Float
            | TokenKind::Text
            | TokenKind::Bytes
            | TokenKind::Char
            | TokenKind::Byte
            | TokenKind::TextLine => true,
            TokenKind::Keyword(keyword) => {
                matches!(keyword, Keyword::SelfValue | Keyword::True | Keyword::False)
            }
            TokenKind::Punct(punct) => {
                matches!(punct, Punct::RParen | Punct::RBracket | Punct::RBrace)
            }
            _ => false,
        }
    }

    /// The arguments in the last parentheses or brackets, labels left out, after the receiver
    /// when there are `count` operands, one more than the arguments.
    fn call(&self, count: usize) -> Option<Vec<String>> {
        let close = self.tokens.len().checked_sub(1)?;
        let bracket = self.word(close) == "]";
        if !bracket && self.word(close) != ")" {
            return None;
        }
        let open = self.opening(close)?;
        let mut texts = self.arguments(open, close)?;
        if count == texts.len() + 1 {
            let before = match bracket {
                true => open,
                false => {
                    let mut name = open.checked_sub(1)?;
                    if self.word(name) == "]" {
                        name = self.opening(name)?.checked_sub(1)?;
                    }
                    let dot = name.checked_sub(1)?;
                    (self.word(dot) == ".").then_some(dot)?
                }
            };
            texts.insert(0, self.text(0, before.checked_sub(1)?)?);
        }
        Some(texts)
    }

    /// The position of the bracket opening the one closing at `close`.
    fn opening(&self, close: usize) -> Option<usize> {
        (0..close)
            .rev()
            .find(|at| self.depths[*at] == self.depths[close])
    }

    /// The texts of the arguments between brackets at `open` and `close`, labels left out.
    fn arguments(&self, open: usize, close: usize) -> Option<Vec<String>> {
        let inner = self.depths[open] + 1;
        let mut texts = Vec::new();
        let mut first = open + 1;
        for at in open + 1..=close {
            if at == close || (self.depths[at] == inner && self.word(at) == ",") {
                if first < at {
                    let labelled = at - first > 2 && self.word(first + 1) == ":";
                    let start = if labelled { first + 2 } else { first };
                    texts.push(self.text(start, at - 1)?);
                }
                first = at + 1;
            }
        }
        Some(texts)
    }

    /// The value stored into the field `name`: what follows `=` at depth 0, or the argument
    /// labelled `name` in the last parentheses.
    fn field(&self, name: &str) -> Option<String> {
        let last = self.tokens.len().checked_sub(1)?;
        if let Some(at) = (0..=last).find(|at| self.depths[*at] == 0 && self.word(*at) == "=") {
            return self.text(at + 1, last);
        }
        let open = self.opening(last)?;
        let inner = self.depths[open] + 1;
        let label = (open + 1..last).find(|at| {
            self.depths[*at] == inner
                && self.word(*at) == name
                && self.word(at + 1) == ":"
                && matches!(self.word(at - 1), "(" | ",")
        })?;
        let end = (label + 2..last)
            .find(|at| self.depths[*at] == inner && self.word(*at) == ",")
            .unwrap_or(last);
        self.text(label + 2, end - 1)
    }
}

/// The symbol writing a binary operator.
fn op_symbol(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Rem => "%",
        BinaryOp::Shl => "<<",
        BinaryOp::Shr => ">>",
        BinaryOp::Band => "&",
        BinaryOp::Bor => "|",
        BinaryOp::Bxor => "^",
        BinaryOp::Eq => "==",
        BinaryOp::Ne => "!=",
        BinaryOp::Lt => "<",
        BinaryOp::Le => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::Ge => ">=",
    }
}

/// What a report writes for an operand's value.
fn shown(body: &mut Body, held: Held, nty: &NTy) -> Shown {
    let ty = nty.ty().clone();
    if let Some(scalar) = ty.scalar() {
        let repr = body.lo.owned_repr(&ty);
        let value = body.adapt(held, nty, repr).scalar();
        return Shown {
            bits: values::bits(body.fx, scalar, value),
            written: written(&ty),
            writer: None,
        };
    }
    if matches!(held, Held::Empty) || !values::writable(body.lo, &ty) {
        return Shown::hidden(body.fx);
    }
    let address = address_of(body, held, &ty);
    Shown::at(body.lo, &ty, address)
}

/// The address of a value held in memory, or in a slot holding a copy of it.
fn address_of(body: &mut Body, held: Held, ty: &Ty) -> IrValue {
    match held {
        Held::Address(address) => address,
        held => {
            let layout = body.lo.layout(ty);
            let slot = body.fx.slot(layout.size, layout.align);
            body.lo.store_held(body.fx, slot, held, ty);
            slot
        }
    }
}

/// How a report writes a value of a type.
pub fn written(ty: &Ty) -> Written {
    match ty.scalar() {
        None => Written::Hidden,
        Some(Scalar::F32) => Written::F32,
        Some(Scalar::F64) => Written::F64,
        Some(Scalar::Bool) => Written::Bool,
        Some(Scalar::Char) => Written::Char,
        Some(scalar) if scalar.is_signed() => Written::Signed,
        Some(_) => Written::Unsigned,
    }
}

/// The type of a literal's value.
fn literal_ty(lo: &Lowering, literal: &Literal) -> Ty {
    match literal {
        Literal::Int(_, scalar) | Literal::Float(_, scalar) => Ty::Scalar(*scalar),
        Literal::Bool(_) => Ty::Scalar(Scalar::Bool),
        Literal::Char(_) => Ty::Scalar(Scalar::Char),
        Literal::Text(_) => lo.std_ty("core", "Str", Vec::new()),
        Literal::Bytes(_) => lo.slice_of(Ty::Scalar(Scalar::U8)),
        Literal::Unit => Ty::Unit,
    }
}

/// The source text of an operand when its operation's text does not show it: an integer or
/// `bool` literal's, or the text the definition of its value spans.
fn operand_text(body: &Body, operand: &Operand) -> String {
    match operand {
        Operand::Literal(Literal::Int(value, _)) => value.to_string(),
        Operand::Literal(Literal::Bool(value)) => value.to_string(),
        Operand::Literal(_) => "_".to_owned(),
        Operand::Value(value) => body.value_text(*value),
        Operand::Place(_, place) => body.value_text(place.root),
    }
}
