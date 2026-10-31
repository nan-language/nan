//! The rules of struct and enum items: their members, inline recursion, closedness, the structural
//! properties they must declare, and their derives.

use super::Checker;
use super::db::ItemId;
use super::props::Property;
use super::sig::{GenericDef, Payload, SigKind};
use super::ty::{Arg, Bound, Subst, Ty, no_args};
use crate::diagnostic::{Code, Offer};
use crate::syntax::ast::{self, ItemKind, Tok, TypeModifier, Visibility};

/// The entries a `derives` clause may list.
const DERIVABLE: [&str; 11] = [
    "Copy", "Eq", "Ord", "Hash", "Default", "Render", "Encode", "Generate", "Send", "Inert",
    "AnyBits",
];

/// The core types that hold their type arguments inline.
const INLINE: [&str; 4] = [
    "core::Option",
    "core::Result",
    "core::Array",
    "core::Untrusted",
];

impl Checker<'_> {
    /// The rules of a struct or enum item.
    pub fn check_type_item(&self, item: ItemId) {
        self.check_members(item);
        self.check_recursion(item);
        self.check_declared_properties(item);
        self.check_derives(item);
        self.check_flexible(item);
    }

    /// `flexible` only on a struct deriving `Encode`.
    fn check_flexible(&self, item: ItemId) {
        let ast = self.db.ast(item);
        let Some((_, token)) = ast
            .type_modifiers
            .iter()
            .find(|(modifier, _)| *modifier == TypeModifier::Flexible)
        else {
            return;
        };
        let encoded = matches!(&ast.kind, ItemKind::Struct(structure)
            if structure.derives.iter().any(|entry| entry.text == "Encode"));
        if !encoded {
            self.report(item, *token, Code::EncodeFlexible);
        }
    }

    /// Distinct member names, labelled payloads, and the `pub` fields of a closed struct.
    fn check_members(&self, item: ItemId) {
        let ast = self.db.ast(item);
        match &ast.kind {
            ItemKind::Struct(structure) => {
                let closed = ast.has_modifier(TypeModifier::Closed);
                let mut seen: Vec<&str> = Vec::new();
                for field in structure.fields() {
                    if seen.contains(&field.name.text.as_str()) {
                        self.report(item, field.name.token, Code::TypeDuplicateMember);
                    }
                    seen.push(&field.name.text);
                    if closed && field.visibility != Visibility::Pub {
                        self.report(item, field.name.token, Code::TypeClosedHiddenField);
                    }
                }
            }
            ItemKind::Enum(enumeration) => {
                let mut seen: Vec<&str> = Vec::new();
                for variant in &enumeration.variants {
                    if seen.contains(&variant.name.text.as_str()) {
                        self.report(item, variant.name.token, Code::TypeDuplicateMember);
                    }
                    seen.push(&variant.name.text);
                    if let Some(fields) = &variant.payload {
                        self.check_payload(item, variant, fields);
                    }
                }
            }
            _ => {}
        }
    }

    /// A payload: one unlabelled type, or fields each labelled by one distinct identifier.
    fn check_payload(&self, item: ItemId, variant: &ast::Variant, fields: &[ast::PayloadField]) {
        fn label(field: &ast::PayloadField) -> Option<&str> {
            match &field.label {
                Some(ast::TypeExpr {
                    kind: ast::TypeKind::Path { path, args: None },
                    ..
                }) if path.segments.len() == 1 => Some(path.segments[0].text.as_str()),
                _ => None,
            }
        }
        let single = matches!(fields, [field] if field.label.is_none());
        let labelled = fields.iter().all(|field| label(field).is_some());
        if !single && !labelled {
            self.report(item, variant.name.token, Code::TypeUnlabelledPayload);
            return;
        }
        let mut seen: Vec<&str> = Vec::new();
        for field in fields {
            if let Some(name) = label(field) {
                if seen.contains(&name) {
                    self.report(item, field.token, Code::TypeDuplicateMember);
                }
                seen.push(name);
            }
        }
    }

    /// The members of a struct or enum in source order: each field's or variant's token and the
    /// types it holds.
    fn members(&self, item: ItemId) -> Vec<(Tok, Vec<Ty>)> {
        match &self.sig(item).kind {
            SigKind::Struct { fields } => fields
                .iter()
                .map(|field| (field.token, vec![field.ty.clone()]))
                .collect(),
            SigKind::Enum { variants } => variants
                .iter()
                .map(|variant| {
                    let types = match &variant.payload {
                        Payload::None => Vec::new(),
                        Payload::Single(ty) => vec![ty.clone()],
                        Payload::Labelled(fields) => {
                            fields.iter().map(|(_, ty)| ty.clone()).collect()
                        }
                    };
                    (variant.token, types)
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// A struct or enum holding an instantiation of itself inline, reported once per cycle: on
    /// the type whose canonical path sorts first, at its first member holding itself.
    fn check_recursion(&self, item: ItemId) {
        let Some((token, _)) = self.members(item).into_iter().find(|(_, types)| {
            types
                .iter()
                .any(|ty| self.holds_inline(ty, item, &mut Vec::new()))
        }) else {
            return;
        };
        let path = self.db.canonical(item);
        let earlier = self.inline_items(item).into_iter().any(|other| {
            other != item
                && self.holds_inline(&Ty::item(other, Vec::new()), item, &mut Vec::new())
                && self.db.canonical(other) < path
        });
        if !earlier {
            self.report(item, token, Code::TypeRecursive);
        }
    }

    /// Whether a type holds an instantiation of `target` inline, through the core types holding
    /// their arguments inline and the members of structs and enums.
    fn holds_inline(&self, ty: &Ty, target: ItemId, seen: &mut Vec<ItemId>) -> bool {
        let Ty::Item(item, args) = ty else {
            return false;
        };
        if *item == target {
            return true;
        }
        if INLINE.iter().any(|path| self.is_std(ty, path)) {
            return args
                .iter()
                .filter_map(Arg::ty)
                .any(|arg| self.holds_inline(arg, target, seen));
        }
        if self.is_base_type(*item) || seen.contains(item) {
            return false;
        }
        seen.push(*item);
        let held = self
            .inline_types(*item)
            .iter()
            .any(|ty| self.holds_inline(ty, target, seen));
        seen.pop();
        held
    }

    /// The types a struct or enum holds inline in its members.
    fn inline_types(&self, item: ItemId) -> Vec<Ty> {
        if !matches!(
            self.db.ast(item).kind,
            ItemKind::Struct(_) | ItemKind::Enum(_)
        ) {
            return Vec::new();
        }
        self.members(item)
            .into_iter()
            .flat_map(|(_, types)| types)
            .collect()
    }

    /// The structs and enums a struct or enum holds inline, at any depth.
    fn inline_items(&self, item: ItemId) -> Vec<ItemId> {
        let mut found = Vec::new();
        let mut pending = vec![item];
        while let Some(current) = pending.pop() {
            for ty in self.inline_types(current) {
                self.collect_inline(&ty, &mut found, &mut pending);
            }
        }
        found
    }

    fn collect_inline(&self, ty: &Ty, found: &mut Vec<ItemId>, pending: &mut Vec<ItemId>) {
        let Ty::Item(item, args) = ty else {
            return;
        };
        if INLINE.iter().any(|path| self.is_std(ty, path)) {
            for arg in args.iter().filter_map(Arg::ty) {
                self.collect_inline(arg, found, pending);
            }
        } else if !self.is_base_type(*item) && !found.contains(item) {
            found.push(*item);
            pending.push(*item);
        }
    }

    /// The fields, variants and interface methods an item declares, with their tokens.
    pub fn members_declared(&self, item: ItemId) -> Vec<(String, Tok)> {
        match &self.db.ast(item).kind {
            ItemKind::Struct(structure) => structure
                .fields()
                .map(|field| (field.name.text.clone(), field.name.token))
                .collect(),
            ItemKind::Enum(enumeration) => enumeration
                .variants
                .iter()
                .map(|variant| (variant.name.text.clone(), variant.name.token))
                .collect(),
            ItemKind::Interface(interface) => interface
                .methods
                .iter()
                .map(|method| (method.name.text.clone(), method.name.token))
                .collect(),
            ItemKind::Layout(layout) => layout
                .fields
                .iter()
                .map(|field| (field.name.text.clone(), field.name.token))
                .chain(
                    layout
                        .tags
                        .iter()
                        .flatten()
                        .map(|tag| (tag.name.text.clone(), tag.name.token)),
                )
                .collect(),
            _ => Vec::new(),
        }
    }

    /// `linear` and `view`, which a struct or enum declares when a member holds a linear type,
    /// its type parameters counted droppable, or a view.
    fn check_declared_properties(&self, item: ItemId) {
        let ast = self.db.ast(item);
        let droppable = self.counted(item);
        for (property, modifier, code) in [
            (
                Property::Linear,
                TypeModifier::Linear,
                Code::PropLinearUndeclared,
            ),
            (Property::View, TypeModifier::View, Code::PropViewUndeclared),
        ] {
            if ast.has_modifier(modifier) {
                continue;
            }
            let first = self
                .members(item)
                .into_iter()
                .find(|(_, types)| types.iter().any(|ty| self.has(&droppable.ty(ty), property)));
            if let Some((token, _)) = first {
                let name = match &ast.kind {
                    ItemKind::Struct(structure) => structure.name.token,
                    ItemKind::Enum(enumeration) => enumeration.name.token,
                    _ => continue,
                };
                let syntax = &self.db.item(item).file.syntax;
                let keyword = syntax.previous(name as usize).unwrap_or(name as usize) as Tok;
                let word = match modifier {
                    TypeModifier::Linear => "linear ",
                    _ => "view ",
                };
                let fix = self.fix_inserting(item, keyword, word);
                self.report_fixed(item, (token, token), code, vec![fix], Offer::Each);
            }
        }
    }

    /// A substitution of a struct's or enum's own type parameters by `Unit`, which has every
    /// property but linear and view: a type under it is read with its parameters counted Copy,
    /// Send, Inert and AnyBits, and droppable.
    fn counted(&self, item: ItemId) -> Subst {
        let generics = &self.sig(item).generics;
        Subst::of(
            generics.iter().map(|def| def.param),
            &generics
                .iter()
                .map(|_| Arg::Ty(Ty::Unit))
                .collect::<Vec<_>>(),
        )
    }

    /// The entries of a `derives` clause, each checked in its own rules' order.
    fn check_derives(&self, item: ItemId) {
        let ast = self.db.ast(item);
        let derives = match &ast.kind {
            ItemKind::Struct(structure) => &structure.derives,
            ItemKind::Enum(enumeration) => &enumeration.derives,
            _ => return,
        };
        let mut seen: Vec<&str> = Vec::new();
        for entry in derives {
            let name = entry.text.as_str();
            if !DERIVABLE.contains(&name) {
                self.report(item, entry.token, Code::DeriveUnknown);
                continue;
            }
            if seen.contains(&name) {
                self.report(item, entry.token, Code::DeriveDuplicate);
                continue;
            }
            seen.push(name);
            if let Some(code) = self.derive_problem(item, name) {
                let token = match code {
                    Code::DeriveConstituentMissing
                    | Code::EncodeLinear
                    | Code::EncodeNotInert
                    | Code::EncodeView
                    | Code::TestGenerateType => ast.token,
                    _ => entry.token,
                };
                let fixes = match code {
                    Code::PropPromiseNotOpen => vec![
                        self.fix_element_deleted(item, entry.token, entry.token)
                            .unwrap_or_else(|| {
                                let syntax = &self.db.item(item).file.syntax;
                                let keyword = syntax
                                    .previous(entry.token as usize)
                                    .unwrap_or(entry.token as usize)
                                    as Tok;
                                self.fix_deleting(item, keyword, entry.token)
                            }),
                    ],
                    _ => Vec::new(),
                };
                self.report_fixed(item, (token, token), code, fixes, Offer::Each);
            }
        }
    }

    /// The rule a derive of the entry `name` on a struct or enum breaks.
    pub fn derive_problem(&self, item: ItemId, name: &str) -> Option<Code> {
        let ast = self.db.ast(item);
        let own = Ty::item(
            item,
            self.sig(item)
                .generics
                .iter()
                .map(super::sig::param_arg)
                .collect::<Vec<_>>(),
        );
        let linear = ast.has_modifier(TypeModifier::Linear);
        let view = ast.has_modifier(TypeModifier::View);
        let counted = self.counted(item);
        let constituents_have = |property: Property| {
            self.constituents(item)
                .iter()
                .all(|ty| self.has(&counted.ty(ty), property))
        };
        match name {
            "Copy" => (linear || view || !constituents_have(Property::Copy))
                .then_some(Code::PropCopyIneligible),
            "Send" | "Inert" | "AnyBits" => {
                let open = ast.visibility == Visibility::Pub && ast.is_open();
                if !open {
                    return Some(Code::PropPromiseNotOpen);
                }
                let property = match name {
                    "Send" => Property::Send,
                    "Inert" => Property::Inert,
                    _ => Property::AnyBits,
                };
                (!self.definition_props(item).has(property)).then_some(Code::PropPromiseUnmet)
            }
            _ => {
                if matches!(name, "Default" | "Encode" | "Generate") && self.is_capability(&own) {
                    return Some(Code::PropCapabilityDerive);
                }
                if name == "Default" && matches!(ast.kind, ItemKind::Enum(_)) {
                    return Some(Code::DeriveEnumDefault);
                }
                if name == "Generate"
                    && (view
                        || !self.has(&counted.ty(&own), Property::Inert)
                        || self.unending_generator(item))
                {
                    return Some(Code::TestGenerateType);
                }
                if name == "Encode" {
                    if view {
                        return Some(Code::EncodeView);
                    }
                    if linear {
                        return Some(Code::EncodeLinear);
                    }
                    if !self.has(&counted.ty(&own), Property::Inert) {
                        return Some(Code::EncodeNotInert);
                    }
                }
                let interface = self.db.core_item(name)?;
                (!self.constituents_implement(item, interface))
                    .then_some(Code::DeriveConstituentMissing)
            }
        }
    }

    /// Whether the derived generator of a struct or enum never ends under the depth rule: a struct
    /// holding itself, or an enum each of whose variants holds it, as a constituent at any depth.
    fn unending_generator(&self, item: ItemId) -> bool {
        let holds_self = |types: &[Ty]| {
            types
                .iter()
                .any(|ty| self.mentions_item(ty, item, &mut Vec::new()))
        };
        let members = self.members(item);
        match self.db.ast(item).kind {
            ItemKind::Struct(_) => members.iter().any(|(_, types)| holds_self(types)),
            _ => !members.is_empty() && members.iter().all(|(_, types)| holds_self(types)),
        }
    }

    /// Whether a type holds `target` as a constituent at any depth, through type arguments and the
    /// members of structs and enums.
    fn mentions_item(&self, ty: &Ty, target: ItemId, seen: &mut Vec<ItemId>) -> bool {
        let Ty::Item(item, args) = ty else {
            return false;
        };
        if *item == target {
            return true;
        }
        if args
            .iter()
            .filter_map(Arg::ty)
            .any(|arg| self.mentions_item(arg, target, seen))
        {
            return true;
        }
        if seen.contains(item) || self.is_base_type(*item) {
            return false;
        }
        seen.push(*item);
        let found = self
            .constituents(*item)
            .iter()
            .any(|constituent| self.mentions_item(constituent, target, seen));
        seen.pop();
        found
    }

    /// The fields a struct's predicates mention: each field with a `where` condition, and each
    /// field a predicate names by its bare name.
    pub fn mentioned_fields(&self, item: ItemId) -> Vec<String> {
        let ItemKind::Struct(structure) = &self.db.ast(item).kind else {
            return Vec::new();
        };
        let fields: Vec<&str> = structure
            .fields()
            .map(|field| field.name.text.as_str())
            .collect();
        let mut mentioned: Vec<String> = Vec::new();
        let mut terms: Vec<&ast::Expr> = Vec::new();
        for member in &structure.members {
            match member {
                ast::StructMember::Field(field) => {
                    if let Some(condition) = &field.where_clause {
                        mentioned.push(field.name.text.clone());
                        terms.push(condition);
                    }
                }
                ast::StructMember::Invariant(condition) => terms.push(condition),
            }
        }
        for term in terms {
            term.visit_paths(&mut |path| {
                if let [name] = &path.segments[..]
                    && fields.contains(&name.text.as_str())
                    && !mentioned.contains(&name.text)
                {
                    mentioned.push(name.text.clone());
                }
            });
        }
        mentioned
    }

    /// The field type of a struct instantiation.
    pub fn field_type(&self, ty: &Ty, name: &str) -> Option<Ty> {
        let Ty::Item(item, args) = ty else {
            return None;
        };
        let sig = self.sig(*item);
        let SigKind::Struct { fields } = &sig.kind else {
            return None;
        };
        let subst = Subst::of(sig.generics.iter().map(|def| def.param), args);
        fields
            .iter()
            .find(|field| field.name == name)
            .map(|field| subst.ty(&field.ty))
    }

    /// Whether every constituent type of a struct or enum implements an interface, its type
    /// parameters counted as implementing it.
    fn constituents_implement(&self, item: ItemId, interface: ItemId) -> bool {
        let bound = Bound {
            interface,
            args: no_args(),
        };
        let generics: Vec<GenericDef> = self
            .sig(item)
            .generics
            .iter()
            .map(|def| GenericDef {
                bounds: def.bounds.iter().cloned().chain([bound.clone()]).collect(),
                ..def.clone()
            })
            .collect();
        let saved = self.scope.replace(generics);
        let written = ["Encode", "Render"]
            .iter()
            .any(|name| self.db.core_item(name) == Some(interface));
        let constituents = match written {
            true => self.declared_constituents(item),
            false => self.constituents(item),
        };
        let implemented = constituents.iter().all(|ty| self.implements(ty, interface));
        *self.scope.borrow_mut() = saved;
        implemented
    }
}
