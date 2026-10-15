//! Tombstones: the forms their rewrites take, what a rewrite names beside its replacement, and the
//! old forms that live items, members and the tombstones of other modules take.

use super::Checker;
use super::db::{ItemId, ModuleId, TypeKey};
use super::interface::{narrower, openness};
use super::resolve::Named;
use super::sig::SigKind;
use super::tir::{Callee, TExpr, TKind};
use super::ty::Ty;
use crate::diagnostic::Code;
use crate::syntax::ast::{self, ExprKind, ItemKind, RetiredItem, RetiredNew, Visibility};

impl Checker<'_> {
    /// The rules of a tombstone, the first it breaks reported on it: the form of its rewrite and
    /// what the rewrite names, then whether its old form is taken.
    pub fn check_tombstone(&self, item: ItemId) {
        let ast = self.db.ast(item);
        let ItemKind::Retired(retired) = &ast.kind else {
            return;
        };
        let rewrite = match &retired.new {
            RetiredNew::Rewrite { expr, .. } => self.rewrite_problem(item, retired, expr),
            RetiredNew::Manual => None,
        };
        let code = rewrite.or_else(|| {
            self.form_taken(item, retired)
                .then_some(Code::RetiredFormTaken)
        });
        if let Some(code) = code {
            self.report(item, ast.token, code);
        }
    }

    /// The rule a tombstone's rewrite breaks: its form, a name resolving to nothing live, and
    /// a name it gives narrower visibility than its replacement's.
    fn rewrite_problem(
        &self,
        item: ItemId,
        retired: &RetiredItem,
        rewrite: &ast::Expr,
    ) -> Option<Code> {
        let module = self.db.module_of(item);
        let (replacement, named) = match &retired.head {
            Some(head) => {
                let sig = self.fn_sig(item)?;
                let failed = sig.params.iter().any(|param| param.ty.has_error())
                    || sig.result.has_error()
                    || sig.fails.as_ref().is_some_and(Ty::has_error);
                if failed {
                    return None;
                }
                let Some(typed) = self
                    .head_rewrite_form(rewrite)
                    .then(|| self.typed_rewrite(item, head, rewrite))
                    .flatten()
                else {
                    return Some(Code::RetiredBadRewrite);
                };
                let replacement = replacement(&typed);
                let mut named = Vec::new();
                rewrite.visit_paths(&mut |path| {
                    if let Ok(Named::Item(named_item)) = self.db.resolve(module, path) {
                        named.push(self.db.ast(named_item).visibility);
                    }
                });
                self.named_members(&typed, &mut named);
                let replacement =
                    replacement.map(|replacement| self.db.ast(replacement).visibility);
                (replacement, named)
            }
            None => {
                let Some((replacement, owner)) = self.path_rewrite(module, retired, rewrite) else {
                    return Some(Code::RetiredBadRewrite);
                };
                let named = owner.map(|owner| self.db.ast(owner).visibility);
                (replacement, named.into_iter().collect())
            }
        };
        let hidden = replacement.is_some_and(|replacement| {
            named
                .iter()
                .any(|visibility| openness(*visibility) < openness(replacement))
        });
        hidden.then_some(Code::RetiredRewriteHidden)
    }

    /// Whether the rewrite of a tombstone with an old head is a call or a path whose only markers
    /// are `mut` and `take`.
    fn head_rewrite_form(&self, rewrite: &ast::Expr) -> bool {
        let inner = match &rewrite.kind {
            ExprKind::Marked {
                failure: None,
                awaited: None,
                mode: Some(_),
                inner,
            } => inner,
            _ => rewrite,
        };
        let mut marked = false;
        rewrite.visit(&mut |expr| {
            if let ExprKind::Marked {
                failure, awaited, ..
            } = &expr.kind
            {
                marked |= failure.is_some() || awaited.is_some();
            }
        });
        !marked && matches!(inner.kind, ExprKind::Call { .. } | ExprKind::Path { .. })
    }

    /// The rewrite of a tombstone without an old head, when it is a path optionally followed by
    /// a member of the old form's type when the old form is a member, each naming something live
    /// or the old form of a `preserves` tombstone: the visibility of its replacement when live,
    /// and the type a member is reached through.
    fn path_rewrite(
        &self,
        module: ModuleId,
        retired: &RetiredItem,
        rewrite: &ast::Expr,
    ) -> Option<(Option<Visibility>, Option<ItemId>)> {
        let (path, member) = match &rewrite.kind {
            ExprKind::Path { path, args: None } => (path, None),
            ExprKind::Field { base, name } => match &base.kind {
                ExprKind::Path { path, args: None } => (path, Some(&name.text)),
                _ => return None,
            },
            _ => return None,
        };
        if retired.member.is_some()
            && (member.is_none()
                || self.db.owner_key(module, &retired.path) != self.db.owner_key(module, path))
        {
            return None;
        }
        let preserved = |form: String| {
            self.db.retired.get(&form).is_some_and(|tombstones| {
                tombstones.iter().any(|tombstone| {
                    matches!(
                        &self.db.ast(*tombstone).kind,
                        ItemKind::Retired(RetiredItem {
                            new: RetiredNew::Rewrite {
                                preserves: true,
                                ..
                            },
                            ..
                        })
                    )
                })
            })
        };
        let Some(member) = member else {
            return match self.db.resolve(module, path) {
                Ok(Named::Item(item)) => Some((Some(self.db.ast(item).visibility), None)),
                _ => preserved(self.db.spelled(module, path)?).then_some((None, None)),
            };
        };
        let key = self.db.owner_key(module, path)?;
        let owner = match key {
            TypeKey::Item(owner) => Some(owner),
            _ => None,
        };
        let live = self
            .db
            .members(key, member)
            .first()
            .map(|function| self.db.ast(*function).visibility)
            .or_else(|| self.field_or_variant(owner?, member));
        match live {
            Some(visibility) => Some((Some(visibility), owner)),
            None => {
                preserved(format!("{}.{member}", self.db.type_path(key))).then_some((None, owner))
            }
        }
    }

    /// The visibility of a field or variant of a struct, layout or enum: a field's the narrower
    /// of its own and its type's.
    fn field_or_variant(&self, owner: ItemId, member: &str) -> Option<Visibility> {
        let own = self.db.ast(owner).visibility;
        match &self.sig(owner).kind {
            SigKind::Struct { fields } | SigKind::Layout { fields, .. } => fields
                .iter()
                .find(|field| field.name == member)
                .map(|field| narrower(field.visibility, own)),
            SigKind::Enum { variants } => variants
                .iter()
                .any(|variant| variant.name == member)
                .then_some(own),
            _ => None,
        }
    }

    /// The visibility of what a typed rewrite names through members: the functions and
    /// interface methods it calls and the fields it reads, its outermost call's callee aside.
    fn named_members(&self, typed: &TExpr, named: &mut Vec<Visibility>) {
        let mut pending: Vec<&TExpr> = Vec::new();
        typed.each_child(&mut |child| pending.push(child));
        while let Some(expr) = pending.pop() {
            if let Some(callee) = replacement(expr) {
                named.push(self.db.ast(callee).visibility);
            }
            if let TKind::Field { base, name } = &expr.kind
                && let Ty::Item(owner, _) = &base.ty
                && let Some(visibility) = self.field_or_variant(*owner, name)
            {
                named.push(visibility);
            }
            expr.each_child(&mut |child| pending.push(child));
        }
    }

    /// Whether a live item or member, or a tombstone of another module with the same identity
    /// that comes first by file path and line, takes a tombstone's old form.
    fn form_taken(&self, item: ItemId, retired: &RetiredItem) -> bool {
        let module = self.db.module_of(item);
        let Some(form) = self.db.old_form(module, retired) else {
            return false;
        };
        let modes_and_types = |function: ItemId| {
            self.fn_sig(function).map(|sig| {
                sig.params
                    .iter()
                    .map(|param| (param.mode, param.ty.clone()))
                    .collect::<Vec<_>>()
            })
        };
        let own = retired.head.as_ref().and_then(|_| modes_and_types(item));
        self.db.about(None, || {
            let package = self.db.package_of(item);
            let items = self
                .db
                .package(package)
                .modules
                .iter()
                .flat_map(|module| self.db.module(*module).items.clone());
            for other in items {
                let ast = self.db.declaration(other);
                if matches!(ast.kind, ItemKind::Retired(_)) {
                    continue;
                }
                if ast.name().is_some() && self.db.canonical(other) == form {
                    let function = matches!(ast.kind, ItemKind::Fn(_));
                    if own.is_none() || !function || modes_and_types(other) == own {
                        return true;
                    }
                }
                let members: Vec<String> = match &self.sig(other).kind {
                    SigKind::Struct { fields } | SigKind::Layout { fields, .. } => {
                        fields.iter().map(|field| field.name.clone()).collect()
                    }
                    SigKind::Enum { variants } => variants
                        .iter()
                        .map(|variant| variant.name.clone())
                        .collect(),
                    SigKind::Interface { methods } => {
                        methods.iter().map(|method| method.name.clone()).collect()
                    }
                    _ => continue,
                };
                let path = self.db.canonical(other);
                if members
                    .iter()
                    .any(|member| format!("{path}.{member}") == form)
                {
                    return true;
                }
            }
            let position = |tombstone: ItemId| {
                let data = self.db.item(tombstone);
                (data.file.path.clone(), data.head_line())
            };
            self.db
                .retired
                .get(&form)
                .into_iter()
                .flatten()
                .any(|&other| {
                    self.db.module_of(other) != module
                        && !self.db.is_std(other)
                        && position(other) < position(item)
                        && match &self.db.ast(other).kind {
                            ItemKind::Retired(earlier) => {
                                earlier.head.is_some() == own.is_some()
                                    && (own.is_none() || modes_and_types(other) == own)
                            }
                            _ => false,
                        }
                })
        })
    }
}

/// What an expression's outermost call or path names: the function it calls, the interface of
/// the method it calls, the type it constructs, or the item it names.
fn replacement(expr: &TExpr) -> Option<ItemId> {
    match &expr.kind {
        TKind::Call(call) => match &call.callee {
            Callee::Function { item } => Some(*item),
            Callee::Method { interface, .. } => Some(interface.interface),
            _ => None,
        },
        TKind::Construct { .. } => match &expr.ty {
            Ty::Item(item, _) => Some(*item),
            _ => None,
        },
        TKind::Value(item) | TKind::FnValue { item, .. } => Some(*item),
        _ => None,
    }
}
