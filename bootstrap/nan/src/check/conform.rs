//! Conformance: whether a type implements an interface, through an impl line, a derive, a bound or
//! a conformance the owning chapters declare for scalars and std types; and the candidates of
//! method resolution.

use std::collections::HashMap;
use std::rc::Rc;

use super::Checker;
use super::db::{ItemId, ModuleId, TypeKey};
use super::props::Property;
use super::sig::{FnSig, GenericDef, Payload, SigKind, param_arg};
use super::ty::{Arg, Args, Bound, Const, Param, Scalar, Subst, Ty, no_args};
use crate::diagnostic::{Code, Reason, Role as ReasonRole};
use crate::interp::{End, Interpreter, Val};
use crate::nir::Role;
use crate::syntax::ast::{ItemKind, Tok};

/// How a type implements an interface, with the interface's arguments for it.
#[derive(Clone, Debug)]
pub enum Conformance {
    /// An impl line, with the substitution of its target's parameters for the type's arguments.
    Impl { line: ItemId, subst: Subst },
    /// A derive of the type's item.
    Derive,
    /// A conformance an owning chapter declares.
    Builtin,
    /// A bound of a generic parameter.
    Bound,
}

/// A function a method call may name.
#[derive(Clone, Debug)]
pub enum Candidate {
    /// A function item named after the type, with the type's arguments substituted.
    Function { item: ItemId, subst: Subst },
    /// A method of an interface the type implements or a bound names, with `Self` and the
    /// interface's parameters substituted; `function` is the implementing function when the
    /// type's impl provides one.
    Method {
        bound: Bound,
        method: usize,
        self_ty: Ty,
        function: Option<(ItemId, Subst)>,
        dynamic: bool,
    },
}

impl Candidate {
    /// What the candidate stands for, so that two naming one function are one.
    fn identity(&self) -> (Option<ItemId>, Option<(ItemId, usize)>) {
        match self {
            Candidate::Function { item, .. } => (Some(*item), None),
            Candidate::Method {
                function: Some((item, _)),
                ..
            } => (Some(*item), None),
            Candidate::Method { bound, method, .. } => (None, Some((bound.interface, *method))),
        }
    }
}

/// The interfaces derives may name, beside `Copy` and the promises.
const DERIVABLE: [&str; 7] = [
    "Eq", "Ord", "Hash", "Default", "Render", "Encode", "Generate",
];

/// The impl lines of a database, by the type they target.
pub type ImplIndex = Rc<HashMap<TypeKey, Vec<ItemId>>>;

impl Checker<'_> {
    /// The impl lines of the database, by the type they target.
    fn impl_index(&self) -> ImplIndex {
        if let Some(index) = self.impls.borrow().as_ref() {
            return index.clone();
        }
        let mut index: HashMap<TypeKey, Vec<ItemId>> = HashMap::new();
        self.db.about(None, || {
            for (position, data) in self.db.items.iter().enumerate() {
                let ItemKind::Impl(implementation) = &data.ast.kind else {
                    continue;
                };
                if let Some(key) = self.db.owner_key(data.module, &implementation.target) {
                    index.entry(key).or_default().push(ItemId(position as u32));
                }
            }
        });
        let index = Rc::new(index);
        *self.impls.borrow_mut() = Some(index.clone());
        index
    }

    /// The impl lines targeting a type.
    pub fn impl_lines(&self, key: TypeKey) -> Vec<ItemId> {
        let lines = self.impl_index().get(&key).cloned().unwrap_or_default();
        for line in &lines {
            self.db.note(*line);
        }
        lines
    }

    /// The canonical path of an interface without its package, for the core interfaces.
    fn core_interface(&self, interface: ItemId) -> Option<String> {
        let path = self.db.canonical(interface);
        path.strip_prefix("core::").map(str::to_owned)
    }

    /// The arguments of `interface` through which `ty` implements it, and how.
    pub fn conformance(&self, ty: &Ty, interface: ItemId) -> Option<(Conformance, Args)> {
        match ty {
            Ty::Error => return Some((Conformance::Builtin, self.error_args(interface))),
            Ty::Never => return Some((Conformance::Builtin, self.error_args(interface))),
            _ => {}
        }
        if let Some(property) = self.property_interface(interface) {
            return self
                .has(ty, property)
                .then(|| (Conformance::Builtin, no_args()));
        }
        if self.core_interface(interface).as_deref() == Some("Any") {
            return Some((Conformance::Builtin, no_args()));
        }
        if let Ty::Param(param) = ty {
            return self
                .param_bounds(*param)
                .into_iter()
                .find(|bound| bound.interface == interface)
                .map(|bound| (Conformance::Bound, bound.args));
        }
        if let Some(key) = type_key(ty) {
            for line in self.impl_lines(key) {
                let sig = self.sig(line);
                let SigKind::Impl {
                    interface: Some(bound),
                    ..
                } = &sig.kind
                else {
                    continue;
                };
                if bound.interface != interface {
                    continue;
                }
                let params: Vec<Param> = sig.generics.iter().map(|def| def.param).collect();
                let subst = Subst::of(params, ty.args());
                let applies = sig
                    .generics
                    .iter()
                    .zip(ty.args())
                    .all(|(def, arg)| match arg {
                        Arg::Ty(arg) => def
                            .bounds
                            .iter()
                            .all(|bound| self.satisfies(arg, &subst.bound(bound))),
                        Arg::Const(_) => true,
                    });
                if applies && self.impl_droppable(line, ty) {
                    let args = subst.args(&bound.args);
                    return Some((Conformance::Impl { line, subst }, args));
                }
            }
            if let TypeKey::Item(item) = key
                && self.derives(item, interface)
            {
                let applies = ty
                    .args()
                    .iter()
                    .filter_map(Arg::ty)
                    .all(|arg| self.conformance(arg, interface).is_some());
                return applies.then(|| (Conformance::Derive, no_args()));
            }
        }
        if let Ty::Item(item, _) = ty
            && let ItemKind::Layout(_) = self.db.ast(*item).kind
        {
            let name = self.core_interface(interface);
            if matches!(name.as_deref(), Some("Eq" | "Hash")) {
                return Some((Conformance::Builtin, no_args()));
            }
        }
        if let Ty::Item(item, _) = ty
            && self.db.is_std(interface)
            && self.db.canonical(interface) == "sys::Capability"
            && self.is_kind(*item)
        {
            return Some((Conformance::Builtin, no_args()));
        }
        self.builtin_conformance(ty, interface)
            .map(|args| (Conformance::Builtin, args))
    }

    /// Whether `ty` implements `bound`, its arguments included.
    pub fn satisfies(&self, ty: &Ty, bound: &Bound) -> bool {
        match self.conformance(ty, bound.interface) {
            Some((_, args)) => {
                bound.args.is_empty()
                    || args.len() != bound.args.len()
                    || args.iter().zip(bound.args.iter()).all(|(found, wanted)| {
                        found == wanted
                            || matches!(found, Arg::Ty(ty) if ty.has_error())
                            || matches!(wanted, Arg::Ty(ty) if ty.has_error())
                    })
            }
            None => false,
        }
    }

    pub fn implements(&self, ty: &Ty, interface: ItemId) -> bool {
        self.conformance(ty, interface).is_some()
    }

    /// The interface arguments a failed type implements an interface with: failed too.
    fn error_args(&self, interface: ItemId) -> Args {
        self.shapes(interface)
            .iter()
            .map(|_| Arg::Ty(Ty::Error))
            .collect()
    }

    /// The structural property a property interface names.
    pub fn property_interface(&self, interface: ItemId) -> Option<Property> {
        Some(match self.core_interface(interface)?.as_str() {
            "Copy" => Property::Copy,
            "Send" => Property::Send,
            "Inert" => Property::Inert,
            "AnyBits" => Property::AnyBits,
            _ => return None,
        })
    }

    /// Whether a struct or enum derives an interface.
    pub fn derives(&self, item: ItemId, interface: ItemId) -> bool {
        let Some(name) = self.core_interface(interface) else {
            return false;
        };
        if !DERIVABLE.contains(&name.as_str()) {
            return false;
        }
        match &self.db.ast(item).kind {
            ItemKind::Struct(structure) => structure.derives.iter().any(|entry| entry.text == name),
            ItemKind::Enum(enumeration) => {
                enumeration.derives.iter().any(|entry| entry.text == name)
            }
            _ => false,
        }
    }

    /// Whether an impl line applies to an instantiation as far as droppable parameters go: a
    /// parameter droppable for the line takes droppable arguments only.
    fn impl_droppable(&self, line: ItemId, ty: &Ty) -> bool {
        self.droppable_for_line(line)
            .into_iter()
            .zip(ty.args())
            .all(|(droppable, arg)| match arg {
                Arg::Ty(arg) if droppable => !self.has(arg, Property::Linear),
                _ => true,
            })
    }

    /// For each generic parameter of an impl line's target, whether it is droppable for the
    /// line: its target bounds it by `Any` and the line lists it without `Any`, or the line is a
    /// std impl line its owning chapter restricts to droppable instantiations.
    pub fn droppable_for_line(&self, line: ItemId) -> Vec<bool> {
        let ItemKind::Impl(implementation) = &self.db.ast(line).kind else {
            return Vec::new();
        };
        let SigKind::Impl {
            target: Some(key), ..
        } = self.sig(line).kind
        else {
            return Vec::new();
        };
        let target_bounds = self.type_generics(key);
        let listed: Vec<bool> = match &implementation.params {
            Some(params) => params
                .iter()
                .map(|param| {
                    param
                        .bounds
                        .iter()
                        .any(|bound| bound.path.last().text == "Any")
                })
                .collect(),
            None if self.db.is_std(line) && self.db.canonical_line_droppable(line) => {
                vec![false; target_bounds.len()]
            }
            None => vec![true; target_bounds.len()],
        };
        target_bounds
            .iter()
            .zip(listed)
            .map(|(def, listed_any)| {
                !listed_any
                    && def
                        .bounds
                        .iter()
                        .any(|bound| self.core_interface(bound.interface).as_deref() == Some("Any"))
            })
            .collect()
    }

    /// The conformances the owning chapters declare for scalars, `Unit` and std types.
    fn builtin_conformance(&self, ty: &Ty, interface: ItemId) -> Option<Args> {
        let name = self.core_interface(interface)?;
        let all_args = |interface: ItemId| {
            ty.args()
                .iter()
                .filter_map(Arg::ty)
                .all(|arg| self.implements(arg, interface))
        };
        let same = |ty: &Ty| -> Args { Rc::from(vec![Arg::Ty(ty.clone()), Arg::Ty(ty.clone())]) };
        match ty {
            Ty::Scalar(scalar) => {
                let integer = scalar.is_integer();
                let float = scalar.is_float();
                let ordered = !float;
                let args = match name.as_str() {
                    "Eq" | "Ord" | "Hash" if ordered => no_args(),
                    "Default" | "Render" | "Encode" | "Generate" => no_args(),
                    "Add" | "Sub" | "Mul" if integer || float => same(ty),
                    "Div" | "Rem" if float || integer && !scalar.is_signed() => same(ty),
                    "BitAnd" | "BitOr" | "BitXor" if integer => same(ty),
                    "Shl" if integer => {
                        Rc::from(vec![Arg::Ty(Ty::Scalar(Scalar::U32)), Arg::Ty(ty.clone())])
                    }
                    "Shr" if integer && !scalar.is_signed() => {
                        Rc::from(vec![Arg::Ty(Ty::Scalar(Scalar::U32)), Arg::Ty(ty.clone())])
                    }
                    "Neg" if float || scalar.is_signed() => Rc::from(vec![Arg::Ty(ty.clone())]),
                    "BitNot" if integer => Rc::from(vec![Arg::Ty(ty.clone())]),
                    "SecretBits" if integer || *scalar == Scalar::Bool => no_args(),
                    _ => return None,
                };
                Some(args)
            }
            Ty::Unit => matches!(
                name.as_str(),
                "Eq" | "Ord" | "Hash" | "Default" | "Render" | "Encode" | "Generate"
            )
            .then(no_args),
            Ty::Item(item, _) if self.db.is_std(*item) => {
                let path = self.db.canonical(*item);
                let holds = match (path.as_str(), name.as_str()) {
                    (
                        "core::Str" | "text::String" | "text::Fixed",
                        "Eq" | "Ord" | "Hash" | "Render",
                    ) => true,
                    ("text::String" | "text::Fixed", "Encode" | "Generate") => true,
                    ("text::Fixed", "Default") => true,
                    ("core::Slice" | "core::SliceMut", "Eq" | "Ord" | "Hash" | "Render") => {
                        all_args(interface)
                    }
                    ("core::Array", "Eq" | "Ord" | "Hash" | "Default" | "Render" | "Encode") => {
                        all_args(interface)
                    }
                    ("core::Array", "Generate") => {
                        ty.type_arg(0)
                            .is_some_and(|element| self.has(element, Property::Copy))
                            && all_args(interface)
                    }
                    ("core::Untrusted", "Render") => true,
                    ("core::Untrusted", "Encode") => all_args(interface),
                    ("core::Untrusted", "Generate") => ty.type_arg(0).is_some_and(|inner| {
                        self.implements(inner, interface) && self.has(inner, Property::Inert)
                    }),
                    (
                        "mem::List" | "mem::Set" | "mem::Bounded" | "mem::Map" | "mem::HashMap",
                        "Eq" | "Hash" | "Render" | "Encode" | "Generate",
                    ) => all_args(interface),
                    ("mem::Bytes", "Eq" | "Ord" | "Hash" | "Render" | "Encode" | "Generate") => {
                        true
                    }
                    ("mem::Box" | "mem::Shared", "Render" | "Encode") => all_args(interface),
                    ("mem::Box", "Generate") => all_args(interface),
                    ("mem::Id", "Eq" | "Ord" | "Hash" | "Encode" | "Generate") => true,
                    ("mem::Handle", "Eq" | "Hash") => true,
                    ("hw::Ptr", "Eq" | "Hash") => true,
                    ("core::Range", "Cursor") => {
                        return ty
                            .type_arg(0)
                            .filter(|element| {
                                element.scalar().is_some_and(|scalar| {
                                    scalar.is_integer() || scalar == Scalar::Char
                                })
                            })
                            .map(|element| Rc::from(vec![Arg::Ty(element.clone())]));
                    }
                    _ => false,
                };
                holds.then(no_args)
            }
            _ => None,
        }
    }

    /// Whether a cursor type's `next` is a projection.
    pub fn next_projects(&self, cursor: &Ty) -> bool {
        self.method_candidates(cursor, "next")
            .into_iter()
            .any(|candidate| match candidate {
                Candidate::Function { item, .. } => {
                    self.fn_sig(item).is_some_and(|sig| sig.is_proj())
                }
                Candidate::Method { bound, method, .. } => match &self.sig(bound.interface).kind {
                    SigKind::Interface { methods } => methods[method].sig.is_proj(),
                    _ => false,
                },
            })
    }

    /// The candidates of method resolution for `name` on a value of type `ty`.
    pub fn method_candidates(&self, ty: &Ty, name: &str) -> Vec<Candidate> {
        let mut candidates = Vec::new();
        match ty {
            Ty::Param(param) => {
                for bound in self.param_bounds(*param) {
                    if let Some(method) = self.interface_method(bound.interface, name) {
                        candidates.push(Candidate::Method {
                            bound,
                            method,
                            self_ty: ty.clone(),
                            function: None,
                            dynamic: false,
                        });
                    }
                }
            }
            Ty::Dyn(bound) => {
                if let Some(method) = self.interface_method(bound.interface, name) {
                    candidates.push(Candidate::Method {
                        bound: (**bound).clone(),
                        method,
                        self_ty: ty.clone(),
                        function: None,
                        dynamic: true,
                    });
                }
            }
            _ => {
                if let Some(Ty::Dyn(bound)) = self.boxed_dyn(ty)
                    && let Some(method) = self.interface_method(bound.interface, name)
                {
                    candidates.push(Candidate::Method {
                        bound: (*bound).clone(),
                        method,
                        self_ty: Ty::Dyn(bound.clone()),
                        function: None,
                        dynamic: true,
                    });
                    return candidates;
                }
                let Some(key) = type_key(ty) else {
                    return candidates;
                };
                let home = match key {
                    TypeKey::Item(item) => Some(self.db.module_of(item)),
                    _ => self.db.package(self.db.core()).root,
                };
                let subst = self.type_subst(key, ty);
                for &function in self.db.members(key, name) {
                    if Some(self.db.module_of(function)) == home && self.member_exists(function, ty)
                    {
                        candidates.push(Candidate::Function {
                            item: function,
                            subst: subst.clone(),
                        });
                    }
                }
                let package = match key {
                    TypeKey::Item(item) => Some(self.db.package_of(item)),
                    _ => Some(self.db.core()),
                };
                for interface in self.implemented_interfaces(ty, key) {
                    let interface_package = self.db.package_of(interface);
                    if Some(interface_package) != package && interface_package != self.db.core() {
                        continue;
                    }
                    let Some(method) = self.interface_method(interface, name) else {
                        continue;
                    };
                    let Some((conformance, args)) = self.conformance(ty, interface) else {
                        continue;
                    };
                    let function = match conformance {
                        Conformance::Impl { line, subst } => self
                            .implementing_function(line, name)
                            .map(|function| (function, subst)),
                        Conformance::Derive | Conformance::Builtin | Conformance::Bound => self
                            .db
                            .members(key, name)
                            .first()
                            .map(|function| (*function, self.type_subst(key, ty))),
                    };
                    candidates.push(Candidate::Method {
                        bound: Bound { interface, args },
                        method,
                        self_ty: ty.clone(),
                        function,
                        dynamic: false,
                    });
                }
                if let Some(protocol) = self.outbox_protocol(ty)
                    && let Some(method) = self.outbox_method(protocol, name)
                {
                    candidates.push(Candidate::Method {
                        bound: Bound {
                            interface: protocol,
                            args: no_args(),
                        },
                        method,
                        self_ty: ty.clone(),
                        function: None,
                        dynamic: false,
                    });
                }
            }
        }
        let mut distinct: Vec<Candidate> = Vec::new();
        for candidate in candidates {
            if !distinct
                .iter()
                .any(|known| known.identity() == candidate.identity())
            {
                distinct.push(candidate);
            }
        }
        distinct
    }

    /// The `mem::Box[Dyn[I]]` contents type, when `ty` is one.
    pub fn boxed_dyn(&self, ty: &Ty) -> Option<Ty> {
        let Ty::Item(item, args) = ty else {
            return None;
        };
        (self.db.is_std(*item) && self.db.canonical(*item) == "mem::Box")
            .then(|| args.first().and_then(Arg::ty).cloned())
            .flatten()
            .filter(|inner| matches!(inner, Ty::Dyn(_)))
    }

    /// The substitution of a type item's parameters by a type's arguments.
    pub fn type_subst(&self, key: TypeKey, ty: &Ty) -> Subst {
        let generics = self.type_generics(key);
        Subst::of(generics.iter().map(|def| def.param), ty.args())
    }

    /// The interfaces a type implements through impl lines, derives and std conformances.
    pub fn implemented_interfaces(&self, ty: &Ty, key: TypeKey) -> Vec<ItemId> {
        let mut interfaces: Vec<ItemId> = Vec::new();
        for line in self.impl_lines(key) {
            if let SigKind::Impl {
                interface: Some(bound),
                ..
            } = &self.sig(line).kind
            {
                interfaces.push(bound.interface);
            }
        }
        for name in DERIVABLE {
            if let Some(interface) = self.db.core_item(name) {
                interfaces.push(interface);
            }
        }
        for name in [
            "Add", "Sub", "Mul", "Div", "Rem", "BitAnd", "BitOr", "BitXor", "Shl", "Shr", "Neg",
            "BitNot", "Cursor",
        ] {
            if let Some(interface) = self.db.std_item("core", name) {
                interfaces.push(interface);
            }
        }
        interfaces.sort();
        interfaces.dedup();
        interfaces
            .into_iter()
            .filter(|interface| self.implements(ty, *interface))
            .collect()
    }

    /// The index of an interface's method of that name, or of the method of that name an outbox
    /// of a protocol calls.
    pub fn interface_method(&self, interface: ItemId, name: &str) -> Option<usize> {
        match &self.sig(interface).kind {
            SigKind::Interface { methods } => methods.iter().position(|method| method.name == name),
            SigKind::Protocol { .. } => self.outbox_method(interface, name),
            _ => None,
        }
    }

    /// The name of an interface's method, or of a method an outbox of a protocol calls.
    pub fn method_name(&self, interface: ItemId, method: usize) -> String {
        match &self.sig(interface).kind {
            SigKind::Interface { methods } => methods[method].name.clone(),
            SigKind::Protocol { operations, .. } => match operations.get(method) {
                Some(operation) => operation.name.clone(),
                None => "summary".to_owned(),
            },
            _ => panic!("a method of no interface or protocol"),
        }
    }

    /// The function `T.m` an impl line's module declares for the impl's target.
    pub fn implementing_function(&self, line: ItemId, name: &str) -> Option<ItemId> {
        let SigKind::Impl {
            target: Some(key), ..
        } = self.sig(line).kind
        else {
            return None;
        };
        let module = self.db.module_of(line);
        self.db
            .members(key, name)
            .iter()
            .copied()
            .find(|function| self.db.module_of(*function) == module)
    }

    /// The head of an interface method with `Self` and the interface's parameters substituted,
    /// or of a method an outbox of a protocol calls.
    pub fn method_head(&self, bound: &Bound, method: usize, self_ty: &Ty) -> Option<FnSig> {
        let sig = self.sig(bound.interface);
        if let SigKind::Protocol { .. } = &sig.kind {
            return self.outbox_head(bound.interface, method, self_ty);
        }
        let SigKind::Interface { methods } = &sig.kind else {
            return None;
        };
        let mut subst = Subst::of(
            sig.generics
                .iter()
                .filter(|def| !def.param.is_self())
                .map(|def| def.param),
            &bound.args,
        );
        subst.insert(
            Param {
                owner: bound.interface,
                index: Param::SELF,
            },
            Arg::Ty(self_ty.clone()),
        );
        let head = &methods[method].sig;
        Some(FnSig {
            generics: head.own_generics().to_vec(),
            inherited: 0,
            params: head
                .params
                .iter()
                .map(|param| super::sig::ParamSig {
                    ty: subst.ty(&param.ty),
                    ..param.clone()
                })
                .collect(),
            result: subst.ty(&head.result),
            fails: head.fails.as_ref().map(|ty| subst.ty(ty)),
            ..head.clone()
        })
    }

    /// Whether a std member that exists only for some instantiations exists for `ty`. Typing a
    /// member of the same type that exists only for some instantiations, its receiver's type
    /// admits the members whose receivers include all of its own.
    fn member_exists(&self, function: ItemId, ty: &Ty) -> bool {
        if !self.db.is_std(function) {
            return true;
        }
        let Some(receivers) = self.receivers(function) else {
            return true;
        };
        if let (Some(Ty::Param(param)), Some(within)) = (ty.type_arg(0), self.within.get())
            && self.db.owners.get(&within) == Some(&TypeKey::Item(param.owner))
        {
            return self
                .receivers(within)
                .is_some_and(|own| receivers.include(own));
        }
        let element = ty.type_arg(0);
        let named = |names: &[&str], bytes_only: bool| {
            element.is_some_and(|inner| match inner {
                Ty::Item(item, args) => {
                    let path = self.db.canonical(*item);
                    names.contains(&path.as_str())
                        && (!bytes_only
                            || path == "mem::Bytes"
                            || args.first() == Some(&Arg::Ty(Ty::Scalar(Scalar::U8))))
                }
                _ => false,
            })
        };
        match receivers {
            Receivers::Copy => element.is_some_and(|element| self.has(element, Property::Copy)),
            Receivers::Bytes => element == Some(&Ty::Scalar(Scalar::U8)),
            Receivers::ByteContents => named(
                &["core::Slice", "core::SliceMut", "core::Array", "mem::Bytes"],
                true,
            ),
            Receivers::WritableByteContents => {
                named(&["core::SliceMut", "core::Array", "mem::Bytes"], true)
            }
            Receivers::OwnedBytes => named(&["mem::Bytes"], true),
            Receivers::Clearable => {
                named(&["core::Array", "mem::Bytes"], true) || named(&["hw::Dma"], false)
            }
            Receivers::Stepped => element.is_some_and(|element| {
                element
                    .scalar()
                    .is_some_and(|scalar| scalar.is_integer() || scalar == Scalar::Char)
            }),
            Receivers::Integer => element.and_then(Ty::scalar).is_some_and(Scalar::is_integer),
            Receivers::Wide => matches!(element, Some(Ty::Scalar(Scalar::U64 | Scalar::I64))),
            Receivers::Bool => element == Some(&Ty::Scalar(Scalar::Bool)),
        }
    }

    /// The receivers of a std member that exists only for some instantiations.
    fn receivers(&self, function: ItemId) -> Option<Receivers> {
        let path = self.db.canonical(function);
        let (owner, member) = path.rsplit_once('.').unwrap_or((&path, ""));
        Some(match (owner, member) {
            ("core::Array", "filled")
            | ("core::SliceMut", "fill" | "copy_from")
            | ("core::Option", "value_or") => Receivers::Copy,
            ("core::Slice" | "core::SliceMut", member)
                if member.ends_with("_le") || member.ends_with("_be") =>
            {
                Receivers::Bytes
            }
            (
                "core::Untrusted",
                "byte_len" | "view" | "prefix" | "split_at" | "copy_prefix" | "try_copy_prefix",
            ) => Receivers::ByteContents,
            ("core::Untrusted", "view_mut") => Receivers::WritableByteContents,
            ("core::Untrusted", "append" | "try_append") => Receivers::OwnedBytes,
            ("core::Untrusted", "into_cleared") => Receivers::Clearable,
            ("core::Range", "next") => Receivers::Stepped,
            (
                "core::Secret",
                "wrapping_add" | "wrapping_sub" | "wrapping_mul" | "shl" | "shr" | "rotate_left"
                | "rotate_right" | "lt" | "le" | "gt" | "ge" | "widen" | "truncate",
            ) => Receivers::Integer,
            ("core::Secret", "mul_high") => Receivers::Wide,
            ("core::Secret", "select") => Receivers::Bool,
            _ => return None,
        })
    }

    /// The type a struct or enum's item is, applied to its own parameters, for a type key.
    pub fn own_type(&self, item: ItemId) -> Ty {
        let args: Vec<Arg> = self.sig(item).generics.iter().map(param_arg).collect();
        Ty::item(item, args)
    }
}

/// The key of the type a value of `ty` is, for member lookup.
pub fn type_key(ty: &Ty) -> Option<TypeKey> {
    match ty {
        Ty::Scalar(scalar) => Some(TypeKey::Scalar(*scalar)),
        Ty::Unit => Some(TypeKey::Unit),
        Ty::Item(item, _) => Some(TypeKey::Item(*item)),
        _ => None,
    }
}

impl Checker<'_> {
    /// The rule a type argument breaks for its generic parameter: a linear argument for a
    /// parameter not bounded by `Any`, or a bound, implied ones included, it does not satisfy.
    pub fn argument_problem(&self, def: &GenericDef, arg: &Ty, subst: &Subst) -> Option<Code> {
        if arg.has_error() || arg.is_never() {
            return None;
        }
        let any = def
            .bounds
            .iter()
            .any(|bound| self.db.canonical(bound.interface) == "core::Any");
        if !any && self.has(arg, Property::Linear) {
            return Some(Code::PropLinearArgument);
        }
        let accepts = self.accepts_views(def.param);
        let boxed_dyn =
            matches!(arg, Ty::Dyn(_)) && self.db.canonical(def.param.owner) == "mem::Box";
        let std_function = self.db.is_std(def.param.owner)
            && matches!(self.db.ast(def.param.owner).kind, ItemKind::Fn(_));
        if !accepts && !boxed_dyn && !std_function && self.has(arg, Property::View) {
            return Some(Code::PropViewArgument);
        }
        let container = self.is_view_container(&Ty::item(def.param.owner, Vec::new()));
        if accepts && !container && self.holds_token(arg) {
            return Some(Code::PropTokenArgument);
        }
        let implied = self.implied(def.param);
        let unsatisfied = def
            .bounds
            .iter()
            .any(|bound| !self.satisfies(arg, &subst.bound(bound)))
            || implied.failure && !self.is_failure_type(arg)
            || implied.cancellable && !self.is_cancellable(arg);
        unsatisfied.then_some(Code::GenericUnsatisfiedBound)
    }

    /// Reports, for an item, a `Dyn` type it writes whose interface is not Dyn-usable, or the
    /// first argument of an instantiation it writes that breaks its parameter's bounds.
    pub fn check_instantiation(&self, item: ItemId, ty: &Ty, token: Tok) {
        if let Ty::Dyn(bound) = ty
            && !self.dyn_usable(bound.interface)
            && self
                .arguments_problem(bound.interface, &bound.args)
                .is_none()
        {
            self.report(item, token, Code::InterfaceNotDynUsable);
            return;
        }
        if let Ty::Fn(fn_ty) = ty
            && let Some(code) = self
                .fn_type_problem(fn_ty)
                .or_else(|| self.fails_problem(fn_ty))
        {
            self.report(item, token, code);
            return;
        }
        if let Some(code) = self.lowlevel_problem(ty) {
            self.report(item, token, code);
            return;
        }
        if let Ty::Fn(fn_ty) = ty
            && let Some(convention) = &fn_ty.convention
        {
            self.check_convention(item, token, convention);
        }
        if self.ambiguous(ty)
            || self
                .constituents_of(ty)
                .iter()
                .any(|constituent| self.ambiguous(constituent))
        {
            self.report(item, token, Code::ErrorFromAmbiguous);
            return;
        }
        if let Ty::Item(target, args) = ty {
            self.check_arguments(item, token, *target, args);
        }
    }

    /// Reports, for an item, the first argument it writes for a generic type or interface that
    /// breaks its parameter's bounds, or records the instantiation of an item with const
    /// parameters.
    pub fn check_arguments(&self, item: ItemId, token: Tok, target: ItemId, args: &Args) {
        let generics = &self.sig(target).generics;
        if let Some(problem) = self.arguments_problem(target, args) {
            self.report_argument(
                item,
                token,
                &generics[problem.index],
                &problem.arg,
                &problem.subst,
                problem.code,
            );
        } else if generics.iter().any(|def| def.constant.is_some()) {
            self.instantiated(item, token, target, args.clone());
        }
    }

    /// The first of a generic type's or interface's arguments that breaks a rule of its
    /// parameter.
    fn arguments_problem(&self, target: ItemId, args: &Args) -> Option<ArgumentProblem> {
        let generics = &self.sig(target).generics;
        let subst = Subst::of(generics.iter().map(|def| def.param), args);
        generics
            .iter()
            .zip(args.iter())
            .enumerate()
            .find_map(|(index, (def, arg))| match arg {
                Arg::Ty(arg) => {
                    self.argument_problem(def, arg, &subst)
                        .map(|code| ArgumentProblem {
                            index,
                            arg: arg.clone(),
                            subst: subst.clone(),
                            code,
                        })
                }
                Arg::Const(_) => None,
            })
    }

    /// Reports a type argument breaking the rule `code` of its generic parameter, with what the
    /// parameter requires, the argument and the chain to the root of the property it breaks.
    pub fn report_argument(
        &self,
        item: ItemId,
        token: Tok,
        def: &GenericDef,
        arg: &Ty,
        subst: &Subst,
        code: Code,
    ) {
        let from = self.db.module_of(item);
        let (requirement, because) = match code {
            Code::PropLinearArgument => (
                "a droppable type".to_owned(),
                self.property_root(arg, from, &|ty| self.has(ty, Property::Linear)),
            ),
            Code::PropViewArgument => (
                "a type that is not a view".to_owned(),
                self.property_root(arg, from, &|ty| self.has(ty, Property::View)),
            ),
            Code::PropTokenArgument => (
                "a type holding no state token".to_owned(),
                self.property_root(arg, from, &|ty| self.holds_token(ty)),
            ),
            _ => (self.unsatisfied_bound(def, arg, subst, from), Vec::new()),
        };
        let expected = format!(
            "{requirement} for {} of {}",
            def.name,
            self.item_written(def.param.owner, from)
        );
        self.report_record(
            item,
            token,
            code,
            Some(expected),
            Some(self.show(arg, from)),
            because,
        );
    }

    /// The requirement of the first bound of a generic parameter, implied ones last, that a
    /// type argument does not satisfy.
    fn unsatisfied_bound(
        &self,
        def: &GenericDef,
        arg: &Ty,
        subst: &Subst,
        from: ModuleId,
    ) -> String {
        if let Some(bound) = def
            .bounds
            .iter()
            .map(|bound| subst.bound(bound))
            .find(|bound| !self.satisfies(arg, bound))
        {
            return format!(
                "a type implementing {}{}",
                self.item_written(bound.interface, from),
                self.show_args(&bound.args, from)
            );
        }
        match self.implied(def.param).failure && !self.is_failure_type(arg) {
            true => "a failure type".to_owned(),
            false => "a cancellable type".to_owned(),
        }
    }

    /// The `declared` reasons leading from a type to the root of a property it has.
    fn property_root(&self, ty: &Ty, from: ModuleId, has: &dyn Fn(&Ty) -> bool) -> Vec<Reason> {
        let mut because = Vec::new();
        let mut passed = Vec::new();
        let mut current = ty.clone();
        while let Ty::Item(item, args) = &current
            && !passed.contains(item)
        {
            passed.push(*item);
            let next = if self.db.is_std(*item) {
                args.iter()
                    .filter_map(Arg::ty)
                    .find(|arg| has(arg))
                    .cloned()
            } else {
                let subst = Subst::of(self.sig(*item).generics.iter().map(|def| def.param), args);
                let found = self.constituent_with(*item, &subst, from, has);
                let (token, subject) = match &found {
                    Some((token, subject, _)) => (*token, subject.clone()),
                    None => (self.db.ast(*item).token, self.show(&current, from)),
                };
                because.push(self.reason(*item, token, ReasonRole::Declared, subject));
                found.map(|(_, _, ty)| ty)
            };
            let Some(next) = next else {
                break;
            };
            current = next;
        }
        because
    }

    /// The first field or variant of a struct or enum, in source order, whose type has a
    /// property: its token, the field written `name: Type` or the variant `Variant(Type)` or
    /// `Variant(label: Type)`, and that type.
    fn constituent_with(
        &self,
        item: ItemId,
        subst: &Subst,
        from: ModuleId,
        has: &dyn Fn(&Ty) -> bool,
    ) -> Option<(Tok, String, Ty)> {
        match &self.sig(item).kind {
            SigKind::Struct { fields } => fields.iter().find_map(|field| {
                let ty = subst.ty(&field.ty);
                has(&ty).then(|| {
                    (
                        field.token,
                        format!("{}: {}", field.name, self.show(&ty, from)),
                        ty,
                    )
                })
            }),
            SigKind::Enum { variants } => variants.iter().find_map(|variant| {
                let payload: Vec<(String, Ty)> = match &variant.payload {
                    Payload::None => Vec::new(),
                    Payload::Single(ty) => vec![(String::new(), subst.ty(ty))],
                    Payload::Labelled(fields) => fields
                        .iter()
                        .map(|(label, ty)| (format!("{label}: "), subst.ty(ty)))
                        .collect(),
                };
                let (label, ty) = payload.into_iter().find(|(_, ty)| has(ty))?;
                let subject = format!("{}({label}{})", variant.name, self.show(&ty, from));
                Some((variant.token, subject, ty))
            }),
            _ => None,
        }
    }
}

/// A type argument breaking a rule of its generic parameter: the parameter's index, the
/// argument, the substitution of its instantiation's arguments and the rule's code.
struct ArgumentProblem {
    index: usize,
    arg: Ty,
    subst: Subst,
    code: Code,
}

impl Checker<'_> {
    /// The constituent types of a struct or enum instantiation, its arguments substituted.
    fn constituents_of(&self, ty: &Ty) -> Vec<Ty> {
        let Ty::Item(item, args) = ty else {
            return Vec::new();
        };
        let subst = Subst::of(self.sig(*item).generics.iter().map(|def| def.param), args);
        self.constituents(*item)
            .iter()
            .map(|constituent| subst.ty(constituent))
            .collect()
    }
}

impl super::db::Db<'_> {
    /// Whether a std impl line without parameters covers droppable instantiations only, as its
    /// owning chapter restricts the owning cursors' and `ArrayCursor`'s.
    pub fn canonical_line_droppable(&self, line: ItemId) -> bool {
        let ItemKind::Impl(implementation) = &self.ast(line).kind else {
            return false;
        };
        matches!(
            implementation.target.last().text.as_str(),
            "ArrayCursor"
                | "ListCursor"
                | "BoundedCursor"
                | "MapIntoCursor"
                | "HashMapIntoCursor"
                | "SetIntoCursor"
        )
    }

    /// Whether a std member counts its owner's parameters as droppable, as its owning chapter
    /// says of the members of `mem::List` that may release elements.
    pub fn canonical_member_droppable(&self, function: ItemId) -> bool {
        matches!(
            self.canonical(function).as_str(),
            "mem::List.truncate"
                | "mem::List.retain"
                | "mem::List.try_push"
                | "mem::List.try_insert"
        )
    }
}

impl Checker<'_> {
    /// Records an instantiation of an item with const parameters that an item's code writes,
    /// whose `where` conditions are judged once the items are elaborated.
    pub fn instantiated(&self, holder: ItemId, token: Tok, target: ItemId, args: Args) {
        if self.muted.get() == 0 && !self.db.is_std(holder) {
            self.instantiations
                .borrow_mut()
                .push((holder, token, target, args));
        }
    }

    /// Reports each instantiation whose arguments break a `where` condition of a const
    /// parameter: by const evaluation when every argument is a value, and otherwise unless the
    /// prover proves it from the `where` conditions in scope (`generic.where-unmet`).
    pub fn check_wheres(&self) {
        let mut judged: Vec<(ItemId, Tok)> = Vec::new();
        let instantiations = self.instantiations.borrow().clone();
        for (holder, token, target, args) in instantiations {
            if judged.contains(&(holder, token)) || self.rejected(holder) {
                continue;
            }
            judged.push((holder, token));
            let Some(unit) = self.unit(target) else {
                continue;
            };
            let conditions: Vec<u32> = unit
                .functions
                .iter()
                .filter(|function| function.role == Role::Where)
                .map(|function| function.index)
                .collect();
            if conditions.is_empty() {
                continue;
            }
            let generics = self.sig(target).generics.clone();
            let consts: Vec<(Scalar, &Arg)> = generics
                .iter()
                .zip(args.iter())
                .filter_map(|(def, arg)| def.constant.map(|scalar| (scalar, arg)))
                .collect();
            let values: Option<Vec<Val>> = consts
                .iter()
                .map(|(_, arg)| match arg {
                    Arg::Const(Const::Int(value)) => Some(Val::Int(*value)),
                    Arg::Const(Const::Bool(value)) => Some(Val::Bool(*value)),
                    _ => None,
                })
                .collect();
            let met = match values {
                Some(values) => conditions.iter().all(|index| {
                    matches!(
                        Interpreter::new(self).run(
                            target,
                            Role::Where,
                            *index,
                            values.clone(),
                            &args
                        ),
                        End::Value(Val::Bool(true))
                    )
                }),
                None => self.wheres_entailed(holder, target, &unit, &conditions, &args, &consts),
            };
            if !met {
                self.report(holder, token, Code::GenericWhereUnmet);
            }
        }
    }
}

/// The receivers a std member existing only for some instantiations admits, by its receiver's
/// type argument.
#[derive(Clone, Copy, PartialEq)]
enum Receivers {
    Copy,
    Bytes,
    ByteContents,
    WritableByteContents,
    OwnedBytes,
    Clearable,
    Stepped,
    /// The integer types.
    Integer,
    /// `u64` and `i64`.
    Wide,
    Bool,
}

impl Receivers {
    /// Whether these receivers include all of `inner`'s.
    fn include(self, inner: Receivers) -> bool {
        self == inner
            || matches!(
                (self, inner),
                (
                    Receivers::ByteContents,
                    Receivers::WritableByteContents | Receivers::OwnedBytes
                ) | (Receivers::WritableByteContents, Receivers::OwnedBytes)
            )
    }
}
