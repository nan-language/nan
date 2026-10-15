//! The std operations the interpreter supplies: named and bit conversions, the members of
//! scalars, ranges, slices, cursors, `Option`, `Untrusted` and texts, the std containers, the
//! testing world, the conformances of scalars and the methods derives write.

mod numbers;
mod pointers;
mod stores;

use std::cmp::Ordering;
use std::rc::Rc;

pub use numbers::{builtin, canonical};

use super::value::{DynVal, Loc, Val, View};
use super::{End, Exec, Flow, Interpreter, Site};
use crate::check::db::ItemId;
use crate::check::sig::SigKind;
use crate::check::ty::{Arg, Scalar, Ty};
use crate::nir::Span;

/// A std function without a body, called by its canonical path; a run reaching one the
/// interpreter does not supply ends missing.
pub fn call(
    interp: &mut Interpreter,
    item: ItemId,
    type_args: &[Arg],
    args: Vec<Val>,
    at: (ItemId, Span),
    site: Site,
) -> Exec<Flow> {
    let path = interp.ck.db.canonical(item);
    let stated = crate::check::stated(&path);
    if stated.is_some_and(|facts| facts.device || facts.privileged) {
        return Err(interp.uninterpretable(at));
    }
    let (owner, member) = path.rsplit_once('.').unwrap_or((path.as_str(), ""));
    if let Some(scalar) = owner.strip_prefix("core::").and_then(Scalar::from_name) {
        return numbers::scalar_member(interp, scalar, member, &args, at);
    }
    if owner == "hw::Ptr"
        && let Some(flow) = pointers::member(interp, member, type_args, &args, at)
    {
        return flow;
    }
    if let Some(flow) = stores::member(interp, owner, member, type_args, &args, at) {
        return flow;
    }
    let option_item = interp.ty_item("core", "Option");
    let option = || option_item;
    let value = match (owner, member) {
        ("core::Range", "next") => range_next(interp, &args[0]),
        ("core::Range", "contains") => {
            let range = interp.deref(&args[0]);
            let fields = range.fields();
            let value = args[1].int();
            let (low, high, inclusive) = (fields[0].int(), fields[1].int(), fields[2].bool());
            Val::Bool(low <= value && (value < high || (inclusive && value <= high)))
        }
        ("core::Range", "is_empty") => {
            let range = interp.deref(&args[0]);
            let fields = range.fields();
            let (low, high, inclusive) = (fields[0].int(), fields[1].int(), fields[2].bool());
            Val::Bool(high < low || (!inclusive && high <= low))
        }
        ("core::Slice" | "core::SliceMut" | "mem::List" | "mem::Bytes", "len")
        | ("core::Str", "byte_len") => Val::Int(interp.view(&args[0]).len as i128),
        (
            "core::Slice" | "core::SliceMut" | "core::Array" | "mem::List" | "mem::Bytes",
            "at" | "at_mut",
        ) => {
            let view = interp.view(&args[0]);
            let index = args[1].int();
            if index < 0 || index as usize >= view.len {
                return Err(interp.bounds_trap(site, at));
            }
            Val::Ref(view.array.with((view.start + index as usize) as u32))
        }
        (
            "core::Slice" | "core::SliceMut" | "core::Array" | "core::Str" | "mem::List"
            | "mem::Bytes",
            "cursor" | "cursor_mut",
        ) => {
            let cursor = interp.ty_item("core", "SliceCursor");
            Val::Struct(cursor, Rc::new(vec![Val::View(interp.view(&args[0]))]))
        }
        ("core::SliceCursor" | "core::SliceCursorMut", "next") => slice_next(interp, &args[0]),
        ("core::Array" | "mem::List" | "mem::Bytes", "view" | "view_mut")
        | ("core::SliceMut", "view")
        | ("core::Str", "as_bytes") => Val::View(interp.view(&args[0])),
        ("core::Slice" | "core::SliceMut", "prefix" | "prefix_mut") => {
            let view = interp.view(&args[0]);
            let count = args[1].int() as usize;
            match count <= view.len {
                true => Val::some(option(), Val::View(View { len: count, ..view })),
                false => Val::none(option()),
            }
        }
        ("core::Slice" | "core::SliceMut", "split_at" | "split_at_mut") => {
            let view = interp.view(&args[0]);
            match split(interp, &view, args[1].int() as usize) {
                Some(parts) => Val::some(option(), parts),
                None => Val::none(option()),
            }
        }
        ("core::Slice" | "core::SliceMut", reader) if integer_access(reader).is_some() => {
            let (width, big) = integer_access(reader).expect("an integer reader");
            let view = interp.view(&args[0]);
            let at = args[1].int() as usize;
            let bytes = interp.view_values(&View {
                start: view.start + at,
                len: width,
                ..view
            });
            let ordered: Box<dyn Iterator<Item = &Val>> = match big {
                true => Box::new(bytes.iter()),
                false => Box::new(bytes.iter().rev()),
            };
            Val::Int(ordered.fold(0, |value, byte| (value << 8) | byte.int()))
        }
        ("core::SliceMut", writer)
            if writer
                .strip_prefix("write_")
                .and_then(integer_access)
                .is_some() =>
        {
            let (width, big) = writer
                .strip_prefix("write_")
                .and_then(integer_access)
                .expect("an integer writer");
            let view = interp.view(&args[0]);
            let at = args[1].int() as usize;
            let value = args[2].int();
            for index in 0..width {
                let shift = match big {
                    true => 8 * (width - 1 - index),
                    false => 8 * index,
                };
                let byte = Val::Int((value >> shift) & 0xFF);
                interp
                    .memory
                    .set(&view.array.with((view.start + at + index) as u32), byte);
            }
            Val::Unit
        }
        ("core::Str", "is_empty") => Val::Bool(interp.view(&args[0]).len == 0),
        ("core::Str", "slice_bytes") => {
            let view = interp.view(&args[0]);
            let bytes = interp.bytes(&args[0]);
            let (start, end) = (args[1].int() as usize, args[2].int() as usize);
            let boundary =
                |at: usize| at == bytes.len() || (at < bytes.len() && bytes[at] & 0xC0 != 0x80);
            match start <= end && end <= bytes.len() && boundary(start) && boundary(end) {
                true => Val::some(
                    option(),
                    Val::View(View {
                        start: view.start + start,
                        len: end - start,
                        ..view
                    }),
                ),
                false => Val::none(option()),
            }
        }
        ("core::Str", "starts_with" | "ends_with" | "find") => {
            let text = interp.bytes(&args[0]);
            let needle = interp.bytes(&args[1]);
            match member {
                "starts_with" => Val::Bool(text.starts_with(&needle)),
                "ends_with" => Val::Bool(text.ends_with(&needle)),
                _ => {
                    let found = (needle.len() <= text.len())
                        .then(|| {
                            (0..=text.len() - needle.len())
                                .find(|start| text[*start..].starts_with(&needle))
                        })
                        .flatten();
                    match found {
                        Some(start) => Val::some(option(), Val::Int(start as i128)),
                        None => Val::none(option()),
                    }
                }
            }
        }
        ("core::Option", "value_or") => match interp.deref(&args[0]) {
            Val::Variant(_, 0, payload) => payload[0].clone(),
            _ => args[1].clone(),
        },
        ("core::Option" | "core::Result", "is_some" | "is_ok") => {
            Val::Bool(matches!(interp.deref(&args[0]), Val::Variant(_, 0, _)))
        }
        ("core::Option" | "core::Result", "is_none" | "is_err") => {
            Val::Bool(matches!(interp.deref(&args[0]), Val::Variant(_, 1, _)))
        }
        ("core::TotalF64" | "core::TotalF32", "eq" | "cmp") => {
            let (left, right) = (interp.deref(&args[0]), interp.deref(&args[1]));
            let (left, right) = (&left.fields()[0], &right.fields()[0]);
            let order = interp.order(left, right);
            match member {
                "eq" => Val::Bool(order == Ordering::Equal),
                _ => ordering_value(interp.ty_item("core", "Ordering"), order),
            }
        }
        ("core::Untrusted", "of") => {
            let untrusted = interp.ty_item("core", "Untrusted");
            Val::Struct(untrusted, Rc::new(vec![args[0].clone()]))
        }
        ("core::Untrusted", "inspect" | "into_cleared") => {
            interp.deref(&args[0]).fields()[0].clone()
        }
        ("core::Untrusted", "byte_len") => {
            let inner = interp.deref(&args[0]).fields()[0].clone();
            Val::Int(interp.view(&inner).len as i128)
        }
        ("core::Untrusted", "view" | "view_mut") => {
            let inner = interp.deref(&args[0]).fields()[0].clone();
            let untrusted = interp.ty_item("core", "Untrusted");
            Val::Struct(untrusted, Rc::new(vec![Val::View(interp.view(&inner))]))
        }
        ("core::Untrusted", "prefix" | "split_at") => {
            let inner = interp.deref(&args[0]).fields()[0].clone();
            let view = interp.view(&inner);
            let at = args[1].int() as usize;
            let untrusted = interp.ty_item("core", "Untrusted");
            let wrap = |view: View| Val::Struct(untrusted, Rc::new(vec![Val::View(view)]));
            match (member, at <= view.len) {
                (_, false) => Val::none(option()),
                ("prefix", true) => Val::some(option(), wrap(View { len: at, ..view })),
                _ => {
                    let parts = interp.ty_item("core", "Parts");
                    let head = wrap(View {
                        len: at,
                        ..view.clone()
                    });
                    let tail = wrap(View {
                        start: view.start + at,
                        len: view.len - at,
                        ..view
                    });
                    Val::some(option(), Val::Struct(parts, Rc::new(vec![head, tail])))
                }
            }
        }
        ("core::Array", "filled") => {
            let count = match type_args.get(1) {
                Some(Arg::Const(crate::check::ty::Const::Int(count))) => *count as usize,
                _ => panic!("an array length"),
            };
            Val::Array(Rc::new(vec![args[0].clone(); count]))
        }
        ("testing::world", "") => interp.world(),
        _ => return Err(End::Missing),
    };
    Ok(Flow::Return(value))
}

/// The width in bytes and byte order, most significant first when `true`, of an integer reader
/// such as `u32_le`.
fn integer_access(name: &str) -> Option<(usize, bool)> {
    let (width, order) = name.split_once('_')?;
    let width = match width {
        "u16" => 2,
        "u32" => 4,
        "u64" => 8,
        _ => return None,
    };
    match order {
        "le" => Some((width, false)),
        "be" => Some((width, true)),
        _ => None,
    }
}

/// The parts of a view split at an index, none past its end.
fn split(interp: &Interpreter, view: &View, at: usize) -> Option<Val> {
    if at > view.len {
        return None;
    }
    let parts = interp.ty_item("core", "Parts");
    let head = View {
        len: at,
        ..view.clone()
    };
    let tail = View {
        start: view.start + at,
        len: view.len - at,
        ..view.clone()
    };
    Some(Val::Struct(
        parts,
        Rc::new(vec![Val::View(head), Val::View(tail)]),
    ))
}

/// `core::Range.next`: the low bound, then the range past it.
fn range_next(interp: &mut Interpreter, cursor: &Val) -> Val {
    let Val::Ref(loc) = cursor else {
        panic!("a range passed by mut");
    };
    let range = interp.memory.get(loc).clone();
    let Val::Struct(item, fields) = &range else {
        panic!("a range value");
    };
    let (low, high, inclusive) = (fields[0].int(), fields[1].int(), fields[2].bool());
    let option = interp.ck.db.core_item("Option").expect("core::Option");
    let empty = high < low || (!inclusive && high <= low);
    if empty {
        return Val::none(option);
    }
    let element = |value: i128| match &fields[0] {
        Val::Char(_) => Val::Char(char::from_u32(value as u32).unwrap_or_default()),
        _ => Val::Int(value),
    };
    let next = if low < high {
        vec![element(low + 1), fields[1].clone(), fields[2].clone()]
    } else {
        vec![fields[0].clone(), fields[1].clone(), Val::Bool(false)]
    };
    interp.memory.set(loc, Val::Struct(*item, Rc::new(next)));
    Val::some(option, element(low))
}

/// The `next` of a slice cursor: a reference to its first element, then the rest.
fn slice_next(interp: &mut Interpreter, cursor: &Val) -> Val {
    let Val::Ref(loc) = cursor else {
        panic!("a cursor passed by mut");
    };
    let state = interp.memory.get(loc).clone();
    let Val::Struct(item, fields) = &state else {
        panic!("a slice cursor value");
    };
    let Val::View(view) = &fields[0] else {
        panic!("a slice cursor's view");
    };
    let option = interp.ck.db.core_item("Option").expect("core::Option");
    if view.len == 0 {
        return Val::none(option);
    }
    let element = view.array.with(view.start as u32);
    let rest = View {
        array: view.array.clone(),
        start: view.start + 1,
        len: view.len - 1,
    };
    interp
        .memory
        .set(loc, Val::Struct(*item, Rc::new(vec![Val::View(rest)])));
    Val::some(option, Val::Ref(element))
}

/// A method of an interface a type implements through a conformance the toolchain supplies;
/// a run reaching one the interpreter does not supply ends missing.
pub fn conformance(interp: &mut Interpreter, ty: &Ty, method: &str, args: Vec<Val>) -> Exec<Flow> {
    let values: Vec<Val> = args.iter().map(|arg| interp.deref(arg)).collect();
    let value = match method {
        "eq" => Val::Bool(interp.equal(&values[0], &values[1])),
        "cmp" => {
            let ordering = interp.ty_item("core", "Ordering");
            ordering_value(ordering, interp.order(&values[0], &values[1]))
        }
        "default" => default_of(ty),
        _ => return Err(End::Missing),
    };
    Ok(Flow::Return(value))
}

/// The default of a scalar type.
fn default_of(ty: &Ty) -> Val {
    match ty {
        Ty::Scalar(Scalar::Bool) => Val::Bool(false),
        Ty::Scalar(Scalar::Char) => Val::Char('\0'),
        Ty::Scalar(Scalar::F32) => Val::F32(0.0),
        Ty::Scalar(Scalar::F64) => Val::F64(0.0),
        Ty::Scalar(_) => Val::Int(0),
        _ => Val::Unit,
    }
}

pub fn ordering_value(item: ItemId, order: Ordering) -> Val {
    let index = match order {
        Ordering::Less => 0,
        Ordering::Equal => 1,
        Ordering::Greater => 2,
    };
    Val::Variant(item, index, Rc::new(Vec::new()))
}

/// A method a derive writes for a type: equality and order over its fields or variants in
/// declaration order, and the default of a struct of scalars; a run reaching another ends
/// missing.
pub fn derived(
    interp: &mut Interpreter,
    owner: ItemId,
    type_args: &[Arg],
    method: &str,
    args: Vec<Val>,
) -> Exec<Flow> {
    let values: Vec<Val> = args.iter().map(|arg| interp.deref(arg)).collect();
    let value = match method {
        "eq" => Val::Bool(interp.equal(&values[0], &values[1])),
        "cmp" => {
            let ordering = interp.ty_item("core", "Ordering");
            ordering_value(ordering, interp.order(&values[0], &values[1]))
        }
        "default" => {
            let sig = interp.ck.sig(owner);
            let SigKind::Struct { fields } = &sig.kind else {
                return Err(End::Missing);
            };
            let subst =
                crate::check::ty::Subst::of(sig.generics.iter().map(|def| def.param), type_args);
            let mut values = Vec::new();
            for field in fields {
                let ty = subst.ty(&field.ty);
                match ty {
                    Ty::Scalar(_) => values.push(default_of(&ty)),
                    _ => return Err(End::Missing),
                }
            }
            Val::Struct(owner, Rc::new(values))
        }
        _ => return Err(End::Missing),
    };
    Ok(Flow::Return(value))
}

/// An intrinsic the toolchain supplies: a `Dyn` view. A run reaching `text::fmt` ends missing,
/// and the others bring a fact that makes them uninterpretable.
pub fn intrinsic(
    interp: &mut Interpreter,
    path: &str,
    type_args: &[Arg],
    args: Vec<Val>,
    at: (ItemId, Span),
) -> Exec<Flow> {
    match path {
        "Dyn.view" => dyn_view(interp, type_args, &args[0])
            .map(Flow::Return)
            .ok_or(End::Missing),
        "text::fmt" => Err(End::Missing),
        _ => Err(interp.uninterpretable(at)),
    }
}

/// A `Dyn[I]` view of a value of type `T`, its type arguments `Dyn[I]` and `T`: the value with
/// the function implementing each of `I`'s methods for `T`.
fn dyn_view(interp: &Interpreter, type_args: &[Arg], value: &Val) -> Option<Val> {
    let (Some(Ty::Dyn(bound)), Some(source)) = (type_args[0].ty(), type_args[1].ty()) else {
        return None;
    };
    let SigKind::Interface { methods } = &interp.ck.sig(bound.interface).kind else {
        return None;
    };
    let methods = methods
        .iter()
        .map(|method| {
            interp
                .ck
                .implementation(source, bound.interface, &method.name)
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Val::Dyn(Rc::new(DynVal {
        value: value.clone(),
        methods,
    })))
}

/// Releases the memory a value's std containers own.
pub fn release(interp: &mut Interpreter, value: &Val) {
    if let Val::Struct(item, _) = value {
        let path = interp.ck.db.canonical(*item);
        if matches!(path.as_str(), "mem::List" | "mem::Bytes" | "mem::Pool") {
            stores::release(interp, value);
        }
    }
}

impl Interpreter<'_, '_> {
    /// The value a reference designates, or the value itself.
    pub(super) fn deref(&self, value: &Val) -> Val {
        match value {
            Val::Ref(loc) => self.deref(&self.memory.get(loc).clone()),
            other => other.clone(),
        }
    }

    /// The elements a slice, text, array, container or cursor value views.
    fn view(&mut self, value: &Val) -> View {
        match value {
            Val::Ref(loc) => match self.memory.get(loc).clone() {
                Val::Array(elements) => View {
                    array: loc.clone(),
                    start: 0,
                    len: elements.len(),
                },
                other => self.view(&other),
            },
            Val::View(view) => view.clone(),
            Val::Struct(_, fields) => match fields.first() {
                Some(Val::Ref(loc)) => {
                    let len = self.memory.get(loc).fields().len();
                    View {
                        array: loc.clone(),
                        start: 0,
                        len,
                    }
                }
                Some(other) => self.view(&other.clone()),
                None => panic!("a view of an empty struct"),
            },
            Val::Array(elements) => {
                let len = elements.len();
                let extent = self.memory.alloc(value.clone(), false, len as u64);
                View {
                    array: Loc {
                        extent,
                        path: Vec::new(),
                    },
                    start: 0,
                    len,
                }
            }
            other => panic!("a view of {other:?}"),
        }
    }

    /// The bytes a text or byte-string value views.
    fn bytes(&mut self, value: &Val) -> Vec<u8> {
        let view = self.view(value);
        self.view_values(&view)
            .iter()
            .map(|byte| byte.int() as u8)
            .collect()
    }

    /// The world `testing::world()` returns: its allocator names a new root cell, and each
    /// other capability is a value of its type.
    fn world(&mut self) -> Val {
        let world = self.ty_item("host", "World");
        let SigKind::Struct { fields } = &self.ck.sig(world).kind else {
            panic!("host::World is a struct");
        };
        let fields = fields.clone();
        let alloc = self.ty_item("mem", "Alloc");
        let cell = self.budgets.root(stores::WORLD_LIMIT);
        let values = fields
            .iter()
            .map(|field| match &field.ty {
                Ty::Item(item, _) if *item == alloc => stores::allocator(self, cell),
                Ty::Item(item, _) => Val::Struct(*item, Rc::new(Vec::new())),
                _ => Val::Unit,
            })
            .collect();
        Val::Struct(world, Rc::new(values))
    }

    /// A std type item, by its module and name.
    pub fn ty_item(&self, module: &str, name: &str) -> ItemId {
        self.ck
            .db
            .std_item(module, name)
            .or_else(|| {
                (module == "core")
                    .then(|| self.ck.db.core_item(name))
                    .flatten()
            })
            .unwrap_or_else(|| panic!("the std item {module}::{name}"))
    }

    fn uninterpretable(&self, at: (ItemId, Span)) -> End {
        End::Uninterpretable(self.report("uninterpretable", at))
    }

    fn bounds_trap(&self, site: Site, at: (ItemId, Span)) -> End {
        if self.stops_at(site, 1) {
            return End::Confirmed;
        }
        self.trap("trap.bounds", at)
    }

    fn conversion_trap(&self, site: Site, at: (ItemId, Span)) -> End {
        if self.stops_at(site, 1) {
            return End::Confirmed;
        }
        self.trap("trap.conversion", at)
    }

    /// Structural equality: of scalars by value, of texts and slices by their elements, of
    /// structs, enum values and arrays part by part.
    pub(super) fn equal(&self, left: &Val, right: &Val) -> bool {
        self.order(left, right) == Ordering::Equal
    }

    /// Structural order: scalars by value, enum values by variant then payload, structs and
    /// arrays part by part in order.
    pub(super) fn order(&self, left: &Val, right: &Val) -> Ordering {
        match (self.deref(left), self.deref(right)) {
            (Val::F32(a), Val::F32(b)) => a.total_cmp(&b),
            (Val::F64(a), Val::F64(b)) => a.total_cmp(&b),
            (Val::View(a), Val::View(b)) => {
                let (a, b) = (self.view_values(&a), self.view_values(&b));
                self.order_all(&a, &b)
            }
            (Val::Variant(_, a, payload_a), Val::Variant(_, b, payload_b)) => a
                .cmp(&b)
                .then_with(|| self.order_all(&payload_a, &payload_b)),
            (Val::Struct(_, a), Val::Struct(_, b)) | (Val::Array(a), Val::Array(b)) => {
                self.order_all(&a, &b)
            }
            (Val::Unit, Val::Unit) => Ordering::Equal,
            (a, b) => a.int().cmp(&b.int()),
        }
    }

    fn order_all(&self, left: &[Val], right: &[Val]) -> Ordering {
        for (a, b) in left.iter().zip(right) {
            let order = self.order(a, b);
            if order != Ordering::Equal {
                return order;
            }
        }
        left.len().cmp(&right.len())
    }
}
