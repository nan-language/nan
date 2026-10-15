//! The typed form of a body: each expression with its type and what it resolves to, the locals it
//! binds, and each call with its callee, arguments, modes and markers. Ownership, digests and
//! limits read bodies in this form.

use super::db::ItemId;
use super::ty::{Args, Bound, FnTy, LimitSet, Ty};
use crate::syntax::ast::{BinaryOp, FailureMarker, Mode, Tok, UnaryOp};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocalId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalKind {
    /// A parameter of the function, closure or test, received in its mode.
    Param(Mode),
    Let,
    Var,
    /// A name a pattern of a `let` or match arm binds.
    Pattern,
    /// A name a `for` pattern binds, in the loop's form.
    Element(Mode),
    /// A capture of a closure, in its mode.
    Capture(Mode),
    /// A quantifier variable.
    Quantified,
}

#[derive(Clone, Debug)]
pub struct Local {
    pub name: String,
    pub ty: Ty,
    pub kind: LocalKind,
    pub token: Tok,
    /// The local of the enclosing body a capture names.
    pub captured: Option<LocalId>,
    /// The closure declaring it, 0 for the item's own body.
    pub closure: u32,
}

impl Local {
    /// Whether modes allow assigning it or accessing it by `mut`.
    pub fn assignable(&self) -> bool {
        matches!(
            self.kind,
            LocalKind::Var
                | LocalKind::Param(Mode::Mut | Mode::Take)
                | LocalKind::Element(Mode::Mut | Mode::Take)
                | LocalKind::Capture(Mode::Mut | Mode::Take)
        )
    }

    /// Whether modes allow moving it.
    pub fn movable(&self) -> bool {
        matches!(
            self.kind,
            LocalKind::Var
                | LocalKind::Let
                | LocalKind::Pattern
                | LocalKind::Param(Mode::Take)
                | LocalKind::Element(Mode::Take)
        )
    }
}

/// What a call calls.
#[derive(Clone, Debug)]
pub enum Callee {
    /// A function item, with its generic arguments.
    Function { item: ItemId },
    /// An interface method, through a generic parameter's bound, `Dyn`, or the impl of `self_ty`.
    Method {
        interface: Bound,
        method: usize,
        self_ty: Ty,
        dynamic: bool,
    },
    /// A function value: a parameter, binding, field or other expression of function type.
    Value(Box<TExpr>),
    /// An operation the toolchain supplies.
    Builtin(Builtin),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Builtin {
    /// A named conversion or bit conversion.
    Conversion(crate::nir::Builtin),
    /// `Dyn[I].view`, `Dyn[I].view_mut` and `mem::Box[Dyn[I]].from_box`.
    DynView,
    /// `text::fmt`.
    Template,
    /// A DMA access call.
    DmaAccess,
    /// `hw::mint_root()`.
    MintRoot,
}

#[derive(Clone, Debug)]
pub struct TArg {
    pub expr: TExpr,
    /// The parameter's mode.
    pub mode: Mode,
}

#[derive(Clone, Debug)]
pub struct Call {
    pub token: Tok,
    pub callee: Callee,
    /// The callee's generic arguments: those of the type or interface it is a member of, then
    /// its own.
    pub type_args: Args,
    /// The arguments, the receiver first for a method call.
    pub args: Vec<TArg>,
    /// The function type of the head called, instantiated.
    pub head: FnTy,
    /// Whether the callee is a projection, and so its result a part, wrapped part or view value.
    pub proj: bool,
    pub failure: Option<(FailureMarker, Tok)>,
    /// Whether the call is marked `await`.
    pub awaited: bool,
}

#[derive(Clone, Debug)]
pub struct TExpr {
    pub token: Tok,
    pub ty: Ty,
    pub kind: TKind,
}

impl TExpr {
    pub fn failed(token: Tok) -> TExpr {
        TExpr {
            token,
            ty: Ty::Error,
            kind: TKind::Failed,
        }
    }

    /// Visits each expression this one holds directly, those of its blocks' statements included.
    pub fn each_child<'e>(&'e self, visit: &mut dyn FnMut(&'e TExpr)) {
        match &self.kind {
            TKind::Field { base: inner, .. }
            | TKind::Unary { operand: inner, .. }
            | TKind::Trusted(inner)
            | TKind::Dbg(inner)
            | TKind::Fail(inner)
            | TKind::Old(inner) => visit(inner),
            TKind::Call(call) | TKind::Index(call) => {
                if let Callee::Value(value) = &call.callee {
                    visit(value);
                }
                for arg in &call.args {
                    visit(&arg.expr);
                }
            }
            TKind::Construct { values, .. } => {
                for (_, value) in values {
                    visit(value);
                }
            }
            TKind::Binary { left, right, .. } => {
                visit(left);
                visit(right);
            }
            TKind::If {
                condition,
                then,
                otherwise,
            } => {
                visit(condition);
                then.each_child(visit);
                if let Some(otherwise) = otherwise {
                    visit(otherwise);
                }
            }
            TKind::Block(block) => block.each_child(visit),
            TKind::Match { scrutinee, arms } => {
                visit(scrutinee);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        visit(guard);
                    }
                    visit(&arm.body);
                }
            }
            TKind::Closure(closure) => visit(&closure.body),
            TKind::When { accesses, body, .. } => {
                for (_, cown, _) in accesses {
                    visit(cown);
                }
                visit(body);
            }
            TKind::Return(value) => {
                if let Some(value) = value {
                    visit(value);
                }
            }
            TKind::Quantifier { range, body, .. } => {
                visit(range);
                visit(body);
            }
            TKind::Failed
            | TKind::Literal(_)
            | TKind::Unit
            | TKind::Result
            | TKind::Register { .. }
            | TKind::ConstParam(_)
            | TKind::Local(_)
            | TKind::Value(_)
            | TKind::FnValue { .. }
            | TKind::Todo(_)
            | TKind::Break(_)
            | TKind::Continue(_) => {}
        }
    }
}

impl TBlock {
    /// Visits each expression the block's statements hold directly.
    pub fn each_child<'e>(&'e self, visit: &mut dyn FnMut(&'e TExpr)) {
        for stmt in &self.stmts {
            stmt.each_child(visit);
        }
    }
}

impl TStmt {
    /// Visits each expression the statement holds directly, those of its blocks included.
    pub fn each_child<'e>(&'e self, visit: &mut dyn FnMut(&'e TExpr)) {
        match &self.kind {
            TStmtKind::Let {
                pattern: _,
                value,
                otherwise,
            } => {
                visit(value);
                if let Some(otherwise) = otherwise {
                    otherwise.each_child(visit);
                }
            }
            TStmtKind::Var { value, .. }
            | TStmtKind::Discard(value)
            | TStmtKind::Defer(value)
            | TStmtKind::Undefer(value)
            | TStmtKind::Expect(value)
            | TStmtKind::Assume(value)
            | TStmtKind::Expr(value) => visit(value),
            TStmtKind::While {
                condition,
                invariants,
                body,
            } => {
                visit(condition);
                invariants.iter().for_each(&mut *visit);
                body.each_child(visit);
            }
            TStmtKind::For {
                iterated,
                invariants,
                body,
                ..
            } => {
                visit(iterated);
                invariants.iter().for_each(&mut *visit);
                body.each_child(visit);
            }
            TStmtKind::Loop { invariants, body } => {
                invariants.iter().for_each(&mut *visit);
                body.each_child(visit);
            }
            TStmtKind::Assign { target, value, .. } => {
                visit(target);
                visit(value);
            }
        }
    }
}

/// The value a literal denotes; a number takes the type of its expression.
#[derive(Clone, Debug, PartialEq)]
pub enum Lit {
    Int(i128),
    /// A float's bits, as wide as its type.
    Float(u64),
    Bool(bool),
    Char(char),
    Text(String),
    Bytes(Vec<u8>),
}

#[derive(Clone, Debug)]
pub enum TKind {
    /// A construct that failed a rule; nothing depends on it.
    Failed,
    Literal(Lit),
    Unit,
    /// `result` in an ensures clause.
    Result,
    /// `old(e)` in an ensures clause.
    Old(Box<TExpr>),
    /// `R.name`, a register of a registers block.
    Register {
        block: ItemId,
        index: usize,
    },
    /// A const generic parameter.
    ConstParam(super::ty::Param),
    Local(LocalId),
    /// A const, registry or contribution, which names a value and never a place.
    Value(ItemId),
    /// A function item used as a value, with its generic arguments.
    FnValue {
        item: ItemId,
        type_args: Args,
    },
    Field {
        base: Box<TExpr>,
        name: String,
    },
    Call(Box<Call>),
    /// `x[i]`, a call of `at` or `at_mut`.
    Index(Box<Call>),
    /// A construction of a struct, layout or variant (by its index), with its values in the
    /// order written; a single payload's value is unlabelled.
    Construct {
        variant: Option<usize>,
        values: Vec<(String, TExpr)>,
    },
    Unary {
        op: UnaryOp,
        operand: Box<TExpr>,
    },
    /// A binary operator, a range included.
    Binary {
        op: BinaryOp,
        left: Box<TExpr>,
        right: Box<TExpr>,
    },
    If {
        condition: Box<TExpr>,
        then: TBlock,
        otherwise: Option<Box<TExpr>>,
    },
    /// A block in expression position: an `else` block, a trusted block or an arm's block.
    Block(TBlock),
    Match {
        scrutinee: Box<TExpr>,
        arms: Vec<TArm>,
    },
    Closure(Box<TClosure>),
    /// `await when`, or `await when attempt` yielding a `Result`.
    When {
        attempt: bool,
        accesses: Vec<(Mode, TExpr, LocalId)>,
        body: Box<TExpr>,
    },
    Trusted(Box<TExpr>),
    /// `todo` with its intent.
    Todo(String),
    Dbg(Box<TExpr>),
    Return(Option<Box<TExpr>>),
    Break(usize),
    Continue(usize),
    Fail(Box<TExpr>),
    /// `all` or `any`, binding `local` to each value of the range.
    Quantifier {
        all: bool,
        local: LocalId,
        range: Box<TExpr>,
        body: Box<TExpr>,
    },
}

#[derive(Clone, Debug)]
pub struct TArm {
    pub token: Tok,
    pub patterns: Vec<TPattern>,
    pub guard: Option<TExpr>,
    pub body: TExpr,
}

#[derive(Clone, Debug)]
pub struct TPattern {
    pub token: Tok,
    pub ty: Ty,
    pub kind: TPatternKind,
}

/// The values a literal, constant or range pattern matches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueSpace {
    /// An inclusive interval of integers or char scalar values.
    Ints(i128, i128),
    Bool(bool),
    /// Text or bytes, which no finite set of values covers.
    Bytes(Vec<u8>),
    /// A value the checker does not know, as a failed constant's.
    Unknown,
}

#[derive(Clone, Debug)]
pub enum TPatternKind {
    Wildcard,
    Binding(LocalId),
    /// A literal, constant or range: it matches without moving or copying.
    Value(ValueSpace, ValueTest),
    /// A struct, layout, variant or `Unit` pattern: the variant's index, and each field's
    /// pattern.
    Fields(Option<usize>, Vec<(String, TPattern)>),
    Failed,
}

/// How a literal, constant or range pattern tests a value: equal to one, or at least a low bound
/// and at most a high one, or below it when the range excludes it.
#[derive(Clone, Debug)]
pub enum ValueTest {
    Eq(Compared),
    Range(Compared, Compared, bool),
}

/// A value a pattern compares with.
#[derive(Clone, Debug)]
pub enum Compared {
    Literal(Lit),
    Const(ItemId),
}

#[derive(Clone, Debug)]
pub struct TClosure {
    pub task: bool,
    pub params: Vec<LocalId>,
    pub captures: Vec<TCapture>,
    pub ty: FnTy,
    pub body: TExpr,
    pub index: u32,
    /// The loops of the enclosing code it lies in, which loops in it number after.
    pub loops: usize,
    /// For the task closure of a lexical spawn, the body closure of its scope.
    pub lent: Option<u32>,
    /// The limits the function type it is written for demands.
    pub demanded: LimitSet,
}

/// A capture: the closure's local, the enclosing local it names, and the mode and token of its
/// entry in the capture list.
#[derive(Clone, Debug)]
pub struct TCapture {
    pub local: LocalId,
    pub outer: LocalId,
    pub listed: Option<(Mode, Tok)>,
}

#[derive(Clone, Debug)]
pub struct TBlock {
    pub end: Tok,
    pub stmts: Vec<TStmt>,
}

#[derive(Clone, Debug)]
pub struct TStmt {
    pub token: Tok,
    pub kind: TStmtKind,
}

#[derive(Clone, Debug)]
pub enum TStmtKind {
    Let {
        pattern: TPattern,
        value: TExpr,
        otherwise: Option<TBlock>,
    },
    Var {
        local: LocalId,
        value: TExpr,
    },
    Discard(TExpr),
    While {
        condition: TExpr,
        invariants: Vec<TExpr>,
        body: TBlock,
    },
    For {
        pattern: TPattern,
        mode: Mode,
        /// The expression the cursor comes from, and the cursor's type.
        iterated: TExpr,
        cursor: Ty,
        invariants: Vec<TExpr>,
        body: TBlock,
    },
    Loop {
        invariants: Vec<TExpr>,
        body: TBlock,
    },
    Defer(TExpr),
    Undefer(TExpr),
    Expect(TExpr),
    Assume(TExpr),
    Expr(TExpr),
    /// `target = value`, or `target op= value` with the operator a compound assignment applies.
    Assign {
        target: TExpr,
        op: Option<BinaryOp>,
        value: TExpr,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClauseRole {
    Requires,
    Ensures,
    Decreases,
}

/// A requires, ensures or decreases clause in typed form, a contract term over the parameters.
#[derive(Clone, Debug)]
pub struct TClause {
    pub token: Tok,
    pub role: ClauseRole,
    pub disputed: bool,
    pub expr: TExpr,
}

/// An example clause, typed as the body of a function without parameters.
#[derive(Clone, Debug)]
pub struct TExample {
    pub token: Tok,
    pub disputed: bool,
    pub body: TBody,
}

/// A body in typed form.
#[derive(Clone, Debug, Default)]
pub struct TBody {
    pub locals: Vec<Local>,
    /// The parameters of the function or test, in order.
    pub params: Vec<LocalId>,
    /// The requires, ensures and decreases clauses, in clause order.
    pub clauses: Vec<TClause>,
    pub examples: Vec<TExample>,
    pub root: Option<TExpr>,
}

impl TBody {
    pub fn local(&self, id: LocalId) -> &Local {
        &self.locals[id.0 as usize]
    }
}
