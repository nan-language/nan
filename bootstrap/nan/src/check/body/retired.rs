//! Retired forms: the rewrite of a tombstone with an old head, typed as that head's body, and the
//! uses naming nothing whose canonical path a tombstone's old form has, which forward to the
//! tombstone's rewrite when it preserves behaviour and are otherwise reported as moved.

use super::call::{Callable, Markers};
use super::{Body, Clauses, Tail};
use crate::check::Checker;
use crate::check::conform::{Candidate, type_key};
use crate::check::db::ItemId;
use crate::check::resolve::Named;
use crate::check::tir::{TBody, TExpr};
use crate::check::ty::Ty;
use crate::diagnostic::{Class, Code, Fix, Offer};
use crate::syntax::ast::{self, ExprKind, FailureMarker, ItemKind, RetiredItem, RetiredNew, Tok};

impl Checker<'_> {
    /// The forwarding function of a tombstone with an old head: a function of its module having
    /// that head, whose body is the rewrite preceded by the `try` and `await` its outermost call
    /// needs; none when it does not check so.
    pub fn typed_rewrite(
        &self,
        tombstone: ItemId,
        head: &ast::Head,
        rewrite: &ast::Expr,
    ) -> Option<TBody> {
        let sig = self.fn_sig(tombstone)?;
        let (mode, inner) = match &rewrite.kind {
            ExprKind::Marked {
                failure: None,
                awaited: None,
                mode: Some(mode),
                inner,
            } => (Some(*mode), &**inner),
            _ => (None, rewrite),
        };
        let typed = |failure: Option<(FailureMarker, Tok)>, awaited: Option<Tok>| {
            self.withheld(|| {
                let mut body = Body::new(self, tombstone, sig.generics.clone());
                body.push_frame(Body::function_frame(sig.result.clone(), sig.fails.clone()));
                body.declare_params(&head.params, &sig);
                let markers = Markers {
                    failure,
                    awaited,
                    mode,
                    ..Markers::default()
                };
                let typed = match inner.kind {
                    ExprKind::Call { .. } => body.call(inner, Some(&sig.result), Some(markers)),
                    _ => body.expr(rewrite, Some(&sig.result)),
                };
                if sig.result != Ty::Unit {
                    body.expect(&typed, &sig.result, rewrite.token);
                } else if !matches!(typed.ty, Ty::Unit | Ty::Never | Ty::Error) {
                    body.unused_result(&Tail {
                        token: rewrite.token,
                        last: rewrite.last,
                        ty: typed.ty.clone(),
                    });
                }
                let frame = body.frames[0].clone();
                let clauses = Clauses::of(head, false);
                body.check_effects(rewrite.token, &frame, &sig.fn_ty(), &clauses, false);
                body.finish(Some(typed))
            })
        };
        let start = self.db.item(tombstone).syntax().tokens[rewrite.token as usize].start;
        let (_, unmarked) = typed(None, None);
        let needs = |code: Code| {
            unmarked.iter().any(|diagnostic| {
                diagnostic.code == code && diagnostic.span.is_some_and(|(at, _)| at == start)
            })
        };
        let failure = needs(Code::ErrorUnhandled).then_some((FailureMarker::Try, rewrite.token));
        let awaited = needs(Code::EffectAwaitMissing).then_some(rewrite.token);
        let (typed, found) = typed(failure, awaited);
        let checks = !found
            .iter()
            .any(|diagnostic| diagnostic.code.class() == Class::Error);
        checks.then_some(typed)
    }
}

impl Body<'_, '_> {
    /// The tombstone whose old form is the member `name` of a type.
    pub fn member_tombstone(&self, ty: &Ty, name: &str) -> Option<ItemId> {
        let key = type_key(ty)?;
        self.tombstone(&format!("{}.{name}", self.ck.db.type_path(key)))
    }

    /// The tombstone whose old form has a canonical path.
    pub fn tombstone(&self, form: &str) -> Option<ItemId> {
        self.ck.db.retired.get(form)?.first().copied()
    }

    fn retired(&self, tombstone: ItemId) -> &RetiredItem {
        match &self.ck.db.ast(tombstone).kind {
            ItemKind::Retired(retired) => retired,
            _ => unreachable!("a tombstone is a retired item"),
        }
    }

    /// Reports a use matching a tombstone, with the exact fix writing its rewrite in place of the
    /// use, and returns the member its rewrite names when it is a rename that forwards.
    fn report_retired(
        &self,
        construct: (Tok, Tok),
        tombstone: ItemId,
        fix: Option<Fix>,
    ) -> Option<ast::Name> {
        let retired = self.retired(tombstone);
        let (code, renamed) = match &retired.new {
            RetiredNew::Rewrite {
                expr,
                preserves: true,
                ..
            } => (
                Code::RetiredForwardedUse,
                match &expr.kind {
                    ExprKind::Field { name, .. } if retired.head.is_none() => Some(name.clone()),
                    _ => None,
                },
            ),
            RetiredNew::Rewrite { .. } => (Code::NameMoved, None),
            RetiredNew::Manual => {
                self.report(construct.0, Code::NameMoved);
                return None;
            }
        };
        self.ck.report_fixed(
            self.item,
            construct,
            code,
            fix.into_iter().collect(),
            Offer::Each,
        );
        renamed
    }

    /// The exact fix writing a tombstone's rewrite in place of a method call on a value, from its
    /// mode marker to `last`: in the using module's path forms, `self` replaced by the receiver's
    /// text and each other parameter by its argument's; none when an argument other than a place
    /// or a literal is not used exactly once, in argument order and before any call.
    fn forwarded_fix(
        &self,
        tombstone: ItemId,
        receiver: Option<&TExpr>,
        start: Tok,
        args: &[ast::Arg],
        markers: &Markers,
    ) -> Option<Fix> {
        let retired = self.retired(tombstone);
        let RetiredNew::Rewrite { expr, .. } = &retired.new else {
            return None;
        };
        let head = retired.head.as_ref()?;
        let data = self.ck.db.item(self.item);
        let last = markers.last?;
        let receiver_text = match receiver {
            Some(receiver) => Some(
                self.ck
                    .written(self.item, receiver.token, markers.receiver?),
            ),
            None => None,
        };
        let mut given: Vec<(String, &ast::Expr)> = Vec::new();
        let mut unnamed = head
            .params
            .iter()
            .filter(|param| !param.is_self())
            .map(|param| param.name());
        for arg in args {
            let value = match &arg.value.kind {
                ExprKind::Marked {
                    failure: None,
                    awaited: None,
                    mode: Some(_),
                    inner,
                } => inner,
                _ => &arg.value,
            };
            let name = match arg.label_name() {
                Some(label) => label.text.clone(),
                None => unnamed.next()?.to_owned(),
            };
            given.push((name, value));
        }
        let tombstone_data = self.ck.db.item(tombstone);
        let source = tombstone_data.syntax();
        let mut occurrences: Vec<(Tok, Tok, String, Option<usize>)> = Vec::new();
        let mut calls: Vec<Tok> = Vec::new();
        let mut unreceived = false;
        expr.visit(&mut |inner| match &inner.kind {
            ExprKind::SelfValue => match &receiver_text {
                Some(text) => occurrences.push((inner.token, inner.last, text.clone(), None)),
                None => unreceived = true,
            },
            ExprKind::Path { path, args: None } if path.segments.len() == 1 => {
                let name = &path.segments[0].text;
                match given.iter().position(|(param, _)| param == name) {
                    Some(index) => occurrences.push((
                        inner.token,
                        inner.last,
                        self.ck
                            .written(self.item, given[index].1.token, given[index].1.last),
                        Some(index),
                    )),
                    None if tombstone_data.module != self.module => {
                        if let Ok(crate::check::resolve::Named::Item(item)) =
                            self.ck.db.resolve(tombstone_data.module, path)
                        {
                            occurrences.push((
                                inner.token,
                                inner.last,
                                self.ck.item_written(item, self.module),
                                None,
                            ));
                        }
                    }
                    None => {}
                }
            }
            ExprKind::Call { .. } => calls.push(inner.last),
            _ => {}
        });
        if unreceived {
            return None;
        }
        let mut order = Vec::new();
        for (index, (_, value)) in given.iter().enumerate() {
            let simple = matches!(
                value.kind,
                ExprKind::Literal(_)
                    | ExprKind::Path { .. }
                    | ExprKind::SelfValue
                    | ExprKind::Field { .. }
            );
            let uses: Vec<Tok> = occurrences
                .iter()
                .filter(|(_, _, _, argument)| *argument == Some(index))
                .map(|(token, ..)| *token)
                .collect();
            if !simple {
                let [used] = uses[..] else {
                    return None;
                };
                if calls.iter().any(|call| *call < used) {
                    return None;
                }
                order.push(used);
            }
        }
        if order.windows(2).any(|pair| pair[0] > pair[1]) {
            return None;
        }
        occurrences.sort_by_key(|(token, ..)| *token);
        let (first, end) = (
            source.tokens[expr.token as usize].start,
            source.tokens[expr.last as usize].end,
        );
        let mut text = String::new();
        let mut at = first;
        for (token, last, written, _) in occurrences {
            let (from, to) = (
                source.tokens[token as usize].start,
                source.tokens[last as usize].end,
            );
            text.push_str(&String::from_utf8_lossy(
                &source.source[at as usize..from as usize],
            ));
            text.push_str(&written);
            at = to;
        }
        text.push_str(&String::from_utf8_lossy(
            &source.source[at as usize..end as usize],
        ));
        let syntax = data.syntax();
        let (from, to) = (
            syntax.tokens[start as usize].start,
            syntax.tokens[last as usize].end,
        );
        Some(self.ck.fix_bytes(self.item, from, to, &text))
    }

    /// A method call naming no member of its receiver's type that a tombstone's old form names:
    /// checked against the old head, or as a call of the member a rename names.
    #[allow(clippy::too_many_arguments)]
    pub fn retired_method(
        &mut self,
        token: Tok,
        receiver: TExpr,
        name: &ast::Name,
        tombstone: ItemId,
        args: &[ast::Arg],
        expected: Option<&Ty>,
        markers: Markers,
    ) -> TExpr {
        let start = markers.mode.map_or(receiver.token, |(_, marker)| marker);
        let fix = self.forwarded_fix(tombstone, Some(&receiver), start, args, &markers);
        let construct = (start, markers.last.unwrap_or(name.token));
        let renamed = self.report_retired(construct, tombstone, fix);
        if self.ck.fn_sig(tombstone).is_some() {
            let subst = type_key(&receiver.ty)
                .map(|key| self.ck.type_subst(key, &receiver.ty))
                .unwrap_or_default();
            let candidate = Candidate::Function {
                item: tombstone,
                subst,
            };
            if let Some(callable) = self.callable(candidate, &receiver.ty.clone()) {
                return self.apply(token, callable, Some(receiver), args, expected, markers);
            }
        }
        match renamed {
            Some(renamed) if self.ck.method_candidates(&receiver.ty, &renamed.text).len() == 1 => {
                self.method_on(token, receiver, &renamed, args, expected, markers)
            }
            _ => {
                self.skim_args(args);
                TExpr::failed(token)
            }
        }
    }
    /// A call whose path names nothing and would have the canonical path a tombstone's old form
    /// has: checked against the old head, or as a call of the function a path rewrite names.
    pub fn retired_call(
        &mut self,
        token: Tok,
        callee: &ast::Expr,
        tombstone: ItemId,
        args: &[ast::Arg],
        expected: Option<&Ty>,
        markers: Markers,
    ) -> TExpr {
        let fix = self.forwarded_fix(tombstone, None, callee.token, args, &markers);
        let construct = (callee.token, markers.last.unwrap_or(callee.last));
        self.report_retired(construct, tombstone, fix);
        let function = match self.ck.fn_sig(tombstone) {
            Some(head) => Some((tombstone, head)),
            None => match &self.retired(tombstone).new {
                RetiredNew::Rewrite {
                    expr:
                        ast::Expr {
                            kind: ExprKind::Path { path, args: None },
                            ..
                        },
                    preserves: true,
                } => match self.ck.db.resolve(self.ck.db.module_of(tombstone), path) {
                    Ok(Named::Item(item)) => self.ck.fn_sig(item).map(|head| (item, head)),
                    _ => None,
                },
                _ => None,
            },
        };
        let Some((item, head)) = function else {
            self.skim_args(args);
            return TExpr::failed(token);
        };
        self.apply(
            token,
            Callable::function(item, head),
            None,
            args,
            expected,
            markers,
        )
    }
}
