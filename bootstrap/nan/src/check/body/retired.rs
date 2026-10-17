//! Retired forms: the rewrite of a tombstone with an old head, typed as that head's body, and the
//! uses matching old forms, which forward to the tombstone's rewrite when it preserves behaviour,
//! are otherwise reported as moved, and are ambiguous when they match several.

use super::call::{Callable, Markers};
use super::expr::TypeTarget;
use super::{Body, Clauses, Tail};
use crate::check::Checker;
use crate::check::conform::{Candidate, type_key};
use crate::check::db::{ItemId, ModuleId, TypeKey};
use crate::check::resolve::{Named, PathError};
use crate::check::sig::SigKind;
use crate::check::tir::{TBody, TExpr};
use crate::check::ty::Ty;
use crate::diagnostic::{Class, Code, Diagnostic, Fix, Offer, Reason, Role};
use crate::syntax::ast::{self, ExprKind, FailureMarker, ItemKind, RetiredItem, RetiredNew, Tok};
use crate::syntax::grammar::Punct;
use crate::syntax::token::TokenKind;

impl Checker<'_> {
    /// The rewrite of a tombstone with an old head, typed as the body of a function of its module
    /// having that head, preceded by the `try` and `await` its outermost call needs; none when it
    /// does not check so.
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

    /// The reason quoting a tombstone: its line declaring its old form when it lies in the source
    /// of a reported package, a note of its old form otherwise.
    fn quoted(&self, tombstone: ItemId) -> Reason {
        let data = self.db.item(tombstone);
        let token = self.db.ast(tombstone).token;
        let tokens = &data.syntax().tokens;
        let arrow = (token as usize..tokens.len())
            .find(|&at| tokens[at].kind == TokenKind::Punct(Punct::FatArrow))
            .expect("a tombstone holds =>");
        let old = self.flat(tombstone, (token + 1, arrow as Tok - 1));
        match self.db.is_reported(tombstone) && !self.db.in_history(tombstone) {
            true => self.reason(tombstone, token, Role::Declared, old),
            false => Reason::note(&old),
        }
    }

    /// Whether a tombstone's old form is visible from a module: where its replacement is, and
    /// everywhere for a `manual` one or while its replacement is being found.
    pub fn old_form_visible(&self, tombstone: ItemId, from: ModuleId) -> bool {
        let ItemKind::Retired(retired) = &self.db.ast(tombstone).kind else {
            return false;
        };
        let replacement = match (&retired.new, &retired.head) {
            (RetiredNew::Manual, _) => return true,
            (_, Some(_)) => {
                if self.replacing.borrow().contains(&tombstone) {
                    return true;
                }
                self.replacing.borrow_mut().push(tombstone);
                let body = self.typed(tombstone);
                self.replacing.borrow_mut().pop();
                body.and_then(|body| crate::check::tombstone::replacement(body.root.as_ref()?))
            }
            (_, None) => match self.rewrite_named(tombstone) {
                Some(Named::Item(item) | Named::Variant(item, _)) => Some(item),
                _ => None,
            },
        };
        replacement.is_none_or(|replacement| self.db.visible(from, replacement))
    }

    /// The tombstone without an old head whose old form is a canonical path, visible from a
    /// module.
    pub fn renaming(&self, form: &str, from: ModuleId) -> Option<ItemId> {
        self.db
            .retired
            .get(form)?
            .iter()
            .copied()
            .find(|&tombstone| {
                matches!(&self.db.ast(tombstone).kind,
                ItemKind::Retired(retired) if retired.head.is_none())
                    && self.old_form_visible(tombstone, from)
            })
    }

    /// The tombstone without an old head whose old form a path written in `module` that names
    /// nothing matches.
    pub fn renamed_path(&self, module: ModuleId, path: &ast::Path) -> Option<ItemId> {
        self.renaming(&self.db.spelled(module, path)?, module)
    }

    /// What a tombstone's rewrite names: what its path names, or the variant or function its
    /// member names of the type its path names.
    fn rewrite_named(&self, tombstone: ItemId) -> Option<Named> {
        let ItemKind::Retired(retired) = &self.db.ast(tombstone).kind else {
            return None;
        };
        let RetiredNew::Rewrite { expr, .. } = &retired.new else {
            return None;
        };
        let module = self.db.module_of(tombstone);
        match &expr.kind {
            ExprKind::Path { path, args: None } => self.db.resolve(module, path).ok(),
            ExprKind::Field { base, name } => {
                let ExprKind::Path { path, args: None } = &base.kind else {
                    return None;
                };
                let key = self.db.owner_key(module, path)?;
                if let TypeKey::Item(item) = key
                    && let SigKind::Enum { variants } = &self.sig(item).kind
                    && let Some(index) = variants
                        .iter()
                        .position(|variant| variant.name == name.text)
                {
                    return Some(Named::Variant(item, index));
                }
                self.db
                    .members(key, &name.text)
                    .first()
                    .copied()
                    .map(Named::Item)
            }
            _ => None,
        }
    }

    /// What a use matching the old form of a `preserves` tombstone without an old head names in
    /// its place; none for another tombstone.
    pub fn forwarding(&self, tombstone: ItemId) -> Option<Named> {
        match &self.db.ast(tombstone).kind {
            ItemKind::Retired(RetiredItem {
                new: RetiredNew::Rewrite {
                    preserves: true, ..
                },
                ..
            }) => self.rewrite_named(tombstone),
            _ => None,
        }
    }

    /// The text of what a tombstone's rewrite names, written in a module's path forms.
    fn rewrite_written(&self, tombstone: ItemId, module: ModuleId) -> Option<String> {
        Some(match self.rewrite_named(tombstone)? {
            Named::Item(item) => self.item_written(item, module),
            Named::Variant(enumeration, variant) => {
                let SigKind::Enum { variants } = &self.sig(enumeration).kind else {
                    return None;
                };
                let owner = self.item_written(enumeration, module);
                format!("{owner}.{}", variants[variant].name)
            }
            _ => return None,
        })
    }

    /// Reports a use of an item on its tokens `first..=last` matching a tombstone's old form,
    /// with `fix` writing the rewrite in its place unless the tombstone has none: forwarding for
    /// `preserves`, moved otherwise, quoting the tombstone.
    fn report_retired(
        &self,
        item: ItemId,
        construct: (Tok, Tok),
        tombstone: ItemId,
        fix: Option<Fix>,
    ) {
        let ItemKind::Retired(retired) = &self.db.ast(tombstone).kind else {
            return;
        };
        let fixes: Vec<Fix> = fix.into_iter().collect();
        let (code, because, fixes) = match &retired.new {
            RetiredNew::Rewrite {
                preserves: true, ..
            } => (Code::RetiredForwardedUse, Vec::new(), fixes),
            RetiredNew::Rewrite { .. } => (Code::NameMoved, vec![self.quoted(tombstone)], fixes),
            RetiredNew::Manual => (Code::NameMoved, vec![self.quoted(tombstone)], Vec::new()),
        };
        let diagnostic = Diagnostic {
            because,
            fixes,
            offer: Offer::Each,
            ..self.on(item, construct, code)
        };
        self.report_diagnostic(item, diagnostic);
    }

    /// Reports a path an item writes on its tokens `first..=last` matching the old form of a
    /// tombstone without an old head, with the exact fix writing what its rewrite names.
    pub fn report_renamed_path(&self, item: ItemId, (first, last): (Tok, Tok), tombstone: ItemId) {
        let fix = self
            .rewrite_written(tombstone, self.db.module_of(item))
            .map(|text| self.fix_replacing(item, first, last, &text));
        self.report_retired(item, (first, last), tombstone, fix);
    }

    /// Reports a path an item writes in `module`, naming nothing, that matches the old form of a
    /// tombstone without an old head, and gives what a forwarding use names in its place.
    pub fn forward_path(&self, item: ItemId, module: ModuleId, path: &ast::Path) -> Option<Named> {
        let tombstone = self.renamed_path(module, path)?;
        let construct = (path.token(), path.segments[path.segments.len() - 1].token);
        self.report_renamed_path(item, construct, tombstone);
        self.forwarding(tombstone)
    }

    /// What a path an item writes in `module` names, forwarding a path that names nothing
    /// through a tombstone without an old head; an error already reported when it matches one
    /// that does not forward.
    pub fn resolve_use(
        &self,
        item: ItemId,
        module: ModuleId,
        path: &ast::Path,
    ) -> Result<Named, PathError> {
        match self.db.resolve(module, path) {
            Err(error) if error.code == Some(Code::NameUnknown) => {
                match self.renamed_path(module, path) {
                    Some(_) => self.forward_path(item, module, path).ok_or(PathError {
                        code: None,
                        token: error.token,
                    }),
                    None => Err(error),
                }
            }
            resolved => resolved,
        }
    }

    /// The tombstone without an old head whose old form is member `name` of a type, visible from
    /// a module.
    pub fn member_renaming(&self, key: TypeKey, name: &str, from: ModuleId) -> Option<ItemId> {
        self.renaming(&format!("{}.{name}", self.db.type_path(key)), from)
    }

    /// Reports a member `name` an item writes on its token, of a type naming no such member,
    /// that matches the old form of a tombstone without an old head, and gives the member a
    /// forwarding use names in its place, on that token.
    pub fn forward_member(
        &self,
        item: ItemId,
        name: &ast::Name,
        tombstone: ItemId,
    ) -> Option<ast::Name> {
        let renamed = match &self.db.ast(tombstone).kind {
            ItemKind::Retired(RetiredItem {
                new: RetiredNew::Rewrite { expr, preserves },
                ..
            }) => match &expr.kind {
                ExprKind::Field { name: renamed, .. } => Some((renamed.clone(), *preserves)),
                _ => None,
            },
            _ => None,
        };
        let fix = renamed
            .as_ref()
            .map(|(renamed, _)| self.fix_replacing(item, name.token, name.token, &renamed.text));
        self.report_retired(item, (name.token, name.token), tombstone, fix);
        renamed
            .filter(|(_, preserves)| *preserves)
            .map(|(renamed, _)| ast::Name {
                text: renamed.text,
                token: name.token,
            })
    }
}

/// A call that may match old forms, its receiver aside: what checking it against a head needs.
#[derive(Clone, Copy)]
pub struct Use<'u> {
    pub token: Tok,
    pub args: &'u [ast::Arg],
    pub expected: Option<&'u Ty>,
    pub markers: Markers,
}

impl Body<'_, '_> {
    /// The canonical path member `name` of a type has.
    pub fn member_form(&self, ty: &Ty, name: &str) -> Option<String> {
        Some(format!("{}.{name}", self.ck.db.type_path(type_key(ty)?)))
    }

    fn retired(&self, tombstone: ItemId) -> &RetiredItem {
        match &self.ck.db.ast(tombstone).kind {
            ItemKind::Retired(retired) => retired,
            _ => unreachable!("a tombstone is a retired item"),
        }
    }

    /// The tombstones whose old form is `form` that a call matches, `live` being the callable of
    /// the live function its path or member names and `headed` making a tombstone's old head the
    /// call's callable: those without an old head when it names none, and when its arguments do
    /// not check against `live`, those with an old head they check against.
    pub fn matched(
        &mut self,
        form: &str,
        live: Option<&Callable>,
        receiver: Option<&TExpr>,
        call: Use,
        headed: impl Fn(&mut Self, ItemId) -> Option<Callable>,
    ) -> Vec<ItemId> {
        let mut tombstones = self.ck.db.retired.get(form).cloned().unwrap_or_default();
        tombstones.retain(|&tombstone| self.ck.old_form_visible(tombstone, self.module));
        let (with_head, without_head): (Vec<ItemId>, Vec<ItemId>) = tombstones
            .into_iter()
            .partition(|&tombstone| self.retired(tombstone).head.is_some());
        let mut matched = match live {
            Some(_) => Vec::new(),
            None => without_head,
        };
        if with_head.is_empty() {
            return matched;
        }
        let (_, alone) = self.probe(|body| body.skim_args(call.args));
        let alone = errors(alone);
        let checks = |body: &mut Self, callable: Callable| {
            let (_, found) = body.probe(|body| {
                let receiver = receiver.cloned();
                body.apply(
                    call.token,
                    callable,
                    receiver,
                    call.args,
                    call.expected,
                    call.markers,
                )
            });
            errors(found).iter().all(|error| alone.contains(error))
        };
        if live.is_some_and(|live| checks(self, live.clone())) {
            return matched;
        }
        for tombstone in with_head {
            if let Some(callable) = headed(self, tombstone)
                && checks(self, callable)
            {
                matched.push(tombstone);
            }
        }
        matched
    }

    /// Reports a use matching several old forms, quoting their tombstones, unless one of them or
    /// one of its arguments fails.
    fn ambiguous_use(&mut self, construct: (Tok, Tok), tombstones: &[ItemId], call: Use) -> TExpr {
        let (_, alone) = self.probe(|body| body.skim_args(call.args));
        let failed = tombstones
            .iter()
            .any(|&tombstone| self.ck.rejected(tombstone));
        if !failed && errors(alone).is_empty() {
            let because = tombstones
                .iter()
                .map(|&tombstone| self.ck.quoted(tombstone))
                .collect();
            let diagnostic = Diagnostic {
                because,
                ..self.ck.on(self.item, construct, Code::RetiredAmbiguousUse)
            };
            self.ck.report_diagnostic(self.item, diagnostic);
        }
        self.skim_args(call.args);
        TExpr::failed(call.token)
    }

    /// The exact fix writing a tombstone's rewrite in place of a call, from `start` to its last
    /// token: in the using module's path forms, `self` replaced by the receiver's text for a call
    /// on a value, and each other parameter by its argument's; none when an argument other than a
    /// place or a literal is not used exactly once, in argument order and before any call.
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
            .filter(|param| receiver.is_none() || !param.is_self())
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
        expr.visit(&mut |inner| {
            let name = match &inner.kind {
                ExprKind::SelfValue => "self",
                ExprKind::Path { path, args: None } if path.segments.len() == 1 => {
                    &path.segments[0].text
                }
                ExprKind::Call { .. } => {
                    calls.push(inner.last);
                    return;
                }
                _ => return,
            };
            let argument = given.iter().position(|(param, _)| param == name);
            match (&inner.kind, argument) {
                (ExprKind::SelfValue, _) if receiver_text.is_some() => occurrences.push((
                    inner.token,
                    inner.last,
                    receiver_text.clone().unwrap_or_default(),
                    None,
                )),
                (_, Some(index)) => occurrences.push((
                    inner.token,
                    inner.last,
                    self.ck
                        .written(self.item, given[index].1.token, given[index].1.last),
                    Some(index),
                )),
                (ExprKind::SelfValue, None) => unreceived = true,
                (ExprKind::Path { path, .. }, None) if tombstone_data.module != self.module => {
                    if let Ok(Named::Item(item)) = self.ck.db.resolve(tombstone_data.module, path) {
                        occurrences.push((
                            inner.token,
                            inner.last,
                            self.ck.item_written(item, self.module),
                            None,
                        ));
                    }
                }
                _ => {}
            }
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

    /// The callable a tombstone's old head makes of a call of its path.
    pub fn retired_head(&mut self, tombstone: ItemId) -> Option<Callable> {
        Some(Callable::function(tombstone, self.ck.fn_sig(tombstone)?))
    }

    /// The field among the `fields` of a construction or pattern that a label names: the field
    /// it names, or, for a struct `owner`, the one a tombstone renaming it forwards to; none when
    /// it names none, which is reported.
    pub fn labelled_field<'f>(
        &self,
        owner: Option<ItemId>,
        label: &ast::Name,
        fields: &'f [(String, Ty)],
    ) -> Option<&'f (String, Ty)> {
        let named = |text: &str| fields.iter().find(|(name, _)| name == text);
        if let Some(field) = named(&label.text) {
            return Some(field);
        }
        let renaming = owner.and_then(|owner| {
            self.ck
                .member_renaming(TypeKey::Item(owner), &label.text, self.module)
        });
        match renaming {
            Some(tombstone) => named(&self.ck.forward_member(self.item, label, tombstone)?.text),
            None => {
                self.report(label.token, Code::NameUnknown);
                None
            }
        }
    }

    /// A tombstone as a candidate member of a type.
    pub fn retired_candidate(&self, tombstone: ItemId, ty: &Ty) -> Candidate {
        Candidate::Function {
            item: tombstone,
            subst: type_key(ty)
                .map(|key| self.ck.type_subst(key, ty))
                .unwrap_or_default(),
        }
    }

    /// A method call on a value matching old forms: checked against the one tombstone's old head,
    /// or as a call of the member a rename names; reported ambiguous when it matches several.
    pub fn retired_method(
        &mut self,
        receiver: TExpr,
        name: &ast::Name,
        matched: &[ItemId],
        call: Use,
    ) -> TExpr {
        let start = call
            .markers
            .mode
            .map_or(receiver.token, |(_, marker)| marker);
        let construct = (start, call.markers.last.unwrap_or(name.token));
        let &[tombstone] = matched else {
            return self.ambiguous_use(construct, matched, call);
        };
        let Use {
            token,
            args,
            expected,
            markers,
        } = call;
        if self.retired(tombstone).head.is_none() {
            return match self.ck.forward_member(self.item, name, tombstone) {
                Some(renamed)
                    if self.ck.method_candidates(&receiver.ty, &renamed.text).len() == 1 =>
                {
                    self.method_on(token, receiver, &renamed, args, expected, markers)
                }
                _ => {
                    self.skim_args(args);
                    TExpr::failed(token)
                }
            };
        }
        let fix = self.forwarded_fix(tombstone, Some(&receiver), start, args, &markers);
        self.ck.report_retired(self.item, construct, tombstone, fix);
        let candidate = self.retired_candidate(tombstone, &receiver.ty);
        match self.callable(candidate, &receiver.ty.clone()) {
            Some(callable) => self.apply(token, callable, Some(receiver), args, expected, markers),
            None => {
                self.skim_args(args);
                TExpr::failed(token)
            }
        }
    }

    /// A call of a member through its type matching old forms, `written_args` when the type path
    /// writes its arguments: checked against the one tombstone's old head, or as a call of the
    /// member a rename names; reported ambiguous when it matches several.
    pub fn retired_member(
        &mut self,
        ty: Ty,
        base: &ast::Expr,
        name: &ast::Name,
        written_args: bool,
        matched: &[ItemId],
        call: Use,
    ) -> TExpr {
        let start = call.markers.mode.map_or(base.token, |(_, marker)| marker);
        let construct = (start, call.markers.last.unwrap_or(name.token));
        let &[tombstone] = matched else {
            return self.ambiguous_use(construct, matched, call);
        };
        let Use {
            token,
            args,
            expected,
            markers,
        } = call;
        if self.retired(tombstone).head.is_none() {
            return match self.ck.forward_member(self.item, name, tombstone) {
                Some(renamed) if self.ck.method_candidates(&ty, &renamed.text).len() == 1 => {
                    let target = TypeTarget::Type(ty);
                    self.member_call(token, target, base, &renamed, args, expected, markers)
                }
                _ => {
                    self.skim_args(args);
                    TExpr::failed(token)
                }
            };
        }
        let fix = self.forwarded_fix(tombstone, None, start, args, &markers);
        self.ck.report_retired(self.item, construct, tombstone, fix);
        let candidate = self.retired_candidate(tombstone, &ty);
        match self.member_callable(candidate, &ty, written_args) {
            Some(callable) => self.apply(token, callable, None, args, expected, markers),
            None => {
                self.skim_args(args);
                TExpr::failed(token)
            }
        }
    }

    /// A call whose path would have the canonical path an old form has, matching old forms with
    /// old heads: checked against the one tombstone's old head; reported ambiguous when it
    /// matches several.
    pub fn retired_call(&mut self, callee: &ast::Expr, matched: &[ItemId], call: Use) -> TExpr {
        let construct = (callee.token, call.markers.last.unwrap_or(callee.last));
        let &[tombstone] = matched else {
            return self.ambiguous_use(construct, matched, call);
        };
        let fix = self.forwarded_fix(tombstone, None, callee.token, call.args, &call.markers);
        self.ck.report_retired(self.item, construct, tombstone, fix);
        let Some(head) = self.ck.fn_sig(tombstone) else {
            self.skim_args(call.args);
            return TExpr::failed(call.token);
        };
        let callable = Callable::function(tombstone, head);
        self.apply(
            call.token,
            callable,
            None,
            call.args,
            call.expected,
            call.markers,
        )
    }
}

/// The code and construct of each error among diagnostics.
fn errors(found: Vec<Diagnostic>) -> Vec<(Code, Option<(u32, u32)>)> {
    found
        .into_iter()
        .filter(|diagnostic| diagnostic.code.class() == Class::Error)
        .map(|diagnostic| (diagnostic.code, diagnostic.span))
        .collect()
}
