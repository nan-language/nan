//! The types lowering builds or inspects: std types by path, the wrapped form of projection
//! results, tag values, layout bit fields, cancellation variants and closure environments.

use super::Lowering;
use crate::check::db::ItemId;
use crate::check::sig::{Payload, SigKind};
use crate::check::ty::{Arg, FnParam, FnTy, Subst, Ty};
use crate::nir::{Function, NTy, Op, Param, Role, Unit};
use crate::syntax::ast::{FnKind, ItemKind, LayoutFieldKind};

impl Lowering<'_, '_> {
    /// A std type by its module path and name, with its arguments.
    pub fn std_ty(&self, module: &str, name: &str, args: Vec<Ty>) -> Ty {
        let item = self
            .ck
            .db
            .std_item(module, name)
            .or_else(|| {
                (module == "core")
                    .then(|| self.ck.db.core_item(name))
                    .flatten()
            })
            .unwrap_or_else(|| panic!("the std type {module}::{name}"));
        Ty::item(item, args.into_iter().map(Arg::Ty).collect())
    }

    /// The root grant of the target lowered for.
    pub fn root_grant(&self) -> Ty {
        let (module, name) = self.ck.db.target.root_grant().expect("a concrete target");
        self.std_ty(module, name, Vec::new())
    }

    pub fn slice_of(&self, element: Ty) -> Ty {
        self.std_ty("core", "Slice", vec![element])
    }

    /// The type a wrapped part of an `Option` or `Result` is laid out as: each payload that is
    /// a part held as the part's address.
    pub fn wrapped_type(&self, ty: &Ty) -> Ty {
        let Ty::Item(item, args) = ty else {
            return ty.clone();
        };
        let args = args
            .iter()
            .map(|arg| match arg {
                Arg::Ty(payload) if self.is_part(payload) => {
                    Arg::Ty(self.std_ty("hw", "Ptr", vec![payload.clone()]))
                }
                other => other.clone(),
            })
            .collect::<Vec<_>>();
        Ty::item(*item, args)
    }

    /// Whether a payload of a wrapped part is a part, held by address: not a view, whose values
    /// a projection builds.
    pub fn is_part(&self, payload: &Ty) -> bool {
        !self.ck.nir_view(payload)
    }

    /// The value of a layout's tag, or the index of an enum's variant held in a register.
    pub fn tag_value(&self, ty: &Ty, variant: &str) -> u64 {
        let Ty::Item(item, _) = ty else {
            panic!("a tag of {ty:?}");
        };
        if let Some(crate::check::lowlevel::LayoutShape::Tags(values)) = self.ck.layout_shape(*item)
            && let SigKind::Layout { tags, .. } = &self.ck.sig(*item).kind
        {
            let index = tags
                .iter()
                .position(|tag| tag == variant)
                .expect("a tag of the layout");
            return values[index];
        }
        match &self.ck.sig(*item).kind {
            SigKind::Enum { variants } => variants
                .iter()
                .position(|candidate| candidate.name == variant)
                .expect("a variant of the enum") as u64,
            _ => panic!("a tag of {ty:?}"),
        }
    }

    /// The bits of a field of an integer layout: its first bit, its width and its type.
    pub fn bit_field(&self, ty: &Ty, name: &str) -> (u32, u32, Ty) {
        let Ty::Item(item, _) = ty else {
            panic!("a bit field of {ty:?}");
        };
        let ItemKind::Layout(layout) = &self.ck.db.ast(*item).kind else {
            panic!("a bit field of the non-layout {ty:?}");
        };
        let Some(crate::check::lowlevel::LayoutShape::Fields(bits)) = self.ck.layout_shape(*item)
        else {
            panic!("a bit field of a layout without fields");
        };
        let SigKind::Layout { fields, .. } = &self.ck.sig(*item).kind else {
            panic!("a layout's fields");
        };
        let index = layout
            .fields
            .iter()
            .position(|field| field.name.text == name)
            .unwrap_or_else(|| panic!("the field {name} of {ty:?}"));
        let field = &bits[index];
        let width = match layout.fields[index].kind {
            LayoutFieldKind::Rest => self.layout_width(ty),
            _ => field.width,
        };
        (field.low, width, fields[index].ty.clone())
    }

    fn layout_width(&self, ty: &Ty) -> u32 {
        match &self.layouts_peek(ty) {
            Some(size) => size * 8,
            None => 64,
        }
    }

    fn layouts_peek(&self, ty: &Ty) -> Option<u32> {
        self.ck.fixed_size(ty).map(|size| size as u32)
    }

    /// The cancellation variant of a cancellable failure type: none inside for `task::Cancelled`
    /// itself, its variant's name for an enum with one; none outside for any other type.
    pub fn cancellation_variant(&self, ty: &Ty) -> Option<Option<String>> {
        if self.ck.is_std(ty, "task::Cancelled") {
            return Some(None);
        }
        let Ty::Item(item, args) = ty else {
            return None;
        };
        let sig = self.ck.sig(*item);
        let SigKind::Enum { variants } = &sig.kind else {
            return None;
        };
        let subst = Subst::of(sig.generics.iter().map(|def| def.param), args);
        variants.iter().find_map(|variant| match &variant.payload {
            Payload::Single(payload)
                if variant.from && self.ck.is_std(&subst.ty(payload), "task::Cancelled") =>
            {
                Some(Some(variant.name.clone()))
            }
            _ => None,
        })
    }

    /// The number of captures of a unit's closure: the operands of the instruction creating it.
    pub fn closure_captures(&self, unit: &Unit, index: u32) -> usize {
        for function in &unit.functions {
            for block in &function.blocks {
                for inst in &block.insts {
                    if let Op::Closure {
                        index: created,
                        captures,
                    } = &inst.op
                        && *created == index
                    {
                        return captures.len();
                    }
                }
            }
        }
        0
    }

    /// A closure's function type: its own parameters, result and failure.
    pub fn closure_type(&self, function: &Function, captures: usize, subst: &Subst) -> FnTy {
        let params = function.params[captures..]
            .iter()
            .map(|param| FnParam {
                mode: param.mode,
                name: param.name.clone(),
                ty: subst.ty(function.ty(param.value).ty()),
            })
            .collect();
        let task = function.closure.as_ref().is_some_and(|head| head.task);
        FnTy {
            kind: if task { FnKind::Task } else { FnKind::Plain },
            params,
            result: subst.ty(&function.result),
            fails: function.fails.as_ref().map(|ty| subst.ty(ty)),
            wait: function.wait,
            limits: Default::default(),
            convention: None,
        }
    }

    /// The offset of each capture in a closure's environment, from `start`: a reference as its
    /// address, any other value as its bytes.
    pub fn capture_layout(&mut self, types: &[NTy], params: &[Param], start: u64) -> Vec<u64> {
        let mut offset = start;
        params
            .iter()
            .map(|param| {
                let (size, align) = self.capture_size_align(&types[param.value.0 as usize]);
                offset = offset.next_multiple_of(align);
                let at = offset;
                offset += size;
                at
            })
            .collect()
    }

    /// The size of a closure's environment whose captures follow `start` bytes.
    pub fn capture_size(&mut self, types: &[NTy], params: &[Param], start: u64) -> u64 {
        let mut offset = start;
        for param in params {
            let (size, align) = self.capture_size_align(&types[param.value.0 as usize]);
            offset = offset.next_multiple_of(align) + size;
        }
        offset.next_multiple_of(8)
    }

    fn capture_size_align(&mut self, nty: &NTy) -> (u64, u64) {
        match nty {
            NTy::Ref(_) | NTy::RefMut(_) => (8, 8),
            NTy::Owned(ty) => {
                let layout = self.layout(ty);
                (layout.size, layout.align)
            }
        }
    }

    /// The role and index of the checking entry of a function, or of its body.
    pub fn entry_role(&self, item: ItemId) -> Role {
        let has_entry = self
            .ck
            .unit(item)
            .is_some_and(|unit| unit.function(Role::Entry, 0).is_some());
        if has_entry { Role::Entry } else { Role::Body }
    }
}
