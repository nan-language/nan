//! `testing::same`: structural equality, by a glue per type comparing two values at addresses.
//! Scalars and layout values compare by bits, structs by fields, enum values by variant and
//! payload; Untrusted values, boxes, shared values and text by contents; arrays, views and
//! collections by elements in order, ignoring the allocators they store; function values by
//! function; and every other std type by being the same table entry, cell or memory.

use cranelift_codegen::ir::{AbiParam, InstBuilder, Signature, Value, condcodes::IntCC, types};
use cranelift_module::FuncId;

use super::super::fx::{Fx, PTR};
use super::super::layout::Shape;
use super::super::runtime::Routine;
use super::super::{CALL_CONV, Glue, Lowering};
use super::Supply;
use super::mem::{stores, vector};
use crate::check::ty::{Scalar, Ty};

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    if supply.path != "testing::same" {
        return false;
    }
    let ty = supply.type_arg(0);
    let left = address(lo, fx, &ty, supply.param(0));
    let right = address(lo, fx, &ty, supply.param(1));
    let same = compare(lo, fx, &ty, left, right);
    supply.ret(lo, fx, super::super::body::Held::Scalar(same));
    true
}

/// The address of a value held one way or another.
fn address(lo: &mut Lowering, fx: &mut Fx, ty: &Ty, held: super::super::body::Held) -> Value {
    match held {
        super::super::body::Held::Address(address) => address,
        other => {
            let layout = lo.layout(ty);
            let slot = fx.slot(layout.size, layout.align);
            lo.store_held(fx, slot, other, ty);
            slot
        }
    }
}

/// Whether two values of a type at addresses are structurally equal.
fn compare(lo: &mut Lowering, fx: &mut Fx, ty: &Ty, left: Value, right: Value) -> Value {
    let glue = glue(lo, ty);
    let glue = lo.func_ref(fx, glue);
    let call = fx.b.ins().call(glue, &[left, right]);
    fx.b.inst_results(call)[0]
}

fn signature() -> Signature {
    let mut signature = Signature::new(CALL_CONV);
    signature.params.push(AbiParam::new(PTR));
    signature.params.push(AbiParam::new(PTR));
    signature.returns.push(AbiParam::new(types::I8));
    signature
}

/// The glue comparing two values of a type.
fn glue(lo: &mut Lowering, ty: &Ty) -> FuncId {
    let ty = ty.clone();
    lo.glue_function(Glue::Same(ty.clone()), signature(), move |lo, fx| {
        let params = fx.entry();
        let same = define(lo, fx, &ty, params[0], params[1]);
        fx.b.ins().return_(&[same]);
    })
}

/// The comparison of two values of a type at addresses.
fn define(lo: &mut Lowering, fx: &mut Fx, ty: &Ty, left: Value, right: Value) -> Value {
    let byte = Ty::Scalar(Scalar::U8);
    match ty {
        Ty::Fn(_) | Ty::Dyn(_) => {
            let left = fx.load(PTR, left, 0);
            let right = fx.load(PTR, right, 0);
            return fx.b.ins().icmp(IntCC::Equal, left, right);
        }
        Ty::Item(item, args) => {
            let path = lo.ck.db.canonical(*item);
            let arg = |index: usize| args.get(index).and_then(|arg| arg.ty()).cloned();
            match path.as_str() {
                "mem::List" | "mem::Set" => {
                    let element = arg(0).expect("an element type");
                    return vectors(lo, fx, &element, left, right);
                }
                "mem::Bytes" | "text::String" => return vectors(lo, fx, &byte, left, right),
                "mem::Map" | "mem::HashMap" => {
                    let entry = lo.std_ty(
                        "mem",
                        "KeyValue",
                        vec![arg(0).expect("a key type"), arg(1).expect("a value type")],
                    );
                    return vectors(lo, fx, &entry, left, right);
                }
                "core::Slice" | "core::SliceMut" => {
                    let element = arg(0).expect("an element type");
                    return views(lo, fx, &element, left, right);
                }
                "core::Str" => return views(lo, fx, &byte, left, right),
                "mem::Bounded" => {
                    let element = arg(0).expect("an element type");
                    return bounded(lo, fx, ty, ("items", &element), left, right);
                }
                "text::Fixed" => return bounded(lo, fx, ty, ("bytes", &byte), left, right),
                "mem::Box" | "mem::Shared" => {
                    let contents = arg(0).expect("a contents type");
                    if !matches!(contents, Ty::Dyn(_)) {
                        let left = fx.load(PTR, left, 0);
                        let right = fx.load(PTR, right, 0);
                        let left = stores::contents_address(lo, fx, ty, left);
                        let right = stores::contents_address(lo, fx, ty, right);
                        return compare(lo, fx, &contents, left, right);
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
    let layout = lo.layout(ty);
    match &layout.shape {
        Shape::Empty => fx.int(types::I8, 1),
        Shape::Scalar { ty: scalar, .. } => {
            let left = fx.load(*scalar, left, 0);
            let right = fx.load(*scalar, right, 0);
            fx.b.ins().icmp(IntCC::Equal, left, right)
        }
        Shape::Record(fields) => {
            let mut same = fx.int(types::I8, 1);
            for field in fields.clone() {
                let left = fx.offset(left, field.offset);
                let right = fx.offset(right, field.offset);
                let part = compare(lo, fx, &field.ty, left, right);
                same = fx.b.ins().band(same, part);
            }
            same
        }
        Shape::Enum(shape) => {
            let variant = lo.variant_index(fx, ty, left);
            let other = lo.variant_index(fx, ty, right);
            let same_variant = fx.b.ins().icmp(IntCC::Equal, variant, other);
            let payloads: Vec<Vec<(u64, Ty)>> = shape
                .variants
                .iter()
                .map(|variant| {
                    variant
                        .fields
                        .iter()
                        .map(|field| (field.offset, field.ty.clone()))
                        .collect()
                })
                .collect();
            let result = fx.var(types::I8, same_variant);
            fx.when(same_variant, |fx| {
                for (index, fields) in payloads.iter().enumerate() {
                    let this = fx.b.ins().icmp_imm_s(IntCC::Equal, variant, index as i64);
                    fx.when(this, |fx| {
                        let mut same = fx.int(types::I8, 1);
                        for (offset, field_ty) in fields {
                            let left = fx.offset(left, *offset);
                            let right = fx.offset(right, *offset);
                            let part = compare(lo, fx, field_ty, left, right);
                            same = fx.b.ins().band(same, part);
                        }
                        fx.b.def_var(result, same);
                    });
                }
            });
            fx.b.use_var(result)
        }
        Shape::Array { element, count } => {
            let count = fx.ptr(*count);
            elements(lo, fx, &element.clone(), left, right, count)
        }
        Shape::Bytes { .. } => {
            let size = fx.ptr(layout.size);
            let order = lo.call_routine(fx, Routine::Compare, &[left, right, size]);
            fx.b.ins().icmp_imm_s(IntCC::Equal, order[0], 0)
        }
    }
}

/// Whether the first `count` elements of a type at two addresses are equal in order.
fn elements(
    lo: &mut Lowering,
    fx: &mut Fx,
    element: &Ty,
    left: Value,
    right: Value,
    count: Value,
) -> Value {
    let size = lo.layout(element).size;
    let zero = fx.ptr(0);
    let yes = fx.int(types::I8, 1);
    let done = fx.repeat(&[zero, yes], |fx, state| {
        let (index, same) = (state[0], state[1]);
        let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, index, count);
        let going = fx.b.ins().band(inside, same);
        let next = fx.choose_in(
            lo,
            going,
            &[PTR, types::I8],
            |lo, fx| {
                let offset = fx.b.ins().imul_imm_s(index, size as i64);
                let at_left = fx.b.ins().iadd(left, offset);
                let at_right = fx.b.ins().iadd(right, offset);
                let same = compare(lo, fx, element, at_left, at_right);
                vec![fx.b.ins().iadd_imm_s(index, 1), same]
            },
            |_, _| vec![index, same],
        );
        (going, next)
    });
    done[1]
}

/// Whether two sequences of elements, each its start and length, are equal in order.
fn sequences(
    lo: &mut Lowering,
    fx: &mut Fx,
    element: &Ty,
    left: (Value, Value),
    right: (Value, Value),
) -> Value {
    let same_len = fx.b.ins().icmp(IntCC::Equal, left.1, right.1);
    fx.choose_in(
        lo,
        same_len,
        &[types::I8],
        |lo, fx| vec![elements(lo, fx, element, left.0, right.0, left.1)],
        |_, fx| vec![fx.int(types::I8, 0)],
    )[0]
}

/// Whether two vectors hold equal elements in order, whatever allocators they store.
fn vectors(lo: &mut Lowering, fx: &mut Fx, element: &Ty, left: Value, right: Value) -> Value {
    let parts = |fx: &mut Fx, address: Value| {
        (
            fx.load(PTR, address, vector::DATA),
            fx.load(PTR, address, vector::LEN),
        )
    };
    let (left, right) = (parts(fx, left), parts(fx, right));
    sequences(lo, fx, element, left, right)
}

/// Whether two views see equal elements in order.
fn views(lo: &mut Lowering, fx: &mut Fx, element: &Ty, left: Value, right: Value) -> Value {
    let parts = |fx: &mut Fx, address: Value| (fx.load(PTR, address, 0), fx.load(PTR, address, 8));
    let (left, right) = (parts(fx, left), parts(fx, right));
    sequences(lo, fx, element, left, right)
}

/// Whether two bounded lists or fixed texts, each its length and then its elements inline in
/// a field, hold equal elements in order.
fn bounded(
    lo: &mut Lowering,
    fx: &mut Fx,
    ty: &Ty,
    (field, element): (&str, &Ty),
    left: Value,
    right: Value,
) -> Value {
    let at = lo.layouts.field(lo.ck, ty, None, field).offset;
    let parts = |fx: &mut Fx, address: Value| (fx.offset(address, at), fx.load(PTR, address, 0));
    let (left, right) = (parts(fx, left), parts(fx, right));
    sequences(lo, fx, element, left, right)
}
