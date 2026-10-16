//! The NIR text notation, in which the specification writes units.

use std::fmt::Write as _;

use super::{
    BlockId, Builtin, Call, Callable, Callee, Constant, FnRef, Function, Inst, Literal, NTy, Op,
    Operand, Payload, Place, Region, Role, Terminator, UnaryOp, Unit,
};
use crate::check::db::ItemId;
use crate::check::ty::{Arg, Const, Param, Ty};
use crate::syntax::ast::Mode;

/// How a unit's package writes the items, types and generic parameters its NIR names.
pub trait Names {
    /// An item's path in alias form; a function named after a type as `Type.name`.
    fn path(&self, item: ItemId) -> String;
    fn ty(&self, ty: &Ty) -> String;
    fn param(&self, param: Param) -> String;
    /// The name of an interface's method, by its index.
    fn method(&self, interface: ItemId, method: u32) -> String;
    /// The name of a register of a registers block, by its index.
    fn register(&self, block: ItemId, register: u32) -> String;
}

impl Role {
    pub fn word(self) -> &'static str {
        match self {
            Role::Body => "body",
            Role::Entry => "entry",
            Role::Closure => "closure",
            Role::Requires => "requires",
            Role::Ensures => "ensures",
            Role::Old => "old",
            Role::Example => "example",
            Role::Decreases => "decreases",
            Role::Invariant => "invariant",
            Role::Assume => "assume",
            Role::Predicate => "predicate",
            Role::Where => "where",
            Role::Default => "default",
        }
    }

    /// Whether the role's functions are numbered by an index.
    pub fn indexed(self) -> bool {
        !matches!(self, Role::Body | Role::Entry | Role::Decreases)
    }
}

/// A unit in the text notation.
pub fn written(unit: &Unit, names: &dyn Names) -> String {
    let mut text = format!("unit {}\n", names.path(unit.item));
    for function in &unit.functions {
        text.push('\n');
        Writer {
            names,
            function,
            text: &mut text,
        }
        .function();
    }
    text
}

struct Writer<'w> {
    names: &'w dyn Names,
    function: &'w Function,
    text: &'w mut String,
}

impl Writer<'_> {
    fn function(&mut self) {
        let function = self.function;
        let role = function.role;
        let mut head = format!("fn {}", role.word());
        if role.indexed() {
            write!(head, " {}", function.index).expect("a string");
        }
        if function
            .closure
            .as_ref()
            .is_some_and(|closure| closure.task)
        {
            head.push_str(" task");
        }
        let params: Vec<String> = function
            .params
            .iter()
            .map(|param| {
                format!(
                    "{} %{}: {}",
                    mode_word(param.mode),
                    param.value.0,
                    self.nty(function.ty(param.value))
                )
            })
            .collect();
        write!(head, " ({})", params.join(", ")).expect("a string");
        if function.result != Ty::Unit {
            write!(head, " -> {}", self.names.ty(&function.result)).expect("a string");
        }
        if let Some(fails) = &function.fails {
            write!(head, " fails {}", self.names.ty(fails)).expect("a string");
        }
        if function.wait {
            head.push_str(" uses wait");
        }
        if let Some(closure) = &function.closure {
            let limits = limits_written(&closure.limits);
            if !limits.is_empty() {
                write!(head, " limits {limits}").expect("a string");
            }
        }
        self.line(0, &head);
        let nested = nested_blocks(function);
        for (index, _) in function.blocks.iter().enumerate() {
            if !nested.contains(&BlockId(index as u32)) {
                self.block(BlockId(index as u32), 0);
            }
        }
    }

    fn line(&mut self, depth: usize, line: &str) {
        for _ in 0..depth {
            self.text.push_str("    ");
        }
        self.text.push_str(line);
        self.text.push('\n');
    }

    fn block(&mut self, id: BlockId, depth: usize) {
        let block = self.function.block(id);
        let params: Vec<String> = block
            .params
            .iter()
            .map(|value| format!("%{}: {}", value.0, self.nty(self.function.ty(*value))))
            .collect();
        let label = match params.is_empty() {
            true => format!("^{}:", id.0),
            false => format!("^{}({}):", id.0, params.join(", ")),
        };
        self.line(depth, &label);
        for inst in &block.insts {
            self.inst(inst, depth + 1);
        }
        let term = self.terminator(&block.term);
        self.line(depth + 1, &term);
    }

    fn inst(&mut self, inst: &Inst, depth: usize) {
        let mut line = String::new();
        if let Some(result) = inst.result {
            write!(line, "%{} = ", result.0).expect("a string");
        }
        if let Some(premise) = inst.trusted {
            write!(line, "trusted {premise} ").expect("a string");
        }
        let region = match &inst.op {
            Op::Quantifier { region, .. } | Op::When { region, .. } => Some(region),
            _ => None,
        };
        line.push_str(&self.op(&inst.op));
        match region {
            Some(region) => {
                line.push_str(" {");
                self.line(depth, &line);
                self.region(region, depth);
                self.line(depth, "}");
            }
            None => self.line(depth, &line),
        }
    }

    fn region(&mut self, region: &Region, depth: usize) {
        for block in &region.blocks {
            self.block(*block, depth);
        }
    }

    fn op(&self, op: &Op) -> String {
        match op {
            Op::Slot { name, ty } => match name {
                Some(name) => format!("slot {name}: {}", self.names.ty(ty)),
                None => format!("slot {}", self.names.ty(ty)),
            },
            Op::Load(place) => format!("load {}", place_written(place)),
            Op::Move(place) => format!("move {}", place_written(place)),
            Op::Ref(place) => format!("ref {}", place_written(place)),
            Op::RefMut(place) => format!("ref mut {}", place_written(place)),
            Op::Release(place) => format!("release {}", place_written(place)),
            Op::Init(place, operand) => {
                format!("init {}, {}", place_written(place), self.operand(operand))
            }
            Op::Assign(place, operand) => {
                format!("assign {}, {}", place_written(place), self.operand(operand))
            }
            Op::FnRef(fn_ref) => format!("fnref {}", self.fn_ref(fn_ref)),
            Op::Constant(constant) => match constant {
                Constant::Item(item) => format!("constant {}", self.names.path(*item)),
                Constant::Param(param) => format!("constant {}", self.names.param(*param)),
                Constant::Register(block, index) => format!(
                    "constant {}.{}",
                    self.names.path(*block),
                    self.names.register(*block, *index)
                ),
            },
            Op::Construct { ty, fields } => {
                format!("construct {}({})", self.names.ty(ty), self.labelled(fields))
            }
            Op::Variant {
                ty,
                variant,
                payload,
            } => {
                let payload = match payload {
                    Payload::None => String::new(),
                    Payload::Single(operand) => format!("({})", self.operand(operand)),
                    Payload::Labelled(fields) => format!("({})", self.labelled(fields)),
                };
                format!("variant {}.{variant}{payload}", self.enum_path(ty))
            }
            Op::Is { place, ty, variant } => format!(
                "is {}, {}.{variant}",
                place_written(place),
                self.enum_path(ty)
            ),
            Op::Closure { index, captures } => {
                format!("closure {index}({})", self.operands(captures))
            }
            Op::Convert { value, to, via } => {
                let via = via
                    .as_ref()
                    .map_or(String::new(), |variant| format!(".{variant}"));
                format!(
                    "convert {}, {}{via}",
                    self.operand(value),
                    self.names.ty(to)
                )
            }
            Op::Unary(op, operand) => {
                let word = match op {
                    UnaryOp::Neg => "neg",
                    UnaryOp::Inv => "inv",
                    UnaryOp::Not => "not",
                };
                format!("{word} {}", self.operand(operand))
            }
            Op::Binary(op, left, right) => format!(
                "{} {}, {}",
                op.word(),
                self.operand(left),
                self.operand(right)
            ),
            Op::Call(call) => self.call(call),
            Op::Check { kind, operand, .. } => {
                format!("check {}, {}", kind.word(), self.operand(operand))
            }
            Op::Assume(operand) => format!("assume {}", self.operand(operand)),
            Op::Dbg(operand) => format!("dbg {}", self.operand(operand)),
            Op::Quantifier {
                all,
                low,
                high,
                inclusive,
                ..
            } => format!(
                "{} {} {} {}",
                if *all { "all" } else { "any" },
                self.operand(low),
                if *inclusive { "..=" } else { "..<" },
                self.operand(high)
            ),
            Op::When {
                attempt, accesses, ..
            } => format!(
                "when{} ({})",
                if *attempt { " attempt" } else { "" },
                self.operands(accesses)
            ),
        }
    }

    /// The path of an enum type as `is` and `variant` write it, without its arguments.
    fn enum_path(&self, ty: &Ty) -> String {
        match ty {
            Ty::Item(item, _) => self.names.path(*item),
            _ => self.names.ty(ty),
        }
    }

    fn labelled(&self, fields: &[(String, Operand)]) -> String {
        let written: Vec<String> = fields
            .iter()
            .map(|(name, operand)| format!("{name}: {}", self.operand(operand)))
            .collect();
        written.join(", ")
    }

    fn operands(&self, operands: &[Operand]) -> String {
        let written: Vec<String> = operands
            .iter()
            .map(|operand| self.operand(operand))
            .collect();
        written.join(", ")
    }

    fn operand(&self, operand: &Operand) -> String {
        match operand {
            Operand::Value(value) => format!("%{}", value.0),
            Operand::Literal(literal) => literal_written(literal),
            Operand::Place(mode, place) => format!("{} {}", mode_word(*mode), place_written(place)),
        }
    }

    fn call(&self, call: &Call) -> String {
        let callee = match &call.callee {
            Callee::Fn(fn_ref) => self.fn_ref(fn_ref),
            Callee::Value(value) => format!("%{}", value.0),
            Callee::Dyn {
                place,
                interface,
                method,
            } => format!(
                "dyn {} {}.{}",
                place_written(place),
                self.names.path(*interface),
                self.names.method(*interface, *method)
            ),
        };
        format!(
            "call {}{callee}({})",
            if call.awaited { "await " } else { "" },
            self.operands(&call.args)
        )
    }

    fn fn_ref(&self, fn_ref: &FnRef) -> String {
        let mut text = match &fn_ref.callable {
            Callable::Item { item, role, index } => {
                let mut text = self.names.path(*item);
                if *role != super::Role::Body {
                    write!(text, " {}", role.word()).expect("a string");
                    if role.indexed() {
                        write!(text, " {index}").expect("a string");
                    }
                }
                text
            }
            Callable::Method { interface, method } => format!(
                "{}.{}",
                self.names.path(*interface),
                self.names.method(*interface, *method)
            ),
            Callable::Derived { owner, method } => {
                format!("{}.{method}", self.names.path(*owner))
            }
            Callable::Builtin(builtin) => match builtin {
                Builtin::Conversion(conversion, scalar) => {
                    format!("{}.{}", scalar.name(), conversion.word())
                }
                Builtin::Bits(conversion, owner) => {
                    let owner = match owner {
                        super::BitsOwner::Scalar(scalar) => scalar.name().to_owned(),
                        super::BitsOwner::Layout(item) => self.names.path(*item),
                    };
                    format!("{owner}.{}", conversion.word())
                }
                Builtin::Intrinsic(path) => (*path).to_owned(),
            },
        };
        if !fn_ref.args.is_empty() {
            let args: Vec<String> = fn_ref
                .args
                .iter()
                .map(|arg| match arg {
                    Arg::Ty(ty) => self.names.ty(ty),
                    Arg::Const(constant) => match constant {
                        Const::Int(value) => value.to_string(),
                        Const::Bool(value) => value.to_string(),
                        Const::Param(param) => self.names.param(*param),
                        Const::Error => "_".to_owned(),
                    },
                })
                .collect();
            write!(text, "[{}]", args.join(", ")).expect("a string");
        }
        text
    }

    fn terminator(&self, term: &Terminator) -> String {
        let target = |jump: &super::Jump| match jump.args.is_empty() {
            true => format!("^{}", jump.block.0),
            false => format!("^{}({})", jump.block.0, self.operands(&jump.args)),
        };
        match term {
            Terminator::Jump(jump) => format!("jump {}", target(jump)),
            Terminator::Branch(operand, then, otherwise) => format!(
                "branch {}, {}, {}",
                self.operand(operand),
                target(then),
                target(otherwise)
            ),
            Terminator::Call { call, ok, fail } => {
                format!("{} ok ^{} fail ^{}", self.call(call), ok.0, fail.0)
            }
            Terminator::Return(None) => "return".to_owned(),
            Terminator::Return(Some(operand)) => format!("return {}", self.operand(operand)),
            Terminator::Fail(operand) => format!("fail {}", self.operand(operand)),
            Terminator::Todo(intent) => format!("todo {}", text_literal(intent)),
            Terminator::Unreachable => "unreachable".to_owned(),
            Terminator::Yield(operand) => format!("yield {}", self.operand(operand)),
        }
    }

    fn nty(&self, ty: &NTy) -> String {
        match ty {
            NTy::Owned(ty) => self.names.ty(ty),
            NTy::Ref(ty) => format!("ref {}", self.names.ty(ty)),
            NTy::RefMut(ty) => format!("ref mut {}", self.names.ty(ty)),
        }
    }
}

/// The blocks of a function that belong to a region, written inside its instruction.
fn nested_blocks(function: &Function) -> Vec<BlockId> {
    let mut nested = Vec::new();
    for block in &function.blocks {
        for inst in &block.insts {
            if let Op::Quantifier { region, .. } | Op::When { region, .. } = &inst.op {
                nested.extend(region.blocks.iter().copied());
            }
        }
    }
    nested
}

fn mode_word(mode: Mode) -> &'static str {
    match mode {
        Mode::Read => "read",
        Mode::Mut => "mut",
        Mode::Take => "take",
    }
}

fn place_written(place: &Place) -> String {
    let mut text = format!("%{}", place.root.0);
    for step in &place.steps {
        write!(text, ".{step}").expect("a string");
    }
    text
}

fn limits_written(limits: &crate::check::ty::LimitSet) -> String {
    let mut words: Vec<String> = Vec::new();
    for (holds, word) in [
        (limits.pure, "pure"),
        (limits.notrap, "notrap"),
        (limits.terminates, "terminates"),
        (limits.noalloc, "noalloc"),
    ] {
        if holds {
            words.push(word.to_owned());
        }
    }
    if let Some(size) = limits.stack {
        words.push(format!("stack {size}"));
    }
    words.join(", ")
}

/// A literal as source writes it, with its literal suffix.
pub fn literal_written(literal: &Literal) -> String {
    match literal {
        Literal::Int(value, scalar) => format!("{value}{}", scalar.name()),
        Literal::Float(bits, scalar) => {
            let value = match scalar {
                crate::check::ty::Scalar::F32 => f64::from(f32::from_bits(*bits as u32)),
                _ => f64::from_bits(*bits),
            };
            let mut written = format!("{value:?}");
            if !written.contains(['.', 'e', 'N', 'i']) {
                written.push_str(".0");
            }
            format!("{written}{}", scalar.name())
        }
        Literal::Bool(value) => value.to_string(),
        Literal::Char(c) => match c {
            '\'' => "'\\''".to_owned(),
            _ => format!("'{}'", escaped(&c.to_string(), '\'')),
        },
        Literal::Text(text) => text_literal(text),
        Literal::Bytes(bytes) => {
            let mut written = String::from("b\"");
            for byte in bytes {
                match byte {
                    b'"' => written.push_str("\\\""),
                    b'\\' => written.push_str("\\\\"),
                    0x20..=0x7E => written.push(char::from(*byte)),
                    _ => write!(written, "\\x{byte:02X}").expect("a string"),
                }
            }
            written.push('"');
            written
        }
        Literal::Unit => "Unit".to_owned(),
    }
}

fn text_literal(text: &str) -> String {
    format!("\"{}\"", escaped(text, '"'))
}

fn escaped(text: &str, quote: char) -> String {
    let mut written = String::new();
    for c in text.chars() {
        match c {
            '\\' => written.push_str("\\\\"),
            '\n' => written.push_str("\\n"),
            '\t' => written.push_str("\\t"),
            c if c == quote => {
                written.push('\\');
                written.push(c);
            }
            c if c.is_control() => write!(written, "\\u{{{:X}}}", c as u32).expect("a string"),
            c => written.push(c),
        }
    }
    written
}
