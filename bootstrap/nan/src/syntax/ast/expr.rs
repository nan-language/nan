//! Lowering statements, expressions and patterns from their trees.

use super::item::Lower;
use super::*;
use crate::syntax::grammar::{Keyword, Punct, Rule};
use crate::syntax::token::TokenKind;
use crate::syntax::tree::{Child, NodeId};

/// An operand or operator of an operator expression, in source order.
enum Piece {
    Operand(Expr),
    Operator(BinaryOp, Tok),
}

impl Lower<'_> {
    /// The statements directly under `node`.
    pub fn stmts(&self, node: NodeId) -> Vec<Stmt> {
        self.nodes(node)
            .into_iter()
            .filter(|stmt| self.rule(*stmt) == Rule::Stmt)
            .map(|stmt| self.stmt(self.nodes(stmt)[0]))
            .collect()
    }

    fn block(&self, node: NodeId) -> Block {
        let tokens = self.tokens(node);
        Block {
            token: tokens[0],
            end: *tokens.last().expect("a block closes"),
            stmts: self.stmts(node),
        }
    }

    fn loop_block(&self, node: NodeId) -> LoopBlock {
        LoopBlock {
            invariants: self
                .nodes(node)
                .into_iter()
                .filter(|line| self.rule(*line) == Rule::LoopInvariant)
                .filter_map(|line| self.find(line, Rule::Expr))
                .map(|expr| self.expr(expr))
                .collect(),
            block: self.block(node),
        }
    }

    /// A statement, from the node of its kind's rule.
    pub fn stmt(&self, node: NodeId) -> Stmt {
        let token = self.first(node);
        let expr_at = |index: usize| {
            let exprs: Vec<NodeId> = self
                .nodes(node)
                .into_iter()
                .filter(|child| self.rule(*child) == Rule::Expr)
                .collect();
            self.expr(exprs[index])
        };
        let kind = match self.rule(node) {
            Rule::LetStmt => StmtKind::Let {
                pattern: self.pattern(self.find(node, Rule::Pattern).expect("a pattern")),
                ty: self.find(node, Rule::TypeExpr).map(|ty| self.type_expr(ty)),
                value: expr_at(0),
                otherwise: self.find(node, Rule::Block).map(|block| self.block(block)),
            },
            Rule::VarStmt => StmtKind::Var {
                name: self.name(self.idents(node)[0]),
                ty: self.find(node, Rule::TypeExpr).map(|ty| self.type_expr(ty)),
                value: expr_at(0),
            },
            Rule::DiscardStmt => StmtKind::Discard(expr_at(0)),
            Rule::WhileStmt => StmtKind::While {
                condition: expr_at(0),
                body: self.loop_block(self.find(node, Rule::LoopBlock).expect("a loop block")),
            },
            Rule::ForStmt => StmtKind::For {
                pattern: self.pattern(self.find(node, Rule::Pattern).expect("a pattern")),
                iterated: expr_at(0),
                body: self.loop_block(self.find(node, Rule::LoopBlock).expect("a loop block")),
            },
            Rule::LoopStmt => StmtKind::Loop {
                label: self.idents(node).get(1).map(|token| self.name(*token)),
                body: self.loop_block(self.find(node, Rule::LoopBlock).expect("a loop block")),
            },
            Rule::DeferStmt => {
                StmtKind::Defer(self.call_expr(self.find(node, Rule::CallExpr).expect("a call")))
            }
            Rule::UndeferStmt => {
                StmtKind::Undefer(self.place(self.find(node, Rule::Place).expect("a place")))
            }
            Rule::ExpectStmt => StmtKind::Expect(expr_at(0)),
            Rule::AssumeStmt => StmtKind::Assume {
                condition: expr_at(0),
                reason: self.text_of(self.tokens_of(node, TokenKind::Text)[0]),
            },
            Rule::TrustedLeadStmt => return self.stmt(self.nodes(node)[0]),
            _ => self.expr_stmt(node),
        };
        Stmt { token, kind }
    }

    /// An expression statement, or an assignment, from an `expr_stmt` node.
    fn expr_stmt(&self, node: NodeId) -> StmtKind {
        let exprs: Vec<NodeId> = self
            .nodes(node)
            .into_iter()
            .filter(|child| self.rule(*child) == Rule::Expr)
            .collect();
        let target = self.expr(exprs[0]);
        let Some(operator) = self.find(node, Rule::AssignOperator) else {
            return StmtKind::Expr(target);
        };
        let op_token = self.tokens(operator)[0];
        let op = match self.kind(op_token) {
            TokenKind::Punct(Punct::PlusEq) => AssignOp::Add,
            TokenKind::Punct(Punct::MinusEq) => AssignOp::Sub,
            TokenKind::Punct(Punct::StarEq) => AssignOp::Mul,
            TokenKind::Punct(Punct::SlashEq) => AssignOp::Div,
            TokenKind::Punct(Punct::PercentEq) => AssignOp::Rem,
            TokenKind::Punct(Punct::AmpEq) => AssignOp::BitAnd,
            TokenKind::Punct(Punct::PipeEq) => AssignOp::BitOr,
            TokenKind::Punct(Punct::CaretEq) => AssignOp::BitXor,
            TokenKind::Punct(Punct::ShlEq) => AssignOp::Shl,
            TokenKind::Punct(Punct::ShrEq) => AssignOp::Shr,
            _ => AssignOp::Set,
        };
        StmtKind::Assign {
            target,
            op,
            op_token,
            value: self.expr(exprs[1]),
        }
    }

    /// A place written after `undefer`, as the expression naming it.
    fn place(&self, node: NodeId) -> Expr {
        let root = self.find(node, Rule::PlaceRoot).expect("a place root");
        let token = self.first(root);
        let mut expr = self.root_expr(token);
        for step in self
            .nodes(node)
            .into_iter()
            .filter(|step| self.rule(*step) == Rule::PlaceStep)
        {
            let last = self.last(step);
            expr = match self.find(step, Rule::Expr) {
                Some(index) => Expr {
                    token,
                    last,
                    kind: ExprKind::Index {
                        base: Box::new(expr),
                        index: Box::new(self.expr(index)),
                    },
                },
                None => Expr {
                    token,
                    last,
                    kind: ExprKind::Field {
                        base: Box::new(expr),
                        name: self.name(self.idents(step)[0]),
                    },
                },
            };
        }
        expr
    }

    /// The expression of a place root: `self` or a one-segment path.
    fn root_expr(&self, token: Tok) -> Expr {
        let kind = if self.kind(token) == TokenKind::Keyword(Keyword::SelfValue) {
            ExprKind::SelfValue
        } else {
            ExprKind::Path {
                path: Path {
                    segments: vec![self.name(token)],
                },
                args: None,
            }
        };
        Expr {
            token,
            last: token,
            kind,
        }
    }

    pub fn expr(&self, node: NodeId) -> Expr {
        match self.rule(node) {
            Rule::Expr => self.expr(self.nodes(node)[0]),
            Rule::JumpExpr => self.jump(node),
            Rule::FailExpr => Expr {
                token: self.first(node),
                last: self.last(node),
                kind: ExprKind::Fail(Box::new(
                    self.expr(self.find(node, Rule::OperatorExpr).expect("a failure")),
                )),
            },
            Rule::QuantifierExpr => self.quantifier(node),
            Rule::OperatorExpr => self.operator_expr(node),
            Rule::PrefixExpr => self.prefix(node),
            Rule::CallExpr => self.call_expr(node),
            rule => unreachable!("{} is no expression", rule.name()),
        }
    }

    fn jump(&self, node: NodeId) -> Expr {
        let tokens = self.tokens(node);
        let token = tokens[0];
        let label = || tokens.get(1).map(|label| self.name(*label));
        let kind = match self.kind(token) {
            TokenKind::Keyword(Keyword::Return) => ExprKind::Return(
                self.find(node, Rule::Expr)
                    .map(|expr| Box::new(self.expr(expr))),
            ),
            TokenKind::Keyword(Keyword::Break) => ExprKind::Break(label()),
            _ => ExprKind::Continue(label()),
        };
        Expr {
            token,
            last: self.last(node),
            kind,
        }
    }

    fn quantifier(&self, node: NodeId) -> Expr {
        let tokens = self.tokens(node);
        let exprs: Vec<NodeId> = self
            .nodes(node)
            .into_iter()
            .filter(|child| self.rule(*child) == Rule::Expr)
            .collect();
        Expr {
            token: tokens[0],
            last: self.last(node),
            kind: ExprKind::Quantifier {
                all: self.text(tokens[0]) == "all",
                name: self.name(tokens[1]),
                range: Box::new(self.expr(exprs[0])),
                body: Box::new(self.expr(exprs[1])),
            },
        }
    }

    fn operator_expr(&self, node: NodeId) -> Expr {
        let mut pieces = Vec::new();
        self.flatten(node, &mut pieces);
        let mut pieces = pieces.into_iter().peekable();
        let first = match pieces.next() {
            Some(Piece::Operand(expr)) => expr,
            _ => unreachable!("an operator expression starts with an operand"),
        };
        climb(first, &mut pieces, 0)
    }

    /// Collects the operands and binary operators of an operator expression in source order.
    fn flatten(&self, node: NodeId, pieces: &mut Vec<Piece>) {
        for child in self.children(node) {
            match *child {
                Child::Node(inner) => match self.rule(inner) {
                    Rule::PrefixExpr => pieces.push(Piece::Operand(self.prefix(inner))),
                    Rule::Negation => {
                        let operand = self.find(inner, Rule::Operand).expect("a negated operand");
                        let mut operand_pieces = Vec::new();
                        self.flatten(operand, &mut operand_pieces);
                        let Some(Piece::Operand(operand)) = operand_pieces.pop() else {
                            unreachable!("a negation holds an operand");
                        };
                        pieces.push(Piece::Operand(Expr {
                            token: self.first(inner),
                            last: operand.last,
                            kind: ExprKind::Unary {
                                op: UnaryOp::Not,
                                operand: Box::new(operand),
                            },
                        }));
                    }
                    _ => self.flatten(inner, pieces),
                },
                Child::Token(token) => {
                    if let Some(op) = self.binary_op(token) {
                        pieces.push(Piece::Operator(op, token));
                    }
                }
            }
        }
    }

    fn binary_op(&self, token: Tok) -> Option<BinaryOp> {
        Some(match self.kind(token) {
            TokenKind::Punct(punct) => match punct {
                Punct::Star => BinaryOp::Mul,
                Punct::Slash => BinaryOp::Div,
                Punct::Percent => BinaryOp::Rem,
                Punct::Plus => BinaryOp::Add,
                Punct::Minus => BinaryOp::Sub,
                Punct::EqEq => BinaryOp::Eq,
                Punct::NotEq => BinaryOp::Ne,
                Punct::Lt => BinaryOp::Lt,
                Punct::Le => BinaryOp::Le,
                Punct::Gt => BinaryOp::Gt,
                Punct::Ge => BinaryOp::Ge,
                Punct::Amp => BinaryOp::BitAnd,
                Punct::Pipe => BinaryOp::BitOr,
                Punct::Caret => BinaryOp::BitXor,
                Punct::Shl => BinaryOp::Shl,
                Punct::Shr => BinaryOp::Shr,
                Punct::DotDotLt => BinaryOp::RangeExclusive,
                Punct::DotDotEq => BinaryOp::RangeInclusive,
                _ => return None,
            },
            TokenKind::Keyword(Keyword::And) => BinaryOp::And,
            TokenKind::Keyword(Keyword::Or) => BinaryOp::Or,
            _ => return None,
        })
    }

    fn prefix(&self, node: NodeId) -> Expr {
        let token = self.first(node);
        if let Some(inner) = self.find(node, Rule::PrefixExpr) {
            let op = match self.kind(token) {
                TokenKind::Punct(Punct::Minus) => UnaryOp::Neg,
                _ => UnaryOp::BitNot,
            };
            let operand = self.prefix(inner);
            return Expr {
                token,
                last: operand.last,
                kind: ExprKind::Unary {
                    op,
                    operand: Box::new(operand),
                },
            };
        }
        if let Some(marked) = self.find(node, Rule::MarkedExpr) {
            return self.marked(marked);
        }
        self.postfix(
            self.find(node, Rule::PostfixExpr)
                .expect("a postfix expression"),
        )
    }

    pub fn call_expr(&self, node: NodeId) -> Expr {
        match self.find(node, Rule::MarkedExpr) {
            Some(marked) => self.marked(marked),
            None => self.postfix(self.find(node, Rule::PostfixExpr).expect("a call")),
        }
    }

    /// Markers and the expression they mark, through the nested `after_*` rules.
    fn marked(&self, node: NodeId) -> Expr {
        let token = self.first(node);
        let mut failure = None;
        let mut awaited = None;
        let mut mode = None;
        let mut current = node;
        loop {
            let mut next = None;
            for child in self.children(current) {
                match *child {
                    Child::Token(marker) => match self.kind(marker) {
                        TokenKind::Keyword(Keyword::Await) => awaited = Some(marker),
                        TokenKind::Keyword(Keyword::Try) => {
                            failure = Some((FailureMarker::Try, marker))
                        }
                        TokenKind::Keyword(Keyword::Attempt) => {
                            failure = Some((FailureMarker::Attempt, marker))
                        }
                        _ => {}
                    },
                    Child::Node(inner) => match self.rule(inner) {
                        Rule::FailureMarker => {
                            let marker = self.tokens(inner)[0];
                            let kind = match self.kind(marker) {
                                TokenKind::Keyword(Keyword::Try) => FailureMarker::Try,
                                _ => FailureMarker::Attempt,
                            };
                            failure = Some((kind, marker));
                        }
                        Rule::Mode => {
                            let marker = self.tokens(inner)[0];
                            let written = match self.kind(marker) {
                                TokenKind::Keyword(Keyword::Mut) => Mode::Mut,
                                _ => Mode::Take,
                            };
                            mode = Some((written, marker));
                        }
                        Rule::AfterFailureMarker | Rule::AfterAwait | Rule::AfterMode => {
                            next = Some(inner)
                        }
                        Rule::PostfixExpr | Rule::WhenExpr => next = Some(inner),
                        _ => {}
                    },
                }
            }
            match next {
                Some(inner) if matches!(self.rule(inner), Rule::PostfixExpr | Rule::WhenExpr) => {
                    let inner = if self.rule(inner) == Rule::PostfixExpr {
                        self.postfix(inner)
                    } else {
                        self.when(inner)
                    };
                    return Expr {
                        token,
                        last: inner.last,
                        kind: ExprKind::Marked {
                            failure,
                            awaited,
                            mode,
                            inner: Box::new(inner),
                        },
                    };
                }
                Some(inner) => current = inner,
                None => unreachable!("markers end in a postfix expression or when"),
            }
        }
    }

    fn when(&self, node: NodeId) -> Expr {
        let token = self.first(node);
        let accesses = self
            .nodes(node)
            .into_iter()
            .filter(|access| self.rule(*access) == Rule::WhenAccess)
            .map(|access| {
                let place = self.find(access, Rule::WhenPlace).expect("a when place");
                let root = self.find(place, Rule::PlaceRoot).expect("a place root");
                let mode = match self.tokens(access).first().map(|token| self.kind(*token)) {
                    Some(TokenKind::Keyword(Keyword::Mut)) => Mode::Mut,
                    Some(TokenKind::Keyword(Keyword::Take)) => Mode::Take,
                    _ => Mode::Read,
                };
                WhenAccess {
                    token: self.first(access),
                    mode,
                    root: self.name(self.first(root)),
                    fields: self
                        .idents(place)
                        .into_iter()
                        .map(|field| self.name(field))
                        .collect(),
                }
            })
            .collect();
        let body = self.find(node, Rule::WhenBody).expect("a when body");
        let body = match self.find(body, Rule::Block) {
            Some(block) => WhenBody::Block(self.block(block)),
            None => WhenBody::Expr(Box::new(
                self.expr(self.find(body, Rule::Expr).expect("a when expression")),
            )),
        };
        Expr {
            token,
            last: self.last(node),
            kind: ExprKind::When { accesses, body },
        }
    }

    fn postfix(&self, node: NodeId) -> Expr {
        let token = self.first(node);
        let mut expr = self.primary(self.find(node, Rule::PrimaryExpr).expect("a primary"));
        for suffix in self
            .nodes(node)
            .into_iter()
            .filter(|suffix| self.rule(*suffix) == Rule::PostfixSuffix)
        {
            let first = self.tokens(suffix)[0];
            let kind = match self.kind(first) {
                TokenKind::Punct(Punct::Dot) => ExprKind::Field {
                    base: Box::new(expr),
                    name: self.name(self.idents(suffix)[0]),
                },
                TokenKind::Punct(Punct::LParen) => ExprKind::Call {
                    callee: Box::new(expr),
                    args: self.args(self.find(suffix, Rule::ArgList)),
                },
                _ => ExprKind::Index {
                    base: Box::new(expr),
                    index: Box::new(
                        self.expr(self.find(suffix, Rule::Expr).expect("an index expression")),
                    ),
                },
            };
            expr = Expr {
                token,
                last: self.last(suffix),
                kind,
            };
        }
        expr
    }

    fn args(&self, list: Option<NodeId>) -> Vec<Arg> {
        self.tree
            .list_elements(list, Rule::Arg)
            .into_iter()
            .map(|arg| {
                let exprs: Vec<Expr> = self
                    .nodes(arg)
                    .into_iter()
                    .filter(|child| self.rule(*child) == Rule::Expr)
                    .map(|expr| self.expr(expr))
                    .collect();
                let mut exprs = exprs.into_iter();
                let first = exprs.next().expect("an argument");
                match exprs.next() {
                    Some(value) => Arg {
                        token: first.token,
                        label: Some(first),
                        value,
                    },
                    None => Arg {
                        token: first.token,
                        label: None,
                        value: first,
                    },
                }
            })
            .collect()
    }

    fn primary(&self, node: NodeId) -> Expr {
        let token = self.first(node);
        let inner = self.nodes(node).first().copied();
        let kind = match inner.map(|inner| (inner, self.rule(inner))) {
            Some((literal, Rule::Literal)) => ExprKind::Literal(self.literal(literal)),
            Some((path, Rule::PathExpr)) => ExprKind::Path {
                path: self.path(self.find(path, Rule::Path).expect("a path")),
                args: self
                    .find(path, Rule::GenericArgs)
                    .map(|args| self.generic_args(args)),
            },
            Some((expr, Rule::OperatorExpr | Rule::QuantifierExpr)) => {
                ExprKind::Paren(Box::new(self.expr(expr)))
            }
            Some((if_expr, Rule::IfExpr)) => return self.if_expr(if_expr),
            Some((match_expr, Rule::MatchExpr)) => self.match_expr(match_expr),
            Some((closure, Rule::ClosureExpr)) => {
                ExprKind::Closure(Box::new(self.closure(closure)))
            }
            Some((trusted, Rule::TrustedExpr)) => self.trusted(trusted),
            Some((todo, Rule::TodoExpr)) => {
                ExprKind::Todo(self.text_of(self.tokens_of(todo, TokenKind::Text)[0]))
            }
            Some((dbg, Rule::DbgExpr)) => ExprKind::Dbg(Box::new(
                self.expr(self.find(dbg, Rule::Expr).expect("a dbg operand")),
            )),
            _ => ExprKind::SelfValue,
        };
        Expr {
            token,
            last: self.last(node),
            kind,
        }
    }

    fn literal(&self, node: NodeId) -> Literal {
        let tokens = self.tokens(node);
        let kind = match self.kind(tokens[0]) {
            TokenKind::Int => LiteralKind::Int,
            TokenKind::Float => LiteralKind::Float,
            TokenKind::Text => LiteralKind::Text,
            TokenKind::TextLine => LiteralKind::TextRun,
            TokenKind::Bytes => LiteralKind::Bytes,
            TokenKind::Char => LiteralKind::Char,
            TokenKind::Byte => LiteralKind::Byte,
            TokenKind::Keyword(Keyword::True) => LiteralKind::True,
            _ => LiteralKind::False,
        };
        Literal { kind, tokens }
    }

    fn if_expr(&self, node: NodeId) -> Expr {
        let blocks: Vec<NodeId> = self
            .nodes(node)
            .into_iter()
            .filter(|child| self.rule(*child) == Rule::Block)
            .collect();
        let otherwise = if let Some(chained) = self.find(node, Rule::IfExpr) {
            Some(Box::new(Else::If(self.if_expr(chained))))
        } else {
            blocks
                .get(1)
                .map(|block| Box::new(Else::Block(self.block(*block))))
        };
        Expr {
            token: self.first(node),
            last: self.last(node),
            kind: ExprKind::If {
                condition: Box::new(self.expr(self.find(node, Rule::Expr).expect("a condition"))),
                then: self.block(blocks[0]),
                otherwise,
            },
        }
    }

    fn match_expr(&self, node: NodeId) -> ExprKind {
        let arms = self
            .nodes(node)
            .into_iter()
            .filter(|arm| self.rule(*arm) == Rule::MatchArm)
            .map(|arm| {
                let patterns = if self.has_token(arm, TokenKind::Keyword(Keyword::Else)) {
                    ArmPatterns::Else
                } else {
                    ArmPatterns::Patterns(
                        self.nodes(arm)
                            .into_iter()
                            .filter(|pattern| self.rule(*pattern) == Rule::Pattern)
                            .map(|pattern| self.pattern(pattern))
                            .collect(),
                    )
                };
                let body = self.find(arm, Rule::ArmBody).expect("an arm body");
                let body = match self.find(body, Rule::Block) {
                    Some(block) => ArmBody::Block(self.block(block)),
                    None => ArmBody::Expr(
                        self.expr(self.find(body, Rule::Expr).expect("an arm expression")),
                    ),
                };
                Arm {
                    token: self.first(arm),
                    patterns,
                    guard: self.find(arm, Rule::Expr).map(|guard| self.expr(guard)),
                    body,
                }
            })
            .collect();
        ExprKind::Match {
            first: self.has_token(node, TokenKind::Keyword(Keyword::First)),
            scrutinee: Box::new(self.expr(self.find(node, Rule::Expr).expect("a scrutinee"))),
            arms,
        }
    }

    fn closure(&self, node: NodeId) -> Closure {
        let captures = self
            .find(node, Rule::CaptureList)
            .map(|list| {
                self.nodes(list)
                    .into_iter()
                    .filter(|capture| self.rule(*capture) == Rule::Capture)
                    .map(|capture| {
                        let mode = self.mode(capture);
                        let root = self.find(capture, Rule::PlaceRoot).expect("a capture root");
                        Capture {
                            mode,
                            token: self.first(capture),
                            name: self.name(self.first(root)),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let body_node = self.find(node, Rule::ClosureBody).expect("a closure body");
        let (result, fails, uses, limits) = self.head_tail(body_node);
        let body = match self.find(body_node, Rule::Block) {
            Some(block) => ClosureBody::Block(self.block(block)),
            None => ClosureBody::Short(Box::new(
                self.expr(self.find(body_node, Rule::Expr).expect("a short body")),
            )),
        };
        Closure {
            captures,
            params: self.fn_type_params(self.find(node, Rule::FnTypeParamList)),
            close: self.close(node),
            result,
            fails,
            uses,
            limits,
            body,
        }
    }

    fn trusted(&self, node: NodeId) -> ExprKind {
        let premise = self.text_of(self.tokens_of(node, TokenKind::Text)[0]);
        let body = self.find(node, Rule::TrustedBody).expect("a trusted body");
        let opener = self
            .tokens(node)
            .into_iter()
            .find(|token| self.kind(*token) == TokenKind::Punct(Punct::LBrace))
            .expect("a trusted body opens");
        let end = self.last(node);
        let expression_only = self.find(body, Rule::TrustedLeadStmt).is_none()
            && !self.has_token(body, TokenKind::Nl);
        if expression_only {
            let expr = self.expr(self.find(body, Rule::Expr).expect("a trusted expression"));
            return ExprKind::Trusted {
                premise,
                body: TrustedBody::Expr(Box::new(expr)),
            };
        }
        let mut stmts = Vec::new();
        if let Some(lead) = self.find(body, Rule::TrustedLeadStmt) {
            stmts.push(self.stmt(lead));
        } else {
            stmts.push(Stmt {
                token: self.first(body),
                kind: self.expr_stmt(body),
            });
        }
        stmts.extend(self.stmts(body));
        ExprKind::Trusted {
            premise,
            body: TrustedBody::Block(Block {
                token: opener,
                end,
                stmts,
            }),
        }
    }

    pub fn pattern(&self, node: NodeId) -> Pattern {
        let token = self.first(node);
        let inner = self.nodes(node).first().copied();
        let kind = match inner.map(|inner| (inner, self.rule(inner))) {
            Some((literal, Rule::LiteralPattern)) => PatternKind::Literal {
                literal: self.pattern_literal(
                    self.find(literal, Rule::PatternLiteral)
                        .expect("a pattern literal"),
                ),
                range: self
                    .find(literal, Rule::RangeTail)
                    .map(|tail| self.range_tail(tail)),
            },
            Some((named, Rule::NamedPattern)) => PatternKind::Named {
                path: self.path(self.find(named, Rule::Path).expect("a pattern path")),
                member: self.idents(named).first().map(|token| self.name(*token)),
                fields: self
                    .tokens(named)
                    .into_iter()
                    .any(|token| self.kind(token) == TokenKind::Punct(Punct::LParen))
                    .then(|| {
                        self.tree
                            .list_elements(
                                self.find(named, Rule::FieldPatternList),
                                Rule::FieldPattern,
                            )
                            .into_iter()
                            .map(|field| {
                                let patterns: Vec<Pattern> = self
                                    .nodes(field)
                                    .into_iter()
                                    .filter(|pattern| self.rule(*pattern) == Rule::Pattern)
                                    .map(|pattern| self.pattern(pattern))
                                    .collect();
                                let mut patterns = patterns.into_iter();
                                let first = patterns.next().expect("a field pattern");
                                match patterns.next() {
                                    Some(pattern) => FieldPattern {
                                        token: first.token,
                                        label: Some(first),
                                        pattern,
                                    },
                                    None => FieldPattern {
                                        token: first.token,
                                        label: None,
                                        pattern: first,
                                    },
                                }
                            })
                            .collect()
                    }),
                range: self
                    .find(named, Rule::RangeTail)
                    .map(|tail| self.range_tail(tail)),
            },
            _ => PatternKind::Wildcard,
        };
        Pattern { token, kind }
    }

    fn pattern_literal(&self, node: NodeId) -> PatternLiteral {
        let tokens = self.tokens(node);
        let negative = self.kind(tokens[0]) == TokenKind::Punct(Punct::Minus);
        let token = *tokens.last().expect("a pattern literal token");
        let kind = match self.kind(token) {
            TokenKind::Int => LiteralKind::Int,
            TokenKind::Float => LiteralKind::Float,
            TokenKind::Text => LiteralKind::Text,
            TokenKind::Bytes => LiteralKind::Bytes,
            TokenKind::Char => LiteralKind::Char,
            TokenKind::Byte => LiteralKind::Byte,
            TokenKind::Keyword(Keyword::True) => LiteralKind::True,
            _ => LiteralKind::False,
        };
        let literal = Literal {
            kind,
            tokens: vec![token],
        };
        if matches!(kind, LiteralKind::Int | LiteralKind::Float) {
            PatternLiteral::Number { negative, literal }
        } else {
            PatternLiteral::Other(literal)
        }
    }

    fn range_tail(&self, node: NodeId) -> (RangeKind, RangeBound) {
        let kind = match self.kind(self.tokens(node)[0]) {
            TokenKind::Punct(Punct::DotDotEq) => RangeKind::Inclusive,
            _ => RangeKind::Exclusive,
        };
        let bound = self.find(node, Rule::RangeBound).expect("a range bound");
        let bound = match self.find(bound, Rule::PatternLiteral) {
            Some(literal) => RangeBound::Literal(self.pattern_literal(literal)),
            None => {
                RangeBound::Path(self.path(self.find(bound, Rule::Path).expect("a bound path")))
            }
        };
        (kind, bound)
    }
}

/// Builds the binary tree of operands joined by operators binding at least as tightly as `floor`,
/// left-associative.
fn climb(
    mut left: Expr,
    pieces: &mut std::iter::Peekable<impl Iterator<Item = Piece>>,
    floor: u8,
) -> Expr {
    while let Some(Piece::Operator(op, _)) = pieces.peek() {
        let level = precedence(*op);
        if level < floor {
            break;
        }
        let Some(Piece::Operator(op, op_token)) = pieces.next() else {
            unreachable!("an operator was peeked");
        };
        let Some(Piece::Operand(mut right)) = pieces.next() else {
            unreachable!("an operator is followed by an operand");
        };
        while let Some(Piece::Operator(next, _)) = pieces.peek() {
            if precedence(*next) <= level {
                break;
            }
            right = climb(right, pieces, level + 1);
        }
        let token = left.token;
        left = Expr {
            token,
            last: right.last,
            kind: ExprKind::Binary {
                op,
                op_token,
                left: Box::new(left),
                right: Box::new(right),
            },
        };
    }
    left
}

/// How tightly an operator binds; the grammar rejects every combination of operators without a
/// relative precedence, so those share a level.
fn precedence(op: BinaryOp) -> u8 {
    match op {
        BinaryOp::Or => 1,
        BinaryOp::And => 2,
        BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
            3
        }
        BinaryOp::Add | BinaryOp::Sub => 4,
        BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem => 5,
        _ => 6,
    }
}
