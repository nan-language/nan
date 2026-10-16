//! Host symbol names: the name of each lowered function in the object's symbol table, as the
//! specification composes it from the package, its code root prefix, the canonical path, the
//! role and the instantiation key; generated glue is named apart under `_NX`.

use super::{Instance, Lowering};
use crate::check::db::ItemId;
use crate::check::ty::{Arg, Args, Const, FnTy, Ty};
use crate::hash::{Hash, checksummed_base32};
use crate::nir::Role;

/// The symbol of an instance.
pub fn name(lo: &mut Lowering, instance: &Instance) -> String {
    match instance {
        Instance::Nir {
            item,
            role,
            index,
            args,
        } => item_symbol(lo, *item, *role, *index, args),
        Instance::Supplied { item, args } => item_symbol(lo, *item, Role::Body, 0, args),
        other => {
            lo.glue += 1;
            format!("_NX{}_{}", glue_word(other), lo.glue)
        }
    }
}

fn glue_word(instance: &Instance) -> &'static str {
    match instance {
        Instance::Derived { .. } => "derived",
        Instance::Conformance { .. } => "conformance",
        Instance::Release(_) => "release",
        Instance::Value(_) => "value",
        Instance::Dyn { .. } => "dyn",
        Instance::Sides(_) => "sides",
        Instance::Nir { .. } | Instance::Supplied { .. } => "item",
    }
}

/// The host symbol name of a NIR function of an item.
fn item_symbol(lo: &Lowering, item: ItemId, role: Role, index: u32, args: &Args) -> String {
    let path = lo
        .ck
        .canonical_path(item)
        .unwrap_or_else(|| lo.ck.db.canonical(item));
    let (package, rest) = path.split_once("::").unwrap_or((path.as_str(), ""));
    let prefix = lo
        .prefixes
        .get(package)
        .cloned()
        .unwrap_or_else(|| "aaaaaaaaaaaa".to_owned());
    let mut symbol = format!("_N{}{package}_{prefix}_{}E", package.len(), segments(rest));
    if role != Role::Body {
        let word = role_word(role);
        symbol.push_str(&format!("F{}{word}{index}", word.len()));
    }
    if !args.is_empty() {
        let key = instantiation_key(lo, item, args);
        symbol.push('_');
        symbol.push_str(&checksummed_base32(&key.0[1..])[..16]);
    }
    symbol
}

/// The segments of a canonical path after its package: each preceded by its length, a member
/// after `.` also by `D`, an impl segment as `I` and the nested forms of its type and interface.
fn segments(rest: &str) -> String {
    let mut out = String::new();
    let mut remaining = rest;
    while !remaining.is_empty() {
        if let Some(inside) = remaining.strip_prefix("impl[") {
            let close = matching_bracket(inside);
            let (target, interface) = inside[..close]
                .split_once(": ")
                .expect("an impl segment's type and interface");
            out.push('I');
            out.push_str(&nested(target));
            out.push_str(&nested(interface));
            remaining = inside[close + 1..].trim_start_matches("::");
            continue;
        }
        let member = remaining.starts_with('.');
        let body = remaining.trim_start_matches('.');
        let end = body.find(['.', ':']).unwrap_or(body.len());
        let segment = &body[..end];
        if member {
            out.push('D');
        }
        out.push_str(&format!("{}{segment}", segment.len()));
        remaining = body[end..].trim_start_matches("::");
    }
    out
}

fn matching_bracket(text: &str) -> usize {
    let mut depth = 0;
    for (at, c) in text.char_indices() {
        match c {
            '[' => depth += 1,
            ']' if depth == 0 => return at,
            ']' => depth -= 1,
            _ => {}
        }
    }
    text.len()
}

/// The nested form of a canonical path: `N`, its package and later segments, `E`.
fn nested(path: &str) -> String {
    let (package, rest) = path.split_once("::").unwrap_or((path, ""));
    format!("N{}{package}{}E", package.len(), segments(rest))
}

fn role_word(role: Role) -> &'static str {
    match role {
        Role::Body => "body",
        Role::Entry => "entry",
        Role::Closure => "closure",
        Role::Requires => "requires",
        Role::Ensures => "ensures",
        Role::Old => "old",
        Role::Example => "example",
        Role::Decreases => "decreases",
        Role::Invariant => "invariant",
        Role::Assume => "assume",
        Role::Predicate => "predicate",
        Role::Where => "where",
        Role::Default => "default",
    }
}

/// The instantiation key of an item with its generic arguments.
fn instantiation_key(lo: &Lowering, item: ItemId, args: &Args) -> Hash {
    let mut text = format!("instance\n{}\n", lo.ck.hashes(item).item.full());
    for arg in args.iter() {
        text.push_str(&arg_line(lo, arg));
        text.push('\n');
    }
    Hash::of(text.as_bytes())
}

/// The line an instantiation key writes for a generic argument.
fn arg_line(lo: &Lowering, arg: &Arg) -> String {
    match arg {
        Arg::Const(Const::Int(value)) => value.to_string(),
        Arg::Const(Const::Bool(value)) => value.to_string(),
        Arg::Const(_) => "_".to_owned(),
        Arg::Ty(ty) => type_hash(lo, ty).full(),
    }
}

/// The type hash of a type.
fn type_hash(lo: &Lowering, ty: &Ty) -> Hash {
    match ty {
        Ty::Fn(fn_ty) => Hash::of(format!("type\n{}", fn_text(lo, fn_ty)).as_bytes()),
        Ty::Item(item, args) => {
            let name = match lo.ck.db.is_std(*item)
                && lo.ck.db.package(lo.ck.db.package_of(*item)).name == "core"
            {
                true => lo.ck.db.declaration(*item).name().unwrap_or_default(),
                false => lo.ck.hashes(*item).item.full(),
            };
            let mut text = format!("type\n{name}\n");
            for arg in args.iter() {
                text.push_str(&arg_line(lo, arg));
                text.push('\n');
            }
            Hash::of(text.as_bytes())
        }
        Ty::Dyn(bound) => {
            let mut text = format!("type\nDyn\n{}\n", lo.ck.hashes(bound.interface).item.full());
            for arg in bound.args.iter() {
                text.push_str(&arg_line(lo, arg));
                text.push('\n');
            }
            Hash::of(text.as_bytes())
        }
        other => Hash::of(format!("type\n{}\n", ty_text(lo, other)).as_bytes()),
    }
}

/// The normalised text of a type, its paths canonical.
pub fn ty_text(lo: &Lowering, ty: &Ty) -> String {
    match ty {
        Ty::Scalar(scalar) => scalar.name().to_owned(),
        Ty::Unit => "Unit".to_owned(),
        Ty::Never => "Never".to_owned(),
        Ty::Item(item, args) => {
            let path = lo.ck.db.canonical(*item);
            match args.is_empty() {
                true => path,
                false => {
                    let args: Vec<String> = args.iter().map(|arg| arg_text(lo, arg)).collect();
                    format!("{path}[{}]", args.join(", "))
                }
            }
        }
        Ty::Fn(fn_ty) => fn_text(lo, fn_ty),
        Ty::Dyn(bound) => format!("Dyn[{}]", lo.ck.db.canonical(bound.interface)),
        other => format!("{other:?}"),
    }
}

fn arg_text(lo: &Lowering, arg: &Arg) -> String {
    match arg {
        Arg::Ty(ty) => ty_text(lo, ty),
        Arg::Const(Const::Int(value)) => value.to_string(),
        Arg::Const(Const::Bool(value)) => value.to_string(),
        Arg::Const(_) => "_".to_owned(),
    }
}

fn fn_text(lo: &Lowering, fn_ty: &FnTy) -> String {
    let params: Vec<String> = fn_ty
        .params
        .iter()
        .map(|param| {
            let mode = match param.mode {
                crate::syntax::ast::Mode::Read => "",
                crate::syntax::ast::Mode::Mut => "mut ",
                crate::syntax::ast::Mode::Take => "take ",
            };
            format!("{mode}{}: {}", param.name, ty_text(lo, &param.ty))
        })
        .collect();
    let mut text = format!("fn({})", params.join(", "));
    if fn_ty.result != Ty::Unit {
        text.push_str(&format!(" -> {}", ty_text(lo, &fn_ty.result)));
    }
    if let Some(fails) = &fn_ty.fails {
        text.push_str(&format!(" fails {}", ty_text(lo, fails)));
    }
    if fn_ty.wait {
        text.push_str(" uses wait");
    }
    text
}
