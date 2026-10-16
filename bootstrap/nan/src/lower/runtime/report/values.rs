//! The values trap reports write beyond scalars: text as a text literal, `mem::Bytes` as a
//! byte-string literal, and a struct, enum, `Option`, `Result`, array or slice whose parts can
//! be written as the construction, variant or `[a, b]` form that builds it, each by a glue
//! writing the value at an address.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, condcodes::IntCC, types};
use cranelift_module::FuncId;

use super::super::super::data::const_arg;
use super::super::super::fx::{Fx, PTR};
use super::super::super::layout::{Shape, scalar_type};
use super::super::super::trap::written;
use super::super::super::{Glue, Lowering};
use super::super::Routine;
use super::{Report, writer_signature};
use crate::check::sig::SigKind;
use crate::check::ty::{Arg, Scalar, Subst, Ty};

/// A field a report writes: its name, empty for the payload of `V(T)`, its type and its offset.
type Part = (String, Ty, u64);

/// How a report writes a value of a type.
enum Form {
    Scalar(Scalar),
    /// Text at the data and length of a vector or view, or in a fixed text.
    Text {
        fixed: bool,
    },
    Bytes,
    /// A construction: the struct's name and its fields.
    Struct(String, Vec<Part>),
    /// The variants of an enum: each as written, and its payload's fields.
    Enum(Vec<(String, Vec<Part>)>),
    Array(Ty, u64),
    Slice(Ty),
}

/// Whether a report writes values of a type otherwise than `_`.
pub fn writable(lo: &mut Lowering, ty: &Ty) -> bool {
    form(lo, ty, &mut Vec::new()).is_some()
}

/// The glue writing a value of a writable type, given the report and the value's address.
pub fn writer(lo: &mut Lowering, ty: &Ty) -> FuncId {
    let ty = ty.clone();
    lo.glue_function(
        Glue::Written(ty.clone()),
        writer_signature(),
        move |lo, fx| {
            let params = fx.entry();
            let report = Report { out: params[0] };
            let form = form(lo, &ty, &mut Vec::new()).expect("a writable type");
            write(lo, fx, report, &ty, &form, params[1]);
            fx.b.ins().return_(&[]);
        },
    )
}

/// How a report writes a value of a type whose parts it can all write, none otherwise.
fn form(lo: &mut Lowering, ty: &Ty, visiting: &mut Vec<Ty>) -> Option<Form> {
    let Ty::Item(item, args) = ty else {
        return match ty {
            Ty::Scalar(scalar) => Some(Form::Scalar(*scalar)),
            _ => None,
        };
    };
    if visiting.contains(ty) || lo.ck.is_capability(ty) {
        return None;
    }
    let arg = |index: usize| args.get(index).and_then(Arg::ty).cloned();
    let path = lo.ck.db.canonical(*item);
    match path.as_str() {
        "core::Str" | "text::String" => return Some(Form::Text { fixed: false }),
        "text::Fixed" => return Some(Form::Text { fixed: true }),
        "mem::Bytes" => return Some(Form::Bytes),
        "core::Untrusted" => return None,
        _ => {}
    }
    visiting.push(ty.clone());
    let parts = |lo: &mut Lowering, visiting: &mut Vec<Ty>, parts: &[Part]| {
        parts
            .iter()
            .all(|(_, part, _)| form(lo, part, visiting).is_some())
    };
    let found = match path.as_str() {
        "core::Array" => {
            let element = arg(0)?;
            form(lo, &element, visiting)?;
            Some(Form::Array(element, const_arg(&args[1]) as u64))
        }
        "core::Slice" | "core::SliceMut" => {
            let element = arg(0)?;
            form(lo, &element, visiting)?;
            Some(Form::Slice(element))
        }
        _ => {
            let sig = lo.ck.sig(*item);
            let subst = Subst::of(sig.generics.iter().map(|def| def.param), args);
            let name = path.rsplit("::").next().unwrap_or(&path).to_owned();
            match &sig.kind {
                SigKind::Struct { fields } => {
                    let fields: Vec<Part> = fields
                        .iter()
                        .map(|field| {
                            let offset = lo.layouts.field(lo.ck, ty, None, &field.name).offset;
                            (field.name.clone(), subst.ty(&field.ty), offset)
                        })
                        .collect();
                    parts(lo, visiting, &fields).then_some(Form::Struct(name, fields))
                }
                SigKind::Enum { .. } => {
                    let layout = lo.layout(ty);
                    let Shape::Enum(shape) = &layout.shape else {
                        visiting.pop();
                        return None;
                    };
                    let bare = matches!(path.as_str(), "core::Option" | "core::Result");
                    let variants: Vec<(String, Vec<Part>)> = shape
                        .variants
                        .iter()
                        .map(|variant| {
                            let written = match bare {
                                true => variant.name.clone(),
                                false => format!("{name}.{}", variant.name),
                            };
                            let fields = variant
                                .fields
                                .iter()
                                .map(|field| (field.name.clone(), field.ty.clone(), field.offset))
                                .collect();
                            (written, fields)
                        })
                        .collect();
                    variants
                        .iter()
                        .all(|(_, fields)| parts(lo, visiting, fields))
                        .then_some(Form::Enum(variants))
                }
                _ => None,
            }
        }
    };
    visiting.pop();
    found
}

/// Writes the value of a type at an address as its form says.
fn write(lo: &mut Lowering, fx: &mut Fx, report: Report, ty: &Ty, form: &Form, value: Value) {
    match form {
        Form::Scalar(scalar) => scalar_part(lo, fx, report, *scalar, value),
        Form::Text { fixed: false } | Form::Bytes => {
            let start = fx.load(PTR, value, 0);
            let len = fx.load(PTR, value, 8);
            let bytes = fx.ptr(u64::from(matches!(form, Form::Bytes)));
            lo.call_routine(fx, Routine::ReportText, &[report.out, start, len, bytes]);
        }
        Form::Text { fixed: true } => {
            let len_at = lo.layouts.field(lo.ck, ty, None, "len").offset;
            let bytes_at = lo.layouts.field(lo.ck, ty, None, "bytes").offset;
            let start = fx.offset(value, bytes_at);
            let len = fx.load(PTR, value, len_at);
            let text = fx.ptr(0);
            lo.call_routine(fx, Routine::ReportText, &[report.out, start, len, text]);
        }
        Form::Struct(name, fields) => {
            report.text(fx, lo, &format!("{name}("));
            labelled(lo, fx, report, fields, value);
            report.text(fx, lo, ")");
        }
        Form::Enum(variants) => {
            let index = lo.variant_index(fx, ty, value);
            for (position, (written, fields)) in variants.iter().enumerate() {
                let this = fx.b.ins().icmp_imm_s(IntCC::Equal, index, position as i64);
                fx.when(this, |fx| {
                    report.text(fx, lo, written);
                    if !fields.is_empty() {
                        report.text(fx, lo, "(");
                        labelled(lo, fx, report, fields, value);
                        report.text(fx, lo, ")");
                    }
                });
            }
        }
        Form::Array(element, count) => {
            let count = fx.ptr(*count);
            elements(lo, fx, report, element, value, count);
        }
        Form::Slice(element) => {
            let start = fx.load(PTR, value, 0);
            let len = fx.load(PTR, value, 8);
            elements(lo, fx, report, element, start, len);
        }
    }
}

/// Writes the fields of a construction or payload at an address, `name: ` before each named
/// one, separated by `, `.
fn labelled(lo: &mut Lowering, fx: &mut Fx, report: Report, fields: &[Part], value: Value) {
    for (index, (name, ty, offset)) in fields.iter().enumerate() {
        let separator = if index == 0 { "" } else { ", " };
        let label = match name.is_empty() {
            true => separator.to_owned(),
            false => format!("{separator}{name}: "),
        };
        if !label.is_empty() {
            report.text(fx, lo, &label);
        }
        let at = fx.offset(value, *offset);
        part(lo, fx, report, ty, at);
    }
}

/// Writes `count` elements from `start` as `[a, b]`, stopping once the report is full.
fn elements(
    lo: &mut Lowering,
    fx: &mut Fx,
    report: Report,
    element: &Ty,
    start: Value,
    count: Value,
) {
    report.text(fx, lo, "[");
    let stride = lo.layout(element).size;
    let any = fx.b.ins().icmp_imm_s(IntCC::NotEqual, count, 0);
    fx.when(any, |fx| {
        let zero = fx.ptr(0);
        fx.repeat(&[zero], |fx, state| {
            let index = state[0];
            let later = fx.b.ins().icmp_imm_s(IntCC::NotEqual, index, 0);
            fx.when(later, |fx| report.text(fx, lo, ", "));
            let offset = fx.b.ins().imul_imm_s(index, stride as i64);
            let at = fx.b.ins().iadd(start, offset);
            part(lo, fx, report, element, at);
            let next = fx.b.ins().iadd_imm_s(index, 1);
            let more = fx.b.ins().icmp(IntCC::UnsignedLessThan, next, count);
            let full = report.full(fx);
            let room = fx.b.ins().bxor_imm_s(full, 1);
            let more = fx.b.ins().band(more, room);
            (more, vec![next])
        });
    });
    report.text(fx, lo, "]");
}

/// Writes a part of a value at an address: a scalar in place, any other by its type's glue.
fn part(lo: &mut Lowering, fx: &mut Fx, report: Report, ty: &Ty, at: Value) {
    match ty {
        Ty::Scalar(scalar) => scalar_part(lo, fx, report, *scalar, at),
        _ => {
            let glue = writer(lo, ty);
            let reference = lo.func_ref(fx, glue);
            fx.b.ins().call(reference, &[report.out, at]);
        }
    }
}

/// Writes the scalar at an address.
fn scalar_part(lo: &mut Lowering, fx: &mut Fx, report: Report, scalar: Scalar, at: Value) {
    let value = fx.load(scalar_type(scalar), at, 0);
    let bits = bits(fx, scalar, value);
    let how = fx.ptr(written(&Ty::Scalar(scalar)) as u64);
    lo.call_routine(fx, Routine::ReportScalar, &[report.out, bits, how]);
}

/// The 64 bits a report reads a scalar's value from: a float's bit pattern, a signed
/// integer's value sign-extended and any other zero-extended.
pub fn bits(fx: &mut Fx, scalar: Scalar, value: Value) -> Value {
    match scalar {
        Scalar::F32 => {
            let raw = fx.b.ins().bitcast(types::I32, MemFlagsData::new(), value);
            fx.b.ins().uextend(types::I64, raw)
        }
        Scalar::F64 => fx.b.ins().bitcast(types::I64, MemFlagsData::new(), value),
        _ if scalar.is_signed() && scalar.bits() < 64 => fx.b.ins().sextend(types::I64, value),
        _ => super::super::super::body::widen(fx, value),
    }
}
