//! Lowering items, heads, types and clauses from their trees.

use super::*;
use crate::syntax::file::SourceFile;
use crate::syntax::grammar::{Keyword, Punct, Rule};
use crate::syntax::literal::text_value;
use crate::syntax::token::TokenKind;
use crate::syntax::tree::{Child, NodeId, Tree};

/// The abstract syntax of an item the parse completed.
pub fn lower(file: &SourceFile, tree: &Tree) -> Option<Item> {
    Lower { file, tree }.item()
}

/// A view of one item's tree over its file, with the lowering of each rule.
#[derive(Clone, Copy)]
pub(super) struct Lower<'a> {
    pub file: &'a SourceFile,
    pub tree: &'a Tree,
}

impl<'a> Lower<'a> {
    pub fn kind(&self, token: Tok) -> TokenKind {
        self.file.tokens[token as usize].kind
    }

    pub fn text(&self, token: Tok) -> &'a str {
        self.file.text(token as usize)
    }

    pub fn rule(&self, node: NodeId) -> Rule {
        self.tree.rule(node)
    }

    pub fn children(&self, node: NodeId) -> &'a [Child] {
        self.tree.children(node)
    }

    pub fn nodes(&self, node: NodeId) -> Vec<NodeId> {
        self.tree.child_nodes(node).collect()
    }

    pub fn tokens(&self, node: NodeId) -> Vec<Tok> {
        self.tree.child_tokens(node).collect()
    }

    pub fn find(&self, node: NodeId, rule: Rule) -> Option<NodeId> {
        self.tree.find(node, rule)
    }

    pub fn first(&self, node: NodeId) -> Tok {
        self.tree.span(node).expect("a lowered node holds tokens").0
    }

    pub fn last(&self, node: NodeId) -> Tok {
        self.tree.span(node).expect("a lowered node holds tokens").1
    }

    pub fn name(&self, token: Tok) -> Name {
        Name {
            text: self.text(token).to_owned(),
            token,
        }
    }

    /// The child tokens of `node` of the given kind.
    pub fn tokens_of(&self, node: NodeId, kind: TokenKind) -> Vec<Tok> {
        self.tokens(node)
            .into_iter()
            .filter(|token| self.kind(*token) == kind)
            .collect()
    }

    pub fn idents(&self, node: NodeId) -> Vec<Tok> {
        self.tokens_of(node, TokenKind::Ident)
    }

    pub fn has_token(&self, node: NodeId, kind: TokenKind) -> bool {
        self.tokens(node)
            .into_iter()
            .any(|token| self.kind(token) == kind)
    }

    pub fn has_word(&self, node: NodeId, word: &str) -> bool {
        self.tokens(node)
            .into_iter()
            .any(|token| self.kind(token) == TokenKind::Ident && self.text(token) == word)
    }

    pub fn text_of(&self, token: Tok) -> Text {
        Text {
            token,
            value: text_value(self.text(token)),
        }
    }

    fn item(&self) -> Option<Item> {
        let start = self.find(self.tree.root(), Rule::ItemStart)?;
        let mut visibility = Visibility::Private;
        let mut visibility_token = None;
        let mut type_modifiers = Vec::new();
        let mut fn_modifier = None;
        let mut kind_node = None;
        for node in self.nodes(start) {
            match self.rule(node) {
                Rule::Visibility => {
                    let token = self.tokens(node)[0];
                    visibility_token = Some(token);
                    visibility = match self.kind(token) {
                        TokenKind::Keyword(Keyword::Pub) => Visibility::Pub,
                        _ => Visibility::Pkg,
                    };
                }
                Rule::ItemModifier => {
                    let inner = self.nodes(node)[0];
                    let token = self.first(inner);
                    match self.rule(inner) {
                        Rule::TypeModifier => {
                            let modifier = match self.text(token) {
                                "closed" => TypeModifier::Closed,
                                "open" => TypeModifier::Open,
                                "linear" => TypeModifier::Linear,
                                _ => TypeModifier::View,
                            };
                            type_modifiers.push((modifier, token));
                        }
                        Rule::FnModifier => {
                            let modifier = match self.kind(token) {
                                TokenKind::Keyword(Keyword::Proj) => FnModifier::Proj,
                                TokenKind::Keyword(Keyword::Interrupt) => FnModifier::Interrupt,
                                TokenKind::Keyword(Keyword::Landing) => FnModifier::Landing,
                                TokenKind::Keyword(Keyword::Foreign) => FnModifier::Foreign,
                                _ => FnModifier::Validator,
                            };
                            fn_modifier.get_or_insert((modifier, token));
                        }
                        _ => {}
                    }
                }
                Rule::ItemKind => kind_node = Some(node),
                _ => {}
            }
        }
        let kind_node = kind_node?;
        let node = *self.nodes(kind_node).first()?;
        let token = visibility_token
            .or(type_modifiers.first().map(|(_, token)| *token))
            .or(fn_modifier.map(|(_, token)| token))
            .unwrap_or_else(|| self.first(kind_node));
        let kind = match self.rule(node) {
            Rule::FnItem => ItemKind::Fn(self.fn_item(node, fn_modifier)?),
            Rule::StructItem => ItemKind::Struct(self.struct_item(node)),
            Rule::EnumItem => ItemKind::Enum(self.enum_item(node)),
            Rule::InterfaceItem => ItemKind::Interface(self.interface_item(node)),
            Rule::ImplItem => ItemKind::Impl(self.impl_item(node)),
            Rule::LayoutItem => ItemKind::Layout(self.layout_item(node)),
            Rule::RegistersItem => ItemKind::Registers(self.registers_item(node)),
            Rule::ConstItem => ItemKind::Const(self.const_item(node)?),
            Rule::ConstAssertItem => ItemKind::ConstAssert(self.expr(self.find(node, Rule::Expr)?)),
            Rule::RegistryItem => ItemKind::Registry(self.registry_item(node)),
            Rule::ContributeItem => ItemKind::Contribute(self.contribute_item(node)),
            Rule::TestItem => ItemKind::Test(self.test_item(node)),
            Rule::RetiredItem => ItemKind::Retired(self.retired_item(node)),
            _ => ItemKind::Package,
        };
        Some(Item {
            token,
            visibility,
            type_modifiers,
            kind,
        })
    }

    pub fn path(&self, node: NodeId) -> Path {
        Path {
            segments: self
                .idents(node)
                .into_iter()
                .map(|token| self.name(token))
                .collect(),
        }
    }

    fn label(&self, node: NodeId) -> Label {
        let tokens = self.tokens(node);
        let idents: Vec<Tok> = self.idents(node);
        let dotted = tokens
            .iter()
            .any(|token| self.kind(*token) == TokenKind::Punct(Punct::Dot));
        let name = self.name(*idents.last().expect("a label holds a name"));
        let owner = dotted.then(|| Path {
            segments: idents[..idents.len() - 1]
                .iter()
                .map(|token| self.name(*token))
                .collect(),
        });
        Label { owner, name }
    }

    pub fn generic_params(&self, node: Option<NodeId>) -> Vec<GenericParam> {
        let Some(node) = node else {
            return Vec::new();
        };
        self.nodes(node)
            .into_iter()
            .filter(|param| self.rule(*param) == Rule::GenericParam)
            .map(|param| GenericParam {
                name: self.name(self.idents(param)[0]),
                bounds: self.bounds(param),
                where_clause: self
                    .find(param, Rule::WhereClause)
                    .and_then(|clause| self.find(clause, Rule::Expr))
                    .map(|expr| self.expr(expr)),
            })
            .collect()
    }

    fn bounds(&self, node: NodeId) -> Vec<Bound> {
        self.nodes(node)
            .into_iter()
            .filter(|bound| self.rule(*bound) == Rule::Bound)
            .map(|bound| self.bound(bound))
            .collect()
    }

    pub fn bound(&self, node: NodeId) -> Bound {
        Bound {
            path: self.path(self.find(node, Rule::Path).expect("a bound has a path")),
            args: self
                .find(node, Rule::GenericArgs)
                .map(|args| self.generic_args(args)),
        }
    }

    pub fn generic_args(&self, node: NodeId) -> Vec<GenericArg> {
        self.nodes(node)
            .into_iter()
            .filter(|arg| self.rule(*arg) == Rule::GenericArg)
            .map(|arg| {
                if let Some(ty) = self.find(arg, Rule::TypeExpr) {
                    return GenericArg::Type(self.type_expr(ty));
                }
                let tokens = self.tokens(arg);
                let last = *tokens.last().expect("an argument holds a token");
                match self.kind(last) {
                    TokenKind::Keyword(Keyword::True) => GenericArg::Bool {
                        value: true,
                        token: last,
                    },
                    TokenKind::Keyword(Keyword::False) => GenericArg::Bool {
                        value: false,
                        token: last,
                    },
                    _ => GenericArg::Int {
                        negative: tokens.len() > 1,
                        token: last,
                    },
                }
            })
            .collect()
    }

    pub fn type_expr(&self, node: NodeId) -> TypeExpr {
        let token = self.first(node);
        let last = self.last(node);
        if let Some(fn_type) = self.find(node, Rule::FnType) {
            return TypeExpr {
                token,
                last,
                kind: TypeKind::Fn(Box::new(self.fn_type(fn_type))),
            };
        }
        let path = self.path(self.find(node, Rule::Path).expect("a type names a path"));
        let args = self
            .find(node, Rule::GenericArgs)
            .map(|args| self.generic_args(args));
        TypeExpr {
            token,
            last,
            kind: TypeKind::Path { path, args },
        }
    }

    /// The `)` closing the parameters among a node's tokens.
    pub fn close(&self, node: NodeId) -> Tok {
        self.tokens_of(node, TokenKind::Punct(Punct::RParen))[0]
    }

    fn fn_type(&self, node: NodeId) -> FnType {
        let kind = match self.find(node, Rule::FnKind) {
            Some(kind) if self.has_token(kind, TokenKind::Keyword(Keyword::Interrupt)) => {
                FnKind::Interrupt
            }
            Some(_) => FnKind::Task,
            None => FnKind::Plain,
        };
        let head = self
            .find(node, Rule::FnTypeHead)
            .expect("a function type head");
        let (result, fails, uses, limits) = self.head_tail(head);
        FnType {
            kind,
            params: self.fn_type_params(self.find(head, Rule::FnTypeParamList)),
            result,
            fails,
            uses,
            limits,
            convention: self.convention(node),
        }
    }

    pub fn fn_type_params(&self, list: Option<NodeId>) -> Vec<FnTypeParam> {
        self.tree
            .list_elements(list, Rule::FnTypeParam)
            .into_iter()
            .map(|param| FnTypeParam {
                mode: self.mode(param),
                name: self.name(self.idents(param)[0]),
                ty: self.type_expr(self.find(param, Rule::TypeExpr).expect("a typed param")),
            })
            .collect()
    }

    /// The result, `fails`, `uses` and `limits` clauses directly under a head node.
    #[allow(clippy::type_complexity)]
    pub fn head_tail(
        &self,
        node: NodeId,
    ) -> (
        Option<TypeExpr>,
        Option<TypeExpr>,
        Option<Uses>,
        Option<Limits>,
    ) {
        let result = self
            .find(node, Rule::FnResult)
            .and_then(|result| self.find(result, Rule::TypeExpr))
            .map(|ty| self.type_expr(ty));
        let fails = self
            .find(node, Rule::FailsClause)
            .and_then(|fails| self.find(fails, Rule::TypeExpr))
            .map(|ty| self.type_expr(ty));
        let uses = self.find(node, Rule::UsesClause).map(|uses| {
            let tokens = self.tokens(uses);
            Uses {
                token: tokens[0],
                word: self.name(tokens[1]),
            }
        });
        let limits = self.find(node, Rule::LimitsClause).map(|clause| Limits {
            limits: self
                .nodes(clause)
                .into_iter()
                .filter(|limit| self.rule(*limit) == Rule::Limit)
                .map(|limit| {
                    if let Some(class) = self.find(limit, Rule::Reversibility) {
                        return Limit {
                            kind: LimitKind::Class,
                            token: self.first(class),
                            size: None,
                            kinds: (self.nodes(class).into_iter())
                                .map(|kind| self.first(kind))
                                .collect(),
                        };
                    }
                    let tokens = self.tokens(limit);
                    let kind = match self.text(tokens[0]) {
                        "pure" => LimitKind::Pure,
                        "notrap" => LimitKind::Notrap,
                        "terminates" => LimitKind::Terminates,
                        "noalloc" => LimitKind::Noalloc,
                        _ => LimitKind::Stack,
                    };
                    Limit {
                        kind,
                        token: tokens[0],
                        size: tokens.get(1).copied(),
                        kinds: Vec::new(),
                    }
                })
                .collect(),
        });
        (result, fails, uses, limits)
    }

    fn convention(&self, node: NodeId) -> Option<Text> {
        let clause = self.find(node, Rule::ConventionClause)?;
        let text = self.tokens_of(clause, TokenKind::Text)[0];
        Some(self.text_of(text))
    }

    pub fn mode(&self, node: NodeId) -> Mode {
        let Some(mode) = self.find(node, Rule::Mode) else {
            return Mode::Read;
        };
        match self.kind(self.tokens(mode)[0]) {
            TokenKind::Keyword(Keyword::Mut) => Mode::Mut,
            _ => Mode::Take,
        }
    }

    pub fn params(&self, list: Option<NodeId>) -> Vec<Param> {
        self.tree
            .list_elements(list, Rule::Param)
            .into_iter()
            .map(|param| {
                let mode = self.mode(param);
                let kind = match self.find(param, Rule::TypeExpr) {
                    Some(ty) => ParamKind::Named {
                        name: self.name(self.idents(param)[0]),
                        ty: self.type_expr(ty),
                    },
                    None => ParamKind::SelfValue,
                };
                Param {
                    token: self.first(param),
                    mode,
                    kind,
                }
            })
            .collect()
    }

    fn head(&self, node: NodeId) -> Head {
        let (result, fails, uses, limits) = self.head_tail(node);
        Head {
            token: self.first(node),
            generics: self.generic_params(self.find(node, Rule::GenericParams)),
            params: self.params(self.find(node, Rule::ParamList)),
            close: self.close(node),
            result,
            fails,
            uses,
            limits,
        }
    }

    fn fn_item(&self, node: NodeId, modifier: Option<(FnModifier, Tok)>) -> Option<FnItem> {
        let head_node = self.find(node, Rule::FnHead)?;
        let end = self.find(node, Rule::FnEnd)?;
        let label = self.label(self.find(head_node, Rule::ItemLabel)?);
        let head = self.head(head_node);
        let clauses = self.clauses(self.find(end, Rule::ClauseRegion));
        let convention = self.convention(end);
        let body = if let Some(body) = self.find(end, Rule::FnBody) {
            let opener = self
                .tokens(end)
                .into_iter()
                .rev()
                .find(|token| self.kind(*token) == TokenKind::Punct(Punct::LBrace))
                .expect("a body opens");
            FnBody::Block(self.fn_body(opener, body))
        } else if let Some(expr) = self.find(end, Rule::Expr) {
            FnBody::Expr(self.expr(expr))
        } else {
            FnBody::None
        };
        Some(FnItem {
            modifier,
            label,
            head,
            convention,
            clauses,
            body,
        })
    }

    /// The block of a `fn_body` opened by `opener`: its statements and the `}` closing it.
    pub fn fn_body(&self, opener: Tok, body: NodeId) -> Block {
        let end = self
            .tokens(body)
            .into_iter()
            .find(|token| self.kind(*token) == TokenKind::Punct(Punct::RBrace))
            .expect("a body closes");
        Block {
            token: opener,
            end,
            stmts: self.stmts(body),
        }
    }

    pub fn clauses(&self, region: Option<NodeId>) -> Vec<Clause> {
        let Some(region) = region else {
            return Vec::new();
        };
        self.nodes(region)
            .into_iter()
            .filter_map(|line| self.nodes(line).first().copied())
            .map(|clause| self.clause(clause))
            .collect()
    }

    fn clause(&self, node: NodeId) -> Clause {
        let token = self.first(node);
        let expr = || self.expr(self.find(node, Rule::Expr).expect("a clause expression"));
        let kind = match self.rule(node) {
            Rule::RequiresClause => ClauseKind::Requires(expr()),
            Rule::EnsuresClause => ClauseKind::Ensures(expr()),
            Rule::ExampleClause => ClauseKind::Example(expr()),
            Rule::DecreasesClause => ClauseKind::Decreases(expr()),
            Rule::WaiveClause => ClauseKind::Waive {
                length: self.has_word(node, "length"),
                reason: self.text_of(self.tokens_of(node, TokenKind::Text)[0]),
            },
            Rule::UndoClause => ClauseKind::Undo {
                target: self.path(self.find(node, Rule::Path).expect("an undoer path")),
                member: self.idents(node).get(1).map(|token| self.name(*token)),
                premise: self.text_of(self.tokens_of(node, TokenKind::Text)[0]),
            },
            _ => {
                return match self.nodes(node).first().copied() {
                    Some(marked) => Clause {
                        token,
                        disputed: true,
                        ..self.clause(marked)
                    },
                    None => Clause {
                        token,
                        disputed: false,
                        kind: ClauseKind::Missing,
                    },
                };
            }
        };
        Clause {
            token,
            disputed: false,
            kind,
        }
    }

    fn derives(&self, node: NodeId) -> (Vec<Name>, Option<Tok>) {
        match self.find(node, Rule::DerivesClause) {
            Some(clause) => (
                self.idents(clause)
                    .into_iter()
                    .map(|token| self.name(token))
                    .collect(),
                Some(self.first(clause)),
            ),
            None => (Vec::new(), None),
        }
    }

    fn struct_item(&self, node: NodeId) -> StructItem {
        let (derives, derives_token) = self.derives(node);
        let members = self
            .nodes(node)
            .into_iter()
            .filter(|member| self.rule(*member) == Rule::StructMember)
            .filter_map(|member| {
                if let Some(field) = self.find(member, Rule::Field) {
                    let visibility = match self.find(field, Rule::Visibility) {
                        Some(visibility) => match self.kind(self.tokens(visibility)[0]) {
                            TokenKind::Keyword(Keyword::Pub) => Visibility::Pub,
                            _ => Visibility::Pkg,
                        },
                        None => Visibility::Private,
                    };
                    return Some(StructMember::Field(Field {
                        token: self.first(field),
                        visibility,
                        name: self.name(self.idents(field)[0]),
                        ty: self.type_expr(self.find(field, Rule::TypeExpr)?),
                        where_clause: self
                            .find(field, Rule::WhereClause)
                            .and_then(|clause| self.find(clause, Rule::Expr))
                            .map(|expr| self.expr(expr)),
                    }));
                }
                let line = self.find(member, Rule::InvariantLine)?;
                Some(StructMember::Invariant(
                    self.expr(self.find(line, Rule::Expr)?),
                ))
            })
            .collect();
        StructItem {
            name: self.name(self.idents(node)[0]),
            generics: self.generic_params(self.find(node, Rule::GenericParams)),
            derives,
            derives_token,
            members,
        }
    }

    fn enum_item(&self, node: NodeId) -> EnumItem {
        let (derives, derives_token) = self.derives(node);
        let variants = self
            .nodes(node)
            .into_iter()
            .filter(|member| self.rule(*member) == Rule::EnumMember)
            .filter_map(|member| self.find(member, Rule::Variant))
            .map(|variant| {
                let payload = self.find(variant, Rule::VariantPayload).map(|payload| {
                    self.tree
                        .list_elements(self.find(payload, Rule::PayloadList), Rule::PayloadField)
                        .into_iter()
                        .map(|field| {
                            let types: Vec<TypeExpr> = self
                                .nodes(field)
                                .into_iter()
                                .filter(|ty| self.rule(*ty) == Rule::TypeExpr)
                                .map(|ty| self.type_expr(ty))
                                .collect();
                            let mut types = types.into_iter();
                            let first = types.next().expect("a payload type");
                            match types.next() {
                                Some(ty) => PayloadField {
                                    token: first.token,
                                    label: Some(first),
                                    ty,
                                },
                                None => PayloadField {
                                    token: first.token,
                                    label: None,
                                    ty: first,
                                },
                            }
                        })
                        .collect()
                });
                Variant {
                    name: self.name(self.idents(variant)[0]),
                    payload,
                    from: self.has_word(variant, "from"),
                }
            })
            .collect();
        EnumItem {
            name: self.name(self.idents(node)[0]),
            generics: self.generic_params(self.find(node, Rule::GenericParams)),
            derives,
            derives_token,
            variants,
        }
    }

    fn interface_item(&self, node: NodeId) -> InterfaceItem {
        let methods = self
            .nodes(node)
            .into_iter()
            .filter(|member| self.rule(*member) == Rule::InterfaceMember)
            .map(|member| {
                let (result, fails, uses, limits) = self.head_tail(member);
                let end = self.find(member, Rule::MethodEnd);
                let body = end
                    .and_then(|end| self.find(end, Rule::MethodBody))
                    .map(|body| {
                        let tokens = self.tokens(body);
                        Block {
                            token: tokens[0],
                            end: *tokens
                                .iter()
                                .rev()
                                .find(|token| self.kind(**token) == TokenKind::Punct(Punct::RBrace))
                                .expect("a default body closes"),
                            stmts: self.stmts(body),
                        }
                    });
                let fn_token = self
                    .tokens(member)
                    .into_iter()
                    .find(|token| self.kind(*token) == TokenKind::Keyword(Keyword::Fn))
                    .expect("a method head");
                Method {
                    token: self.first(member),
                    proj: self.has_token(member, TokenKind::Keyword(Keyword::Proj)),
                    name: self.name(self.idents(member)[0]),
                    head: Head {
                        token: fn_token,
                        generics: self.generic_params(self.find(member, Rule::GenericParams)),
                        params: self.params(self.find(member, Rule::ParamList)),
                        close: self.close(member),
                        result,
                        fails,
                        uses,
                        limits,
                    },
                    clauses: self.clauses(end.and_then(|end| self.find(end, Rule::ClauseRegion))),
                    body,
                }
            })
            .collect();
        InterfaceItem {
            name: self.name(self.idents(node)[0]),
            generics: self.generic_params(self.find(node, Rule::GenericParams)),
            methods,
        }
    }

    fn impl_item(&self, node: NodeId) -> ImplItem {
        let params = self.find(node, Rule::ImplParams).map(|params| {
            self.nodes(params)
                .into_iter()
                .filter(|param| self.rule(*param) == Rule::ImplParam)
                .map(|param| ImplParam {
                    name: self.name(self.idents(param)[0]),
                    bounds: self.bounds(param),
                })
                .collect()
        });
        ImplItem {
            target: self.path(self.find(node, Rule::Path).expect("an impl target")),
            params,
            interface: self.bound(self.find(node, Rule::Bound).expect("an impl interface")),
        }
    }

    fn layout_item(&self, node: NodeId) -> LayoutItem {
        let repr_node = self.find(node, Rule::LayoutRepr).expect("a layout repr");
        let order = self
            .find(repr_node, Rule::LayoutOrder)
            .map(|order| self.name(self.tokens(order)[0]));
        let repr_tokens = self.tokens(repr_node);
        let repr = if self.text(repr_tokens[0]) == "bytes"
            && repr_tokens
                .get(1)
                .is_some_and(|token| self.kind(*token) == TokenKind::Int)
        {
            LayoutRepr::Bytes {
                size: repr_tokens[1],
                order,
            }
        } else {
            LayoutRepr::Integer {
                ty: self.name(repr_tokens[0]),
                order,
            }
        };
        let tags = self.has_word(node, "tags").then(|| {
            self.nodes(node)
                .into_iter()
                .filter(|line| self.rule(*line) == Rule::TagLine)
                .map(|line| Tag {
                    name: self.name(self.idents(line)[0]),
                    value: self.tokens_of(line, TokenKind::Int)[0],
                })
                .collect()
        });
        let fields = self
            .nodes(node)
            .into_iter()
            .filter(|member| self.rule(*member) == Rule::LayoutMember)
            .map(|member| {
                let field = self
                    .find(member, Rule::LayoutField)
                    .expect("a layout field");
                let ints = self.tokens_of(field, TokenKind::Int);
                let kind = if let Some(ty) = self.find(field, Rule::TypeExpr) {
                    LayoutFieldKind::Typed {
                        ty: self.type_expr(ty),
                        at: ints[0],
                    }
                } else if self.has_word(field, "rest") {
                    LayoutFieldKind::Rest
                } else {
                    LayoutFieldKind::Bit {
                        low: ints[0],
                        high: ints.get(1).copied(),
                    }
                };
                LayoutField {
                    name: self.name(self.idents(member)[0]),
                    kind,
                }
            })
            .collect();
        LayoutItem {
            name: self.name(self.idents(node)[0]),
            repr,
            tags,
            fields,
        }
    }

    fn registers_item(&self, node: NodeId) -> RegistersItem {
        let registers = self
            .nodes(node)
            .into_iter()
            .filter(|member| self.rule(*member) == Rule::RegisterMember)
            .filter_map(|member| self.find(member, Rule::RegisterLine))
            .filter_map(|line| {
                let tail = self.find(line, Rule::RegisterTail)?;
                let ints = self.tokens_of(tail, TokenKind::Int);
                let access = self.find(tail, Rule::RegisterAccess).expect("an access");
                let words: Vec<&str> = self
                    .tokens(access)
                    .into_iter()
                    .map(|token| self.text(token))
                    .collect();
                let access = match words[..] {
                    ["read"] => Access::Read,
                    ["read", "write"] => Access::ReadWrite,
                    ["write"] => Access::Write,
                    _ => Access::WriteOneClear,
                };
                Some(Register {
                    name: self.name(self.idents(line)[0]),
                    ty: self.type_expr(self.find(tail, Rule::TypeExpr)?),
                    at: ints[0],
                    stride: ints.get(1).copied(),
                    access,
                })
            })
            .collect();
        RegistersItem {
            name: self.name(self.idents(node)[0]),
            registers,
        }
    }

    fn const_item(&self, node: NodeId) -> Option<ConstItem> {
        Some(ConstItem {
            name: self.name(self.idents(node)[0]),
            ty: self.find(node, Rule::TypeExpr).map(|ty| self.type_expr(ty)),
            value: self.expr(self.find(node, Rule::Expr)?),
        })
    }

    fn registry_item(&self, node: NodeId) -> RegistryItem {
        let idents = self.idents(node);
        let key = *idents.last().expect("a key field");
        RegistryItem {
            name: self.name(idents[0]),
            entry: self.type_expr(self.find(node, Rule::TypeExpr).expect("an entry type")),
            key: self.name(key),
            unique: self
                .find(node, Rule::UniqueList)
                .map(|list| {
                    self.idents(list)[1..]
                        .iter()
                        .map(|token| self.name(*token))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    fn contribute_item(&self, node: NodeId) -> ContributeItem {
        ContributeItem {
            registry: self.path(self.find(node, Rule::Path).expect("a registry path")),
            name: self.name(self.idents(node)[0]),
            value: self.expr(self.find(node, Rule::Expr).expect("a contribution value")),
        }
    }

    fn test_item(&self, node: NodeId) -> TestItem {
        let tokens = self.tokens(node);
        let opener = tokens
            .iter()
            .copied()
            .find(|token| self.kind(*token) == TokenKind::Punct(Punct::LBrace))
            .expect("a test body opens");
        let target = self.find(node, Rule::TestTarget).expect("a test target");
        TestItem {
            disputed: self.text(tokens[0]) == "todo",
            name: self.name(self.idents(node)[0]),
            target: self.path(self.find(target, Rule::Path).expect("a target path")),
            member: self.idents(target).first().map(|token| self.name(*token)),
            params: self
                .find(node, Rule::TestParams)
                .map(|params| self.params(self.find(params, Rule::ParamList))),
            body: self.fn_body(opener, self.find(node, Rule::FnBody).expect("a test body")),
        }
    }

    fn retired_item(&self, node: NodeId) -> RetiredItem {
        let old = self.find(node, Rule::RetiredOld).expect("an old form");
        let head = self
            .find(old, Rule::RetiredHead)
            .map(|head| self.head(head));
        let new = self.find(node, Rule::RetiredNew).expect("a rewrite");
        let new = match self.find(new, Rule::Expr) {
            Some(expr) => {
                let class = self.find(new, Rule::RetiredClass).expect("a class");
                RetiredNew::Rewrite {
                    expr: self.expr(expr),
                    preserves: self.tokens_of(class, TokenKind::Text).is_empty(),
                }
            }
            None => RetiredNew::Manual,
        };
        RetiredItem {
            path: self.path(self.find(old, Rule::Path).expect("an old path")),
            member: self.idents(old).first().map(|token| self.name(*token)),
            head,
            new,
        }
    }
}
