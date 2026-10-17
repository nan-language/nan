//! Typing expressions: literals, paths and member paths, fields, operators, ranges, conditionals
//! and matches, closures, `await when`, trusted code, holes, quantifiers and jumps.

use std::rc::Rc;

use super::call::Intrinsic;
use super::{Body, Clauses, Frame, Written};
use crate::check::conform::type_key;
use crate::check::db::{ItemId, TypeKey};
use crate::check::resolve::Named;
use crate::check::sig::{
    Problems, SigKind, int_literal, limit_problems, limit_set, literal_suffix,
};
use crate::check::tir::{
    Lit, LocalId, LocalKind, TArm, TCapture, TClosure, TExpr, TKind, TStmtKind,
};
use crate::check::ty::{Arg, FnParam, FnTy, Scalar, Ty};
use crate::check::{Checker, Member};
use crate::diagnostic::{Code, Fix};
use crate::syntax::ast::{
    self, BinaryOp, ExprKind, FnKind, ItemKind, LiteralKind, Mode, Tok, UnaryOp,
};
use crate::syntax::literal::text_value;
use crate::workspace::{Authority, Target};

impl Checker<'_> {
    /// A core type item applied to its arguments, or a failed type when std lacks it.
    pub fn core_ty(&self, name: &str, args: Vec<Arg>) -> Ty {
        match self
            .db
            .core_item(name)
            .or_else(|| self.db.std_item("core", name))
        {
            Some(item) => Ty::item(item, args),
            None => Ty::Error,
        }
    }

    pub fn str_ty(&self) -> Ty {
        self.core_ty("Str", Vec::new())
    }

    pub fn slice_ty(&self, element: Ty) -> Ty {
        self.core_ty("Slice", vec![Arg::Ty(element)])
    }

    pub fn option_ty(&self, value: Ty) -> Ty {
        self.core_ty("Option", vec![Arg::Ty(value)])
    }

    pub fn result_ty(&self, value: Ty, error: Ty) -> Ty {
        self.core_ty("Result", vec![Arg::Ty(value), Arg::Ty(error)])
    }

    /// The canonical path of the item a type instantiates, when it is a std item.
    pub fn std_path(&self, ty: &Ty) -> Option<String> {
        match ty {
            Ty::Item(item, _) if self.db.is_std(*item) => Some(self.db.canonical(*item)),
            _ => None,
        }
    }

    pub fn is_std(&self, ty: &Ty, path: &str) -> bool {
        self.std_path(ty).as_deref() == Some(path)
    }
}

impl Body<'_, '_> {
    /// Records a typed hole with the bindings in scope of its type, outside probes.
    fn record_hole(&self, token: Tok, ty: &Ty) {
        if self.ck.muted.get() > 0 {
            return;
        }
        let bindings = self
            .scopes
            .iter()
            .flatten()
            .filter(|&&local| self.local(local).ty == *ty)
            .map(|&local| self.local(local).name.clone())
            .collect();
        self.ck.holes.borrow_mut().push(crate::check::Hole {
            item: self.item,
            token,
            expected: ty.clone(),
            bindings,
        });
    }

    /// Reports `a.b` where `a` names a module or alias, with the fix writing `a::b`.
    pub fn dot_for_path(&self, name: &ast::Name) {
        let syntax = &self.ck.db.item(self.item).file.syntax;
        let dot = syntax
            .previous(name.token as usize)
            .unwrap_or(name.token as usize) as Tok;
        let fix = self.ck.fix_replacing(self.item, dot, dot, "::");
        self.report_fixed((dot, name.token), Code::NameDotForPath, vec![fix]);
    }

    /// Reports `Option` or `Result`, starting at `first`, before a core variant, with the fix
    /// removing the qualifier.
    pub fn qualified_core_variant(&self, first: Tok, name: &ast::Name) {
        let syntax = &self.ck.db.item(self.item).file.syntax;
        let start = syntax.tokens[first as usize].start;
        let end = syntax.tokens[name.token as usize].start;
        let fix = self.ck.fix_bytes(self.item, start, end, "");
        self.report_fixed(
            (first, name.token),
            Code::TypeQualifiedCoreVariant,
            vec![fix],
        );
    }
}

/// How an operator application is written: its operands, the target and value of a compound
/// assignment `target op= value` when `compound`.
pub struct Operands<'e> {
    pub left: &'e ast::Expr,
    pub right: &'e ast::Expr,
    pub compound: bool,
}

impl Body<'_, '_> {
    /// The fix of signed `/`, `%` or `>>`: `a.div_trunc(b)`, `a.rem_trunc(b)` or
    /// `a.shr_arith(b)`, a left operand that is no postfix expression parenthesised; for a
    /// compound assignment to a binding or field path `p`, `p = ` and that fixed form.
    fn signed_operation(&self, op: BinaryOp, operands: &Operands) -> Option<Fix> {
        let method = match op {
            BinaryOp::Div => "div_trunc",
            BinaryOp::Rem => "rem_trunc",
            _ => "shr_arith",
        };
        let (left, right) = (operands.left, operands.right);
        let written = self.ck.written(self.item, left.token, left.last);
        let receiver = match left.kind {
            ExprKind::Unary { .. } | ExprKind::Binary { .. } => format!("({written})"),
            _ => written.clone(),
        };
        let argument = self.ck.written(self.item, right.token, right.last);
        let applied = format!("{receiver}.{method}({argument})");
        let text = if operands.compound {
            if !is_path_place(left) {
                return None;
            }
            format!("{written} = {applied}")
        } else {
            applied
        };
        Some(
            self.ck
                .fix_replacing(self.item, left.token, right.last, &text),
        )
    }
}

/// Whether an expression is a binding or a field path from one.
fn is_path_place(expr: &ast::Expr) -> bool {
    match &expr.kind {
        ExprKind::Path { path, args: None } => path.segments.len() == 1,
        ExprKind::SelfValue => true,
        ExprKind::Field { base, .. } => is_path_place(base),
        _ => false,
    }
}

/// Whether an expression's type needs an expected type to be fixed: an unsuffixed number literal,
/// such a literal negated or parenthesised, a hole or `None`.
pub fn needs_expected(expr: &ast::Expr, text: &dyn Fn(Tok) -> String) -> bool {
    match &expr.kind {
        ExprKind::Literal(literal) => {
            matches!(literal.kind, LiteralKind::Int | LiteralKind::Float)
                && literal_suffix(&text(literal.tokens[0])).is_none()
        }
        ExprKind::Unary {
            op: UnaryOp::Neg | UnaryOp::BitNot,
            operand,
        }
        | ExprKind::Paren(operand) => needs_expected(operand, text),
        ExprKind::Todo(_) => true,
        ExprKind::Path { path, args: None } => {
            path.segments.len() == 1 && path.segments[0].text == "None"
        }
        _ => false,
    }
}

impl Body<'_, '_> {
    /// Types an expression against its expected type, reporting a type that differs.
    pub fn check(&mut self, expr: &ast::Expr, expected: &Ty) -> TExpr {
        let typed = self.expr(expr, Some(expected));
        self.expect(&typed, expected, expr.token);
        typed
    }

    /// Types an expression whose context already failed a rule: what it holds is checked, and
    /// nothing about its own type is reported.
    pub fn skim(&mut self, expr: &ast::Expr) -> TExpr {
        self.expr(expr, Some(&Ty::Error))
    }

    /// Reports a typed expression whose type differs from its expected type, a function or
    /// function value expected to fit a function type judged by fit.
    pub fn expect(&self, typed: &TExpr, expected: &Ty, token: Tok) {
        if typed.ty == *expected
            || typed.ty.is_never()
            || typed.ty.has_error()
            || expected.has_error()
        {
            return;
        }
        if let (Ty::Fn(wanted), Ty::Fn(found)) = (expected, &typed.ty) {
            self.fit(found, wanted, token);
            return;
        }
        self.report_mismatch(token, expected, &typed.ty);
    }

    /// The fit rule: kind, head, failures and waiting, in this order, the first unmet reported.
    pub fn fit(&self, found: &FnTy, wanted: &FnTy, token: Tok) {
        let kind_fits = match wanted.kind {
            FnKind::Plain if wanted.convention.is_none() => {
                found.kind == FnKind::Plain && found.convention.is_none()
            }
            FnKind::Plain => found.convention == wanted.convention,
            FnKind::Task => {
                matches!(found.kind, FnKind::Plain | FnKind::Task) && found.convention.is_none()
            }
            FnKind::Interrupt => found.kind == FnKind::Interrupt,
        };
        if !kind_fits {
            self.report(token, Code::EffectFitKind);
            return;
        }
        let heads_fit = found.params.len() == wanted.params.len()
            && found
                .params
                .iter()
                .zip(&wanted.params)
                .all(|(found, wanted)| found.mode == wanted.mode && same(&found.ty, &wanted.ty))
            && same(&found.result, &wanted.result);
        if !heads_fit {
            self.report(token, Code::EffectFitHead);
            return;
        }
        if let Some(fails) = &found.fails
            && !wanted
                .fails
                .as_ref()
                .is_some_and(|wanted| same(fails, wanted))
            && *fails != Ty::Never
        {
            self.report(token, Code::EffectFitFails);
            return;
        }
        if found.wait && !wanted.wait {
            self.report(token, Code::EffectFitWait);
        }
    }

    pub fn expr(&mut self, expr: &ast::Expr, expected: Option<&Ty>) -> TExpr {
        self.expr_jump(expr, expected, false)
    }

    /// Types an expression; `jump` when it stands where `return`, `break`, `continue` and `fail`
    /// may: as a whole expression statement or a match arm's expression body.
    pub fn expr_jump(&mut self, expr: &ast::Expr, expected: Option<&Ty>, jump: bool) -> TExpr {
        let typed = self.expr_kind(expr, expected, jump);
        TExpr {
            last: expr.last,
            ..typed
        }
    }

    fn expr_kind(&mut self, expr: &ast::Expr, expected: Option<&Ty>, jump: bool) -> TExpr {
        let token = expr.token;
        match &expr.kind {
            ExprKind::Literal(literal) => self.literal(token, literal, expected, false),
            ExprKind::Path { path, args } => self.path_expr(token, path, args.as_deref(), expected),
            ExprKind::SelfValue => match self.lookup_local("self") {
                Some(local) => self.local_expr(token, local),
                None => {
                    self.report(token, Code::NameUnknown);
                    TExpr::failed(token)
                }
            },
            ExprKind::Paren(inner) => {
                let typed = self.expr(inner, expected);
                TExpr { token, ..typed }
            }
            ExprKind::If { .. } => self.value_if(expr, expected),
            ExprKind::Match { .. } => self.value_match(expr, expected),
            ExprKind::Closure(_) => {
                self.report(token, Code::ClosureSecondClass);
                TExpr::failed(token)
            }
            ExprKind::Trusted { premise, body } => {
                if matches!(body, ast::TrustedBody::Block(_)) && !jump {
                    self.report(token, Code::TrustedBlockValue);
                }
                self.trusted(token, premise, body, expected)
            }
            ExprKind::Todo(reason) => {
                if reason.value.trim_matches(' ').is_empty() {
                    self.report(reason.token, Code::TodoEmptyReason);
                }
                if self.cx.contract {
                    self.report(token, Code::ContractIllDefined);
                }
                self.frames.last_mut().expect("a frame").todo = true;
                let statement = (self.statement == Some((token, expr.last))).then_some(Ty::Unit);
                match expected.or(statement.as_ref()) {
                    Some(ty) => {
                        self.ck.report(self.item, token, Code::TodoHole);
                        self.record_hole(token, ty);
                        TExpr {
                            token,
                            last: token,
                            ty: ty.clone(),
                            kind: TKind::Todo(reason.value.clone()),
                        }
                    }
                    None => {
                        self.report(token, Code::TodoUntyped);
                        TExpr::failed(token)
                    }
                }
            }
            ExprKind::Dbg(inner) => {
                self.report(token, Code::TestDbg);
                let typed = self.expr(inner, expected);
                TExpr {
                    token,
                    last: token,
                    ty: typed.ty.clone(),
                    kind: TKind::Dbg(Box::new(typed)),
                }
            }
            ExprKind::Quantifier {
                all,
                name,
                range,
                body,
            } => self.quantifier(token, *all, name, range, body),
            ExprKind::Field { base, name } => self.field_expr(token, base, name, expected),
            ExprKind::Call { .. } => self.call(expr, expected, None),
            ExprKind::Index { base, index } => self.index(token, base, index, false),
            ExprKind::Unary { op, operand } => self.unary(token, *op, operand, expected),
            ExprKind::Binary {
                op,
                op_token,
                left,
                right,
            } => self.binary(token, *op, *op_token, left, right, expected),
            ExprKind::Marked { .. } => self.marked(expr, expected),
            ExprKind::When { .. } => self.when(expr, None, expected),
            ExprKind::Return(_)
            | ExprKind::Break(_)
            | ExprKind::Continue(_)
            | ExprKind::Fail(_)
                if !jump =>
            {
                self.report(token, Code::ExprMisplacedJump);
                TExpr::failed(token)
            }
            ExprKind::Return(value) => self.return_expr(token, value.as_deref()),
            ExprKind::Break(label) => {
                let index = self.jump_target(token, label.as_ref(), true);
                TExpr {
                    token,
                    last: token,
                    ty: Ty::Never,
                    kind: index.map_or(TKind::Failed, TKind::Break),
                }
            }
            ExprKind::Continue(label) => {
                let index = self.jump_target(token, label.as_ref(), false);
                TExpr {
                    token,
                    last: token,
                    ty: Ty::Never,
                    kind: index.map_or(TKind::Failed, TKind::Continue),
                }
            }
            ExprKind::Fail(value) => self.fail(token, value),
        }
    }

    /// A literal of the type its suffix or expected type fixes; `negative` for the operand of a
    /// negative literal.
    pub fn literal(
        &mut self,
        token: Tok,
        literal: &ast::Literal,
        expected: Option<&Ty>,
        negative: bool,
    ) -> TExpr {
        let text = self.text(literal.tokens[0]).to_owned();
        let ty = match literal.kind {
            LiteralKind::Int | LiteralKind::Float => {
                let is_int = literal.kind == LiteralKind::Int;
                let suffix_ty = literal_suffix(&text);
                let ty = match (suffix_ty, expected) {
                    (Some(scalar), _) => Ty::Scalar(scalar),
                    (None, Some(Ty::Error)) | (None, Some(Ty::Never)) => {
                        return TExpr::failed(token);
                    }
                    (None, Some(Ty::Scalar(scalar)))
                        if (is_int && scalar.is_integer()) || (!is_int && scalar.is_float()) =>
                    {
                        Ty::Scalar(*scalar)
                    }
                    (None, Some(other)) => {
                        let shown = if is_int {
                            "an integer type"
                        } else {
                            "a float type"
                        };
                        self.ck.report_expected(
                            self.item,
                            token,
                            Code::TypeMismatch,
                            self.ck.show(other, self.module),
                            shown.to_owned(),
                        );
                        return TExpr::failed(token);
                    }
                    (None, None) => {
                        self.report(token, Code::TypeLiteralUntyped);
                        return TExpr::failed(token);
                    }
                };
                if let Ty::Scalar(scalar) = ty {
                    if is_int && scalar.is_integer() {
                        let value = int_literal(&text).unwrap_or(i128::MAX);
                        let value = if negative { -value } else { value };
                        let (low, high) = scalar.range();
                        if value < low || value > high {
                            self.report(token, Code::TypeLiteralRange);
                            return TExpr::failed(token);
                        }
                    } else if !is_int {
                        let digits: String = text
                            .trim_end_matches("f32")
                            .trim_end_matches("f64")
                            .chars()
                            .filter(|c| *c != '_')
                            .collect();
                        let infinite = match scalar {
                            Scalar::F32 => digits.parse::<f32>().is_ok_and(f32::is_infinite),
                            _ => digits.parse::<f64>().is_ok_and(f64::is_infinite),
                        };
                        if infinite {
                            self.report(token, Code::TypeLiteralRange);
                            return TExpr::failed(token);
                        }
                    } else {
                        self.report_mismatch(token, &ty, &Ty::Scalar(Scalar::Usize));
                        return TExpr::failed(token);
                    }
                }
                ty
            }
            LiteralKind::Text | LiteralKind::TextRun => self.ck.str_ty(),
            LiteralKind::Bytes => self.ck.slice_ty(Ty::Scalar(Scalar::U8)),
            LiteralKind::Char => Ty::Scalar(Scalar::Char),
            LiteralKind::Byte => Ty::Scalar(Scalar::U8),
            LiteralKind::True | LiteralKind::False => Ty::Scalar(Scalar::Bool),
        };
        let value = self.literal_value(literal, &ty, negative);
        TExpr {
            token,
            last: token,
            ty,
            kind: TKind::Literal(value),
        }
    }

    /// The value a literal of a valid type denotes.
    fn literal_value(&self, literal: &ast::Literal, ty: &Ty, negative: bool) -> Lit {
        let text = self.text(literal.tokens[0]);
        let quoted = |text: &str| text_value(&format!("\"{}\"", &text[1..text.len() - 1]));
        match literal.kind {
            LiteralKind::Int => {
                let value = int_literal(text).unwrap_or_default();
                Lit::Int(if negative { -value } else { value })
            }
            LiteralKind::Float => {
                let digits: String = text
                    .trim_end_matches("f32")
                    .trim_end_matches("f64")
                    .chars()
                    .filter(|c| *c != '_')
                    .collect();
                let digits = if negative {
                    format!("-{digits}")
                } else {
                    digits
                };
                Lit::Float(match ty {
                    Ty::Scalar(Scalar::F32) => {
                        u64::from(digits.parse::<f32>().unwrap_or_default().to_bits())
                    }
                    _ => digits.parse::<f64>().unwrap_or_default().to_bits(),
                })
            }
            LiteralKind::Text => Lit::Text(text_value(text)),
            LiteralKind::TextRun => {
                let lines: Vec<String> = literal
                    .tokens
                    .iter()
                    .map(|token| text_value(&format!("\"{}\"", &self.text(*token)[2..])))
                    .collect();
                Lit::Text(lines.join("\n"))
            }
            LiteralKind::Bytes => Lit::Bytes(quoted(&text[1..]).chars().map(|c| c as u8).collect()),
            LiteralKind::Char => Lit::Char(quoted(text).chars().next().unwrap_or_default()),
            LiteralKind::Byte => {
                Lit::Int(quoted(&text[1..]).chars().next().map_or(0, |c| c as i128))
            }
            LiteralKind::True => Lit::Bool(true),
            LiteralKind::False => Lit::Bool(false),
        }
    }

    /// A use of a local, a `take` parameter appearing in an ensures clause only inside `old`.
    fn local_expr(&mut self, token: Tok, local: LocalId) -> TExpr {
        self.note_use(local);
        let taken = matches!(self.local(local).kind, LocalKind::Param(Mode::Take));
        if taken && self.cx.ensures && !self.cx.old {
            self.report(token, Code::ContractTakenParameter);
        }
        TExpr {
            token,
            last: token,
            ty: self.local(local).ty.clone(),
            kind: TKind::Local(local),
        }
    }

    /// A path in an expression: a local, a const parameter, `result`, `Unit`, or an item.
    fn path_expr(
        &mut self,
        token: Tok,
        path: &ast::Path,
        args: Option<&[ast::GenericArg]>,
        expected: Option<&Ty>,
    ) -> TExpr {
        if path.segments.len() == 1 && args.is_none() {
            let name = path.segments[0].text.as_str();
            if self.cx.ensures
                && name == "result"
                && let Some(result) = self.result.clone()
            {
                if self.cx.old {
                    self.report(token, Code::ContractOldForm);
                }
                return TExpr {
                    token,
                    last: token,
                    ty: result,
                    kind: TKind::Result,
                };
            }
            if let Some(local) = self.lookup_local(name) {
                return self.local_expr(token, local);
            }
            if let Some(def) = self.generics.iter().find(|def| def.name == name) {
                return match def.constant {
                    Some(scalar) => TExpr {
                        token,
                        last: token,
                        ty: Ty::Scalar(scalar),
                        kind: TKind::ConstParam(def.param),
                    },
                    None => {
                        self.report(token, Code::NameUnknown);
                        TExpr::failed(token)
                    }
                };
            }
        }
        match self.intrinsic(path) {
            Some(Intrinsic::Template) => {
                self.report(token, Code::TextTemplatePosition);
                return TExpr::failed(token);
            }
            Some(Intrinsic::MintRoot) => {
                self.report(token, Code::HwMintRootMisplaced);
                return TExpr::failed(token);
            }
            None => {}
        }
        let named = match self.ck.resolve_use(self.item, self.module, path) {
            Ok(named) => named,
            Err(error) => {
                self.report_path(error);
                return TExpr::failed(token);
            }
        };
        match named {
            Named::Unit if args.is_none() => TExpr {
                token,
                last: token,
                ty: Ty::Unit,
                kind: TKind::Unit,
            },
            Named::Variant(enumeration, variant) => {
                self.unit_variant(token, enumeration, variant, args, expected)
            }
            Named::Item(item) => {
                if !self.ck.db.visible(self.module, item) {
                    self.report(token, Code::NamePrivate);
                }
                self.item_value(token, item, expected)
            }
            Named::Module(_) => {
                self.report(token, Code::NameUnknown);
                TExpr::failed(token)
            }
            _ => {
                self.report(token, Code::NameUnknown);
                TExpr::failed(token)
            }
        }
    }

    /// An item named as a value: a const, registry, contribution or function.
    fn item_value(&mut self, token: Tok, item: ItemId, expected: Option<&Ty>) -> TExpr {
        let sig = self.ck.sig(item);
        let ty = match (&self.ck.db.ast(item).kind, &sig.kind) {
            (ItemKind::Const(_), SigKind::Const { ty }) => ty.clone(),
            (ItemKind::Registry(_), SigKind::Registry { entry }) => self.ck.slice_ty(entry.clone()),
            (ItemKind::Contribute(contribution), _) => {
                self.ck.contribution_entry(item, contribution)
            }
            (ItemKind::Fn(_), SigKind::Fn(function)) => {
                return self.function_value(token, item, function, expected);
            }
            _ => {
                self.report(token, Code::NameUnknown);
                return TExpr::failed(token);
            }
        };
        TExpr {
            token,
            last: token,
            ty,
            kind: TKind::Value(item),
        }
    }

    /// A function item used as a value: its function type, a generic one taking its arguments
    /// from the expected function type.
    fn function_value(
        &mut self,
        token: Tok,
        item: ItemId,
        function: &crate::check::sig::FnSig,
        expected: Option<&Ty>,
    ) -> TExpr {
        if function.is_proj() {
            self.report(token, Code::ClosureProjectionValue);
            return TExpr::failed(token);
        }
        self.ck.check_validator_use(self.item, token, item);
        if self.ck.is_trusted_primitive(item) {
            self.report(token, Code::TrustedPrimitiveOutside);
        }
        if function.modifier == Some(ast::FnModifier::Landing)
            && !self.ck.is_platform_contribution(self.item)
        {
            self.report(token, Code::HwLandingCalled);
        }
        let ty = function.fn_ty();
        let mut subst = crate::check::ty::Subst::default();
        if !function.generics.is_empty() {
            let params: Vec<_> = function.generics.iter().map(|def| def.param).collect();
            if let Some(Ty::Fn(wanted)) = expected {
                let mut infer = super::call::Infer::new(params.clone());
                infer.unify(&Ty::Fn(Rc::new(ty.clone())), &Ty::Fn(wanted.clone()));
                subst = infer.subst;
            }
            if params.iter().any(|param| subst.get(*param).is_none()) {
                self.report(token, Code::GenericUninferred);
                return TExpr::failed(token);
            }
        }
        let type_args = function
            .generics
            .iter()
            .map(|def| match def.constant {
                Some(_) => subst.arg(&Arg::Const(crate::check::ty::Const::Param(def.param))),
                None => Arg::Ty(subst.ty(&Ty::Param(def.param))),
            })
            .collect();
        TExpr {
            token,
            last: token,
            ty: Ty::Fn(Rc::new(subst.fn_ty(&ty))),
            kind: TKind::FnValue { item, type_args },
        }
    }

    /// A unit variant written as a value, its enum's arguments written, from the expected type,
    /// or uninferred.
    pub fn unit_variant(
        &mut self,
        token: Tok,
        enumeration: ItemId,
        variant: usize,
        args: Option<&[ast::GenericArg]>,
        expected: Option<&Ty>,
    ) -> TExpr {
        let sig = self.ck.sig(enumeration);
        let SigKind::Enum { variants } = &sig.kind else {
            return TExpr::failed(token);
        };
        if !matches!(variants[variant].payload, crate::check::sig::Payload::None) {
            self.report(token, Code::TypePayloadShape);
            return TExpr::failed(token);
        }
        let ty = match self.enum_args(token, enumeration, args, expected) {
            Some(ty) => ty,
            None => return TExpr::failed(token),
        };
        TExpr {
            token,
            last: token,
            ty,
            kind: TKind::Construct {
                variant: Some(variant),
                values: Vec::new(),
            },
        }
    }

    /// The type of a generic type's construction or variant path: its arguments as written, or
    /// fixed by the expected type, or uninferred when it has parameters.
    pub fn enum_args(
        &mut self,
        token: Tok,
        item: ItemId,
        args: Option<&[ast::GenericArg]>,
        expected: Option<&Ty>,
    ) -> Option<Ty> {
        let generics = self.ck.shapes(item);
        if let Some(args) = args {
            let ctx = crate::check::sig::TypeCtx {
                item: self.item,
                module: self.module,
                generics: &self.generics,
                self_param: self.self_param,
            };
            let mut problems = Problems::default();
            if args.len() != generics.len() {
                self.report(token, Code::GenericArity);
                return None;
            }
            let written: Vec<Arg> = args
                .iter()
                .zip(generics.iter())
                .map(|(arg, def)| self.ck.generic_arg(ctx, arg, def, &mut problems))
                .collect();
            let ty = Ty::item(item, written);
            problems.instantiations.push((ty.clone(), token));
            self.ck.report_problems(self.item, &problems);
            return Some(ty);
        }
        if generics.is_empty() {
            return Some(Ty::item(item, Vec::new()));
        }
        match expected {
            Some(ty @ Ty::Item(found, _)) if *found == item => Some(ty.clone()),
            Some(Ty::Error) => None,
            _ => {
                self.report(token, Code::GenericUninferred);
                None
            }
        }
    }

    /// `base.name`: a member through a type or interface, or a field of a value.
    fn field_expr(
        &mut self,
        token: Tok,
        base: &ast::Expr,
        name: &ast::Name,
        expected: Option<&Ty>,
    ) -> TExpr {
        if let Some(target) = self.type_target(base) {
            return match target {
                TypeTarget::Type(ty) => self.member_value(token, &ty, name, expected),
                TypeTarget::Interface(_) => {
                    self.report(token, Code::ExprMethodValue);
                    TExpr::failed(token)
                }
                TypeTarget::Module => {
                    self.dot_for_path(name);
                    TExpr::failed(token)
                }
                TypeTarget::Failed => TExpr::failed(token),
            };
        }
        let typed_base = self.expr(base, None);
        self.field_of(token, typed_base, name, true)
    }

    /// A field of a value: of its struct or layout, where visible; `read` unless the field is
    /// the target of `=`.
    pub fn field_of(&mut self, token: Tok, base: TExpr, name: &ast::Name, read: bool) -> TExpr {
        let ty = match &base.ty {
            Ty::Error => return TExpr::failed(token),
            Ty::Item(item, args) => {
                let sig = self.ck.sig(*item);
                let fields = match &sig.kind {
                    SigKind::Struct { fields } => Some(fields.clone()),
                    SigKind::Layout { fields, .. } => Some(fields.clone()),
                    _ => None,
                };
                let found = fields
                    .as_ref()
                    .and_then(|fields| fields.iter().find(|field| field.name == name.text));
                match found {
                    Some(field) => {
                        self.name_member(name.token, Member::Field(*item, name.text.clone()));
                        if read {
                            self.ck.record_read(*item, &name.text);
                        }
                        if !self
                            .ck
                            .db
                            .field_visible(self.module, *item, field.visibility)
                        {
                            self.report(name.token, Code::TypeHiddenField);
                        }
                        let subst = crate::check::ty::Subst::of(
                            sig.generics.iter().map(|def| def.param),
                            args,
                        );
                        subst.ty(&field.ty)
                    }
                    None => {
                        if let Some(tombstone) =
                            self.ck
                                .member_renaming(TypeKey::Item(*item), &name.text, self.module)
                        {
                            return match self.ck.forward_member(self.item, name, tombstone) {
                                Some(renamed) => self.field_of(token, base, &renamed, read),
                                None => TExpr::failed(token),
                            };
                        }
                        if self.ck.method_candidates(&base.ty, &name.text).is_empty() {
                            self.unknown_member(name, &base.ty, None);
                        } else {
                            self.report(name.token, Code::ExprMethodValue);
                        }
                        return TExpr::failed(token);
                    }
                }
            }
            _ => {
                if self.ck.method_candidates(&base.ty, &name.text).is_empty() {
                    self.unknown_member(name, &base.ty, None);
                } else {
                    self.report(name.token, Code::ExprMethodValue);
                }
                return TExpr::failed(token);
            }
        };
        TExpr {
            token,
            last: token,
            ty,
            kind: TKind::Field {
                base: Box::new(base),
                name: name.text.clone(),
            },
        }
    }

    /// What the base of a member path names when it names no value: a type, an interface, a
    /// module or alias, or nothing that resolves.
    pub fn type_target(&mut self, base: &ast::Expr) -> Option<TypeTarget> {
        let ExprKind::Path { path, args } = &base.kind else {
            return None;
        };
        if path.segments.len() == 1 && args.is_none() {
            let name = &path.segments[0].text;
            if self.lookup_local(name).is_some() {
                return None;
            }
            if let Some(def) = self.generics.iter().find(|def| def.name == *name) {
                return Some(match def.constant {
                    Some(_) => return None,
                    None => TypeTarget::Type(Ty::Param(def.param)),
                });
            }
            if name == "Self"
                && let Some(param) = self.self_param
            {
                return Some(TypeTarget::Type(Ty::Param(param)));
            }
        }
        let named = match self.ck.resolve_use(self.item, self.module, path) {
            Ok(named) => named,
            Err(_) => return None,
        };
        let type_expr = ast::TypeExpr {
            token: base.token,
            last: base.last,
            kind: ast::TypeKind::Path {
                path: path.clone(),
                args: args.clone(),
            },
        };
        match named {
            Named::Module(_) => Some(TypeTarget::Module),
            Named::Scalar(_) | Named::Unit | Named::Dyn => {
                Some(TypeTarget::Type(self.type_expr(&type_expr)))
            }
            Named::Item(item) => match &self.ck.db.ast(item).kind {
                ItemKind::Struct(_) | ItemKind::Enum(_) | ItemKind::Layout(_) => {
                    if !self.ck.db.visible(self.module, item) {
                        self.report(base.token, Code::NamePrivate);
                    }
                    let ty = match args {
                        Some(_) => self.type_expr(&type_expr),
                        None => self.ck.own_type(item),
                    };
                    Some(TypeTarget::Type(ty))
                }
                ItemKind::Registers(_) => Some(TypeTarget::Type(Ty::Registers(item))),
                ItemKind::Interface(_) => Some(TypeTarget::Interface(item)),
                _ => None,
            },
            Named::Variant(..) | Named::Never => Some(TypeTarget::Failed),
        }
    }

    /// `T.name` written without an argument list: a unit variant, a tag, a register, or a
    /// function as a value.
    fn member_value(
        &mut self,
        token: Tok,
        ty: &Ty,
        name: &ast::Name,
        expected: Option<&Ty>,
    ) -> TExpr {
        match ty {
            Ty::Error => return TExpr::failed(token),
            Ty::Registers(item) => {
                if let SigKind::Registers { registers } = &self.ck.sig(*item).kind
                    && let Some(index) = registers
                        .iter()
                        .position(|register| register.name == name.text)
                {
                    let register = &registers[index];
                    let ty = match self.ck.db.std_item("hw", "Register") {
                        Some(register_item) => Ty::item(
                            register_item,
                            vec![Arg::Ty(Ty::Registers(*item)), Arg::Ty(register.ty.clone())],
                        ),
                        None => Ty::Error,
                    };
                    return TExpr {
                        token,
                        last: token,
                        ty,
                        kind: TKind::Register {
                            block: *item,
                            index,
                        },
                    };
                }
                self.report(name.token, Code::NameUnknown);
                return TExpr::failed(token);
            }
            _ => {}
        }
        if let Ty::Item(item, args) = ty {
            let item = *item;
            if self.core_variant_qualified(item, &name.text) {
                self.qualified_core_variant(token, name);
                return TExpr::failed(token);
            }
            match &self.ck.sig(item).kind {
                SigKind::Enum { variants } => {
                    if let Some(variant) = variants
                        .iter()
                        .position(|variant| variant.name == name.text)
                    {
                        self.name_member(name.token, Member::Variant(item, name.text.clone()));
                        let ty = if self.ck.shapes(item).is_empty() {
                            Ty::item(item, Vec::new())
                        } else if args.iter().any(
                            |arg| matches!(arg, Arg::Ty(Ty::Param(param)) if param.owner == item),
                        ) {
                            match expected {
                                Some(expected @ Ty::Item(found, _)) if *found == item => {
                                    expected.clone()
                                }
                                Some(Ty::Error) => return TExpr::failed(token),
                                _ => {
                                    self.report(token, Code::GenericUninferred);
                                    return TExpr::failed(token);
                                }
                            }
                        } else {
                            ty.clone()
                        };
                        if !matches!(variants[variant].payload, crate::check::sig::Payload::None) {
                            self.report(token, Code::TypePayloadShape);
                            return TExpr::failed(token);
                        }
                        return TExpr {
                            token,
                            last: token,
                            ty,
                            kind: TKind::Construct {
                                variant: Some(variant),
                                values: Vec::new(),
                            },
                        };
                    }
                }
                SigKind::Layout { tags, .. } if tags.contains(&name.text) => {
                    return TExpr {
                        token,
                        last: token,
                        ty: ty.clone(),
                        kind: TKind::Construct {
                            variant: tags.iter().position(|tag| *tag == name.text),
                            values: Vec::new(),
                        },
                    };
                }
                _ => {}
            }
        }
        if ty.scalar().is_some() && crate::check::body::call::is_conversion(&name.text) {
            self.report(token, Code::NumConversionValue);
            return TExpr::failed(token);
        }
        let candidates = self.ck.method_candidates(ty, &name.text);
        let function = candidates.iter().find_map(|candidate| match candidate {
            crate::check::conform::Candidate::Function { item, .. } => Some(*item),
            crate::check::conform::Candidate::Method {
                function: Some((item, _)),
                ..
            } => Some(*item),
            _ => None,
        });
        match function {
            Some(function) => match self.ck.fn_sig(function) {
                Some(sig) => {
                    if !self.ck.db.visible(self.module, function) {
                        self.report(name.token, Code::NamePrivate);
                    }
                    self.name_member(name.token, Member::Function(function));
                    self.function_value(token, function, &sig, expected)
                }
                None => TExpr::failed(token),
            },
            None => {
                if let Some(tombstone) = type_key(ty)
                    .and_then(|key| self.ck.member_renaming(key, &name.text, self.module))
                {
                    return match self.ck.forward_member(self.item, name, tombstone) {
                        Some(renamed) => self.member_value(token, ty, &renamed, expected),
                        None => TExpr::failed(token),
                    };
                }
                let written = self.ck.written(self.item, token, name.token);
                if !self
                    .ck
                    .report_newer_release(self.item, (token, name.token), &written)
                {
                    self.report(name.token, Code::NameUnknown);
                }
                TExpr::failed(token)
            }
        }
    }

    /// Whether `Option` or `Result` qualifies one of their core variants.
    pub fn core_variant_qualified(&self, item: ItemId, name: &str) -> bool {
        let path = self.ck.db.canonical(item);
        matches!(
            (path.as_str(), name),
            ("core::Option", "Some" | "None") | ("core::Result", "Ok" | "Err")
        )
    }

    fn unary(
        &mut self,
        token: Tok,
        op: UnaryOp,
        operand: &ast::Expr,
        expected: Option<&Ty>,
    ) -> TExpr {
        if op == UnaryOp::Not {
            let typed = self.check(operand, &Ty::Scalar(Scalar::Bool));
            return TExpr {
                token,
                last: token,
                ty: Ty::Scalar(Scalar::Bool),
                kind: TKind::Unary {
                    op,
                    operand: Box::new(typed),
                },
            };
        }
        if op == UnaryOp::Neg
            && let ExprKind::Literal(literal) = &operand.kind
            && matches!(literal.kind, LiteralKind::Int | LiteralKind::Float)
        {
            let typed = self.literal(token, literal, expected, true);
            if let Some(scalar) = typed.ty.scalar()
                && scalar.is_integer()
                && !scalar.is_signed()
                && literal.kind == LiteralKind::Int
            {
                let value = int_literal(self.text(literal.tokens[0])).unwrap_or(0);
                if value != 0 {
                    self.report(token, Code::TypeLiteralRange);
                    return TExpr::failed(token);
                }
            }
            return typed;
        }
        let typed = self.expr(operand, expected);
        let ty = typed.ty.clone();
        let result = match &ty {
            Ty::Error => Ty::Error,
            Ty::Scalar(scalar)
                if op == UnaryOp::Neg && scalar.is_integer() && !scalar.is_signed() =>
            {
                self.report(token, Code::NumUnsignedNegation);
                Ty::Error
            }
            _ => {
                let name = if op == UnaryOp::Neg { "Neg" } else { "BitNot" };
                match self.ck.db.std_item("core", name) {
                    Some(interface) => match self.ck.conformance(&ty, interface) {
                        Some((_, args)) => {
                            args.first().and_then(Arg::ty).cloned().unwrap_or(Ty::Error)
                        }
                        None => {
                            self.report(token, Code::InterfaceNotImplemented);
                            Ty::Error
                        }
                    },
                    None => Ty::Error,
                }
            }
        };
        TExpr {
            token,
            last: token,
            ty: result,
            kind: TKind::Unary {
                op,
                operand: Box::new(typed),
            },
        }
    }

    fn binary(
        &mut self,
        token: Tok,
        op: BinaryOp,
        op_token: Tok,
        left: &ast::Expr,
        right: &ast::Expr,
        expected: Option<&Ty>,
    ) -> TExpr {
        if matches!(op, BinaryOp::And | BinaryOp::Or) {
            let left = self.check(left, &Ty::Scalar(Scalar::Bool));
            let right = self.check(right, &Ty::Scalar(Scalar::Bool));
            return TExpr {
                token,
                last: token,
                ty: Ty::Scalar(Scalar::Bool),
                kind: TKind::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
            };
        }
        if op.is_range() {
            return self.range(token, op, left, right, expected);
        }
        let item = self.item;
        let ck = self.ck;
        let text = |token: Tok| ck.db.item(item).text(token).to_owned();
        let left_typed = if needs_expected(left, &text) {
            let shift = matches!(op, BinaryOp::Shl | BinaryOp::Shr);
            if !shift && !needs_expected(right, &text) {
                let right_typed = self.expr(right, None);
                let left_typed = self.expr(left, Some(&right_typed.ty));
                let operands = Operands {
                    left,
                    right,
                    compound: false,
                };
                return self.operator(token, op, op_token, left_typed, Some(right_typed), operands);
            }
            let hint = if op.is_comparison() { None } else { expected };
            self.expr(left, hint)
        } else {
            self.expr(left, None)
        };
        let operands = Operands {
            left,
            right,
            compound: false,
        };
        self.operator(token, op, op_token, left_typed, None, operands)
    }

    /// The binary operator `target op value` of a compound assignment, its target typed.
    pub fn binary_op(
        &mut self,
        op: BinaryOp,
        op_token: Tok,
        typed: &TExpr,
        target: &ast::Expr,
        value: &ast::Expr,
    ) -> TExpr {
        let operands = Operands {
            left: target,
            right: value,
            compound: true,
        };
        self.operator(target.token, op, op_token, typed.clone(), None, operands)
    }

    /// Applies an operator to a typed left operand: the operator interface its type implements
    /// fixes the right operand's expected type and the result.
    fn operator(
        &mut self,
        token: Tok,
        op: BinaryOp,
        op_token: Tok,
        left: TExpr,
        right_typed: Option<TExpr>,
        operands: Operands,
    ) -> TExpr {
        let right = operands.right;
        let ty = left.ty.clone();
        let right_check = |body: &mut Self, expected: Option<&Ty>| match right_typed.clone() {
            Some(typed) => {
                if let Some(expected) = expected {
                    body.expect(&typed, expected, right.token);
                }
                typed
            }
            None => match expected {
                Some(expected) => body.check(right, expected),
                None => body.expr(right, None),
            },
        };
        let (right, result) = match &ty {
            Ty::Error => (right_check(self, Some(&Ty::Error)), Ty::Error),
            Ty::Scalar(scalar) if scalar.is_float() && op.is_comparison() => {
                (right_check(self, Some(&ty)), Ty::Scalar(Scalar::Bool))
            }
            _ if op.is_comparison() => {
                let name = if matches!(op, BinaryOp::Eq | BinaryOp::Ne) {
                    "Eq"
                } else {
                    "Ord"
                };
                let interface = self.ck.db.core_item(name);
                let implements =
                    interface.is_some_and(|interface| self.ck.implements(&ty, interface));
                if !implements {
                    self.report(op_token, Code::InterfaceNotImplemented);
                    (
                        right_check(self, Some(&Ty::Error)),
                        Ty::Scalar(Scalar::Bool),
                    )
                } else {
                    (right_check(self, Some(&ty)), Ty::Scalar(Scalar::Bool))
                }
            }
            _ => {
                if let Ty::Scalar(scalar) = &ty {
                    let code = match op {
                        BinaryOp::Div | BinaryOp::Rem if scalar.is_signed() => {
                            Some(Code::NumSignedDivision)
                        }
                        BinaryOp::Shr if scalar.is_signed() => Some(Code::NumSignedShiftRight),
                        _ => None,
                    };
                    if let Some(code) = code {
                        let fixes = self.signed_operation(op, &operands).into_iter().collect();
                        let construct = (operands.left.token, operands.right.last);
                        self.report_fixed(construct, code, fixes);
                        let right = right_check(self, Some(&ty));
                        return TExpr {
                            token,
                            last: token,
                            ty: Ty::Error,
                            kind: TKind::Binary {
                                op,
                                left: Box::new(left),
                                right: Box::new(right),
                            },
                        };
                    }
                }
                let name = match op {
                    BinaryOp::Add => "Add",
                    BinaryOp::Sub => "Sub",
                    BinaryOp::Mul => "Mul",
                    BinaryOp::Div => "Div",
                    BinaryOp::Rem => "Rem",
                    BinaryOp::BitAnd => "BitAnd",
                    BinaryOp::BitOr => "BitOr",
                    BinaryOp::BitXor => "BitXor",
                    BinaryOp::Shl => "Shl",
                    _ => "Shr",
                };
                let conformance = self
                    .ck
                    .db
                    .std_item("core", name)
                    .and_then(|interface| self.ck.conformance(&ty, interface));
                match conformance {
                    Some((_, args)) if args.len() == 2 => {
                        let rhs = args[0].ty().cloned().unwrap_or(Ty::Error);
                        let out = args[1].ty().cloned().unwrap_or(Ty::Error);
                        (right_check(self, Some(&rhs)), out)
                    }
                    Some(_) => (right_check(self, Some(&Ty::Error)), Ty::Error),
                    None => {
                        self.report(op_token, Code::InterfaceNotImplemented);
                        (right_check(self, Some(&Ty::Error)), Ty::Error)
                    }
                }
            }
        };
        TExpr {
            token,
            last: token,
            ty: result,
            kind: TKind::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            },
        }
    }

    /// A range expression: both bounds of one integer scalar or `char` type, a `core::Range`.
    fn range(
        &mut self,
        token: Tok,
        op: BinaryOp,
        low: &ast::Expr,
        high: &ast::Expr,
        expected: Option<&Ty>,
    ) -> TExpr {
        let item = self.item;
        let ck = self.ck;
        let text = |token: Tok| ck.db.item(item).text(token).to_owned();
        let element_hint = expected.and_then(|expected| {
            self.ck
                .is_std(expected, "core::Range")
                .then(|| expected.type_arg(0).cloned())
                .flatten()
        });
        let (low_typed, high_typed) = if needs_expected(low, &text) && !needs_expected(high, &text)
        {
            let high_typed = self.expr(high, None);
            let low_typed = self.check(low, &high_typed.ty.clone());
            (low_typed, high_typed)
        } else if needs_expected(low, &text) {
            let low_typed = self.expr(low, element_hint.as_ref());
            let high_typed = self.check(high, &low_typed.ty.clone());
            (low_typed, high_typed)
        } else {
            let low_typed = self.expr(low, None);
            let high_typed = self.check(high, &low_typed.ty.clone());
            (low_typed, high_typed)
        };
        let element = low_typed.ty.clone();
        let valid = match &element {
            Ty::Error => true,
            Ty::Scalar(scalar) => scalar.is_integer() || *scalar == Scalar::Char,
            _ => false,
        };
        if !valid {
            self.report(token, Code::ExprRangeType);
        }
        let ty = if valid && !element.has_error() {
            self.ck.core_ty("Range", vec![Arg::Ty(element)])
        } else {
            Ty::Error
        };
        TExpr {
            token,
            last: token,
            ty,
            kind: TKind::Binary {
                op,
                left: Box::new(low_typed),
                right: Box::new(high_typed),
            },
        }
    }

    /// An `if` or `match` in statement position: its blocks hold statements and its arms'
    /// expression bodies are in statement position.
    pub fn statement_branches(&mut self, expr: &ast::Expr) -> TExpr {
        let token = expr.token;
        match &expr.kind {
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                let condition = self.check(condition, &Ty::Scalar(Scalar::Bool));
                let (then, then_diverges) = self.block(then);
                let (otherwise, otherwise_diverges) = match otherwise.as_deref() {
                    Some(ast::Else::Block(block)) => {
                        let (typed, diverges) = self.block(block);
                        (
                            Some(Box::new(TExpr {
                                token: block.token,
                                last: block.end,
                                ty: if diverges { Ty::Never } else { Ty::Unit },
                                kind: TKind::Block(typed),
                            })),
                            diverges,
                        )
                    }
                    Some(ast::Else::If(chained)) => {
                        let typed = self.statement_branches(chained);
                        let diverges = typed.ty.is_never();
                        (Some(Box::new(typed)), diverges)
                    }
                    None => (None, false),
                };
                let diverges = then_diverges && otherwise.is_some() && otherwise_diverges;
                TExpr {
                    token,
                    last: token,
                    ty: if diverges { Ty::Never } else { Ty::Unit },
                    kind: TKind::If {
                        condition: Box::new(condition),
                        then,
                        otherwise,
                    },
                }
            }
            ExprKind::Match {
                first,
                scrutinee,
                arms,
                ..
            } => self.match_expr(token, *first, scrutinee, arms, None, true),
            _ => self.expr(expr, None),
        }
    }

    /// A value-yielding `if`: each branch diverges or holds one value statement, and an `else`.
    fn value_if(&mut self, expr: &ast::Expr, expected: Option<&Ty>) -> TExpr {
        let ExprKind::If {
            condition,
            then,
            otherwise,
        } = &expr.kind
        else {
            unreachable!("an if expression");
        };
        let token = expr.token;
        let condition = self.check(condition, &Ty::Scalar(Scalar::Bool));
        let mut fixed: Option<Ty> = expected.cloned();
        let then_typed = self.value_block(then, &mut fixed);
        let otherwise_typed = match otherwise.as_deref() {
            Some(ast::Else::Block(block)) => Some(self.value_block(block, &mut fixed)),
            Some(ast::Else::If(chained)) => {
                let typed = self.value_if(chained, fixed.as_ref());
                if fixed.is_none() && !typed.ty.is_never() {
                    fixed = Some(typed.ty.clone());
                }
                Some(typed)
            }
            None => {
                self.report(token, Code::ExprMissingElse);
                None
            }
        };
        let all_never = then_typed.ty.is_never()
            && otherwise_typed
                .as_ref()
                .is_some_and(|typed| typed.ty.is_never());
        let ty = if all_never {
            Ty::Never
        } else {
            fixed.unwrap_or(Ty::Error)
        };
        let then_block = match then_typed.kind {
            TKind::Block(block) => block,
            _ => unreachable!("a branch block"),
        };
        TExpr {
            token,
            last: token,
            ty,
            kind: TKind::If {
                condition: Box::new(condition),
                then: then_block,
                otherwise: otherwise_typed.map(Box::new),
            },
        }
    }

    /// A branch block of a value-yielding `if`: it diverges, or holds one expression statement
    /// whose expression is its value.
    fn value_block(&mut self, block: &ast::Block, fixed: &mut Option<Ty>) -> TExpr {
        if let [stmt] = &block.stmts[..]
            && let ast::StmtKind::Expr(value) = &stmt.kind
        {
            self.push_scope();
            let typed = self.expr_jump(value, fixed.as_ref(), true);
            self.pop_scope();
            match fixed {
                Some(ty) => self.expect(&typed, ty, value.token),
                None if !typed.ty.is_never() => *fixed = Some(typed.ty.clone()),
                None => {}
            }
            let ty = typed.ty.clone();
            let stmt = crate::check::tir::TStmt {
                token: stmt.token,
                kind: TStmtKind::Expr(typed),
            };
            return TExpr {
                token: block.token,
                last: block.end,
                ty,
                kind: TKind::Block(crate::check::tir::TBlock {
                    end: block.end,
                    stmts: vec![stmt],
                }),
            };
        }
        let (typed, diverges, tail) = self.tail_block(block);
        if !diverges {
            let at = block.stmts.first().map_or(block.token, |stmt| stmt.token);
            self.report(at, Code::ExprBranchForm);
        } else if let Some(tail) = tail {
            self.unused_result(&tail);
        }
        TExpr {
            token: block.token,
            last: block.end,
            ty: if diverges { Ty::Never } else { Ty::Error },
            kind: TKind::Block(typed),
        }
    }

    fn value_match(&mut self, expr: &ast::Expr, expected: Option<&Ty>) -> TExpr {
        let ExprKind::Match {
            first,
            scrutinee,
            arms,
            ..
        } = &expr.kind
        else {
            unreachable!("a match expression");
        };
        self.match_expr(expr.token, *first, scrutinee, arms, expected, false)
    }

    /// A `match`: its scrutinee, then each arm's patterns, guard and body, its arms' disjointness
    /// and exhaustiveness.
    fn match_expr(
        &mut self,
        token: Tok,
        first: bool,
        scrutinee: &ast::Expr,
        arms: &[ast::Arm],
        expected: Option<&Ty>,
        statement: bool,
    ) -> TExpr {
        let scrutinee_typed = self.expr(scrutinee, None);
        let scrutinee_ty = scrutinee_typed.ty.clone();
        let mut fixed: Option<Ty> = expected.cloned();
        let mut typed_arms = Vec::new();
        let mut else_seen = false;
        let mut all_never = true;
        for arm in arms {
            self.push_scope();
            let patterns = match &arm.patterns {
                ast::ArmPatterns::Patterns(patterns) => {
                    self.arm_patterns(arm.token, patterns, &scrutinee_ty)
                }
                ast::ArmPatterns::Else => {
                    if else_seen {
                        self.report(arm.token, Code::ExprOverlappingArms);
                    }
                    else_seen = true;
                    Vec::new()
                }
            };
            let guard = arm.guard.as_ref().map(|guard| {
                if !first {
                    let fix = self.ck.fix_appending(self.item, token, " first");
                    let construct = (guard.token, guard.last);
                    self.report_fixed(construct, Code::ExprGuardOutsideFirst, vec![fix]);
                }
                self.check(guard, &Ty::Scalar(Scalar::Bool))
            });
            let body = match &arm.body {
                ast::ArmBody::Block(block) => {
                    let (typed, diverges, tail) = self.tail_block(block);
                    if !statement && !diverges {
                        let at = block.stmts.first().map_or(block.token, |stmt| stmt.token);
                        self.report(at, Code::ExprBranchForm);
                    } else if let Some(tail) = tail {
                        self.unused_result(&tail);
                    }
                    TExpr {
                        token: block.token,
                        last: block.end,
                        ty: if diverges { Ty::Never } else { Ty::Unit },
                        kind: TKind::Block(typed),
                    }
                }
                ast::ArmBody::Expr(body) if statement => {
                    let enclosing = self.statement.replace((body.token, body.last));
                    let typed = match &body.kind {
                        ExprKind::If { .. } | ExprKind::Match { .. } => {
                            self.statement_branches(body)
                        }
                        _ => self.expr_jump(body, None, true),
                    };
                    self.statement = enclosing;
                    if !matches!(typed.ty, Ty::Unit | Ty::Never | Ty::Error) {
                        self.unused_result(&super::Tail {
                            token: body.token,
                            last: body.last,
                            ty: typed.ty.clone(),
                        });
                    }
                    typed
                }
                ast::ArmBody::Expr(body) => {
                    let typed = self.expr_jump(body, fixed.as_ref(), true);
                    match &fixed {
                        Some(ty) => self.expect(&typed, ty, body.token),
                        None if !typed.ty.is_never() => fixed = Some(typed.ty.clone()),
                        None => {}
                    }
                    typed
                }
            };
            all_never &= body.ty.is_never();
            self.pop_scope();
            typed_arms.push(TArm {
                token: arm.token,
                patterns,
                guard,
                body,
            });
        }
        if !scrutinee_ty.has_error() {
            self.check_arms(token, first, &scrutinee_ty, &typed_arms, arms);
        }
        let ty = if all_never {
            Ty::Never
        } else if statement {
            Ty::Unit
        } else {
            fixed.unwrap_or(Ty::Error)
        };
        TExpr {
            token,
            last: token,
            ty,
            kind: TKind::Match {
                scrutinee: Box::new(scrutinee_typed),
                arms: typed_arms,
            },
        }
    }

    /// A closure written as an argument for a parameter of function type `wanted`, in the role
    /// `written` gives it.
    pub fn closure(
        &mut self,
        token: Tok,
        closure: &ast::Closure,
        wanted: Option<&FnTy>,
        written: Written,
    ) -> TExpr {
        let task = wanted.is_some_and(|wanted| wanted.kind == FnKind::Task);
        self.closures += 1;
        let index = self.closures;
        if written == Written::ScopeBody {
            self.scope_bodies.push(index);
        }
        let mut seen: Vec<&str> = Vec::new();
        for capture in &closure.captures {
            if seen.contains(&capture.name.text.as_str()) {
                self.report(capture.token, Code::ClosureCaptureDuplicate);
            }
            seen.push(&capture.name.text);
        }
        let listed: Vec<(String, Mode, Tok)> = closure
            .captures
            .iter()
            .map(|capture| (capture.name.text.clone(), capture.mode, capture.token))
            .collect();
        let params: Vec<FnParam> = closure
            .params
            .iter()
            .map(|param| FnParam {
                mode: param.mode,
                name: param.name.text.clone(),
                ty: self.type_expr(&param.ty),
            })
            .collect();
        let written_result = closure.result.as_ref().map(|ty| self.type_expr(ty));
        let fails = closure.fails.as_ref().map(|ty| self.type_expr(ty));
        let wait = closure.uses.as_ref().is_some_and(ast::Uses::is_wait);
        let short_result = wanted.map(|wanted| wanted.result.clone());
        let result = match (&closure.body, &written_result) {
            (_, Some(result)) => result.clone(),
            (ast::ClosureBody::Short(_), None) => short_result.clone().unwrap_or(Ty::Error),
            (ast::ClosureBody::Block(_), None) => Ty::Unit,
        };
        for capture in &closure.captures {
            match self.lookup_local(&capture.name.text) {
                Some(local) => self.note_use(local),
                None => self.report(capture.name.token, Code::NameUnknown),
            }
        }
        let loops = self.loops.len();
        let scopes = self.scopes.len();
        self.push_frame(Frame {
            result: result.clone(),
            failure: fails.clone(),
            test: false,
            closure: index,
            task,
            listed,
            captures: Vec::new(),
            loops,
            scopes,
            todo: false,
            may_fail: None,
            several_failures: false,
            may_wait: false,
        });
        self.push_scope();
        let param_ids: Vec<_> = closure
            .params
            .iter()
            .zip(&params)
            .map(|(param, typed)| {
                self.declare(&param.name, typed.ty.clone(), LocalKind::Param(param.mode))
            })
            .collect();
        let (body, result) = match &closure.body {
            ast::ClosureBody::Short(expr) => {
                let typed = self.expr(expr, short_result.as_ref());
                let result = typed.ty.clone();
                (typed, result)
            }
            ast::ClosureBody::Block(block) => {
                let typed = self.result_block(block, &result);
                (
                    TExpr {
                        token: block.token,
                        last: block.end,
                        ty: Ty::Unit,
                        kind: TKind::Block(typed),
                    },
                    result,
                )
            }
        };
        self.pop_scope();
        let frame = self.frames.pop().expect("the closure's frame");
        let closure_ty = FnTy {
            kind: if task { FnKind::Task } else { FnKind::Plain },
            params,
            result,
            fails,
            wait,
            limits: limit_set(closure.limits.as_ref(), false, |token| {
                self.text(token).to_owned()
            }),
            convention: None,
        };
        if let Some(limits) = &closure.limits {
            let text = |token: Tok| self.text(token).to_owned();
            for problem in limit_problems(limits, wait, false, text) {
                self.ck
                    .report_problem(self.item, problem.token, problem.code);
            }
        }
        if let Some(code) = self.ck.fn_type_problem(&closure_ty) {
            self.report(token, code);
        }
        if task
            && matches!(self.ck.db.target, Target::Uefi | Target::Bare)
            && closure_ty.limits.stack.is_none()
        {
            self.report(token, Code::SuspendStackMissing);
        }
        if let (Some(fails), Some(written)) = (&closure_ty.fails, &closure.fails)
            && !self.ck.is_failure_type(fails)
        {
            self.report(written.token, Code::ErrorFailureType);
        }
        if let ast::ClosureBody::Short(expr) = &closure.body
            && (frame.may_fail.is_some() || frame.may_wait)
        {
            let fixes = self
                .long_closure(token, closure, expr, &closure_ty, &frame)
                .into_iter()
                .collect();
            self.report_fixed((expr.token, expr.last), Code::ClosureShortEffect, fixes);
        } else if matches!(closure.body, ast::ClosureBody::Block(_)) {
            self.check_effects(
                token,
                &frame,
                &closure_ty,
                &Clauses::of_closure(closure),
                false,
            );
        }
        let captures = frame
            .captures
            .iter()
            .map(|(outer, inner)| TCapture {
                local: *inner,
                outer: *outer,
                listed: frame
                    .listed
                    .iter()
                    .find(|(name, _, _)| *name == self.local(*outer).name)
                    .map(|(_, mode, token)| (*mode, *token)),
            })
            .collect();
        for (name, _, capture_token) in &frame.listed {
            let used = frame
                .captures
                .iter()
                .any(|(outer, _)| self.local(*outer).name == *name);
            if !used
                && let Some(capture) = closure
                    .captures
                    .iter()
                    .find(|capture| capture.token == *capture_token)
            {
                let fix = self
                    .ck
                    .fix_capture_deleted(self.item, capture.token, capture.name.token);
                let construct = (capture.token, capture.name.token);
                self.report_fixed(construct, Code::ClosureCaptureUnused, vec![fix]);
            }
        }
        TExpr {
            token,
            last: token,
            ty: Ty::Fn(Rc::new(closure_ty.clone())),
            kind: TKind::Closure(Box::new(TClosure {
                task,
                params: param_ids,
                captures,
                ty: closure_ty,
                body,
                index,
                loops,
                lent: match written {
                    Written::Lent(scope) => Some(scope),
                    _ => None,
                },
                demanded: wanted.map(|wanted| wanted.limits).unwrap_or_default(),
            })),
        }
    }

    /// The long form of a short closure written at `token` whose expression may fail or wait:
    /// its head with the result, `fails` and `uses wait` the expression needs, and a block
    /// returning the expression.
    fn long_closure(
        &self,
        token: Tok,
        closure: &ast::Closure,
        expr: &ast::Expr,
        ty: &FnTy,
        frame: &Frame,
    ) -> Option<Fix> {
        if frame.several_failures {
            return None;
        }
        let mut head = self.ck.written(self.item, token, closure.close);
        if !matches!(ty.result, Ty::Unit | Ty::Never) {
            head += &format!(" -> {}", self.ck.show(&ty.result, self.module));
        }
        if let Some(failure) = &frame.may_fail {
            head += &format!(" fails {}", self.ck.show(failure, self.module));
        }
        if frame.may_wait {
            head += " uses wait";
        }
        let written = self.ck.written(self.item, expr.token, expr.last);
        let statement = if matches!(ty.result, Ty::Unit | Ty::Never) {
            written
        } else {
            format!("return {written}")
        };
        let indentation = self.ck.indentation(self.item, token);
        let long = format!("{head} {{\n{indentation}    {statement}\n{indentation}}}");
        Some(self.ck.fix_replacing(self.item, token, expr.last, &long))
    }

    /// The rules a body's effects judge its head by: `fails` and `uses wait` written exactly when
    /// the body needs them, the unneeded ones judged unless `exempt`, and a cancellable failure
    /// type beside waiting.
    pub fn check_effects(
        &self,
        token: Tok,
        frame: &Frame,
        head: &FnTy,
        clauses: &Clauses,
        exempt: bool,
    ) {
        let construct = (token, clauses.last);
        let has_fails = head.fails.is_some();
        let add = |after: Tok, text: &str| vec![self.ck.fix_appending(self.item, after, text)];
        if let Some(raised) = frame.may_fail.as_ref().filter(|_| !has_fails) {
            let fixes = if clauses.fixable && !frame.several_failures && !raised.has_error() {
                add(
                    clauses.after,
                    &format!(" fails {}", self.ck.show(raised, self.module)),
                )
            } else {
                Vec::new()
            };
            self.report_fixed(construct, Code::ItemFailsMissing, fixes);
        }
        if frame.may_wait && !head.wait {
            if head.kind == FnKind::Interrupt {
                self.report(token, Code::KernelIrqWait);
            } else {
                let fixes = match (clauses.fixable, clauses.fails) {
                    (false, _) => Vec::new(),
                    (true, Some(fails)) => add(fails.last, " uses wait"),
                    (true, None) => add(clauses.after, " fails task::Cancelled uses wait"),
                };
                self.report_fixed(construct, Code::ItemWaitMissing, fixes);
            }
        }
        if self.ck.cancel_unmapped(head, frame.may_fail.is_some()) {
            let fixes = match &head.fails {
                None => add(clauses.after, " fails task::Cancelled"),
                Some(fails) => self.ck.cancelled_variant_fix(fails).into_iter().collect(),
            };
            self.report_fixed(construct, Code::ErrorCancelUnmapped, fixes);
        }
        if exempt || frame.todo {
            return;
        }
        let syntax = &self.ck.db.item(self.item).file.syntax;
        if frame.may_fail.is_none() && has_fails && !head.wait {
            let fixes = clauses.fails.and_then(|fails| {
                let keyword = syntax.previous(fails.token as usize)? as Tok;
                Some(self.ck.fix_deleting(self.item, keyword, fails.last))
            });
            self.report_fixed(
                construct,
                Code::ItemFailsUnneeded,
                fixes.into_iter().collect(),
            );
        }
        if !frame.may_wait && head.wait {
            let fixes = clauses
                .uses
                .map(|uses| self.ck.fix_deleting(self.item, uses.token, uses.word.token));
            self.report_fixed(
                construct,
                Code::ItemWaitUnneeded,
                fixes.into_iter().collect(),
            );
        }
    }

    fn trusted(
        &mut self,
        token: Tok,
        premise: &ast::Text,
        body: &ast::TrustedBody,
        expected: Option<&Ty>,
    ) -> TExpr {
        if premise.value.trim_matches(' ').is_empty() {
            self.report(premise.token, Code::TrustedEmptyPremise);
        }
        if self.cx.trusted {
            self.report(token, Code::TrustedNested);
        }
        self.ck.require(self.item, token, Authority::Trusted);
        let saved = self.cx;
        self.cx.trusted = true;
        let typed = match body {
            ast::TrustedBody::Expr(expr) => self.expr(expr, expected),
            ast::TrustedBody::Block(block) => {
                let (typed, diverges) = self.block(block);
                TExpr {
                    token: block.token,
                    last: block.end,
                    ty: if diverges { Ty::Never } else { Ty::Unit },
                    kind: TKind::Block(typed),
                }
            }
        };
        self.cx = saved;
        TExpr {
            token,
            last: token,
            ty: typed.ty.clone(),
            kind: TKind::Trusted(Box::new(typed)),
        }
    }

    fn quantifier(
        &mut self,
        token: Tok,
        all: bool,
        name: &ast::Name,
        range: &ast::Expr,
        body: &ast::Expr,
    ) -> TExpr {
        if !self.cx.quantifiers {
            self.report(token, Code::ContractQuantifierOutside);
            return TExpr::failed(token);
        }
        let range_typed = self.expr(range, None);
        let element = match &range_typed.ty {
            ty if self.ck.is_std(ty, "core::Range") => ty.type_arg(0).cloned().unwrap_or(Ty::Error),
            Ty::Error => Ty::Error,
            _ => {
                self.report(range.token, Code::ContractIllDefined);
                Ty::Error
            }
        };
        let is_range_expr = matches!(&range.kind, ExprKind::Binary { op, .. } if op.is_range());
        if !is_range_expr && !range_typed.ty.has_error() {
            self.report(range.token, Code::ContractIllDefined);
        }
        self.push_scope();
        let local = self.declare(name, element, LocalKind::Quantified);
        let body_typed = self.check(body, &Ty::Scalar(Scalar::Bool));
        self.pop_scope();
        TExpr {
            token,
            last: token,
            ty: Ty::Scalar(Scalar::Bool),
            kind: TKind::Quantifier {
                all,
                local,
                range: Box::new(range_typed),
                body: Box::new(body_typed),
            },
        }
    }

    fn return_expr(&mut self, token: Tok, value: Option<&ast::Expr>) -> TExpr {
        let result = self.frame().result.clone();
        let typed = match value {
            Some(value) if result == Ty::Unit => {
                let typed = self.expr(value, None);
                let unit = matches!(&value.kind, ExprKind::Path { path, args: None }
                    if path.segments.len() == 1 && path.segments[0].text == "Unit");
                let fix = if unit {
                    self.ck.fix_deleting(self.item, value.token, value.last)
                } else if typed.ty.is_never() {
                    self.ck.fix_deleting_forward(self.item, token, token)
                } else {
                    let written = self.ck.written(self.item, value.token, value.last);
                    let discard = if typed.ty == Ty::Unit { "" } else { "discard " };
                    let indentation = self.ck.indentation(self.item, token);
                    let text = format!("{discard}{written}\n{indentation}return");
                    self.ck.fix_replacing(self.item, token, value.last, &text)
                };
                self.report_fixed((token, value.last), Code::StmtReturnValue, vec![fix]);
                Some(Box::new(typed))
            }
            Some(value) => Some(Box::new(self.check(value, &result))),
            None => {
                if result != Ty::Unit && !result.has_error() {
                    self.report_mismatch(token, &result, &Ty::Unit);
                }
                None
            }
        };
        TExpr {
            token,
            last: token,
            ty: Ty::Never,
            kind: TKind::Return(typed),
        }
    }

    /// The loop a `break` or `continue` acts on, within the current frame.
    fn jump_target(
        &mut self,
        token: Tok,
        label: Option<&ast::Name>,
        breaks: bool,
    ) -> Option<usize> {
        let base = self.frame().loops;
        let found = match label {
            Some(label) => {
                let found = (base..self.loops.len())
                    .rev()
                    .find(|index| self.loops[*index].label.as_deref() == Some(label.text.as_str()));
                if found.is_none() {
                    self.report(label.token, Code::StmtUnknownLabel);
                    return None;
                }
                if let Some(index) = found {
                    self.loops[index].label_used = true;
                }
                found
            }
            None => {
                if self.loops.len() == base {
                    self.report(token, Code::StmtNoLoop);
                    return None;
                }
                Some(self.loops.len() - 1)
            }
        };
        if breaks && let Some(index) = found {
            self.loops[index].broken = true;
        }
        found
    }

    fn fail(&mut self, token: Tok, value: &ast::Expr) -> TExpr {
        if self.cx.example {
            self.report(token, Code::ExampleForm);
        }
        let failure = self.frame().failure.clone();
        let test = self.frame().test;
        let typed = match (&failure, test) {
            (Some(failure), false) => {
                let typed = self.expr(value, Some(failure));
                if !typed.ty.has_error()
                    && !failure.has_error()
                    && !self.ck.converts(&typed.ty, failure)
                {
                    let fixes = self
                        .ck
                        .variant_added_fix(&typed.ty, failure)
                        .into_iter()
                        .collect();
                    self.report_fixed((token, value.last), Code::ErrorUnconvertible, fixes);
                }
                typed
            }
            _ => self.expr(value, None),
        };
        let raised = typed.ty.clone();
        self.frames.last_mut().expect("a frame").raise(&raised);
        TExpr {
            token,
            last: token,
            ty: Ty::Never,
            kind: TKind::Fail(Box::new(typed)),
        }
    }

    /// `await when`: the cowns its accesses name, and its body with each place naming the
    /// cown's value.
    pub fn when(&mut self, expr: &ast::Expr, attempt: Option<Tok>, expected: Option<&Ty>) -> TExpr {
        let ExprKind::When { accesses, body } = &expr.kind else {
            return self.expr(expr, expected);
        };
        let token = expr.token;
        if self.cx.when {
            self.report(token, Code::TaskWhenWait);
        }
        self.frames.last_mut().expect("a frame").may_wait = true;
        let mut typed_accesses = Vec::new();
        let mut bound = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for access in accesses {
            let path = std::iter::once(access.root.text.clone())
                .chain(access.fields.iter().map(|field| field.text.clone()))
                .collect::<Vec<_>>()
                .join(".");
            if seen.contains(&path) {
                let last = access.fields.last().unwrap_or(&access.root).token;
                let fixes = self.ck.fix_element_deleted(self.item, access.token, last);
                self.report_fixed(
                    (access.token, last),
                    Code::TaskWhenDuplicate,
                    fixes.into_iter().collect(),
                );
            }
            seen.push(path);
            let mut place = self.path_expr(
                access.root.token,
                &ast::Path {
                    segments: vec![access.root.clone()],
                },
                None,
                None,
            );
            for field in &access.fields {
                place = self.field_of(access.root.token, place, field, true);
            }
            let value_ty = if self.ck.is_std(&place.ty, "task::Cown") {
                place.ty.type_arg(0).cloned().unwrap_or(Ty::Error)
            } else {
                if !place.ty.has_error() {
                    self.report(access.token, Code::TaskWhenNotCown);
                }
                Ty::Error
            };
            bound.push((access.root.clone(), value_ty, access.mode));
            typed_accesses.push((access.mode, place));
        }
        self.push_scope();
        let mut accesses_typed = Vec::new();
        for ((root, value_ty, mode), (access_mode, place)) in bound.into_iter().zip(typed_accesses)
        {
            let id = crate::check::tir::LocalId(self.locals.len() as u32);
            self.locals.push(crate::check::tir::Local {
                name: root.text.clone(),
                ty: value_ty,
                kind: LocalKind::Param(mode),
                token: root.token,
                captured: None,
                closure: self.frame().closure,
            });
            self.scopes.last_mut().expect("a scope").push(id);
            accesses_typed.push((access_mode, place, id));
        }
        let saved = self.cx;
        self.cx.when = true;
        let body_typed = match body {
            ast::WhenBody::Block(block) => {
                let (typed, diverges) = self.block(block);
                TExpr {
                    token: block.token,
                    last: block.end,
                    ty: if diverges { Ty::Never } else { Ty::Unit },
                    kind: TKind::Block(typed),
                }
            }
            ast::WhenBody::Expr(value) => self.expr(value, expected),
        };
        self.cx = saved;
        self.pop_scope();
        let value = match body {
            ast::WhenBody::Block(_) => Ty::Unit,
            ast::WhenBody::Expr(_) => body_typed.ty.clone(),
        };
        let ty = match attempt {
            Some(_) => match self.ck.db.std_item("task", "Poisoned") {
                Some(poisoned) => self.ck.result_ty(value, Ty::item(poisoned, Vec::new())),
                None => Ty::Error,
            },
            None => value,
        };
        TExpr {
            token,
            last: token,
            ty,
            kind: TKind::When {
                attempt: attempt.is_some(),
                accesses: accesses_typed,
                body: Box::new(body_typed),
            },
        }
    }

    /// An assignment target: a place, a local and a path of fields and indexes from it, the
    /// target of `=` when `assigned`.
    pub fn place(&mut self, target: &ast::Expr, assigned: bool) -> Option<TExpr> {
        match &target.kind {
            ExprKind::Path { path, args: None } if path.segments.len() == 1 => {
                let local = self.lookup_local(&path.segments[0].text);
                match local {
                    Some(local) => Some(TExpr {
                        token: target.token,
                        last: target.last,
                        ty: self.local(local).ty.clone(),
                        kind: TKind::Local(local),
                    }),
                    None => {
                        self.expr(target, None);
                        None
                    }
                }
            }
            ExprKind::SelfValue => Some(self.expr(target, None)),
            ExprKind::Field { base, name } => {
                if self.type_target(base).is_some() {
                    self.expr(target, None);
                    return None;
                }
                let base = self.place(base, false)?;
                Some(self.field_of(target.token, base, name, !assigned))
            }
            ExprKind::Index { base, index } => {
                let typed = self.index(target.token, base, index, true);
                Some(typed)
            }
            _ => {
                self.expr(target, None);
                None
            }
        }
    }
}

/// What the base of a member path names when it names no value.
pub enum TypeTarget {
    Type(Ty),
    Interface(ItemId),
    Module,
    Failed,
}

/// Whether two types are the same, a failed part matching anything.
pub fn same(left: &Ty, right: &Ty) -> bool {
    left == right || left.has_error() || right.has_error()
}

impl Checker<'_> {
    /// The entry type of the registry a contribution adds to, when its path names one.
    pub fn contribution_entry(&self, item: ItemId, contribution: &ast::ContributeItem) -> Ty {
        match self
            .db
            .resolve(self.db.module_of(item), &contribution.registry)
        {
            Ok(Named::Item(registry)) => match &self.sig(registry).kind {
                SigKind::Registry { entry } => entry.clone(),
                _ => Ty::Error,
            },
            _ => Ty::Error,
        }
    }

    /// Generic arguments as diagnostics write them after what they apply to, from a module:
    /// nothing without any.
    pub fn show_args(&self, args: &[Arg], from: crate::check::db::ModuleId) -> String {
        if args.is_empty() {
            return String::new();
        }
        let written: Vec<String> = args
            .iter()
            .map(|arg| match arg {
                Arg::Ty(ty) => self.show(ty, from),
                Arg::Const(constant) => match constant {
                    crate::check::ty::Const::Int(value) => value.to_string(),
                    crate::check::ty::Const::Bool(value) => value.to_string(),
                    crate::check::ty::Const::Param(param) => self.param_name(*param),
                    crate::check::ty::Const::Error => "_".to_owned(),
                },
            })
            .collect();
        format!("[{}]", written.join(", "))
    }

    /// A type as diagnostics write it, from a module.
    pub fn show(&self, ty: &Ty, from: crate::check::db::ModuleId) -> String {
        let args = |args: &[Arg]| self.show_args(args, from);
        match ty {
            Ty::Scalar(scalar) => scalar.name().to_owned(),
            Ty::Unit => "Unit".to_owned(),
            Ty::Never => "Never".to_owned(),
            Ty::Error => "_".to_owned(),
            Ty::Item(item, arguments) => {
                format!("{}{}", self.item_written(*item, from), args(arguments))
            }
            Ty::Registers(item) => self.item_written(*item, from),
            Ty::Param(param) => self.param_name(*param),
            Ty::Dyn(bound) => format!(
                "Dyn[{}{}]",
                self.item_written(bound.interface, from),
                args(&bound.args)
            ),
            Ty::Fn(fn_ty) => {
                let params: Vec<String> = fn_ty
                    .params
                    .iter()
                    .map(|param| {
                        let mode = match param.mode {
                            Mode::Read => "",
                            Mode::Mut => "mut ",
                            Mode::Take => "take ",
                        };
                        format!("{mode}{}: {}", param.name, self.show(&param.ty, from))
                    })
                    .collect();
                let kind = match fn_ty.kind {
                    FnKind::Plain => "",
                    FnKind::Task => "task ",
                    FnKind::Interrupt => "interrupt ",
                };
                let mut text = format!("{kind}fn({})", params.join(", "));
                if fn_ty.result != Ty::Unit {
                    text.push_str(&format!(" -> {}", self.show(&fn_ty.result, from)));
                }
                if let Some(fails) = &fn_ty.fails {
                    text.push_str(&format!(" fails {}", self.show(fails, from)));
                }
                if fn_ty.wait {
                    text.push_str(" uses wait");
                }
                text
            }
        }
    }

    fn param_name(&self, param: crate::check::ty::Param) -> String {
        if param.is_self() {
            return "Self".to_owned();
        }
        let sig = self.sig(param.owner);
        if let Some(def) = sig.generics.iter().find(|def| def.param == param) {
            return def.name.clone();
        }
        if let SigKind::Interface { methods } = &sig.kind {
            for method in methods {
                if let Some(def) = method.sig.generics.iter().find(|def| def.param == param) {
                    return def.name.clone();
                }
            }
        }
        "_".to_owned()
    }

    /// An item's path as a module writes it: bare in its own module or as core vocabulary.
    pub fn item_written(&self, item: ItemId, from: crate::check::db::ModuleId) -> String {
        let name = self.db.ast(item).name().unwrap_or_default();
        let module = self.db.module_of(item);
        let named = name.split('.').next().unwrap_or_default();
        if module == from
            || crate::check::resolve::is_core_vocabulary(named) && self.db.is_std(item)
        {
            return name;
        }
        let package = self.db.package(self.db.module(module).package);
        let data = self.db.module(module);
        if package.std {
            return match data.root {
                true => format!("{}::{name}", package.name),
                false => format!("{}::{}::{name}", package.name, data.name),
            };
        }
        if self.db.module(from).package == data.package {
            return format!(
                "{}::{name}",
                if data.root { &package.name } else { &data.name }
            );
        }
        let alias = self
            .db
            .package(self.db.module(from).package)
            .deps
            .iter()
            .find(|(_, target)| **target == Some(data.package))
            .map(|(alias, _)| alias.clone())
            .unwrap_or_else(|| package.name.clone());
        match data.root {
            true => format!("{alias}::{name}"),
            false => format!("{alias}::{}::{name}", data.name),
        }
    }
}
