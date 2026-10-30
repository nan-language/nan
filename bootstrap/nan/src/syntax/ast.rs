//! The abstract syntax of items, lowered from their trees: what the checker reads. Every node
//! keeps the index of its first token, from which its line and text are found.

mod expr;
mod item;
mod visit;

pub use item::lower;

/// The index of a token of the item's file.
pub type Tok = u32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    pub text: String,
    pub token: Tok,
}

/// A `::`-separated path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Path {
    pub segments: Vec<Name>,
}

impl Path {
    pub fn token(&self) -> Tok {
        self.segments[0].token
    }

    pub fn last(&self) -> &Name {
        self.segments.last().expect("a path has a segment")
    }

    pub fn written(&self) -> String {
        let segments: Vec<&str> = self
            .segments
            .iter()
            .map(|name| name.text.as_str())
            .collect();
        segments.join("::")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Private,
    Pkg,
    Pub,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mode {
    Read,
    Mut,
    Take,
}

impl Mode {
    pub fn word(self) -> &'static str {
        match self {
            Mode::Read => "",
            Mode::Mut => "mut",
            Mode::Take => "take",
        }
    }
}

#[derive(Clone, Debug)]
pub struct TypeExpr {
    pub token: Tok,
    pub last: Tok,
    pub kind: TypeKind,
}

#[derive(Clone, Debug)]
pub enum TypeKind {
    Path {
        path: Path,
        args: Option<Vec<GenericArg>>,
    },
    Fn(Box<FnType>),
}

#[derive(Clone, Debug)]
pub enum GenericArg {
    Type(TypeExpr),
    Int { negative: bool, token: Tok },
    Bool { value: bool, token: Tok },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FnKind {
    Plain,
    Task,
    Interrupt,
}

#[derive(Clone, Debug)]
pub struct FnType {
    pub kind: FnKind,
    pub params: Vec<FnTypeParam>,
    pub result: Option<TypeExpr>,
    pub fails: Option<TypeExpr>,
    pub uses: Option<Uses>,
    pub limits: Option<Limits>,
    pub convention: Option<Text>,
}

#[derive(Clone, Debug)]
pub struct FnTypeParam {
    pub mode: Mode,
    pub name: Name,
    pub ty: TypeExpr,
}

/// A `uses` clause: `wait`, or the unknown word written.
#[derive(Clone, Debug)]
pub struct Uses {
    pub token: Tok,
    pub word: Name,
}

impl Uses {
    pub fn is_wait(&self) -> bool {
        self.word.text == "wait"
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LimitKind {
    Pure,
    Notrap,
    Terminates,
    Noalloc,
    Class,
    Stack,
}

#[derive(Clone, Debug)]
pub struct Limit {
    pub kind: LimitKind,
    pub token: Tok,
    /// The `INT` of `stack N`.
    pub size: Option<Tok>,
    /// The kinds of an `irreversible` class.
    pub kinds: Vec<Tok>,
}

#[derive(Clone, Debug)]
pub struct Limits {
    pub limits: Vec<Limit>,
}

/// A `TEXT` token and its value.
#[derive(Clone, Debug)]
pub struct Text {
    pub token: Tok,
    pub value: String,
}

#[derive(Clone, Debug)]
pub struct GenericParam {
    pub name: Name,
    pub bounds: Vec<Bound>,
    pub where_clause: Option<Expr>,
}

#[derive(Clone, Debug)]
pub struct Bound {
    pub path: Path,
    pub args: Option<Vec<GenericArg>>,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub token: Tok,
    pub mode: Mode,
    pub kind: ParamKind,
}

#[derive(Clone, Debug)]
pub enum ParamKind {
    Named { name: Name, ty: TypeExpr },
    SelfValue,
}

impl Param {
    pub fn name(&self) -> &str {
        match &self.kind {
            ParamKind::Named { name, .. } => &name.text,
            ParamKind::SelfValue => "self",
        }
    }

    pub fn is_self(&self) -> bool {
        matches!(self.kind, ParamKind::SelfValue)
    }
}

/// The name of a function: `name`, or `Type.name` with the type's path.
#[derive(Clone, Debug)]
pub struct Label {
    pub owner: Option<Path>,
    pub name: Name,
}

impl Label {
    pub fn written(&self) -> String {
        match &self.owner {
            Some(owner) => format!("{}.{}", owner.written(), self.name.text),
            None => self.name.text.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Head {
    pub token: Tok,
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    /// The `)` closing the parameters.
    pub close: Tok,
    pub result: Option<TypeExpr>,
    pub fails: Option<TypeExpr>,
    pub uses: Option<Uses>,
    pub limits: Option<Limits>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FnModifier {
    Proj,
    Interrupt,
    Landing,
    Foreign,
    Validator,
}

#[derive(Clone, Debug)]
pub enum ClauseKind {
    Requires(Expr),
    Ensures(Expr),
    Example(Expr),
    Decreases(Expr),
    /// `todo "question"` without a clause: a missing specification.
    Missing,
    Waive {
        length: bool,
        reason: Text,
    },
    /// `undo` naming the undoer, as a path and the member it names through it, and its premise.
    Undo {
        target: Path,
        member: Option<Name>,
        premise: Text,
    },
}

#[derive(Clone, Debug)]
pub struct Clause {
    pub token: Tok,
    /// Whether `todo TEXT` precedes the clause it marks.
    pub disputed: bool,
    pub kind: ClauseKind,
}

#[derive(Clone, Debug)]
pub enum FnBody {
    Block(Block),
    Expr(Expr),
    /// A foreign fn's convention, or a std declaration's missing body.
    None,
}

#[derive(Clone, Debug)]
pub struct FnItem {
    pub modifier: Option<(FnModifier, Tok)>,
    pub label: Label,
    pub head: Head,
    pub convention: Option<Text>,
    pub clauses: Vec<Clause>,
    pub body: FnBody,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeModifier {
    Closed,
    Open,
    Linear,
    View,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub token: Tok,
    pub visibility: Visibility,
    pub name: Name,
    pub ty: TypeExpr,
    pub where_clause: Option<Expr>,
}

#[derive(Clone, Debug)]
pub enum StructMember {
    Field(Field),
    Invariant(Expr),
}

#[derive(Clone, Debug)]
pub struct StructItem {
    pub name: Name,
    pub generics: Vec<GenericParam>,
    pub derives: Vec<Name>,
    pub derives_token: Option<Tok>,
    pub members: Vec<StructMember>,
}

impl StructItem {
    pub fn fields(&self) -> impl Iterator<Item = &Field> {
        self.members.iter().filter_map(|member| match member {
            StructMember::Field(field) => Some(field),
            StructMember::Invariant(_) => None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct PayloadField {
    pub token: Tok,
    /// The type expression before `:`, which must be one identifier.
    pub label: Option<TypeExpr>,
    pub ty: TypeExpr,
}

#[derive(Clone, Debug)]
pub struct Variant {
    pub name: Name,
    pub payload: Option<Vec<PayloadField>>,
    pub from: bool,
}

#[derive(Clone, Debug)]
pub struct EnumItem {
    pub name: Name,
    pub generics: Vec<GenericParam>,
    pub derives: Vec<Name>,
    pub derives_token: Option<Tok>,
    pub variants: Vec<Variant>,
}

#[derive(Clone, Debug)]
pub struct Method {
    pub token: Tok,
    pub proj: bool,
    pub name: Name,
    pub head: Head,
    pub clauses: Vec<Clause>,
    pub body: Option<Block>,
}

#[derive(Clone, Debug)]
pub struct InterfaceItem {
    pub name: Name,
    pub generics: Vec<GenericParam>,
    pub methods: Vec<Method>,
}

#[derive(Clone, Debug)]
pub struct ImplParam {
    pub name: Name,
    pub bounds: Vec<Bound>,
}

#[derive(Clone, Debug)]
pub struct ImplItem {
    pub target: Path,
    pub params: Option<Vec<ImplParam>>,
    pub interface: Bound,
}

#[derive(Clone, Debug)]
pub enum LayoutRepr {
    Bytes { size: Tok, order: Option<Name> },
    Integer { ty: Name, order: Option<Name> },
}

#[derive(Clone, Debug)]
pub enum LayoutFieldKind {
    Bit { low: Tok, high: Option<Tok> },
    Rest,
    Typed { ty: TypeExpr, at: Tok },
}

#[derive(Clone, Debug)]
pub struct LayoutField {
    pub name: Name,
    pub kind: LayoutFieldKind,
}

#[derive(Clone, Debug)]
pub struct Tag {
    pub name: Name,
    pub value: Tok,
}

#[derive(Clone, Debug)]
pub struct LayoutItem {
    pub name: Name,
    pub repr: LayoutRepr,
    /// Tags of a tags layout; none for a layout of fields.
    pub tags: Option<Vec<Tag>>,
    pub fields: Vec<LayoutField>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    ReadWrite,
    Write,
    WriteOneClear,
}

#[derive(Clone, Debug)]
pub struct Register {
    pub name: Name,
    pub ty: TypeExpr,
    pub at: Tok,
    pub stride: Option<Tok>,
    pub access: Access,
}

#[derive(Clone, Debug)]
pub struct RegistersItem {
    pub name: Name,
    pub registers: Vec<Register>,
}

#[derive(Clone, Debug)]
pub struct ConstItem {
    pub name: Name,
    pub ty: Option<TypeExpr>,
    pub value: Expr,
}

#[derive(Clone, Debug)]
pub struct RegistryItem {
    pub name: Name,
    pub entry: TypeExpr,
    pub key: Name,
    pub unique: Vec<Name>,
}

#[derive(Clone, Debug)]
pub struct ContributeItem {
    pub registry: Path,
    pub name: Name,
    pub value: Expr,
}

#[derive(Clone, Debug)]
pub struct TestItem {
    /// Whether `todo TEXT` precedes the test, which disputes it.
    pub disputed: bool,
    pub name: Name,
    /// The test target: a path, and the member it names through it.
    pub target: Path,
    pub member: Option<Name>,
    pub params: Option<Vec<Param>>,
    pub body: Block,
}

#[derive(Clone, Debug)]
pub enum RetiredNew {
    Manual,
    Rewrite { expr: Expr, preserves: bool },
}

#[derive(Clone, Debug)]
pub struct RetiredItem {
    pub path: Path,
    pub member: Option<Name>,
    pub head: Option<Head>,
    pub new: RetiredNew,
}

#[derive(Clone, Debug)]
pub enum ItemKind {
    Fn(FnItem),
    Struct(StructItem),
    Enum(EnumItem),
    Interface(InterfaceItem),
    Impl(ImplItem),
    Layout(LayoutItem),
    Registers(RegistersItem),
    Const(ConstItem),
    ConstAssert(Expr),
    Registry(RegistryItem),
    Contribute(ContributeItem),
    Test(TestItem),
    Retired(RetiredItem),
    Package,
}

#[derive(Clone, Debug)]
pub struct Item {
    /// The first token of the item's head, after its doc comment.
    pub token: Tok,
    pub visibility: Visibility,
    pub type_modifiers: Vec<(TypeModifier, Tok)>,
    pub kind: ItemKind,
}

impl ItemKind {
    /// Whether the item is a function with a body.
    pub fn is_fn_with_body(&self) -> bool {
        matches!(self, ItemKind::Fn(function) if !matches!(function.body, FnBody::None))
    }
}

impl Item {
    pub fn has_modifier(&self, modifier: TypeModifier) -> bool {
        self.type_modifiers
            .iter()
            .any(|(written, _)| *written == modifier)
    }

    /// The name the item declares, `Type.name` for a function named after a type.
    pub fn name(&self) -> Option<String> {
        Some(match &self.kind {
            ItemKind::Fn(item) => item.label.written(),
            ItemKind::Struct(item) => item.name.text.clone(),
            ItemKind::Enum(item) => item.name.text.clone(),
            ItemKind::Interface(item) => item.name.text.clone(),
            ItemKind::Layout(item) => item.name.text.clone(),
            ItemKind::Registers(item) => item.name.text.clone(),
            ItemKind::Const(item) => item.name.text.clone(),
            ItemKind::Registry(item) => item.name.text.clone(),
            ItemKind::Contribute(item) => item.name.text.clone(),
            ItemKind::Test(item) => item.name.text.clone(),
            ItemKind::Impl(_)
            | ItemKind::ConstAssert(_)
            | ItemKind::Retired(_)
            | ItemKind::Package => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Block {
    pub token: Tok,
    /// The closing `}`.
    pub end: Tok,
    pub stmts: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub struct LoopBlock {
    pub invariants: Vec<Expr>,
    pub block: Block,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOp {
    Set,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

impl AssignOp {
    /// The binary operator a compound assignment applies.
    pub fn binary(self) -> Option<BinaryOp> {
        Some(match self {
            AssignOp::Set => return None,
            AssignOp::Add => BinaryOp::Add,
            AssignOp::Sub => BinaryOp::Sub,
            AssignOp::Mul => BinaryOp::Mul,
            AssignOp::Div => BinaryOp::Div,
            AssignOp::Rem => BinaryOp::Rem,
            AssignOp::BitAnd => BinaryOp::BitAnd,
            AssignOp::BitOr => BinaryOp::BitOr,
            AssignOp::BitXor => BinaryOp::BitXor,
            AssignOp::Shl => BinaryOp::Shl,
            AssignOp::Shr => BinaryOp::Shr,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Stmt {
    pub token: Tok,
    pub kind: StmtKind,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    Let {
        pattern: Pattern,
        ty: Option<TypeExpr>,
        value: Expr,
        otherwise: Option<Block>,
    },
    Var {
        name: Name,
        ty: Option<TypeExpr>,
        value: Expr,
    },
    Discard(Expr),
    While {
        condition: Expr,
        body: LoopBlock,
    },
    For {
        pattern: Pattern,
        iterated: Expr,
        body: LoopBlock,
    },
    Loop {
        label: Option<Name>,
        body: LoopBlock,
    },
    Defer(Expr),
    Undefer(Expr),
    Expect(Expr),
    Assume {
        condition: Expr,
        reason: Text,
    },
    Expr(Expr),
    Assign {
        target: Expr,
        op: AssignOp,
        op_token: Tok,
        value: Expr,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Mul,
    Div,
    Rem,
    Add,
    Sub,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    RangeExclusive,
    RangeInclusive,
}

impl BinaryOp {
    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge
        )
    }

    pub fn is_range(self) -> bool {
        matches!(self, BinaryOp::RangeExclusive | BinaryOp::RangeInclusive)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    BitNot,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiteralKind {
    Int,
    Float,
    Text,
    TextRun,
    Bytes,
    Char,
    Byte,
    True,
    False,
}

#[derive(Clone, Debug)]
pub struct Literal {
    pub kind: LiteralKind,
    /// The literal's tokens: one, or each line of a text run.
    pub tokens: Vec<Tok>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureMarker {
    Try,
    Attempt,
}

#[derive(Clone, Debug)]
pub struct Arg {
    pub token: Tok,
    /// The expression before `:`, which must be one identifier.
    pub label: Option<Expr>,
    pub value: Expr,
}

impl Arg {
    /// The label as a name, when it is one identifier.
    pub fn label_name(&self) -> Option<&Name> {
        match &self.label.as_ref()?.kind {
            ExprKind::Path { path, args: None } if path.segments.len() == 1 => {
                Some(&path.segments[0])
            }
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum ArmPatterns {
    Patterns(Vec<Pattern>),
    Else,
}

#[derive(Clone, Debug)]
pub struct Arm {
    pub token: Tok,
    pub patterns: ArmPatterns,
    pub guard: Option<Expr>,
    pub body: ArmBody,
}

#[derive(Clone, Debug)]
pub enum ArmBody {
    Block(Block),
    Expr(Expr),
}

#[derive(Clone, Debug)]
pub struct Capture {
    pub mode: Mode,
    pub token: Tok,
    pub name: Name,
}

#[derive(Clone, Debug)]
pub enum ClosureBody {
    Short(Box<Expr>),
    Block(Block),
}

#[derive(Clone, Debug)]
pub struct Closure {
    pub captures: Vec<Capture>,
    pub params: Vec<FnTypeParam>,
    /// The `)` closing the parameters.
    pub close: Tok,
    pub result: Option<TypeExpr>,
    pub fails: Option<TypeExpr>,
    pub uses: Option<Uses>,
    pub limits: Option<Limits>,
    pub body: ClosureBody,
}

#[derive(Clone, Debug)]
pub struct WhenAccess {
    pub token: Tok,
    pub mode: Mode,
    /// The place root and its field names.
    pub root: Name,
    pub fields: Vec<Name>,
}

#[derive(Clone, Debug)]
pub enum WhenBody {
    Block(Block),
    Expr(Box<Expr>),
}

#[derive(Clone, Debug)]
pub enum TrustedBody {
    Expr(Box<Expr>),
    Block(Block),
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub token: Tok,
    /// The expression's last token.
    pub last: Tok,
    pub kind: ExprKind,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Literal(Literal),
    Path {
        path: Path,
        args: Option<Vec<GenericArg>>,
    },
    SelfValue,
    Paren(Box<Expr>),
    If {
        condition: Box<Expr>,
        then: Block,
        otherwise: Option<Box<Else>>,
    },
    Match {
        first: bool,
        scrutinee: Box<Expr>,
        arms: Vec<Arm>,
    },
    Closure(Box<Closure>),
    Trusted {
        premise: Text,
        body: TrustedBody,
    },
    Todo(Text),
    Dbg(Box<Expr>),
    /// `all` or `any`, binding `name` to each value of `range`.
    Quantifier {
        all: bool,
        name: Name,
        range: Box<Expr>,
        body: Box<Expr>,
    },
    Field {
        base: Box<Expr>,
        name: Name,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Arg>,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        op_token: Tok,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// Markers before a postfix expression or `when`, in the order written.
    Marked {
        failure: Option<(FailureMarker, Tok)>,
        awaited: Option<Tok>,
        mode: Option<(Mode, Tok)>,
        inner: Box<Expr>,
    },
    When {
        accesses: Vec<WhenAccess>,
        body: WhenBody,
    },
    Return(Option<Box<Expr>>),
    Break(Option<Name>),
    Continue(Option<Name>),
    Fail(Box<Expr>),
}

#[derive(Clone, Debug)]
pub enum Else {
    If(Expr),
    Block(Block),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeKind {
    Exclusive,
    Inclusive,
}

#[derive(Clone, Debug)]
pub enum PatternLiteral {
    Number { negative: bool, literal: Literal },
    Other(Literal),
}

impl PatternLiteral {
    pub fn literal(&self) -> &Literal {
        match self {
            PatternLiteral::Number { literal, .. } | PatternLiteral::Other(literal) => literal,
        }
    }
}

#[derive(Clone, Debug)]
pub enum RangeBound {
    Literal(PatternLiteral),
    Path(Path),
}

#[derive(Clone, Debug)]
pub struct FieldPattern {
    pub token: Tok,
    /// The pattern before `:`, which must be one identifier.
    pub label: Option<Pattern>,
    pub pattern: Pattern,
}

impl FieldPattern {
    pub fn label_name(&self) -> Option<&Name> {
        match &self.label.as_ref()?.kind {
            PatternKind::Named {
                path,
                member: None,
                fields: None,
                range: None,
            } if path.segments.len() == 1 => Some(&path.segments[0]),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Pattern {
    pub token: Tok,
    pub kind: PatternKind,
}

#[derive(Clone, Debug)]
pub enum PatternKind {
    Wildcard,
    Literal {
        literal: PatternLiteral,
        range: Option<(RangeKind, RangeBound)>,
    },
    Named {
        path: Path,
        member: Option<Name>,
        fields: Option<Vec<FieldPattern>>,
        range: Option<(RangeKind, RangeBound)>,
    },
}

#[cfg(test)]
mod tests;
