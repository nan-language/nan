//! Release: what the end of a value's scope does. No user code runs; the memory and resources a
//! value holds return, through the release of each part that holds any.

use cranelift_codegen::ir::{InstBuilder, Value};

use super::Instance;
use super::Lowering;
use super::fx::Fx;
use super::layout::Shape;
use super::supplied;
use crate::check::props::Property;
use crate::check::ty::Ty;

impl Lowering<'_, '_> {
    /// Releases the value of a type at an address.
    pub fn release(&mut self, fx: &mut Fx, ty: &Ty, address: Value) {
        if !self.needs_release(ty) {
            return;
        }
        let glue = self.instance(Instance::Release(ty.clone()));
        let reference = self.func_ref(fx, glue);
        fx.b.ins().call(reference, &[address]);
    }

    /// Whether releasing a value of a type does anything.
    pub fn needs_release(&mut self, ty: &Ty) -> bool {
        if let Some(needed) = self.releases.get(ty) {
            return *needed;
        }
        self.releases.insert(ty.clone(), false);
        let needed = self.compute_needs_release(ty);
        self.releases.insert(ty.clone(), needed);
        needed
    }

    fn compute_needs_release(&mut self, ty: &Ty) -> bool {
        match ty {
            Ty::Item(item, _) => {
                if self.ck.has(ty, Property::Copy) || self.ck.has(ty, Property::View) {
                    return false;
                }
                if self.ck.db.is_std(*item) {
                    let path = self.ck.db.canonical(*item);
                    if let Some(needed) = supplied::base_needs_release(self, &path, ty) {
                        return needed;
                    }
                }
                let layout = self.layout(ty);
                let parts: Vec<Ty> = match &layout.shape {
                    Shape::Record(fields) | Shape::Bytes { fields, .. } => {
                        fields.iter().map(|field| field.ty.clone()).collect()
                    }
                    Shape::Enum(shape) => shape
                        .variants
                        .iter()
                        .flat_map(|variant| variant.fields.iter().map(|field| field.ty.clone()))
                        .collect(),
                    Shape::Array { element, .. } => vec![element.clone()],
                    _ => Vec::new(),
                };
                parts.iter().any(|part| self.needs_release(part))
            }
            Ty::Fn(fn_ty) => fn_ty.kind == crate::syntax::ast::FnKind::Task,
            _ => false,
        }
    }
}

/// The release of a value of a type, at the address its one parameter holds.
pub fn define(lo: &mut Lowering, fx: &mut Fx, ty: &Ty) {
    let params = fx.entry();
    let address = params[0];
    if let Ty::Item(item, _) = ty
        && lo.ck.db.is_std(*item)
        && supplied::release_base(lo, fx, ty, address)
    {
        fx.b.ins().return_(&[]);
        return;
    }
    if let Ty::Fn(_) = ty {
        let env = fx.load(super::fx::PTR, address, 8);
        lo.call_routine(fx, super::runtime::Routine::TaskEnvFree, &[env]);
        fx.b.ins().return_(&[]);
        return;
    }
    let layout = lo.layout(ty);
    match &layout.shape {
        Shape::Record(fields) | Shape::Bytes { fields, .. } => {
            for field in fields.clone() {
                let at = fx.offset(address, field.offset);
                lo.release(fx, &field.ty, at);
            }
        }
        Shape::Array { element, count } => {
            let element = element.clone();
            let size = lo.layout(&element).size;
            let start = fx.ptr(0);
            let end = fx.ptr(*count);
            fx.each(start, end, |fx, index| {
                let stride = fx.ptr(size);
                let offset = fx.b.ins().imul(index, stride);
                let at = fx.b.ins().iadd(address, offset);
                lo.release(fx, &element, at);
            });
        }
        Shape::Enum(shape) => {
            for variant in &shape.variants {
                let fields: Vec<_> = variant
                    .fields
                    .iter()
                    .filter(|field| lo.needs_release(&field.ty))
                    .cloned()
                    .collect();
                if fields.is_empty() {
                    continue;
                }
                let is = lo.variant_is(fx, address, ty, &variant.name);
                fx.when(is, |fx| {
                    for field in &fields {
                        let at = fx.offset(address, field.offset);
                        lo.release(fx, &field.ty, at);
                    }
                });
            }
        }
        _ => {}
    }
    fx.b.ins().return_(&[]);
}
