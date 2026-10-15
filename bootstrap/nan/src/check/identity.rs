//! The identity of items: their normalised and interface forms, item hashes and interface hashes,
//! from what the checker knows of the names, facts and bodies those forms write.

use super::Checker;
use super::db::{ItemId, ModuleId, PackageId};
use super::props::Property;
use super::resolve::{Named, is_core_vocabulary};
use super::sig::SigKind;
use super::tir::{Callee, TExpr, TKind, TPattern, TPatternKind, TStmtKind};
use super::ty::Ty;
use crate::format::normal::{Forms, Known, Reach, forms, interface_text};
use crate::hash::Hash;
use crate::syntax::ast::{self, ItemKind, Visibility};

/// An item's hashes; a const has no interface hash before its value is known.
pub struct Hashes {
    pub item: Hash,
    pub interface: Option<Hash>,
}

impl Hashes {
    pub(super) fn of(forms: Forms) -> Hashes {
        Hashes {
            item: Hash::of(format!("item\n{}", forms.normalised).as_bytes()),
            interface: forms
                .interface
                .map(|form| Hash::of(format!("interface\n{form}").as_bytes())),
        }
    }
}

impl Checker<'_> {
    pub fn hashes(&self, item: ItemId) -> Hashes {
        Hashes::of(self.forms(item))
    }

    /// The hashes of a package's `package` item, whose paths read as its root module's.
    pub fn package_hashes(&self, package: PackageId) -> Option<Hashes> {
        let data = self.db.package(package);
        let file = &data.source.files.first()?.syntax;
        let item = file
            .items
            .iter()
            .find(|item| item.tree.is_some() && !item.abandoned)?;
        let root = data.root?;
        let resolve = |segments: &[&str]| self.canonically(root, segments);
        let known = Known {
            resolve: &resolve,
            own: Some(data.name.clone()),
            transparent: false,
            fields: Reach::Module,
            facts: None,
        };
        Some(Hashes::of(forms(file, item, &known)))
    }

    pub fn forms(&self, item: ItemId) -> Forms {
        let data = self.db.item(item);
        let resolve = |segments: &[&str]| self.canonically(data.module, segments);
        forms(
            &data.file.syntax,
            &data.file.syntax.items[data.index],
            &self.known(item, &resolve, None),
        )
    }

    /// The interface text of an item, as its canonical format writes it: what consumers see, or
    /// what code in the module `viewer` sees.
    pub fn interface_text(&self, item: ItemId, viewer: Option<ModuleId>) -> Vec<String> {
        let data = self.db.item(item);
        let resolve = |segments: &[&str]| self.canonically(data.module, segments);
        interface_text(
            &data.file.syntax,
            &data.file.syntax.items[data.index],
            &self.known(item, &resolve, viewer),
        )
    }

    /// What the forms of an item write beyond its syntax; the fields and transparent body seen
    /// wherever the item is visible, or from the module `viewer`.
    fn known<'r>(
        &self,
        item: ItemId,
        resolve: &'r dyn Fn(&[&str]) -> Option<String>,
        viewer: Option<ModuleId>,
    ) -> Known<'r> {
        let data = self.db.item(item);
        let reach = match viewer {
            None => data.ast.visibility,
            Some(viewer) if viewer == data.module => Visibility::Private,
            Some(viewer) if self.db.module(viewer).package == self.db.package_of(item) => {
                Visibility::Pkg
            }
            Some(_) => Visibility::Pub,
        };
        Known {
            resolve,
            own: self.own_path(item),
            transparent: self.transparent_seen(item, viewer),
            fields: match reach {
                Visibility::Private => Reach::Module,
                Visibility::Pkg => Reach::Package,
                Visibility::Pub => Reach::Consumer,
            },
            facts: self.facts_line(item),
        }
    }

    /// The canonical path of the item a path written in a module names, or the core vocabulary
    /// name it names.
    pub(super) fn canonically(&self, module: ModuleId, segments: &[&str]) -> Option<String> {
        let path = ast::Path {
            segments: segments
                .iter()
                .map(|segment| ast::Name {
                    text: (*segment).to_owned(),
                    token: 0,
                })
                .collect(),
        };
        let last = segments.last()?;
        match self.db.resolve(module, &path).ok()? {
            Named::Item(item)
                if is_core_vocabulary(last) && self.db.package_of(item) == self.db.core() =>
            {
                Some((*last).to_owned())
            }
            Named::Item(item) => Some(self.db.canonical(item)),
            Named::Module(_) => None,
            _ => Some((*last).to_owned()),
        }
    }

    /// The canonical path of an item; none for a tombstone.
    pub fn canonical_path(&self, item: ItemId) -> Option<String> {
        let data = self.db.item(item);
        let module = self.db.module_path(data.module);
        match &data.ast.kind {
            ItemKind::Retired(_) => None,
            ItemKind::ConstAssert(_) => Some(format!(
                "{module}::assert[{}]",
                self.hashes(item).item.short()
            )),
            ItemKind::Impl(_) => {
                let SigKind::Impl {
                    target, interface, ..
                } = &self.sig(item).kind
                else {
                    return None;
                };
                Some(format!(
                    "{module}::impl[{}: {}]",
                    self.db.type_path((*target)?),
                    self.db.canonical(interface.as_ref()?.interface)
                ))
            }
            _ => self.own_path(item),
        }
    }

    /// The canonical path the item's declared name and closing label write, or a tombstone's
    /// old form's.
    pub(super) fn own_path(&self, item: ItemId) -> Option<String> {
        let data = self.db.item(item);
        match &data.ast.kind {
            ItemKind::Impl(_) | ItemKind::ConstAssert(_) => None,
            ItemKind::Package => Some(self.db.package(self.db.package_of(item)).name.clone()),
            ItemKind::Retired(retired) => self.db.old_form(data.module, retired),
            _ => Some(self.db.canonical(item)),
        }
    }

    /// The facts line of a struct or enum: `facts`, then its interface facts in order, each
    /// resting on dependent parameters followed by their names in brackets.
    fn facts_line(&self, item: ItemId) -> Option<String> {
        let ast = self.db.ast(item);
        if !matches!(ast.kind, ItemKind::Struct(_) | ItemKind::Enum(_)) {
            return None;
        }
        let own = self.own_type(item);
        let props = self.definition_props(item);
        let public = ast.visibility == Visibility::Pub;
        let seen = self.consumer_props(item);
        let promised = |property: Property| public && seen.has(property);
        let token = self.is_state_token(&own);
        let generics = &self.sig(item).generics;
        let facts: Vec<String> = [
            ("linear", props.linear, Some(Property::Linear)),
            ("view", props.view, Some(Property::View)),
            ("token", token, None),
            ("holds-token", !token && self.holds_token(&own), None),
            ("Copy", props.copy, Some(Property::Copy)),
            ("Send", promised(Property::Send), Some(Property::Send)),
            ("Inert", promised(Property::Inert), Some(Property::Inert)),
            (
                "AnyBits",
                promised(Property::AnyBits),
                Some(Property::AnyBits),
            ),
            ("parsing", public && self.is_parsing(&own), None),
        ]
        .into_iter()
        .filter(|(_, holds, _)| *holds)
        .map(|(name, _, property)| {
            let resting: Vec<&str> = property
                .map(|property| self.dependent_params(item, property))
                .unwrap_or_default()
                .into_iter()
                .filter_map(|index| generics.get(index).map(|def| def.name.as_str()))
                .collect();
            if resting.is_empty() {
                name.to_owned()
            } else {
                format!("{name}[{}]", resting.join(", "))
            }
        })
        .collect();
        Some(if facts.is_empty() {
            "facts".to_owned()
        } else {
            format!("facts {}", facts.join(", "))
        })
    }

    /// Whether code of `holder` unfolds calls to a transparent function: every field, item and
    /// member its body names is visible to it.
    pub fn unfolds(&self, function: ItemId, holder: ItemId) -> bool {
        self.transparent_seen(function, Some(self.db.module_of(holder)))
    }

    /// Whether a transparent function's body is seen: from the module `viewer`, when every
    /// field, item and member it names is visible there; and otherwise in its interface form, when
    /// the function is not generic, no member of a generic type, and its body names only fields,
    /// items and members visible wherever the function is.
    fn transparent_seen(&self, item: ItemId, viewer: Option<ModuleId>) -> bool {
        let ast = self.db.ast(item);
        let ItemKind::Fn(function) = &ast.kind else {
            return false;
        };
        if !matches!(function.body, ast::FnBody::Expr(_))
            || viewer.is_none() && self.fn_sig(item).is_none_or(|sig| !sig.generics.is_empty())
        {
            return false;
        }
        let Some(body) = self.typed_body(item) else {
            return false;
        };
        let Some(root) = &body.root else {
            return false;
        };
        let mut seen = true;
        self.visit_names(root, &mut |named| {
            seen &= match (named, viewer) {
                (Names::Item(named), Some(viewer)) => self.db.visible(viewer, named),
                (Names::Field(owner, name), Some(viewer)) => {
                    self.field_visible(viewer, owner, &name)
                }
                (Names::Item(named), None) => self.seen_with(named, item),
                (Names::Field(owner, name), None) => self.field_seen_with(owner, &name, item),
            };
        });
        seen
    }

    /// Whether a field of a struct is visible from a module.
    fn field_visible(&self, viewer: ModuleId, owner: ItemId, name: &str) -> bool {
        match &self.sig(owner).kind {
            SigKind::Struct { fields } => fields
                .iter()
                .find(|field| field.name == name)
                .is_none_or(|field| self.db.field_visible(viewer, owner, field.visibility)),
            _ => true,
        }
    }

    /// Whether an item is visible wherever a function is.
    fn seen_with(&self, named: ItemId, function: ItemId) -> bool {
        let named = self.db.ast(named).visibility;
        match self.db.ast(function).visibility {
            Visibility::Private => true,
            Visibility::Pkg => named != Visibility::Private,
            Visibility::Pub => named == Visibility::Pub,
        }
    }

    /// Whether a field of a struct is visible wherever a function is.
    fn field_seen_with(&self, owner: ItemId, name: &str, function: ItemId) -> bool {
        let SigKind::Struct { fields } = &self.sig(owner).kind else {
            return true;
        };
        let Some(field) = fields.iter().find(|field| field.name == name) else {
            return true;
        };
        let narrowest = match (self.db.ast(owner).visibility, field.visibility) {
            (Visibility::Private, _) | (_, Visibility::Private) => Visibility::Private,
            (Visibility::Pkg, _) | (_, Visibility::Pkg) => Visibility::Pkg,
            _ => Visibility::Pub,
        };
        match self.db.ast(function).visibility {
            Visibility::Private => true,
            Visibility::Pkg => narrowest != Visibility::Private,
            Visibility::Pub => narrowest == Visibility::Pub,
        }
    }

    /// Visits the items and fields an expression names.
    fn visit_names(&self, expr: &TExpr, visit: &mut dyn FnMut(Names)) {
        match &expr.kind {
            TKind::Value(item) | TKind::FnValue { item, .. } => visit(Names::Item(*item)),
            TKind::Call(call) | TKind::Index(call) => {
                if let Callee::Function { item, .. } = &call.callee {
                    visit(Names::Item(*item));
                }
            }
            TKind::Field { base, name } => {
                if let Ty::Item(owner, _) = &base.ty {
                    visit(Names::Field(*owner, name.clone()));
                }
            }
            TKind::Construct { values, .. } => {
                if let Ty::Item(owner, _) = &expr.ty {
                    visit(Names::Item(*owner));
                    for (name, _) in values {
                        visit(Names::Field(*owner, name.clone()));
                    }
                }
            }
            TKind::Match { arms, .. } => {
                for arm in arms {
                    for pattern in &arm.patterns {
                        self.visit_pattern_names(pattern, visit);
                    }
                }
            }
            TKind::Block(block) => {
                for stmt in &block.stmts {
                    if let TStmtKind::Let { pattern, .. } = &stmt.kind {
                        self.visit_pattern_names(pattern, visit);
                    }
                }
            }
            _ => {}
        }
        expr.each_child(&mut |child| self.visit_names(child, visit));
    }

    fn visit_pattern_names(&self, pattern: &TPattern, visit: &mut dyn FnMut(Names)) {
        if let TPatternKind::Fields(_, fields) = &pattern.kind
            && let Ty::Item(owner, _) = &pattern.ty
        {
            visit(Names::Item(*owner));
            for (name, field) in fields {
                visit(Names::Field(*owner, name.clone()));
                self.visit_pattern_names(field, visit);
            }
        }
    }
}

/// What a body names.
enum Names {
    Item(ItemId),
    Field(ItemId, String),
}
