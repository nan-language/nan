//! Structural properties: linear, view, Copy, Send, Inert and AnyBits of every type, computed from
//! structure for structs and enums and read from the owning chapters' tables for base types;
//! with capability types, interior mutability and state tokens.

use super::Checker;
use super::db::ItemId;
use super::sig::SigKind;
use super::ty::{Arg, FnTy, Param, Scalar, Ty};
use crate::syntax::ast::{FnKind, ItemKind, LayoutFieldKind, TypeModifier};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Props {
    pub linear: bool,
    pub view: bool,
    pub copy: bool,
    pub send: bool,
    pub inert: bool,
    pub any_bits: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Property {
    Linear,
    View,
    Copy,
    Send,
    Inert,
    AnyBits,
}

impl Props {
    pub fn has(&self, property: Property) -> bool {
        match property {
            Property::Linear => self.linear,
            Property::View => self.view,
            Property::Copy => self.copy,
            Property::Send => self.send,
            Property::Inert => self.inert,
            Property::AnyBits => self.any_bits,
        }
    }

    const NUMBER: Props = Props {
        linear: false,
        view: false,
        copy: true,
        send: true,
        inert: true,
        any_bits: true,
    };
}

/// How a base type has a property.
#[derive(Clone, Copy)]
enum Rule {
    No,
    Yes,
    /// Exactly when its first type argument has it.
    First,
    /// Exactly when every type argument has it, brands excepted.
    All,
    /// When at least one type argument has it.
    Some,
}

/// The six properties of a base type, in the order linear, view, Copy, Send, Inert, AnyBits.
type Facts = [Rule; 6];

use Rule::{All, First, No, Some as AnyArg, Yes};

const AFFINE_SEND: Facts = [No, No, No, Yes, No, No];
const COPY_SEND: Facts = [No, No, Yes, Yes, No, No];
const LINEAR_SEND: Facts = [Yes, No, No, Yes, No, No];
const LINEAR_ONLY: Facts = [Yes, No, No, No, No, No];
const VIEW_ONLY: Facts = [No, Yes, No, No, No, No];
const SEND_INERT_AS: Facts = [No, No, No, First, First, No];
const VIEW_SEND_INERT_AS: Facts = [No, Yes, No, First, First, No];

/// The base types and their properties, by canonical path.
const BASE_TYPES: &[(&str, Facts)] = &[
    ("core::Array", [First, First, First, First, First, First]),
    ("core::Option", [AnyArg, AnyArg, All, All, All, No]),
    ("core::Result", [AnyArg, AnyArg, All, All, All, No]),
    ("core::Untrusted", [First, First, First, First, First, No]),
    ("core::Slice", VIEW_SEND_INERT_AS),
    ("core::SliceMut", VIEW_SEND_INERT_AS),
    ("core::Str", [No, Yes, No, Yes, Yes, No]),
    ("core::SliceCursor", VIEW_SEND_INERT_AS),
    ("core::SliceCursorMut", VIEW_SEND_INERT_AS),
    ("core::ArrayCursor", [First, First, No, First, First, No]),
    ("core::CharCursor", [No, Yes, No, Yes, Yes, No]),
    ("core::atomic::AtomicU8", AFFINE_SEND),
    ("core::atomic::AtomicU16", AFFINE_SEND),
    ("core::atomic::AtomicU32", AFFINE_SEND),
    ("core::atomic::AtomicU64", AFFINE_SEND),
    ("core::atomic::AtomicUsize", AFFINE_SEND),
    ("core::atomic::AtomicBool", AFFINE_SEND),
    ("mem::Alloc", COPY_SEND),
    ("mem::Heap", COPY_SEND),
    ("mem::Budget", LINEAR_SEND),
    ("mem::Id", [No, No, Yes, Yes, Yes, Yes]),
    ("mem::Handle", [No, No, Yes, Yes, Yes, No]),
    ("mem::Arena", [No, No, No, First, No, No]),
    ("mem::OnceMap", [No, No, No, All, No, No]),
    ("mem::List", [First, No, No, First, First, No]),
    ("mem::ListCursor", [First, No, No, First, First, No]),
    ("mem::Pool", SEND_INERT_AS),
    ("mem::Shared", SEND_INERT_AS),
    ("mem::Map", [No, No, No, All, All, No]),
    ("mem::HashMap", [No, No, No, All, All, No]),
    ("mem::Set", SEND_INERT_AS),
    ("mem::MapIntoCursor", [No, No, No, All, All, No]),
    ("mem::HashMapIntoCursor", [No, No, No, All, All, No]),
    ("mem::SetIntoCursor", SEND_INERT_AS),
    ("mem::BoundedCursor", SEND_INERT_AS),
    ("mem::Bytes", [No, No, No, Yes, Yes, No]),
    ("mem::Bounded", [No, No, First, First, First, No]),
    ("mem::Entry", [No, Yes, No, All, All, No]),
    ("mem::ArenaCursor", VIEW_SEND_INERT_AS),
    ("mem::PoolCursor", VIEW_SEND_INERT_AS),
    ("mem::MapCursor", [No, Yes, No, All, All, No]),
    ("mem::MapCursorMut", [No, Yes, No, All, All, No]),
    ("mem::HashMapCursor", [No, Yes, No, All, All, No]),
    ("mem::HashMapCursorMut", [No, Yes, No, All, All, No]),
    ("mem::SetCursor", VIEW_SEND_INERT_AS),
    ("mem::Box", [First, No, No, First, First, No]),
    ("text::String", [No, No, No, Yes, Yes, No]),
    ("text::Fixed", [No, No, Yes, Yes, Yes, No]),
    ("text::Interner", AFFINE_SEND),
    ("task::Scope", [No, Yes, No, Yes, No, No]),
    ("task::Join", [First, No, No, No, No, No]),
    ("task::Cown", AFFINE_SEND),
    ("task::Sender", AFFINE_SEND),
    ("task::Receiver", [First, No, No, Yes, No, No]),
    ("task::Executor", COPY_SEND),
    ("sys::Fs", COPY_SEND),
    ("sys::Net", COPY_SEND),
    ("sys::Clock", COPY_SEND),
    ("sys::Random", COPY_SEND),
    ("sys::Log", COPY_SEND),
    ("sys::Stdio", COPY_SEND),
    ("sys::fs::File", AFFINE_SEND),
    ("sys::net::Listener", AFFINE_SEND),
    ("sys::net::Conn", LINEAR_SEND),
    ("sys::Revoker", LINEAR_SEND),
    ("sys::Cx", VIEW_ONLY),
    ("sys::Outbox", COPY_SEND),
    ("hw::Ptr", [No, No, Yes, Yes, Yes, Yes]),
    ("hw::UserAddr", [No, No, Yes, Yes, Yes, Yes]),
    ("hw::Register", [No, No, Yes, Yes, Yes, No]),
    ("hw::Mmio", LINEAR_SEND),
    ("hw::Ports", LINEAR_SEND),
    ("hw::Dma", LINEAR_SEND),
    ("hw::DmaShared", LINEAR_SEND),
    ("hw::Ring0", LINEAR_SEND),
    ("hw::PageTableRoot", LINEAR_SEND),
    ("hw::IdtInstalled", LINEAR_SEND),
    ("hw::Idt", [No, No, No, Yes, Yes, No]),
    ("hw::IrqContext", LINEAR_ONLY),
    ("hw::SavedContext", LINEAR_ONLY),
    ("hw::SpinGuard", LINEAR_ONLY),
    ("hw::TrapFrame", [No, No, No, No, No, No]),
    ("hw::Region", LINEAR_SEND),
    ("hw::PhysFrame", LINEAR_SEND),
    ("hw::IrqShared", LINEAR_SEND),
    ("hw::Signal", AFFINE_SEND),
    ("hw::SpinLock", AFFINE_SEND),
    ("hw::PerCpu", AFFINE_SEND),
    ("hw::RunQueue", AFFINE_SEND),
    ("hw::Resumable", COPY_SEND),
    ("hw::Anchor", [No, No, No, No, No, No]),
    ("boot::Machine", LINEAR_ONLY),
    ("boot::Kernel", LINEAR_ONLY),
    ("boot::Services", VIEW_ONLY),
];

/// The primitive capabilities, by canonical path.
const PRIMITIVE_CAPABILITIES: [&str; 26] = [
    "mem::Alloc",
    "mem::Budget",
    "mem::Heap",
    "task::Executor",
    "sys::Fs",
    "sys::Net",
    "sys::Clock",
    "sys::Random",
    "sys::Log",
    "sys::Stdio",
    "sys::fs::File",
    "sys::net::Listener",
    "sys::net::Conn",
    "sys::Revoker",
    "boot::Machine",
    "boot::Kernel",
    "boot::Services",
    "hw::Mmio",
    "hw::Ports",
    "hw::Dma",
    "hw::DmaShared",
    "hw::Ring0",
    "hw::PageTableRoot",
    "hw::Region",
    "hw::PhysFrame",
    "hw::IrqShared",
];

/// The base types with interior mutability, by canonical path.
pub const INTERIOR: [&str; 14] = [
    "task::Cown",
    "core::atomic::AtomicU8",
    "core::atomic::AtomicU16",
    "core::atomic::AtomicU32",
    "core::atomic::AtomicU64",
    "core::atomic::AtomicUsize",
    "core::atomic::AtomicBool",
    "mem::Arena",
    "mem::OnceMap",
    "text::Interner",
    "hw::SpinLock",
    "hw::IrqShared",
    "hw::PerCpu",
    "hw::Signal",
];

/// The std types whose type parameters other than the first are brands, affecting no property.
const BRANDED: [&str; 3] = ["mem::Pool", "mem::Handle", "mem::PoolCursor"];

impl Checker<'_> {
    /// The facts of a base type, when the item is one.
    fn base_facts(&self, item: ItemId) -> Option<&'static Facts> {
        if !self.db.is_std(item) {
            return None;
        }
        let path = self.db.canonical(item);
        BASE_TYPES
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, facts)| facts)
    }

    /// Whether an item is a base type, whose properties its owning chapter gives.
    pub fn is_base_type(&self, item: ItemId) -> bool {
        self.base_facts(item).is_some()
    }

    pub fn is_primitive_capability(&self, item: ItemId) -> bool {
        self.db.is_std(item) && PRIMITIVE_CAPABILITIES.contains(&self.db.canonical(item).as_str())
    }

    /// The properties of a type, as code of the item `from` sees them.
    pub fn props(&self, ty: &Ty) -> Props {
        match ty {
            Ty::Scalar(scalar) => match scalar {
                Scalar::Bool | Scalar::Char => Props {
                    any_bits: false,
                    ..Props::NUMBER
                },
                _ => Props::NUMBER,
            },
            Ty::Unit => Props::NUMBER,
            Ty::Never => Props {
                any_bits: false,
                ..Props::NUMBER
            },
            Ty::Error => Props {
                linear: false,
                view: false,
                copy: true,
                send: true,
                inert: true,
                any_bits: true,
            },
            Ty::Fn(fn_ty) => fn_props(fn_ty),
            Ty::Dyn(_) => Props {
                view: true,
                ..Props::default()
            },
            Ty::Registers(_) => Props::default(),
            Ty::Param(param) => self.param_props(*param),
            Ty::Item(item, args) => self.item_props(*item, args),
        }
    }

    pub fn has(&self, ty: &Ty, property: Property) -> bool {
        self.props(ty).has(property)
    }

    fn item_props(&self, item: ItemId, args: &[Arg]) -> Props {
        let types: Vec<&Ty> = args.iter().filter_map(Arg::ty).collect();
        if let Some(facts) = self.base_facts(item) {
            let branded = BRANDED.contains(&self.db.canonical(item).as_str());
            let considered: Vec<&Ty> = if branded {
                types.iter().take(1).copied().collect()
            } else {
                types.clone()
            };
            let apply = |rule: Rule, property: Property| match rule {
                Rule::No => false,
                Rule::Yes => true,
                Rule::First => considered.first().is_some_and(|ty| self.has(ty, property)),
                Rule::All => considered.iter().all(|ty| self.has(ty, property)),
                Rule::Some => considered.iter().any(|ty| self.has(ty, property)),
            };
            return Props {
                linear: apply(facts[0], Property::Linear),
                view: apply(facts[1], Property::View),
                copy: apply(facts[2], Property::Copy),
                send: apply(facts[3], Property::Send),
                inert: apply(facts[4], Property::Inert),
                any_bits: apply(facts[5], Property::AnyBits),
            };
        }
        let ast = self.db.ast(item);
        if let ItemKind::Layout(layout) = &ast.kind {
            let any_bits = layout.tags.is_none()
                && match &self.sig(item).kind {
                    SigKind::Layout { fields, .. } => layout
                        .fields
                        .iter()
                        .zip(fields)
                        .filter(|(written, _)| {
                            matches!(written.kind, LayoutFieldKind::Typed { .. })
                        })
                        .all(|(_, field)| self.has(&field.ty, Property::AnyBits)),
                    _ => false,
                };
            return Props {
                any_bits,
                ..Props::NUMBER
            };
        }
        let definition = self.definition_props(item);
        let declared_linear = ast.has_modifier(TypeModifier::Linear);
        let linear = declared_linear || types.iter().any(|ty| self.has(ty, Property::Linear));
        let view = ast.has_modifier(TypeModifier::View);
        let dependent = |property: Property| {
            definition.has(property)
                && self
                    .dependent_params(item, property)
                    .iter()
                    .all(|index| types.get(*index).is_none_or(|ty| self.has(ty, property)))
        };
        Props {
            linear,
            view,
            copy: !linear && !view && dependent(Property::Copy),
            send: dependent(Property::Send),
            inert: dependent(Property::Inert),
            any_bits: dependent(Property::AnyBits),
        }
    }

    /// The properties of a struct or enum at its definition, each type parameter assumed to have
    /// each property.
    pub fn definition_props(&self, item: ItemId) -> Props {
        self.db.note(item);
        let key = (item, self.consumer_view(item));
        if let Some(known) = self.definitions.borrow().get(&key) {
            return known.unwrap_or_else(|| self.assumed_props(item));
        }
        self.definitions.borrow_mut().insert(key, None);
        let props = self
            .db
            .about(Some(item), || self.compute_definition_props(item));
        self.definitions.borrow_mut().insert(key, Some(props));
        props
    }

    /// The properties a struct or enum is taken to have while its definition is being decided,
    /// through a constituent that reaches it again: those computed from constituents hold.
    fn assumed_props(&self, item: ItemId) -> Props {
        let ast = self.db.ast(item);
        let linear = ast.has_modifier(TypeModifier::Linear);
        let view = ast.has_modifier(TypeModifier::View);
        Props {
            linear,
            view,
            copy: false,
            send: true,
            inert: true,
            any_bits: !linear && !view,
        }
    }

    fn compute_definition_props(&self, item: ItemId) -> Props {
        let ast = self.db.ast(item);
        let derives = |name: &str| match &ast.kind {
            ItemKind::Struct(structure) => structure.derives.iter().any(|entry| entry.text == name),
            ItemKind::Enum(enumeration) => {
                enumeration.derives.iter().any(|entry| entry.text == name)
            }
            _ => false,
        };
        let constituents = self.constituents(item);
        let all = |property: Property| {
            constituents.iter().all(|ty| match ty {
                Ty::Param(param) if param.owner == item => true,
                _ => self.has(ty, property),
            })
        };
        let linear = ast.has_modifier(TypeModifier::Linear);
        let view = ast.has_modifier(TypeModifier::View);
        let has_predicate = match &ast.kind {
            ItemKind::Struct(structure) => structure.members.iter().any(|member| match member {
                crate::syntax::ast::StructMember::Field(field) => field.where_clause.is_some(),
                crate::syntax::ast::StructMember::Invariant(_) => true,
            }),
            _ => false,
        };
        let is_enum = matches!(ast.kind, ItemKind::Enum(_));
        let foreign_open = !self.db.is_std(item)
            && ast.has_modifier(TypeModifier::Open)
            && self.consumer_view(item);
        let promise = |property: Property, name: &str| {
            if foreign_open {
                derives(name)
            } else {
                all(property)
            }
        };
        Props {
            linear,
            view,
            copy: derives("Copy") && !linear && !view,
            send: promise(Property::Send, "Send"),
            inert: promise(Property::Inert, "Inert"),
            any_bits: !foreign_open
                && !linear
                && !view
                && !has_predicate
                && !is_enum
                && all(Property::AnyBits)
                || foreign_open && derives("AnyBits"),
        }
    }

    /// Whether the code being checked sees an open type only as a consumer. Every check runs from
    /// the workspace, so a type of another package of the workspace is seen as one.
    fn consumer_view(&self, item: ItemId) -> bool {
        self.current_package()
            .is_some_and(|package| package != self.db.package_of(item))
    }

    /// The field and payload types of a struct or enum, over its own type parameters.
    pub fn constituents(&self, item: ItemId) -> Vec<Ty> {
        match &self.sig(item).kind {
            SigKind::Struct { fields } => fields.iter().map(|field| field.ty.clone()).collect(),
            SigKind::Enum { variants } => variants
                .iter()
                .flat_map(|variant| match &variant.payload {
                    super::sig::Payload::None => Vec::new(),
                    super::sig::Payload::Single(ty) => vec![ty.clone()],
                    super::sig::Payload::Labelled(fields) => {
                        fields.iter().map(|(_, ty)| ty.clone()).collect()
                    }
                })
                .collect(),
            SigKind::Layout { fields, .. } => fields.iter().map(|field| field.ty.clone()).collect(),
            _ => Vec::new(),
        }
    }

    /// The indexes of the type parameters of a struct or enum that a property depends on: those
    /// occurring at a dependent position of a constituent.
    pub fn dependent_params(&self, item: ItemId, property: Property) -> Vec<usize> {
        self.dependent_params_from(item, property, &mut Vec::new())
    }

    /// The dependent parameters of a struct or enum, those of the items in `seen`, being decided
    /// through a constituent reaching them again, counted as none, as are those of an item
    /// without parameters.
    fn dependent_params_from(
        &self,
        item: ItemId,
        property: Property,
        seen: &mut Vec<ItemId>,
    ) -> Vec<usize> {
        if seen.contains(&item) || self.shapes(item).is_empty() {
            return Vec::new();
        }
        seen.push(item);
        let mut found = Vec::new();
        for constituent in self.constituents(item) {
            self.collect_dependent(&constituent, item, property, &mut found, seen);
        }
        seen.pop();
        found.sort();
        found.dedup();
        found
    }

    fn collect_dependent(
        &self,
        ty: &Ty,
        owner: ItemId,
        property: Property,
        found: &mut Vec<usize>,
        seen: &mut Vec<ItemId>,
    ) {
        match ty {
            Ty::Param(param) if param.owner == owner && !param.is_self() => {
                found.push(param.index as usize)
            }
            Ty::Item(item, args) => {
                let follows: Vec<usize> = match self.base_facts(*item) {
                    Some(facts) => {
                        let rule = facts[property as usize];
                        match rule {
                            Rule::First => vec![0],
                            Rule::All | Rule::Some => (0..args.len()).collect(),
                            _ => Vec::new(),
                        }
                    }
                    None => self.dependent_params_from(*item, property, seen),
                };
                for index in follows {
                    if let Some(Arg::Ty(arg)) = args.get(index) {
                        self.collect_dependent(arg, owner, property, found, seen);
                    }
                }
            }
            _ => {}
        }
    }

    fn param_props(&self, param: Param) -> Props {
        let bounds = self.param_bounds(param);
        let names: Vec<String> = bounds
            .iter()
            .map(|bound| self.db.canonical(bound.interface))
            .collect();
        let bound = |name: &str| names.iter().any(|written| written == name);
        let copy = bound("core::Copy");
        let any = bound("core::Any") || param.is_self();
        let view =
            self.accepts_views(param) && !copy && !bound("core::Send") && !bound("core::AnyBits");
        Props {
            linear: any && !copy,
            view,
            copy,
            send: bound("core::Send"),
            inert: bound("core::Inert"),
            any_bits: bound("core::AnyBits"),
        }
    }

    /// The bounds of a generic parameter in scope.
    pub fn param_bounds(&self, param: Param) -> Vec<super::ty::Bound> {
        if let Some(def) = self.scope.borrow().iter().find(|def| def.param == param) {
            return def.bounds.clone();
        }
        let sig = self.sig(param.owner);
        let in_sig = sig
            .generics
            .iter()
            .find(|def| def.param == param)
            .map(|def| def.bounds.clone());
        if let Some(bounds) = in_sig {
            return bounds;
        }
        if let SigKind::Interface { methods } = &sig.kind {
            for method in methods {
                if let Some(def) = method.sig.generics.iter().find(|def| def.param == param) {
                    return def.bounds.clone();
                }
            }
        }
        if let SigKind::Fn(function) = &sig.kind
            && let Some(def) = function.generics.iter().find(|def| def.param == param)
        {
            return def.bounds.clone();
        }
        Vec::new()
    }

    /// Whether a type parameter accepts view arguments: `Self` or an interface's parameter, a
    /// parameter of a `view` struct or enum or of a core type taking any argument.
    pub fn accepts_views(&self, param: Param) -> bool {
        if param.is_self() {
            return true;
        }
        let owner = self.db.ast(param.owner);
        match &owner.kind {
            ItemKind::Interface(_) => param.index < 256,
            ItemKind::Struct(_) | ItemKind::Enum(_) => {
                owner.has_modifier(TypeModifier::View)
                    || self.db.is_std(param.owner)
                        && matches!(
                            self.db.canonical(param.owner).as_str(),
                            "core::Array"
                                | "core::ArrayCursor"
                                | "core::Option"
                                | "core::Result"
                                | "core::Untrusted"
                                | "core::Slice"
                                | "core::SliceMut"
                        )
            }
            _ => false,
        }
    }

    /// Whether a type has interior mutability: it is or contains a base type with it.
    pub fn has_interior(&self, ty: &Ty) -> bool {
        self.contains(ty, &|checker, item| {
            checker.db.is_std(item) && INTERIOR.contains(&checker.db.canonical(item).as_str())
        })
    }

    /// Whether a type is a capability type: a primitive capability, or one holding one among its
    /// constituents or at an argument Inert depends on.
    pub fn is_capability(&self, ty: &Ty) -> bool {
        self.contains(ty, &|checker, item| checker.is_primitive_capability(item))
    }

    /// Whether a type is or contains, through its constituents and type arguments, an item the
    /// test accepts.
    fn contains(&self, ty: &Ty, test: &dyn Fn(&Checker, ItemId) -> bool) -> bool {
        let mut seen = Vec::new();
        self.contains_from(ty, test, &mut seen)
    }

    fn contains_from(
        &self,
        ty: &Ty,
        test: &dyn Fn(&Checker, ItemId) -> bool,
        seen: &mut Vec<ItemId>,
    ) -> bool {
        let Ty::Item(item, args) = ty else {
            return false;
        };
        if test(self, *item) {
            return true;
        }
        if args
            .iter()
            .filter_map(Arg::ty)
            .any(|arg| self.contains_from(arg, test, seen))
        {
            return true;
        }
        if seen.contains(item) || self.db.is_std(*item) {
            return false;
        }
        seen.push(*item);
        self.constituents(*item)
            .iter()
            .any(|constituent| self.contains_from(constituent, test, seen))
    }

    /// Whether a type is a state token: a struct declared `view` without fields.
    pub fn is_state_token(&self, ty: &Ty) -> bool {
        let Ty::Item(item, _) = ty else {
            return false;
        };
        let ast = self.db.ast(*item);
        matches!(&ast.kind, ItemKind::Struct(structure) if structure.fields().next().is_none())
            && ast.has_modifier(TypeModifier::View)
            && !self.is_base_type(*item)
    }

    /// Whether a type is or contains a state token.
    pub fn holds_token(&self, ty: &Ty) -> bool {
        self.is_state_token(ty)
            || self.contains(ty, &|checker, item| {
                checker.is_state_token(&Ty::item(item, Vec::new()))
            })
    }
}

fn fn_props(fn_ty: &FnTy) -> Props {
    match fn_ty.kind {
        FnKind::Task => Props {
            linear: true,
            send: true,
            ..Props::default()
        },
        _ => Props {
            copy: true,
            inert: true,
            ..Props::default()
        },
    }
}
