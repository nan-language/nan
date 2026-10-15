//! NIR: the typed static single assignment form of checked source, which the prover and the
//! interpreter read. A unit holds the NIR functions of one item; each function numbers its blocks
//! and values as elaboration orders them, and keeps the type of every value.

#[cfg(test)]
mod text;

use crate::check::db::ItemId;
use crate::check::ty::{Args, Scalar, Ty};
use crate::syntax::ast::{Mode, Tok};

#[cfg(test)]
pub use text::{Names, written};

/// The role of a NIR function in its unit, in the role table's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    Body,
    Entry,
    Closure,
    Requires,
    Ensures,
    Old,
    Example,
    Decreases,
    Invariant,
    Assume,
    Predicate,
    Where,
    Default,
}

impl Role {
    /// Whether its obligation sites are well-definedness obligations of contract terms.
    pub fn is_contract(self) -> bool {
        !matches!(
            self,
            Role::Body | Role::Entry | Role::Closure | Role::Example | Role::Default
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Value(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u32);

/// A NIR type: a source type, or a reference designating a place of one.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NTy {
    Owned(Ty),
    Ref(Ty),
    RefMut(Ty),
}

impl NTy {
    /// The type of the value, or of the place a reference designates.
    pub fn ty(&self) -> &Ty {
        match self {
            NTy::Owned(ty) | NTy::Ref(ty) | NTy::RefMut(ty) => ty,
        }
    }

    pub fn is_reference(&self) -> bool {
        !matches!(self, NTy::Owned(_))
    }
}

/// A value and the steps from it to a part: field names, and variant names before the payload
/// they reach, told apart by their naming conventions.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Place {
    pub root: Value,
    pub steps: Vec<String>,
}

impl Place {
    pub fn of(root: Value) -> Place {
        Place {
            root,
            steps: Vec::new(),
        }
    }

    pub fn step(&self, name: &str) -> Place {
        let mut steps = self.steps.clone();
        steps.push(name.to_owned());
        Place {
            root: self.root,
            steps,
        }
    }
}

/// Whether a step names a variant, by its PascalCase spelling.
pub fn is_variant_step(step: &str) -> bool {
    step.starts_with(|c: char| c.is_ascii_uppercase())
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Literal {
    Int(i128, Scalar),
    /// A float's bits, as wide as its type.
    Float(u64, Scalar),
    Bool(bool),
    Char(char),
    Text(String),
    Bytes(Vec<u8>),
    Unit,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Operand {
    Value(Value),
    Literal(Literal),
    /// A place passed with a mode; `take` moves it when the call starts.
    Place(Mode, Place),
}

/// An operation the toolchain supplies that no item declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Builtin {
    /// A named conversion to a number type: `T.widen`, `T.fit` and the others.
    Conversion(Conversion, Scalar),
    /// `T.to_bits`, `T.from_bits` or `T.checked_from_bits` of a scalar or layout type.
    Bits(BitConversion, BitsOwner),
    /// An intrinsic the toolchain supplies, by its path: `text::fmt`, `Dyn.view`,
    /// `hw::mint_root` or a DMA access.
    Intrinsic(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Conversion {
    Widen,
    Fit,
    CheckedFit,
    Truncate,
    Saturate,
    Nearest,
}

impl Conversion {
    pub fn word(self) -> &'static str {
        match self {
            Conversion::Widen => "widen",
            Conversion::Fit => "fit",
            Conversion::CheckedFit => "checked_fit",
            Conversion::Truncate => "truncate",
            Conversion::Saturate => "saturate",
            Conversion::Nearest => "nearest",
        }
    }

    pub fn named(name: &str) -> Option<Conversion> {
        Some(match name {
            "widen" => Conversion::Widen,
            "fit" => Conversion::Fit,
            "checked_fit" => Conversion::CheckedFit,
            "truncate" => Conversion::Truncate,
            "saturate" => Conversion::Saturate,
            "nearest" => Conversion::Nearest,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BitConversion {
    To,
    From,
    CheckedFrom,
}

impl BitConversion {
    pub fn word(self) -> &'static str {
        match self {
            BitConversion::To => "to_bits",
            BitConversion::From => "from_bits",
            BitConversion::CheckedFrom => "checked_from_bits",
        }
    }

    pub fn named(name: &str) -> Option<BitConversion> {
        Some(match name {
            "to_bits" => BitConversion::To,
            "from_bits" => BitConversion::From,
            "checked_from_bits" => BitConversion::CheckedFrom,
            _ => return None,
        })
    }
}

/// The type whose bit conversion is called.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BitsOwner {
    Scalar(Scalar),
    Layout(ItemId),
}

/// What a function reference calls.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Callable {
    /// A NIR function of an item's unit; the body of a function item, or of a std function the
    /// interpreter supplies.
    Item {
        item: ItemId,
        role: Role,
        index: u32,
    },
    /// An interface method called through a bound, its `Self` first among the arguments.
    Method {
        interface: ItemId,
        method: u32,
    },
    /// The method a derive writes for a type.
    Derived {
        owner: ItemId,
        method: &'static str,
    },
    Builtin(Builtin),
}

/// A function with its type arguments in full.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FnRef {
    pub callable: Callable,
    pub args: Args,
}

impl FnRef {
    pub fn item(item: ItemId, args: Args) -> FnRef {
        FnRef {
            callable: Callable::Item {
                item,
                role: Role::Body,
                index: 0,
            },
            args,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Callee {
    Fn(FnRef),
    /// An indirect call through a function value.
    Value(Value),
    /// Method `method` of `interface` on the `Dyn` at a place.
    Dyn {
        place: Place,
        interface: ItemId,
        method: u32,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub awaited: bool,
    pub callee: Callee,
    pub args: Vec<Operand>,
}

/// What a `constant` instruction names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Constant {
    /// A const item or contribution.
    Item(ItemId),
    /// A const parameter, by its position among the unit's generic parameters.
    Param(crate::check::ty::Param),
    /// A register of a registers block, by its index there: an `hw::Register` value.
    Register(ItemId, u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Neg,
    Inv,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
    Band,
    Bor,
    Bxor,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl BinaryOp {
    pub fn word(self) -> &'static str {
        match self {
            BinaryOp::Add => "add",
            BinaryOp::Sub => "sub",
            BinaryOp::Mul => "mul",
            BinaryOp::Div => "div",
            BinaryOp::Rem => "rem",
            BinaryOp::Shl => "shl",
            BinaryOp::Shr => "shr",
            BinaryOp::Band => "band",
            BinaryOp::Bor => "bor",
            BinaryOp::Bxor => "bxor",
            BinaryOp::Eq => "eq",
            BinaryOp::Ne => "ne",
            BinaryOp::Lt => "lt",
            BinaryOp::Le => "le",
            BinaryOp::Gt => "gt",
            BinaryOp::Ge => "ge",
        }
    }

    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge
        )
    }
}

/// The kinds of `check`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CheckKind {
    Requires,
    Ensures,
    Predicate,
    Expect,
    Invariant,
    Decreases,
}

impl CheckKind {
    pub fn word(self) -> &'static str {
        match self {
            CheckKind::Requires => "requires",
            CheckKind::Ensures => "ensures",
            CheckKind::Predicate => "predicate",
            CheckKind::Expect => "expect",
            CheckKind::Invariant => "invariant",
            CheckKind::Decreases => "decreases",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Payload {
    None,
    Single(Operand),
    Labelled(Vec<(String, Operand)>),
}

/// The blocks of a region, the first receiving its parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub blocks: Vec<BlockId>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Slot {
        name: Option<String>,
        ty: Ty,
    },
    Load(Place),
    Move(Place),
    Ref(Place),
    RefMut(Place),
    Init(Place, Operand),
    Assign(Place, Operand),
    Release(Place),
    FnRef(FnRef),
    Constant(Constant),
    Construct {
        ty: Ty,
        fields: Vec<(String, Operand)>,
    },
    Variant {
        ty: Ty,
        variant: String,
        payload: Payload,
    },
    Is {
        place: Place,
        ty: Ty,
        variant: String,
    },
    Closure {
        index: u32,
        captures: Vec<Operand>,
    },
    /// Failure conversion into `to`, through its from variant `via`.
    Convert {
        value: Operand,
        to: Ty,
        via: Option<String>,
    },
    Unary(UnaryOp, Operand),
    Binary(BinaryOp, Operand, Operand),
    Call(Call),
    /// An obligation that the operand is `true`; an open one is never checked.
    Check {
        kind: CheckKind,
        operand: Operand,
        open: bool,
    },
    Assume(Operand),
    Quantifier {
        all: bool,
        low: Operand,
        high: Operand,
        inclusive: bool,
        region: Region,
    },
    When {
        attempt: bool,
        accesses: Vec<Operand>,
        region: Region,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Inst {
    pub result: Option<Value>,
    /// The index of the premise of the trusted code it comes from.
    pub trusted: Option<u32>,
    pub op: Op,
    pub span: Span,
}

/// A target block with the operands its parameters receive.
#[derive(Clone, Debug, PartialEq)]
pub struct Jump {
    pub block: BlockId,
    pub args: Vec<Operand>,
}

impl Jump {
    pub fn to(block: BlockId) -> Jump {
        Jump {
            block,
            args: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Terminator {
    Jump(Jump),
    /// Continues at the first target when the operand is `true`, else at the second.
    Branch(Operand, Jump, Jump),
    /// A call that may fail, continuing at `ok` with its result or at `fail` with its failure,
    /// each that block's one parameter.
    Call {
        call: Call,
        ok: BlockId,
        fail: BlockId,
    },
    Return(Option<Operand>),
    Fail(Operand),
    Todo(String),
    Unreachable,
    Yield(Operand),
}

impl Terminator {
    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            Terminator::Jump(jump) => vec![jump.block],
            Terminator::Branch(_, then, otherwise) => vec![then.block, otherwise.block],
            Terminator::Call { ok, fail, .. } => vec![*ok, *fail],
            _ => Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub params: Vec<Value>,
    pub insts: Vec<Inst>,
    pub term: Terminator,
    pub term_span: Span,
}

/// The source range a part of a unit comes from, by the tokens of its item's file it spans.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub first: Tok,
    pub last: Tok,
}

impl Span {
    pub fn at(token: Tok) -> Span {
        Span {
            first: token,
            last: token,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub mode: Mode,
    pub value: Value,
    pub name: String,
    pub span: Span,
}

/// The head of a closure function: whether it is a task closure, and its limits.
#[derive(Clone, Debug, PartialEq)]
pub struct ClosureHead {
    pub task: bool,
    pub limits: crate::check::ty::LimitSet,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Function {
    pub role: Role,
    pub index: u32,
    pub params: Vec<Param>,
    pub result: Ty,
    pub fails: Option<Ty>,
    pub wait: bool,
    pub closure: Option<ClosureHead>,
    /// Its blocks by number, the entry first.
    pub blocks: Vec<Block>,
    /// The type of each value, by number.
    pub types: Vec<NTy>,
    pub span: Span,
}

impl Function {
    pub fn block(&self, id: BlockId) -> &Block {
        &self.blocks[id.0 as usize]
    }

    pub fn ty(&self, value: Value) -> &NTy {
        &self.types[value.0 as usize]
    }
}

/// The NIR of one item.
#[derive(Clone, Debug, PartialEq)]
pub struct Unit {
    pub item: ItemId,
    /// Its functions by role, in the role table's order, then by index.
    pub functions: Vec<Function>,
}

impl Unit {
    pub fn function(&self, role: Role, index: u32) -> Option<&Function> {
        self.functions
            .iter()
            .find(|function| function.role == role && function.index == index)
    }
}
