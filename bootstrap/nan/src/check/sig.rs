//! Signatures: what each item declares to the code naming it, resolved from its head, fields,
//! variants or methods, with the problems found resolving them.

use std::rc::Rc;

use super::Checker;
use super::db::{ItemId, ModuleId, TypeKey};
use super::resolve::{Named, PathError};
use super::reversibility::Class;
use super::ty::{Arg, Args, Bound, Const, FnParam, FnTy, LimitSet, Param, Scalar, Ty};
use crate::diagnostic::Code;
use crate::interp::Val;
use crate::syntax::ast::{
    self, FnKind, FnModifier, GenericArg, ItemKind, LimitKind, Mode, Tok, TypeKind, Visibility,
};

/// A diagnostic found resolving a signature, reported when its item is checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub code: Code,
    pub token: Tok,
}

/// What resolving a signature finds: its diagnostics, the type mismatches with their expected
/// and found texts, and the instantiations, `Dyn` and function types and bounds it writes,
/// checked once every signature is known.
#[derive(Clone, Debug, Default)]
pub struct Problems {
    pub found: Vec<Problem>,
    pub mismatches: Vec<(Tok, String, String)>,
    pub instantiations: Vec<(Ty, Tok)>,
    pub bounds: Vec<(Bound, Tok)>,
    /// The paths naming nothing that match the old form of a tombstone without an old head, by
    /// their first and last tokens, with the tombstone.
    pub renamed: Vec<((Tok, Tok), ItemId)>,
}

impl Problems {
    pub fn push(&mut self, problem: Problem) {
        self.found.push(problem);
    }
}

#[derive(Clone, Debug)]
pub struct GenericDef {
    pub name: String,
    pub param: Param,
    pub bounds: Vec<Bound>,
    /// The type of a const parameter.
    pub constant: Option<Scalar>,
}

/// Where a type expression is resolved: the item writing it, its module and the generic
/// parameters in scope.
#[derive(Clone, Copy)]
pub struct TypeCtx<'g> {
    pub item: ItemId,
    pub module: ModuleId,
    pub generics: &'g [GenericDef],
    /// The `Self` of an interface item.
    pub self_param: Option<Param>,
}

#[derive(Clone, Debug)]
pub struct ParamSig {
    pub name: String,
    pub mode: Mode,
    pub ty: Ty,
    pub token: Tok,
    pub is_self: bool,
}

#[derive(Clone, Debug)]
pub struct FnSig {
    /// The generic parameters in scope: the owner type's, then its own.
    pub generics: Vec<GenericDef>,
    /// How many of `generics` the owner type gives.
    pub inherited: usize,
    pub params: Vec<ParamSig>,
    pub result: Ty,
    pub fails: Option<Ty>,
    pub wait: bool,
    pub limits: LimitSet,
    pub modifier: Option<FnModifier>,
    pub convention: Option<String>,
}

impl FnSig {
    pub fn is_proj(&self) -> bool {
        self.modifier == Some(FnModifier::Proj)
    }

    pub fn has_self(&self) -> bool {
        self.params.first().is_some_and(|param| param.is_self)
    }

    pub fn own_generics(&self) -> &[GenericDef] {
        &self.generics[self.inherited..]
    }

    /// Whether the head is an abandon method's: `fn T.abandon(take self)` without modifier, own
    /// generic parameters, other parameters, result, `fails` or `uses wait`.
    pub fn abandon_head(&self) -> bool {
        let receiver =
            matches!(&self.params[..], [param] if param.is_self && param.mode == Mode::Take);
        receiver
            && self.modifier.is_none()
            && self.own_generics().is_empty()
            && self.result == Ty::Unit
            && self.fails.is_none()
            && !self.wait
    }

    /// Whether a type of the head failed.
    pub fn has_error(&self) -> bool {
        self.params.iter().any(|param| param.ty.has_error())
            || self.result.has_error()
            || self.fails.as_ref().is_some_and(Ty::has_error)
    }

    /// The function type of the head.
    pub fn fn_ty(&self) -> FnTy {
        let kind = match self.modifier {
            Some(FnModifier::Interrupt) => FnKind::Interrupt,
            _ => FnKind::Plain,
        };
        FnTy {
            kind,
            params: self
                .params
                .iter()
                .map(|param| FnParam {
                    mode: param.mode,
                    name: param.name.clone(),
                    ty: param.ty.clone(),
                })
                .collect(),
            result: self.result.clone(),
            fails: self.fails.clone(),
            wait: self.wait,
            limits: self.limits,
            convention: self.convention.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct FieldSig {
    pub name: String,
    pub ty: Ty,
    pub visibility: Visibility,
    pub token: Tok,
}

#[derive(Clone, Debug)]
pub enum Payload {
    None,
    Single(Ty),
    Labelled(Vec<(String, Ty)>),
}

#[derive(Clone, Debug)]
pub struct VariantSig {
    pub name: String,
    pub payload: Payload,
    pub from: bool,
    pub token: Tok,
}

#[derive(Clone, Debug)]
pub struct MethodSig {
    pub name: String,
    pub sig: FnSig,
    pub default: bool,
}

#[derive(Clone, Debug)]
pub struct RegisterSig {
    pub name: String,
    pub ty: Ty,
    pub access: ast::Access,
}

#[derive(Clone, Debug)]
pub enum SigKind {
    Fn(FnSig),
    Struct {
        fields: Vec<FieldSig>,
    },
    Enum {
        variants: Vec<VariantSig>,
    },
    Interface {
        methods: Vec<MethodSig>,
    },
    Impl {
        target: Option<TypeKey>,
        target_ty: Ty,
        interface: Option<Bound>,
    },
    Layout {
        fields: Vec<FieldSig>,
        tags: Vec<String>,
        bits: Option<Ty>,
    },
    Registers {
        registers: Vec<RegisterSig>,
    },
    Const {
        ty: Ty,
    },
    Registry {
        entry: Ty,
    },
    Other,
}

/// An item's signature: its generic parameters and what its kind declares.
#[derive(Clone, Debug)]
pub struct Sig {
    pub generics: Vec<GenericDef>,
    pub kind: SigKind,
    pub problems: Problems,
}

impl Sig {
    /// Whether a type the signature declares failed.
    pub fn has_error(&self) -> bool {
        let fields = |fields: &[FieldSig]| fields.iter().any(|field| field.ty.has_error());
        match &self.kind {
            SigKind::Fn(sig) => sig.has_error(),
            SigKind::Struct { fields: declared } => fields(declared),
            SigKind::Enum { variants } => variants.iter().any(|variant| match &variant.payload {
                Payload::None => false,
                Payload::Single(ty) => ty.has_error(),
                Payload::Labelled(labelled) => labelled.iter().any(|(_, ty)| ty.has_error()),
            }),
            SigKind::Interface { methods } => methods.iter().any(|method| method.sig.has_error()),
            SigKind::Impl { target_ty, .. } => target_ty.has_error(),
            SigKind::Layout {
                fields: declared,
                bits,
                ..
            } => fields(declared) || bits.as_ref().is_some_and(Ty::has_error),
            SigKind::Registers { registers } => {
                registers.iter().any(|register| register.ty.has_error())
            }
            SigKind::Const { ty } | SigKind::Registry { entry: ty } => ty.has_error(),
            SigKind::Other => false,
        }
    }
}

/// The offset of an interface method's own generic parameters in their owner's numbering.
pub fn method_param(index: usize, param: usize) -> u16 {
    (256 * (index + 1) + param) as u16
}

impl Checker<'_> {
    pub fn sig(&self, item: ItemId) -> Rc<Sig> {
        self.db.note(item);
        if let Some(sig) = self.sigs.borrow().get(&item) {
            return sig.clone();
        }
        let head = self.db.item(item).head_line();
        let sig = Rc::new(self.db.about(Some(item), || {
            self.db.on_line(head, || self.compute_sig(item))
        }));
        self.sigs.borrow_mut().insert(item, sig.clone());
        sig
    }

    /// The generic parameters an item declares, without their bounds: what naming it needs.
    pub fn shapes(&self, item: ItemId) -> Rc<Vec<GenericDef>> {
        self.db.note(item);
        if let Some(shapes) = self.shapes.borrow().get(&item) {
            return shapes.clone();
        }
        let params: &[ast::GenericParam] = match &self.db.ast(item).kind {
            ItemKind::Struct(structure) => &structure.generics,
            ItemKind::Enum(enumeration) => &enumeration.generics,
            ItemKind::Interface(interface) => &interface.generics,
            ItemKind::Fn(function) => &function.head.generics,
            _ => &[],
        };
        let shapes: Rc<Vec<GenericDef>> = Rc::new(
            params
                .iter()
                .enumerate()
                .map(|(index, param)| GenericDef {
                    name: param.name.text.clone(),
                    param: Param {
                        owner: item,
                        index: index as u16,
                    },
                    bounds: Vec::new(),
                    constant: const_type(param),
                })
                .collect(),
        );
        self.shapes.borrow_mut().insert(item, shapes.clone());
        shapes
    }

    /// The signature of a function item.
    pub fn fn_sig(&self, item: ItemId) -> Option<FnSig> {
        match &self.sig(item).kind {
            SigKind::Fn(sig) => Some(sig.clone()),
            _ => None,
        }
    }

    /// The generic parameters of a type item, or of the type a function is named after.
    pub fn type_generics(&self, key: TypeKey) -> Vec<GenericDef> {
        match key {
            TypeKey::Item(item) => self.sig(item).generics.clone(),
            _ => Vec::new(),
        }
    }

    /// The type a type key names, applied to its own generic parameters.
    pub fn key_ty(&self, key: TypeKey) -> Ty {
        match key {
            TypeKey::Scalar(scalar) => Ty::Scalar(scalar),
            TypeKey::Unit => Ty::Unit,
            TypeKey::Item(item) => {
                let args: Vec<Arg> = self.sig(item).generics.iter().map(param_arg).collect();
                Ty::item(item, args)
            }
        }
    }

    fn compute_sig(&self, item: ItemId) -> Sig {
        let data = self.db.item(item);
        let module = data.module;
        let mut problems = Problems::default();
        let (generics, kind) = match &data.ast.kind {
            ItemKind::Fn(function) => {
                let owner = self.db.owners.get(&item).copied();
                let mut inherited = owner.map(|key| self.type_generics(key)).unwrap_or_default();
                self.add_impl_bounds(item, &mut inherited);
                if self.db.is_std(item) && self.db.canonical_member_droppable(item) {
                    for def in &mut inherited {
                        def.bounds
                            .retain(|bound| self.db.canonical(bound.interface) != "core::Any");
                    }
                }
                let self_ty = owner.map(|key| self.key_ty(key));
                let sig = self.head_sig(
                    module,
                    item,
                    0,
                    &function.head,
                    inherited,
                    self_ty,
                    None,
                    function.modifier.map(|(modifier, _)| modifier),
                    function.convention.as_ref().map(|text| text.value.clone()),
                    &mut problems,
                );
                (sig.generics.clone(), SigKind::Fn(sig))
            }
            ItemKind::Retired(retired) => match &retired.head {
                Some(head) => {
                    let owner = retired
                        .member
                        .as_ref()
                        .and_then(|_| self.db.owner_key(module, &retired.path));
                    let inherited = owner.map(|key| self.type_generics(key)).unwrap_or_default();
                    let self_ty = owner.map(|key| self.key_ty(key));
                    let sig = self.head_sig(
                        module,
                        item,
                        0,
                        head,
                        inherited,
                        self_ty,
                        None,
                        None,
                        None,
                        &mut problems,
                    );
                    (sig.generics.clone(), SigKind::Fn(sig))
                }
                None => (Vec::new(), SigKind::Other),
            },
            ItemKind::Struct(structure) => {
                let generics = self.generic_defs(
                    module,
                    item,
                    0,
                    &structure.generics,
                    &[],
                    None,
                    &mut problems,
                );
                let ctx = TypeCtx {
                    item,
                    module,
                    generics: &generics,
                    self_param: None,
                };
                let fields = structure
                    .fields()
                    .map(|field| FieldSig {
                        name: field.name.text.clone(),
                        ty: self.type_expr(ctx, &field.ty, &mut problems),
                        visibility: field.visibility,
                        token: field.token,
                    })
                    .collect();
                (generics, SigKind::Struct { fields })
            }
            ItemKind::Enum(enumeration) => {
                let generics = self.generic_defs(
                    module,
                    item,
                    0,
                    &enumeration.generics,
                    &[],
                    None,
                    &mut problems,
                );
                let ctx = TypeCtx {
                    item,
                    module,
                    generics: &generics,
                    self_param: None,
                };
                let variants = enumeration
                    .variants
                    .iter()
                    .map(|variant| {
                        let payload = match &variant.payload {
                            None => Payload::None,
                            Some(fields) if fields.len() == 1 && fields[0].label.is_none() => {
                                Payload::Single(self.type_expr(ctx, &fields[0].ty, &mut problems))
                            }
                            Some(fields) => Payload::Labelled(
                                fields
                                    .iter()
                                    .map(|field| {
                                        let label = field
                                            .label
                                            .as_ref()
                                            .and_then(single_name)
                                            .unwrap_or_default();
                                        (label, self.type_expr(ctx, &field.ty, &mut problems))
                                    })
                                    .collect(),
                            ),
                        };
                        VariantSig {
                            name: variant.name.text.clone(),
                            payload,
                            from: variant.from,
                            token: variant.name.token,
                        }
                    })
                    .collect();
                (generics, SigKind::Enum { variants })
            }
            ItemKind::Interface(interface) => {
                let self_param = Param {
                    owner: item,
                    index: Param::SELF,
                };
                let mut generics = self.generic_defs(
                    module,
                    item,
                    0,
                    &interface.generics,
                    &[],
                    Some(self_param),
                    &mut problems,
                );
                let self_bound = Bound {
                    interface: item,
                    args: generics.iter().map(param_arg).collect(),
                };
                let mut self_bounds = vec![self_bound];
                self_bounds.extend(self.db.core_item("Any").map(|any| Bound {
                    interface: any,
                    args: Rc::from(Vec::new()),
                }));
                generics.push(GenericDef {
                    name: "Self".to_owned(),
                    param: self_param,
                    bounds: self_bounds,
                    constant: None,
                });
                let methods = interface
                    .methods
                    .iter()
                    .enumerate()
                    .map(|(index, method)| {
                        let sig = self.head_sig(
                            module,
                            item,
                            index + 1,
                            &method.head,
                            generics.clone(),
                            Some(Ty::Param(self_param)),
                            Some(self_param),
                            method.proj.then_some(FnModifier::Proj),
                            None,
                            &mut problems,
                        );
                        MethodSig {
                            name: method.name.text.clone(),
                            sig,
                            default: method.body.is_some(),
                        }
                    })
                    .collect();
                (generics, SigKind::Interface { methods })
            }
            ItemKind::Impl(implementation) => {
                let target = self.db.owner_key(module, &implementation.target);
                let mut generics = target
                    .map(|key| self.type_generics(key))
                    .unwrap_or_default();
                let ctx = TypeCtx {
                    item,
                    module,
                    generics: &generics,
                    self_param: None,
                };
                if let Some(params) = &implementation.params {
                    let added: Vec<Vec<Bound>> = params
                        .iter()
                        .map(|param| {
                            param
                                .bounds
                                .iter()
                                .filter_map(|bound| self.bound(ctx, bound, &mut problems))
                                .collect()
                        })
                        .collect();
                    for (def, bounds) in generics.iter_mut().zip(added) {
                        def.bounds.extend(bounds);
                    }
                }
                let ctx = TypeCtx {
                    item,
                    module,
                    generics: &generics,
                    self_param: None,
                };
                let interface = self.bound(ctx, &implementation.interface, &mut problems);
                let target_ty = target.map_or(Ty::Error, |key| self.key_ty(key));
                (
                    generics,
                    SigKind::Impl {
                        target,
                        target_ty,
                        interface,
                    },
                )
            }
            ItemKind::Layout(layout) => {
                let ctx = TypeCtx {
                    item,
                    module,
                    generics: &[],
                    self_param: None,
                };
                let bits = match &layout.repr {
                    ast::LayoutRepr::Integer { ty, .. } => {
                        Scalar::from_name(&ty.text).map(Ty::Scalar)
                    }
                    ast::LayoutRepr::Bytes { size, .. } => {
                        let size = int_literal(data.text(*size)).unwrap_or(0);
                        let array = self.db.core_item("Array");
                        array.map(|array| {
                            Ty::item(
                                array,
                                vec![
                                    Arg::Ty(Ty::Scalar(Scalar::U8)),
                                    Arg::Const(Const::Int(size)),
                                ],
                            )
                        })
                    }
                };
                let fields = layout
                    .fields
                    .iter()
                    .map(|field| {
                        let ty = match &field.kind {
                            ast::LayoutFieldKind::Bit { high: None, .. } => {
                                Ty::Scalar(Scalar::Bool)
                            }
                            ast::LayoutFieldKind::Bit {
                                low,
                                high: Some(high),
                            } => {
                                let width = int_literal(data.text(*high)).unwrap_or(0)
                                    - int_literal(data.text(*low)).unwrap_or(0)
                                    + 1;
                                Ty::Scalar(smallest_unsigned(width))
                            }
                            ast::LayoutFieldKind::Rest => bits.clone().unwrap_or(Ty::Error),
                            ast::LayoutFieldKind::Typed { ty, .. } => {
                                self.type_expr(ctx, ty, &mut problems)
                            }
                        };
                        FieldSig {
                            name: field.name.text.clone(),
                            ty,
                            visibility: data.ast.visibility,
                            token: field.name.token,
                        }
                    })
                    .collect();
                let tags = layout
                    .tags
                    .iter()
                    .flatten()
                    .map(|tag| tag.name.text.clone())
                    .collect();
                (Vec::new(), SigKind::Layout { fields, tags, bits })
            }
            ItemKind::Registers(registers) => {
                let ctx = TypeCtx {
                    item,
                    module,
                    generics: &[],
                    self_param: None,
                };
                let registers = registers
                    .registers
                    .iter()
                    .map(|register| RegisterSig {
                        name: register.name.text.clone(),
                        ty: self.type_expr(ctx, &register.ty, &mut problems),
                        access: register.access,
                    })
                    .collect();
                (Vec::new(), SigKind::Registers { registers })
            }
            ItemKind::Const(constant) => {
                let ctx = TypeCtx {
                    item,
                    module,
                    generics: &[],
                    self_param: None,
                };
                let ty = constant
                    .ty
                    .as_ref()
                    .map_or(Ty::Error, |ty| self.type_expr(ctx, ty, &mut problems));
                (Vec::new(), SigKind::Const { ty })
            }
            ItemKind::Registry(registry) => {
                let ctx = TypeCtx {
                    item,
                    module,
                    generics: &[],
                    self_param: None,
                };
                let entry = self.type_expr(ctx, &registry.entry, &mut problems);
                (Vec::new(), SigKind::Registry { entry })
            }
            _ => (Vec::new(), SigKind::Other),
        };
        Sig {
            generics,
            kind,
            problems,
        }
    }

    /// The impl lines a function `T.m` implements a method of: those of its module whose target is
    /// `T` and whose interface declares a method `m`.
    pub fn implemented_lines(&self, function: ItemId) -> Vec<ItemId> {
        self.db.note(function);
        if let Some(known) = self.implemented.borrow().get(&function) {
            return known.clone();
        }
        let lines = self.find_implemented_lines(function);
        self.implemented
            .borrow_mut()
            .insert(function, lines.clone());
        lines
    }

    fn find_implemented_lines(&self, function: ItemId) -> Vec<ItemId> {
        let ItemKind::Fn(head) = &self.db.ast(function).kind else {
            return Vec::new();
        };
        let Some(key) = self.db.owners.get(&function).copied() else {
            return Vec::new();
        };
        let module = self.db.module_of(function);
        let name = &head.label.name.text;
        self.db.about(Some(function), || {
            self.db
                .module(module)
                .items
                .iter()
                .copied()
                .filter(|&line| {
                    let ItemKind::Impl(implementation) = &self.db.ast(line).kind else {
                        return false;
                    };
                    self.db.owner_key(module, &implementation.target) == Some(key)
                        && matches!(self.db.resolve(module, &implementation.interface.path), Ok(Named::Item(interface))
                            if matches!(&self.db.ast(interface).kind,
                                ItemKind::Interface(declared) if declared.methods.iter().any(|method| method.name.text == *name)))
                })
                .collect()
        })
    }

    /// Adds to a member function's inherited parameters the bounds of each impl line it implements
    /// a method of, and counts as droppable each parameter droppable for such a line.
    fn add_impl_bounds(&self, function: ItemId, inherited: &mut [GenericDef]) {
        for line in self.implemented_lines(function) {
            for (def, line_def) in inherited.iter_mut().zip(&self.sig(line).generics) {
                for bound in &line_def.bounds {
                    if !def.bounds.contains(bound) {
                        def.bounds.push(bound.clone());
                    }
                }
            }
            for (def, droppable) in inherited.iter_mut().zip(self.droppable_for_line(line)) {
                if droppable {
                    def.bounds
                        .retain(|bound| self.db.canonical(bound.interface) != "core::Any");
                }
            }
        }
    }

    /// The signature of a function or interface method head, `method` being the method's index
    /// plus one or 0 for an item.
    #[allow(clippy::too_many_arguments)]
    pub fn head_sig(
        &self,
        module: ModuleId,
        owner: ItemId,
        method: usize,
        head: &ast::Head,
        inherited: Vec<GenericDef>,
        self_ty: Option<Ty>,
        self_param: Option<Param>,
        modifier: Option<FnModifier>,
        convention: Option<String>,
        problems: &mut Problems,
    ) -> FnSig {
        let inherited_count = inherited.len();
        let own = self.generic_defs(
            module,
            owner,
            method,
            &head.generics,
            &inherited,
            self_param,
            problems,
        );
        let mut generics = inherited;
        generics.extend(own);
        let ctx = TypeCtx {
            item: owner,
            module,
            generics: &generics,
            self_param,
        };
        let params = head
            .params
            .iter()
            .map(|param| match &param.kind {
                ast::ParamKind::Named { name, ty } => ParamSig {
                    name: name.text.clone(),
                    mode: param.mode,
                    ty: self.type_expr(ctx, ty, problems),
                    token: param.token,
                    is_self: false,
                },
                ast::ParamKind::SelfValue => ParamSig {
                    name: "self".to_owned(),
                    mode: param.mode,
                    ty: self_ty.clone().unwrap_or(Ty::Error),
                    token: param.token,
                    is_self: true,
                },
            })
            .collect();
        let result = head
            .result
            .as_ref()
            .map_or(Ty::Unit, |ty| self.type_expr(ctx, ty, problems));
        let fails = head
            .fails
            .as_ref()
            .map(|ty| self.type_expr(ctx, ty, problems));
        let wait = head.uses.as_ref().is_some_and(ast::Uses::is_wait);
        let interrupt = modifier == Some(FnModifier::Interrupt);
        let text = |token: Tok| self.db.item(owner).text(token).to_owned();
        if let Some(limits) = &head.limits {
            problems
                .found
                .extend(limit_problems(limits, wait, interrupt, text));
        }
        FnSig {
            generics,
            inherited: inherited_count,
            params,
            result,
            fails,
            wait,
            limits: limit_set(head.limits.as_ref(), interrupt, text),
            modifier,
            convention,
        }
    }

    /// The generic parameters an item or method declares, numbered from `method`'s offset, their
    /// bounds resolved with every parameter in scope.
    #[allow(clippy::too_many_arguments)]
    pub fn generic_defs(
        &self,
        module: ModuleId,
        owner: ItemId,
        method: usize,
        params: &[ast::GenericParam],
        inherited: &[GenericDef],
        self_param: Option<Param>,
        problems: &mut Problems,
    ) -> Vec<GenericDef> {
        let mut defs: Vec<GenericDef> = inherited.to_vec();
        let start = defs.len();
        for (index, param) in params.iter().enumerate() {
            let number = if method == 0 {
                index as u16
            } else {
                method_param(method - 1, index)
            };
            defs.push(GenericDef {
                name: param.name.text.clone(),
                param: Param {
                    owner,
                    index: number,
                },
                bounds: Vec::new(),
                constant: const_type(param),
            });
        }
        for (index, param) in params.iter().enumerate() {
            if defs[start + index].constant.is_some() {
                continue;
            }
            let ctx = TypeCtx {
                item: owner,
                module,
                generics: &defs,
                self_param,
            };
            let bounds: Vec<Bound> = param
                .bounds
                .iter()
                .filter_map(|bound| self.bound(ctx, bound, problems))
                .collect();
            defs[start + index].bounds = bounds;
        }
        defs.split_off(start)
    }

    /// What a path written in `module` names, forwarding a path that names nothing through a
    /// tombstone without an old head, which `problems` records.
    fn resolve_forwarded(
        &self,
        module: ModuleId,
        path: &ast::Path,
        problems: &mut Problems,
    ) -> Result<Named, PathError> {
        match self.db.resolve(module, path) {
            Err(error) if error.code == Some(Code::NameUnknown) => {
                let Some(tombstone) = self.renamed_path(module, path) else {
                    return Err(error);
                };
                let last = path.segments[path.segments.len() - 1].token;
                problems.renamed.push(((path.token(), last), tombstone));
                self.forwarding(tombstone).ok_or(PathError {
                    code: None,
                    token: error.token,
                })
            }
            resolved => resolved,
        }
    }

    /// The interface a bound names, with its arguments.
    pub fn bound(
        &self,
        ctx: TypeCtx,
        bound: &ast::Bound,
        problems: &mut Problems,
    ) -> Option<Bound> {
        let token = bound.path.token();
        let named = match self.resolve_forwarded(ctx.module, &bound.path, problems) {
            Ok(named) => named,
            Err(error) => {
                if let Some(code) = error.code {
                    problems.push(Problem {
                        code,
                        token: error.token,
                    });
                }
                return None;
            }
        };
        let Named::Item(interface) = named else {
            problems.push(Problem {
                code: Code::InterfaceNotInterface,
                token,
            });
            return None;
        };
        if !matches!(self.db.ast(interface).kind, ItemKind::Interface(_)) {
            problems.push(Problem {
                code: Code::InterfaceNotInterface,
                token,
            });
            return None;
        }
        if !self.db.visible(ctx.module, interface) {
            problems.push(Problem {
                code: Code::NamePrivate,
                token,
            });
        }
        let params = self.shapes(interface);
        let args = self.generic_args(ctx, &params, bound.args.as_deref(), token, problems)?;
        let bound = Bound { interface, args };
        problems.bounds.push((bound.clone(), token));
        Some(bound)
    }

    /// The arguments a type, bound or `Dyn` writes for `params`, all of them (`generic.arity`).
    fn generic_args(
        &self,
        ctx: TypeCtx,
        params: &[GenericDef],
        args: Option<&[GenericArg]>,
        token: Tok,
        problems: &mut Problems,
    ) -> Option<Args> {
        let args = args.unwrap_or(&[]);
        if args.len() != params.len() {
            problems.push(Problem {
                code: Code::GenericArity,
                token,
            });
            return None;
        }
        Some(
            args.iter()
                .zip(params)
                .map(|(arg, param)| self.generic_arg(ctx, arg, param, problems))
                .collect(),
        )
    }

    pub fn generic_arg(
        &self,
        ctx: TypeCtx,
        arg: &GenericArg,
        param: &GenericDef,
        problems: &mut Problems,
    ) -> Arg {
        match (arg, param.constant) {
            (GenericArg::Int { negative, token }, Some(scalar)) => {
                let text = self.db.item(ctx.item).text(*token);
                let written = literal_suffix(text);
                if !scalar.is_integer() || written.is_some_and(|written| written != scalar) {
                    let found = written.map_or("an integer type", Scalar::name);
                    problems
                        .mismatches
                        .push((*token, scalar.name().to_owned(), found.to_owned()));
                    return Arg::Const(Const::Error);
                }
                let value = int_literal(text).unwrap_or(0);
                let value = if *negative { -value } else { value };
                let (low, high) = scalar.range();
                if value < low || value > high {
                    problems.push(Problem {
                        code: Code::TypeLiteralRange,
                        token: *token,
                    });
                    return Arg::Const(Const::Error);
                }
                Arg::Const(Const::Int(value))
            }
            (GenericArg::Bool { value, token }, Some(scalar)) => {
                if scalar != Scalar::Bool {
                    problems.mismatches.push((
                        *token,
                        scalar.name().to_owned(),
                        Scalar::Bool.name().to_owned(),
                    ));
                    return Arg::Const(Const::Error);
                }
                Arg::Const(Const::Bool(*value))
            }
            (GenericArg::Type(ty), Some(scalar)) => {
                Arg::Const(self.const_arg(ctx, ty, scalar, problems))
            }
            (GenericArg::Type(ty), None) => Arg::Ty(self.type_expr(ctx, ty, problems)),
            (GenericArg::Int { token, .. } | GenericArg::Bool { token, .. }, None) => {
                problems.push(Problem {
                    code: Code::GenericArgumentKind,
                    token: *token,
                });
                Arg::Ty(Ty::Error)
            }
        }
    }

    /// A const argument of type `scalar` written as a type expression: a const item's value or a
    /// const parameter.
    fn const_arg(
        &self,
        ctx: TypeCtx,
        ty: &ast::TypeExpr,
        scalar: Scalar,
        problems: &mut Problems,
    ) -> Const {
        let TypeKind::Path { path, args: None } = &ty.kind else {
            problems.push(Problem {
                code: Code::GenericArgumentKind,
                token: ty.token,
            });
            return Const::Error;
        };
        if path.segments.len() == 1
            && let Some(def) = ctx
                .generics
                .iter()
                .find(|def| def.name == path.segments[0].text)
        {
            let Some(written) = def.constant else {
                problems.push(Problem {
                    code: Code::GenericArgumentKind,
                    token: ty.token,
                });
                return Const::Error;
            };
            if written != scalar {
                let found = written.name().to_owned();
                problems
                    .mismatches
                    .push((ty.token, scalar.name().to_owned(), found));
                return Const::Error;
            }
            return Const::Param(def.param);
        }
        match self.resolve_forwarded(ctx.module, path, problems) {
            Ok(Named::Item(item)) if matches!(self.db.ast(item).kind, ItemKind::Const(_)) => {
                if let SigKind::Const { ty: written } = &self.sig(item).kind
                    && *written != Ty::Scalar(scalar)
                    && !written.has_error()
                {
                    let found = self.show(written, ctx.module);
                    problems
                        .mismatches
                        .push((ty.token, scalar.name().to_owned(), found));
                    return Const::Error;
                }
                match self.const_value(item).map(|value| value.value.clone()) {
                    Some(Val::Int(value)) => Const::Int(value),
                    Some(Val::Bool(value)) => Const::Bool(value),
                    _ => Const::Error,
                }
            }
            Ok(_) => {
                problems.push(Problem {
                    code: Code::GenericArgumentKind,
                    token: ty.token,
                });
                Const::Error
            }
            Err(error) => {
                if let Some(code) = error.code {
                    problems.push(Problem {
                        code,
                        token: error.token,
                    });
                }
                Const::Error
            }
        }
    }

    /// The type a type expression denotes.
    pub fn type_expr(&self, ctx: TypeCtx, ty: &ast::TypeExpr, problems: &mut Problems) -> Ty {
        match &ty.kind {
            TypeKind::Fn(fn_type) => {
                let written = Ty::Fn(Rc::new(self.fn_type(ctx, fn_type, problems)));
                problems.instantiations.push((written.clone(), ty.token));
                written
            }
            TypeKind::Path { path, args } => {
                let token = path.token();
                if path.segments.len() == 1 {
                    let name = &path.segments[0].text;
                    if name == "Self"
                        && let Some(param) = ctx.self_param
                    {
                        return Ty::Param(param);
                    }
                    if let Some(def) = ctx.generics.iter().find(|def| def.name == *name) {
                        if args.is_some() || def.constant.is_some() {
                            problems.push(Problem {
                                code: Code::TypeNotAType,
                                token,
                            });
                            return Ty::Error;
                        }
                        return Ty::Param(def.param);
                    }
                }
                let named = match self.resolve_forwarded(ctx.module, path, problems) {
                    Ok(named) => named,
                    Err(error) => {
                        if let Some(code) = error.code {
                            problems.push(Problem {
                                code,
                                token: error.token,
                            });
                        }
                        return Ty::Error;
                    }
                };
                let arity = |problems: &mut Problems| {
                    if args.is_some() {
                        problems.push(Problem {
                            code: Code::GenericArity,
                            token,
                        });
                    }
                };
                match named {
                    Named::Scalar(scalar) => {
                        arity(problems);
                        Ty::Scalar(scalar)
                    }
                    Named::Unit => {
                        arity(problems);
                        Ty::Unit
                    }
                    Named::Never => {
                        arity(problems);
                        Ty::Never
                    }
                    Named::Dyn => {
                        let Some([GenericArg::Type(argument)]) = args.as_deref() else {
                            problems.push(Problem {
                                code: Code::GenericArity,
                                token,
                            });
                            return Ty::Error;
                        };
                        let TypeKind::Path {
                            path: interface,
                            args: interface_args,
                        } = &argument.kind
                        else {
                            problems.push(Problem {
                                code: Code::InterfaceNotInterface,
                                token: argument.token,
                            });
                            return Ty::Error;
                        };
                        let bound = ast::Bound {
                            path: interface.clone(),
                            args: interface_args.clone(),
                        };
                        match self.bound(ctx, &bound, problems) {
                            Some(bound) => {
                                let ty = Ty::Dyn(Rc::new(bound));
                                problems.instantiations.push((ty.clone(), token));
                                ty
                            }
                            None => Ty::Error,
                        }
                    }
                    Named::Item(item) => {
                        if !self.db.visible(ctx.module, item) {
                            problems.push(Problem {
                                code: Code::NamePrivate,
                                token,
                            });
                        }
                        match &self.db.ast(item).kind {
                            ItemKind::Struct(_) | ItemKind::Enum(_) | ItemKind::Layout(_) => {
                                let params = self.shapes(item);
                                match self.generic_args(
                                    ctx,
                                    &params,
                                    args.as_deref(),
                                    token,
                                    problems,
                                ) {
                                    Some(args) => {
                                        let ty = Ty::Item(item, args);
                                        if !params.is_empty() {
                                            problems.instantiations.push((ty.clone(), token));
                                        }
                                        ty
                                    }
                                    None => Ty::Error,
                                }
                            }
                            ItemKind::Registers(_) => {
                                arity(problems);
                                Ty::Registers(item)
                            }
                            _ => {
                                problems.push(Problem {
                                    code: Code::TypeNotAType,
                                    token,
                                });
                                Ty::Error
                            }
                        }
                    }
                    Named::Module(_) | Named::Variant(..) => {
                        problems.push(Problem {
                            code: Code::TypeNotAType,
                            token,
                        });
                        Ty::Error
                    }
                }
            }
        }
    }

    pub fn fn_type(&self, ctx: TypeCtx, fn_type: &ast::FnType, problems: &mut Problems) -> FnTy {
        let wait = fn_type.uses.as_ref().is_some_and(ast::Uses::is_wait);
        let interrupt = fn_type.kind == FnKind::Interrupt;
        let text = |token: Tok| self.db.item(ctx.item).text(token).to_owned();
        if let Some(limits) = &fn_type.limits {
            problems
                .found
                .extend(limit_problems(limits, wait, interrupt, text));
        }
        FnTy {
            kind: fn_type.kind,
            params: fn_type
                .params
                .iter()
                .map(|param| FnParam {
                    mode: param.mode,
                    name: param.name.text.clone(),
                    ty: self.type_expr(ctx, &param.ty, problems),
                })
                .collect(),
            result: fn_type
                .result
                .as_ref()
                .map_or(Ty::Unit, |ty| self.type_expr(ctx, ty, problems)),
            fails: fn_type
                .fails
                .as_ref()
                .map(|ty| self.type_expr(ctx, ty, problems)),
            wait,
            limits: limit_set(fn_type.limits.as_ref(), interrupt, text),
            convention: fn_type.convention.as_ref().map(|text| text.value.clone()),
        }
    }
}

/// The argument naming a generic parameter itself.
pub fn param_arg(def: &GenericDef) -> Arg {
    match def.constant {
        Some(_) => Arg::Const(Const::Param(def.param)),
        None => Arg::Ty(Ty::Param(def.param)),
    }
}

/// The type of a const parameter: the scalar its first bound names.
fn const_type(param: &ast::GenericParam) -> Option<Scalar> {
    let bound = param.bounds.first()?;
    match &bound.path.segments[..] {
        [name] if bound.args.is_none() => Scalar::from_name(&name.text),
        _ => None,
    }
}

/// The name a payload or field label is, when it is one identifier.
fn single_name(label: &ast::TypeExpr) -> Option<String> {
    match &label.kind {
        TypeKind::Path { path, args: None } if path.segments.len() == 1 => {
            Some(path.segments[0].text.clone())
        }
        _ => None,
    }
}

/// The rules of a `limits` clause: only a stack size and a class on an interrupt fn, each limit
/// once and each kind of a class once, neither `noalloc` nor a class beside `pure`, a stack size
/// from 1 to 4,294,967,295 written without a suffix, and neither `pure` nor `terminates` beside
/// `uses wait`.
pub fn limit_problems(
    limits: &ast::Limits,
    wait: bool,
    interrupt: bool,
    text: impl Fn(Tok) -> String,
) -> Vec<Problem> {
    let mut found = Vec::new();
    let mut seen: Vec<LimitKind> = Vec::new();
    let pure = limits
        .limits
        .iter()
        .any(|limit| limit.kind == LimitKind::Pure);
    for limit in &limits.limits {
        let code = if interrupt && !matches!(limit.kind, LimitKind::Stack | LimitKind::Class) {
            Some(Code::KernelIrqLimit)
        } else if seen.contains(&limit.kind) {
            Some(Code::LimitDuplicate)
        } else if matches!(limit.kind, LimitKind::Noalloc | LimitKind::Class) && pure {
            Some(Code::LimitRedundant)
        } else if limit.kind == LimitKind::Stack {
            let size = limit.size.map(&text);
            let valid = size.as_deref().is_some_and(|size| {
                !size.contains(['u', 'i'])
                    && int_literal(size)
                        .is_some_and(|value| (1..=i128::from(u32::MAX)).contains(&value))
            });
            (!valid).then_some(Code::LimitStackSize)
        } else if wait && limit.kind == LimitKind::Pure {
            Some(Code::LimitImpure)
        } else if wait && limit.kind == LimitKind::Terminates {
            Some(Code::LimitMayNotTerminate)
        } else {
            None
        };
        if let Some(code) = code {
            found.push(Problem {
                code,
                token: limit.token,
            });
        }
        seen.push(limit.kind);
        let kinds: Vec<String> = limit.kinds.iter().map(|kind| text(*kind)).collect();
        for (at, kind) in limit.kinds.iter().enumerate() {
            if kinds[..at].contains(&kinds[at]) {
                found.push(Problem {
                    code: Code::LimitDuplicate,
                    token: *kind,
                });
            }
        }
    }
    found
}

/// The demands of a `limits` clause, with the `notrap`, `terminates` and `noalloc` an interrupt
/// fn implies.
pub fn limit_set(
    limits: Option<&ast::Limits>,
    interrupt: bool,
    text: impl Fn(Tok) -> String,
) -> LimitSet {
    let mut set = LimitSet {
        notrap: interrupt,
        terminates: interrupt,
        noalloc: interrupt,
        ..LimitSet::default()
    };
    for limit in limits.iter().flat_map(|limits| &limits.limits) {
        match limit.kind {
            LimitKind::Pure => set.pure = true,
            LimitKind::Notrap => set.notrap = true,
            LimitKind::Terminates => set.terminates = true,
            LimitKind::Noalloc => set.noalloc = true,
            LimitKind::Class => {
                let kinds: Vec<String> = limit.kinds.iter().map(|kind| text(*kind)).collect();
                set.class = Class::named(&text(limit.token), kinds.iter().map(String::as_str));
            }
            LimitKind::Stack => {
                set.stack = limit
                    .size
                    .and_then(|size| int_literal(&text(size)))
                    .and_then(|size| u64::try_from(size).ok())
            }
        }
    }
    set
}

/// The value of an `INT` token without its suffix, when it fits.
pub fn int_literal(text: &str) -> Option<i128> {
    let digits: String = text.chars().filter(|c| *c != '_').collect();
    let (radix, digits) = match digits.strip_prefix("0x") {
        Some(hex) => (16, hex.to_owned()),
        None => match digits.strip_prefix("0b") {
            Some(binary) => (2, binary.to_owned()),
            None => (10, digits),
        },
    };
    let end = digits
        .find(|c: char| !c.is_digit(radix))
        .unwrap_or(digits.len());
    i128::from_str_radix(&digits[..end], radix).ok()
}

/// The scalar a number literal's suffix names, if it has one.
pub fn literal_suffix(text: &str) -> Option<Scalar> {
    let (body, markers): (&str, &[char]) = match text.strip_prefix("0x") {
        Some(hex) => (hex, &['u', 'i']),
        None => (text.trim_start_matches("0b"), &['u', 'i', 'f']),
    };
    let at = body.find(markers)?;
    Scalar::from_name(&body[at..])
}

/// The smallest unsigned integer type holding `width` bits.
fn smallest_unsigned(width: i128) -> Scalar {
    match width {
        ..=8 => Scalar::U8,
        9..=16 => Scalar::U16,
        17..=32 => Scalar::U32,
        _ => Scalar::U64,
    }
}
