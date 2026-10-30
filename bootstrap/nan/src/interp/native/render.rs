//! Rendering in interpreter runs: renderers, the `text::fmt` template, nested renderings, and
//! the renderings of derived and built-in `Render` conformances. A renderer holds a reference to
//! its state, the bytes written so far and the render budget.

use std::rc::Rc;

use super::super::value::{DynVal, Loc, Val};
use super::super::{End, Exec, Flow, Interpreter, Site};
use super::stores::{entries, place};
use crate::check::db::ItemId;
use crate::check::sig::{Payload, SigKind};
use crate::check::ty::{Arg, Scalar, Subst, Ty};
use crate::nir::{Builtin, Callable, FnRef, Span};
use crate::syntax::lexer::is_rejected;

/// The positions of a renderer state's parts: the bytes written and the render budget.
const WRITTEN: usize = 0;
const BUDGET: usize = 1;

/// The intrinsic rendering a template.
const TEMPLATE_RENDER: &str = "text::fmt.render";

/// `text::fmt`, or the rendering of the template it returns; none for another intrinsic.
pub fn intrinsic(
    interp: &mut Interpreter,
    path: &str,
    type_args: &[Arg],
    args: Vec<Val>,
    at: (ItemId, Span),
    site: Site,
) -> Option<Exec<Flow>> {
    match path {
        "text::fmt" => {
            let render = FnRef {
                callable: Callable::Builtin(Builtin::Intrinsic(TEMPLATE_RENDER)),
                args: type_args.to_vec().into(),
            };
            Some(Ok(Flow::Return(Val::Dyn(Rc::new(DynVal {
                value: Val::Array(Rc::new(args)),
                methods: vec![render],
            })))))
        }
        TEMPLATE_RENDER => {
            Some(template(interp, type_args, &args, at, site).map(|()| Flow::Return(Val::Unit)))
        }
        _ => None,
    }
}

/// Writes a template's text between holes and each hole's argument through its `Render`.
fn template(
    interp: &mut Interpreter,
    types: &[Arg],
    args: &[Val],
    at: (ItemId, Span),
    site: Site,
) -> Exec<()> {
    let parts = interp.deref(&args[0]).fields().to_vec();
    let out = &args[1];
    let text = String::from_utf8(interp.bytes(&parts[0])).expect("a template's text");
    let mut chars = text.chars().peekable();
    let mut literal = String::new();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                literal.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                literal.push('}');
            }
            '{' => {
                write(interp, out, literal.as_bytes());
                literal.clear();
                let name: String = chars.by_ref().take_while(|c| *c != '}').collect();
                let index = (1..parts.len())
                    .step_by(2)
                    .find(|index| interp.bytes(&parts[*index]) == name.as_bytes())
                    .expect("a hole's argument");
                let ty = types[index + 1].ty().cloned().expect("an argument's type");
                render(interp, &parts[index + 1], &ty, out, at, site)?;
            }
            c => literal.push(c),
        }
    }
    write(interp, out, literal.as_bytes());
    Ok(())
}

/// A member of `core::Renderer`; none for another member.
pub fn renderer(
    interp: &mut Interpreter,
    member: &str,
    type_args: &[Arg],
    args: &[Val],
    at: (ItemId, Span),
    site: Site,
) -> Option<Exec<Flow>> {
    let value = match member {
        "write_nested" => {
            let ty = type_args.first().and_then(Arg::ty).cloned()?;
            return Some(
                nested(interp, &args[1], &ty, &args[0], at, site).map(|()| Flow::Return(Val::Unit)),
            );
        }
        "write" => {
            let text = interp.bytes(&args[1]);
            write(interp, &args[0], &text);
            Val::Unit
        }
        "write_char" => {
            let Val::Char(c) = interp.deref(&args[1]) else {
                panic!("a char value");
            };
            write(interp, &args[0], c.to_string().as_bytes());
            Val::Unit
        }
        "remaining" | "is_elided" => {
            let state = state(interp, &args[0]);
            let written = interp
                .memory
                .get(&state.with(WRITTEN as u32))
                .fields()
                .len() as i128;
            let budget = interp.memory.get(&state.with(BUDGET as u32)).int();
            match member {
                "remaining" => Val::Int((budget - written).max(0)),
                _ => Val::Bool(written > budget),
            }
        }
        _ => return None,
    };
    Some(Ok(Flow::Return(value)))
}

/// The output of rendering a `Dyn[Render]` with a render budget: the full rendering when it
/// fits, otherwise its longest prefix ending at a char boundary that leaves room for `...`, cut
/// to the budget.
pub fn output(
    interp: &mut Interpreter,
    value: &Val,
    budget: usize,
    at: (ItemId, Span),
    site: Site,
) -> Exec<Vec<u8>> {
    let state = Val::Array(Rc::new(vec![
        Val::Array(Rc::new(Vec::new())),
        Val::Int(budget as i128),
    ]));
    let state = reference(interp.memory.alloc(state, true, 0));
    let item = interp.ty_item("core", "Renderer");
    let renderer = Val::Struct(item, Rc::new(vec![state.clone()]));
    let out = reference(interp.memory.alloc(renderer, true, 0));
    let Val::Dyn(dynamic) = interp.deref(value) else {
        panic!("a Dyn[Render] value");
    };
    match interp.call_function(
        &dynamic.methods[0],
        vec![dynamic.value.clone(), out],
        at,
        site,
    )? {
        Flow::Return(_) => {}
        _ => panic!("a rendering ending otherwise than by returning"),
    }
    let rendered: Vec<u8> = interp
        .memory
        .get(&place(&state).with(WRITTEN as u32))
        .fields()
        .iter()
        .map(|byte| byte.int() as u8)
        .collect();
    if rendered.len() <= budget {
        return Ok(rendered);
    }
    let dots = &b"..."[..3.min(budget)];
    let mut kept = budget - dots.len();
    while kept > 0 && rendered[kept] & 0xC0 == 0x80 {
        kept -= 1;
    }
    let mut output = rendered[..kept].to_vec();
    output.extend_from_slice(dots);
    Ok(output)
}

/// Renders a value of a type through its `Render`.
pub fn render(
    interp: &mut Interpreter,
    value: &Val,
    ty: &Ty,
    out: &Val,
    at: (ItemId, Span),
    site: Site,
) -> Exec<()> {
    let flow = match ty {
        Ty::Dyn(_) => {
            let Val::Dyn(dynamic) = interp.deref(value) else {
                panic!("a Dyn value");
            };
            let args = vec![dynamic.value.clone(), out.clone()];
            interp.call_function(&dynamic.methods[0], args, at, site)?
        }
        _ => {
            let interface = interp.ty_item("core", "Render");
            let fn_ref = FnRef {
                callable: Callable::Method {
                    interface,
                    method: 0,
                },
                args: Rc::from(vec![Arg::Ty(ty.clone())]),
            };
            interp.call_function(&fn_ref, vec![value.clone(), out.clone()], at, site)?
        }
    };
    match flow {
        Flow::Return(_) => Ok(()),
        _ => panic!("a rendering ending otherwise than by returning"),
    }
}

/// Writes the nested rendering of a value of a type: a char or text as its literal, a box's or
/// shared value's contents nested, anything else through its `Render`.
fn nested(
    interp: &mut Interpreter,
    value: &Val,
    ty: &Ty,
    out: &Val,
    at: (ItemId, Span),
    site: Site,
) -> Exec<()> {
    let inner = interp.deref(value);
    match ty {
        Ty::Scalar(Scalar::Char) => {
            let Val::Char(c) = inner else {
                panic!("a char value");
            };
            write(interp, out, literal(&c.to_string(), '\'').as_bytes());
        }
        Ty::Item(item, args) => match interp.ck.db.canonical(*item).as_str() {
            path @ ("core::Str" | "text::String" | "text::Fixed") => {
                let bytes = text_bytes(interp, &inner, path);
                let text = String::from_utf8(bytes).expect("well-formed text");
                write(interp, out, literal(&text, '"').as_bytes());
            }
            "mem::Box" | "mem::Shared" => {
                let contents = interp.deref(&inner.fields()[0]);
                let element = args.first().and_then(Arg::ty).cloned().unwrap_or(Ty::Unit);
                nested(interp, &contents, &element, out, at, site)?;
            }
            _ => render(interp, value, ty, out, at, site)?,
        },
        _ => render(interp, value, ty, out, at, site)?,
    }
    Ok(())
}

/// A text as a char or text literal quoted by `quote`: `\`, the quote and the named escapes
/// escaped, and every other code point outside `CHARACTER` written `\u{H}`.
fn literal(text: &str, quote: char) -> String {
    let mut out = String::from(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if is_rejected(c) => out.push_str(&format!("\\u{{{:X}}}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// The bytes of a `Str`, `text::String` or `text::Fixed` value.
fn text_bytes(interp: &mut Interpreter, value: &Val, path: &str) -> Vec<u8> {
    match path {
        "text::Fixed" => {
            let len = value.fields()[1].int() as usize;
            value.fields()[0].fields()[..len]
                .iter()
                .map(|byte| byte.int() as u8)
                .collect()
        }
        "text::String" => entries(interp, value)
            .iter()
            .map(|byte| byte.int() as u8)
            .collect(),
        _ => interp.bytes(value),
    }
}

/// The rendering a derive writes: a struct's name and fields, or an enum's name, variant and
/// payload, the variants of `Option` and `Result` without the name.
pub fn derived(
    interp: &mut Interpreter,
    owner: ItemId,
    type_args: &[Arg],
    args: &[Val],
    at: (ItemId, Span),
    site: Site,
) -> Exec<()> {
    let value = interp.deref(&args[0]);
    let out = &args[1];
    let sig = interp.ck.sig(owner);
    let subst = Subst::of(sig.generics.iter().map(|def| def.param), type_args);
    let name = interp.ck.db.canonical(owner);
    let name = name.rsplit("::").next().expect("a name").to_owned();
    match &sig.kind {
        SigKind::Struct { fields } => {
            write(interp, out, format!("{name}(").as_bytes());
            let labelled: Vec<(String, Ty)> = fields
                .iter()
                .map(|field| (field.name.clone(), subst.ty(&field.ty)))
                .collect();
            labelled_values(interp, &labelled, value.fields(), out, at, site)?;
            write(interp, out, b")");
        }
        SigKind::Enum { variants } => {
            let Val::Variant(_, index, payload) = &value else {
                panic!("an enum value");
            };
            let variant = &variants[*index as usize];
            let bare = matches!(name.as_str(), "Option" | "Result") && interp.ck.db.is_std(owner);
            let named = match bare {
                true => variant.name.clone(),
                false => format!("{name}.{}", variant.name),
            };
            write(interp, out, named.as_bytes());
            match &variant.payload {
                Payload::None => {}
                Payload::Single(ty) => {
                    write(interp, out, b"(");
                    nested(interp, &payload[0], &subst.ty(ty), out, at, site)?;
                    write(interp, out, b")");
                }
                Payload::Labelled(labels) => {
                    write(interp, out, b"(");
                    let labelled: Vec<(String, Ty)> = labels
                        .iter()
                        .map(|(label, ty)| (label.clone(), subst.ty(ty)))
                        .collect();
                    labelled_values(interp, &labelled, payload, out, at, site)?;
                    write(interp, out, b")");
                }
            }
        }
        other => panic!("a derived Render of {other:?}"),
    }
    Ok(())
}

/// Renders values with their labels, `label: value`, separated by `, `.
fn labelled_values(
    interp: &mut Interpreter,
    labelled: &[(String, Ty)],
    values: &[Val],
    out: &Val,
    at: (ItemId, Span),
    site: Site,
) -> Exec<()> {
    for (index, ((label, ty), value)) in labelled.iter().zip(values).enumerate() {
        if index > 0 {
            write(interp, out, b", ");
        }
        write(interp, out, format!("{label}: ").as_bytes());
        nested(interp, value, ty, out, at, site)?;
    }
    Ok(())
}

/// The rendering of a type whose `Render` the toolchain supplies.
pub fn builtin(
    interp: &mut Interpreter,
    ty: &Ty,
    args: &[Val],
    at: (ItemId, Span),
    site: Site,
) -> Exec<()> {
    let value = interp.deref(&args[0]);
    let out = &args[1];
    let arg = |args: &[Arg], index: usize| {
        args.get(index)
            .and_then(Arg::ty)
            .cloned()
            .unwrap_or(Ty::Unit)
    };
    match ty {
        Ty::Scalar(scalar) => {
            let text = match (scalar, &value) {
                (Scalar::Bool, _) => value.bool().to_string(),
                (Scalar::Char, Val::Char(c)) => c.to_string(),
                (_, Val::F32(float)) => float_text(f64::from(*float), Some(*float)),
                (_, Val::F64(float)) => float_text(*float, None),
                _ => value.int().to_string(),
            };
            write(interp, out, text.as_bytes());
        }
        Ty::Unit => write(interp, out, b"Unit"),
        Ty::Item(item, args) => match interp.ck.db.canonical(*item).as_str() {
            path @ ("core::Str" | "text::String" | "text::Fixed") => {
                let bytes = text_bytes(interp, &value, path);
                write(interp, out, &bytes);
            }
            "core::Untrusted" => write(interp, out, b"<untrusted>"),
            "mem::Bytes" => {
                let mut text = String::from("b\"");
                for byte in entries(interp, &value) {
                    match byte.int() as u8 {
                        b'"' => text.push_str("\\\""),
                        b'\\' => text.push_str("\\\\"),
                        b'\n' => text.push_str("\\n"),
                        b'\r' => text.push_str("\\r"),
                        b'\t' => text.push_str("\\t"),
                        0 => text.push_str("\\0"),
                        byte @ 0x20..=0x7E => text.push(byte as char),
                        byte => text.push_str(&format!("\\x{byte:02X}")),
                    }
                }
                text.push('"');
                write(interp, out, text.as_bytes());
            }
            "mem::Box" | "mem::Shared" => {
                let contents = interp.deref(&value.fields()[0]);
                render(interp, &contents, &arg(args, 0), out, at, site)?;
            }
            "mem::Map" | "mem::HashMap" => {
                write(interp, out, b"{");
                for (index, entry) in entries(interp, &value).iter().enumerate() {
                    if index > 0 {
                        write(interp, out, b", ");
                    }
                    let parts = entry.fields();
                    nested(interp, &parts[0], &arg(args, 0), out, at, site)?;
                    write(interp, out, b": ");
                    nested(interp, &parts[1], &arg(args, 1), out, at, site)?;
                }
                write(interp, out, b"}");
            }
            path => {
                let elements = match path {
                    "core::Array" => value.fields().to_vec(),
                    "core::Slice" | "core::SliceMut" => {
                        let view = interp.view(&value);
                        interp.view_values(&view)
                    }
                    "mem::List" | "mem::Set" => entries(interp, &value),
                    "mem::Bounded" => value.fields()[0].fields().to_vec(),
                    _ => return Err(End::Missing),
                };
                write(interp, out, b"[");
                for (index, element) in elements.iter().enumerate() {
                    if index > 0 {
                        write(interp, out, b", ");
                    }
                    nested(interp, element, &arg(args, 0), out, at, site)?;
                }
                write(interp, out, b"]");
            }
        },
        _ => return Err(End::Missing),
    }
    Ok(())
}

/// A float as `Render` writes it: `NaN`, `inf`, `-inf`, or the shortest digits of its own
/// type, positional for a magnitude of 0 or in [10^-5, 10^16) and otherwise scientific.
pub fn float_text(value: f64, narrow: Option<f32>) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf" } else { "-inf" }.to_owned();
    }
    let scientific = match narrow {
        Some(narrow) => format!("{:e}", narrow.abs()),
        None => format!("{:e}", value.abs()),
    };
    let (mantissa, exponent) = scientific.split_once('e').expect("an exponent");
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let exponent: i32 = exponent.parse().expect("an exponent");
    let sign = if value.is_sign_negative() { "-" } else { "" };
    let magnitude = value.abs();
    if magnitude == 0.0 || (1e-5..1e16).contains(&magnitude) {
        let body = if exponent >= 0 {
            let whole = exponent as usize + 1;
            let mut integer = digits.clone();
            while integer.len() < whole {
                integer.push('0');
            }
            let fraction = &integer[whole..];
            let fraction = if fraction.is_empty() { "0" } else { fraction };
            format!("{}.{fraction}", &integer[..whole])
        } else {
            format!("0.{}{digits}", "0".repeat((-exponent - 1) as usize))
        };
        return format!("{sign}{body}");
    }
    let fraction = if digits.len() > 1 { &digits[1..] } else { "0" };
    format!("{sign}{}.{fraction}e{exponent}", &digits[..1])
}

/// Appends bytes to what a renderer has written.
fn write(interp: &mut Interpreter, out: &Val, bytes: &[u8]) {
    let written = state(interp, out).with(WRITTEN as u32);
    let mut held = interp.memory.get(&written).fields().to_vec();
    held.extend(bytes.iter().map(|byte| Val::Int(i128::from(*byte))));
    interp.memory.set(&written, Val::Array(Rc::new(held)));
}

/// The place of a renderer's state.
fn state(interp: &Interpreter, out: &Val) -> Loc {
    place(&interp.deref(out).fields()[0])
}

fn reference(extent: u32) -> Val {
    Val::Ref(Loc {
        extent,
        path: Vec::new(),
    })
}

#[cfg(test)]
mod tests;
