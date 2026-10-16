//! Tombstones: the forms their rewrites take, what a rewrite names beside its replacement, and the
//! old forms that live items, members and the tombstones of other modules take.

use std::collections::HashMap;
use std::rc::Rc;

use super::Checker;
use super::db::{ItemId, ModuleId, PackageId, TypeKey};
use super::interface::{narrower, openness};
use super::resolve::Named;
use super::sig::SigKind;
use super::tir::{Callee, TExpr, TKind};
use super::ty::Ty;
use crate::cache::Cache;
use crate::diagnostic::Code;
use crate::release;
use crate::store::Store;
use crate::syntax::ast::{self, ExprKind, ItemKind, RetiredItem, RetiredNew, Visibility};

impl Checker<'_> {
    /// The aliases tombstones give the keys of a type's fields and variants: each old key, by
    /// the key its rewrite reaches through further tombstones.
    pub fn key_aliases(&self, item: ItemId) -> HashMap<Vec<u8>, Vec<u8>> {
        let prefix = format!("{}.", self.db.type_path(TypeKey::Item(item)));
        let renamed: HashMap<String, String> = self
            .db
            .retired
            .iter()
            .filter_map(|(form, tombstones)| {
                let old = form.strip_prefix(&prefix)?;
                let ItemKind::Retired(retired) = &self.db.ast(*tombstones.first()?).kind else {
                    return None;
                };
                let RetiredNew::Rewrite {
                    expr,
                    preserves: true,
                } = &retired.new
                else {
                    return None;
                };
                let new = match &expr.kind {
                    ExprKind::Field { name, .. } => name.text.clone(),
                    ExprKind::Path { path, .. } => path.segments.last()?.text.clone(),
                    _ => return None,
                };
                Some((old.to_owned(), new))
            })
            .collect();
        renamed
            .keys()
            .map(|old| {
                let mut current = old.clone();
                let mut steps = 0;
                while let Some(next) = renamed.get(&current)
                    && steps <= renamed.len()
                {
                    current = next.clone();
                    steps += 1;
                }
                (old.clone().into_bytes(), current.into_bytes())
            })
            .collect()
    }

    /// The rules of a tombstone, the first it breaks reported on it: the form of its rewrite and
    /// what the rewrite names, then whether its old form is taken.
    pub fn check_tombstone(&self, item: ItemId) {
        let ast = self.db.ast(item);
        let ItemKind::Retired(retired) = &ast.kind else {
            return;
        };
        let package = self.db.package_of(item);
        let foreign = retired.path.segments.len() > 1
            && retired.path.segments.first().is_some_and(|first| {
                self.db.module_named(package, &first.text).is_none()
                    && self
                        .db
                        .alias(package, &first.text)
                        .is_some_and(|aliased| aliased != package)
            });
        let rewrite = match &retired.new {
            _ if foreign => Some(Code::RetiredForeignPath),
            RetiredNew::Rewrite { expr, .. } => self.rewrite_problem(item, retired, expr),
            RetiredNew::Manual => None,
        };
        let code = rewrite
            .or_else(|| {
                (!self.db.in_history(item) && self.form_taken(item, retired))
                    .then_some(Code::RetiredFormTaken)
            })
            .or_else(|| {
                self.unused_tombstone(item, retired)
                    .then_some(Code::UnusedTombstone)
            });
        if let Some(code) = code {
            self.report(item, ast.token, code);
        }
    }

    /// Whether a tombstone retires a member of a type deriving `Encode`, whose key it keeps.
    pub fn keeps_encode_key(&self, module: ModuleId, retired: &RetiredItem) -> bool {
        if retired.member.is_none() {
            return false;
        }
        let Ok(Named::Item(owner)) = self.db.resolve(module, &retired.path) else {
            return false;
        };
        let derives = match &self.db.ast(owner).kind {
            ItemKind::Struct(structure) => &structure.derives,
            ItemKind::Enum(enumeration) => &enumeration.derives,
            _ => return false,
        };
        derives.iter().any(|name| name.text == "Encode")
    }

    /// Whether a tombstone in source retires nothing the release its package's history head
    /// names exports: no item or member consumers see has its old form's canonical path, a
    /// function with the old head's parameter modes and types for an old head. A tombstone
    /// keeping an `Encode` key is never unused.
    fn unused_tombstone(&self, item: ItemId, retired: &RetiredItem) -> bool {
        let module = self.db.module_of(item);
        if self.db.in_history(item) || self.keeps_encode_key(module, retired) {
            return false;
        }
        let Some(form) = self.db.old_form(module, retired) else {
            return false;
        };
        let Some(exported) = self.head_exports(self.db.package_of(item)) else {
            return false;
        };
        if let Some(entry) = exported.get(&form) {
            let data = self.db.item(item);
            let line =
                String::from_utf8_lossy(data.syntax().line_text(data.head_line())).into_owned();
            let old = line
                .strip_prefix("retired ")
                .and_then(|rest| rest.split_once(" => "));
            return match (&retired.head, old, entry.text.first()) {
                (Some(_), Some((head, _)), Some(text)) => params(head) != params(text),
                _ => false,
            };
        }
        let Some((owner, member)) = form.rsplit_once('.') else {
            return true;
        };
        let members = exported
            .get(owner)
            .map(|entry| &entry.text[1..])
            .unwrap_or_default();
        !members.iter().any(|line| {
            let line = line.trim();
            let line = line.strip_prefix("pub ").unwrap_or(line);
            let line = line.strip_prefix("fn ").unwrap_or(line);
            let name = line
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .next()
                .unwrap_or_default();
            name == member
        })
    }

    /// The exported items of the release a package's history head names, from the local store.
    fn head_exports(&self, package: PackageId) -> Option<Rc<super::Exported>> {
        if let Some(known) = self.head_exports.borrow().get(&package) {
            return known.clone();
        }
        let head = self.db.package(package).source.machine.history()?.head?;
        let exported = Cache::open()
            .ok()
            .and_then(Result::ok)
            .and_then(|cache| Store::new(cache.store()).entries().ok()?.ok())
            .and_then(|entries| entries.into_iter().find(|entry| entry.root() == head))
            .map(|entry| Rc::new(release::exported(&entry.artefact)));
        self.head_exports
            .borrow_mut()
            .insert(package, exported.clone());
        exported
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
                    .and_then(|body| body.root)
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
    pub fn head_rewrite_form(&self, rewrite: &ast::Expr) -> bool {
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

    /// Reports a live item, and each member of it, whose canonical path is the old form that a
    /// tombstone of its package's history keeps, a function only when it has that tombstone's
    /// old parameter modes and types.
    pub fn check_history_forms(&self, item: ItemId) {
        let ast = self.db.ast(item);
        if ast.name().is_none() || matches!(ast.kind, ItemKind::Retired(_) | ItemKind::Impl(_)) {
            return;
        }
        let package = self.db.package_of(item);
        let kept = |form: &str| -> Vec<ItemId> {
            self.db
                .retired
                .get(form)
                .into_iter()
                .flatten()
                .copied()
                .filter(|&tombstone| {
                    self.db.in_history(tombstone) && self.db.package_of(tombstone) == package
                })
                .collect()
        };
        let path = self.db.canonical(item);
        let modes_and_types = |function: ItemId| {
            self.fn_sig(function).map(|sig| {
                sig.params
                    .iter()
                    .map(|param| (param.mode, param.ty.clone()))
                    .collect::<Vec<_>>()
            })
        };
        let taken = kept(&path).into_iter().any(|tombstone| {
            let ItemKind::Retired(retired) = &self.db.ast(tombstone).kind else {
                return false;
            };
            retired.head.is_none()
                || !matches!(ast.kind, ItemKind::Fn(_))
                || modes_and_types(item) == modes_and_types(tombstone)
        });
        if taken {
            self.report(item, ast.token, Code::RetiredFormTaken);
        }
        let members: Vec<&ast::Name> = match &ast.kind {
            ItemKind::Struct(structure) => structure.fields().map(|field| &field.name).collect(),
            ItemKind::Enum(enumeration) => enumeration
                .variants
                .iter()
                .map(|variant| &variant.name)
                .collect(),
            ItemKind::Interface(interface) => interface
                .methods
                .iter()
                .map(|method| &method.name)
                .collect(),
            ItemKind::Layout(layout) => layout.fields.iter().map(|field| &field.name).collect(),
            _ => Vec::new(),
        };
        for member in members {
            if !kept(&format!("{path}.{}", member.text)).is_empty() {
                self.report(item, member.token, Code::RetiredFormTaken);
            }
        }
    }

    /// Whether a live item or member, a tombstone of the history with the same identity, or a
    /// tombstone of another module with the same identity that comes first by file path and line,
    /// takes a tombstone's old form.
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
                    let earlier = self.db.in_history(other)
                        || (self.db.module_of(other) != module && position(other) < position(item));
                    earlier
                        && !self.db.is_std(other)
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

/// The modes and types of the parameters a head's text writes, without their names.
fn params(head: &str) -> Vec<String> {
    let Some(open) = head.find('(') else {
        return Vec::new();
    };
    let mut depth = 0;
    let mut close = head.len();
    for (at, c) in head[open..].char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => {
                depth -= 1;
                if depth == 0 {
                    close = open + at;
                    break;
                }
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    let mut depth = 0;
    let mut start = open + 1;
    for (at, c) in head[open + 1..close].char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                found.push(head[start..open + 1 + at].to_owned());
                start = open + 2 + at;
            }
            _ => {}
        }
    }
    found.push(head[start..close].to_owned());
    found
        .iter()
        .map(|param| {
            let param = param.trim();
            match param.split_once(':') {
                Some((named, ty)) => match named.trim().split_once(' ') {
                    Some((mode, _)) => format!("{mode} {}", ty.trim()),
                    None => ty.trim().to_owned(),
                },
                None => param.to_owned(),
            }
        })
        .filter(|param| !param.is_empty())
        .collect()
}
