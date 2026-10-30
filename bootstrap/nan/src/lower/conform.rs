//! Conformances lowering writes: the methods derives write for structs and enums, and the
//! conformances the toolchain supplies to scalars and std types, of `Eq`, `Ord`, `Hash`,
//! `Default`, `Render` and `Encode`; and the calls generated code makes of interface methods.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::Lowering;
use super::abi::{Abi, Repr, Returned};
use super::body::Held;
use super::call::{Called, Invoked, adapt_value};
use super::fx::{Fx, PTR};
use super::layout::Shape;
use crate::check::db::ItemId;
use crate::check::sig::SigKind;
use crate::check::ty::{Arg, Args, FnTy, Ty};
use crate::nir::{Callable, FnRef, NTy};

impl Lowering<'_, '_> {
    /// A core interface by name.
    pub fn core_interface(&self, name: &str) -> ItemId {
        self.ck
            .db
            .core_item(name)
            .unwrap_or_else(|| panic!("the core interface {name}"))
    }

    /// The head of a derived method for its owner applied to `args`, and whether it is a
    /// projection.
    pub fn derived_head(&mut self, owner: ItemId, method: &str, args: &Args) -> FnTy {
        let (ty, own) = derived_parts(self, owner, args);
        self.conformance_head(&ty, method, &own)
    }

    /// The head of an interface method a type implements through a conformance, with the
    /// method's own type arguments.
    pub fn conformance_head(&mut self, ty: &Ty, method: &str, own: &Args) -> FnTy {
        let interface = self.core_interface(method_interface(method));
        let index = self
            .ck
            .interface_method(interface, method)
            .expect("a method of the interface");
        self.method_head(ty, interface, own, index as u32).0
    }

    /// Calls method `method` of a core interface on a value of a concrete type, with `own`
    /// type arguments of the method, giving its result.
    pub fn call_method(
        &mut self,
        fx: &mut Fx,
        ty: &Ty,
        interface: &str,
        own: Vec<Arg>,
        args: &[Held],
    ) -> Held {
        let interface = self.core_interface(interface);
        self.call_interface(fx, ty, interface, 0, own, args).result
    }

    /// Calls method `method` of an interface on a value of a concrete type, with `own` type
    /// arguments of the method.
    pub fn call_interface(
        &mut self,
        fx: &mut Fx,
        ty: &Ty,
        interface: ItemId,
        method: u32,
        own: Vec<Arg>,
        args: &[Held],
    ) -> Called {
        let mut fn_args = vec![Arg::Ty(ty.clone())];
        fn_args.extend(own);
        let fn_ref = FnRef {
            callable: Callable::Method { interface, method },
            args: fn_args.into(),
        };
        let instance = self.callable_instance(&fn_ref, false);
        let abi = self.abi(&instance);
        let id = self.instance(instance);
        self.invoke(fx, Invoked::Direct(id), &abi, args)
    }

    /// Whether two values of a type at addresses are equal by `Eq.eq`.
    pub fn equal(&mut self, fx: &mut Fx, ty: &Ty, left: Value, right: Value) -> Value {
        let left = self.held_at(fx, ty, left);
        let right = self.held_at(fx, ty, right);
        self.call_method(fx, ty, "Eq", Vec::new(), &[left, right])
            .scalar()
    }

    /// The order of two values of a type at addresses by `Ord.cmp`: 0 less, 1 equal, 2 greater.
    pub fn order(&mut self, fx: &mut Fx, ty: &Ty, left: Value, right: Value) -> Value {
        let left = self.held_at(fx, ty, left);
        let right = self.held_at(fx, ty, right);
        let ordering = self.call_method(fx, ty, "Ord", Vec::new(), &[left, right]);
        self.ordering_index(fx, ordering)
    }

    /// The variant index of a `core::Ordering` value.
    pub fn ordering_index(&mut self, fx: &mut Fx, ordering: Held) -> Value {
        match ordering {
            Held::Scalar(value) => value,
            Held::Address(address) => fx.load(types::I8, address, 0),
            Held::Empty => fx.int(types::I8, 1),
        }
    }
}

/// The core interface a conformance method belongs to.
fn method_interface(method: &str) -> &'static str {
    match method {
        "eq" => "Eq",
        "cmp" => "Ord",
        "hash" => "Hash",
        "default" => "Default",
        "render" => "Render",
        "encode" | "decode" => "Encode",
        "generate" => "Generate",
        other => panic!("the conformance method {other}"),
    }
}

/// Defines a method a derive writes.
pub fn derived(
    lo: &mut Lowering,
    fx: &mut Fx,
    abi: &Abi,
    owner: ItemId,
    method: &str,
    args: &Args,
) {
    let (ty, _) = derived_parts(lo, owner, args);
    match method {
        "encode" | "decode" => super::supplied::codecs::calls::through(lo, fx, abi, &ty, method),
        "render" => super::supplied::render::define_render(lo, fx, abi, &ty),
        "generate" => super::supplied::generate::derived(lo, fx, abi, &ty),
        _ => composite(lo, fx, abi, &ty, method),
    }
}

/// The type a derived method is of and the method's own type arguments, from the arguments of
/// its instance: its owner's, then its own.
fn derived_parts(lo: &Lowering, owner: ItemId, args: &Args) -> (Ty, Args) {
    let count = lo.ck.sig(owner).generics.len().min(args.len());
    let (type_args, own) = args.split_at(count);
    (Ty::Item(owner, type_args.into()), own.into())
}

/// Defines a conformance the toolchain supplies.
pub fn supplied(lo: &mut Lowering, fx: &mut Fx, abi: &Abi, ty: &Ty, method: &str) {
    if let "encode" | "decode" = method {
        super::supplied::codecs::calls::through(lo, fx, abi, ty, method);
        return;
    }
    if method == "render" {
        super::supplied::render::define_render(lo, fx, abi, ty);
        return;
    }
    if method == "generate" {
        super::supplied::generate::conformance(lo, fx, abi, ty);
        return;
    }
    if let Ty::Scalar(_) | Ty::Unit = ty {
        scalar(lo, fx, abi, ty, method);
        return;
    }
    if let Ty::Item(item, _) = ty
        && let SigKind::Layout {
            bits: Some(bits), ..
        } = &lo.ck.sig(*item).kind
    {
        layout(lo, fx, abi, &bits.clone(), method);
        return;
    }
    if super::supplied::mem::conformance(lo, fx, abi, ty, method) {
        return;
    }
    composite(lo, fx, abi, ty, method);
}

/// The parameters of a conformance method being defined.
fn params(fx: &mut Fx, abi: &Abi) -> (Option<Value>, Vec<Held>) {
    let incoming = fx.entry();
    let mut incoming = incoming.into_iter();
    let sret = (abi.returned == Returned::Indirect).then(|| incoming.next().expect("a result"));
    let held = abi
        .params
        .iter()
        .map(|(_, repr)| match repr {
            Repr::Scalar(_) => Held::Scalar(incoming.next().expect("a parameter")),
            Repr::Address => Held::Address(incoming.next().expect("a parameter")),
            Repr::Empty => Held::Empty,
        })
        .collect();
    (sret, held)
}

/// Returns a result as the conformance's form gives it.
fn finish(lo: &mut Lowering, fx: &mut Fx, abi: &Abi, sret: Option<Value>, held: Held) {
    match abi.returned {
        Returned::Scalar(_) => {
            let nty = NTy::Owned(abi.result.clone());
            let repr = lo.owned_repr(&abi.result);
            let scalar = adapt_value(lo, fx, held, &nty, repr).scalar();
            fx.b.ins().return_(&[scalar]);
        }
        Returned::Indirect => {
            let layout = lo.layout(&abi.result);
            let target = sret.expect("a result address");
            if let Held::Address(source) = held
                && source != target
            {
                lo.copy(fx, target, source, layout.size, layout.align);
            }
            fx.b.ins().return_(&[]);
        }
        _ => {
            fx.b.ins().return_(&[]);
        }
    }
}

/// The conformances of scalars and `Unit`.
fn scalar(lo: &mut Lowering, fx: &mut Fx, abi: &Abi, ty: &Ty, method: &str) {
    let (sret, params) = params(fx, abi);
    let value = |index: usize, fx: &mut Fx, lo: &mut Lowering| -> Option<Value> {
        let nty = NTy::Owned(ty.clone());
        let repr = lo.owned_repr(ty);
        match adapt_value(lo, fx, params[index], &nty, repr) {
            Held::Scalar(value) => Some(value),
            _ => None,
        }
    };
    let signed = ty.scalar().is_some_and(crate::check::ty::Scalar::is_signed);
    let held = match method {
        "eq" => match (value(0, fx, lo), value(1, fx, lo)) {
            (Some(a), Some(b)) => Held::Scalar(fx.b.ins().icmp(IntCC::Equal, a, b)),
            _ => Held::Scalar(fx.int(types::I8, 1)),
        },
        "cmp" => match (value(0, fx, lo), value(1, fx, lo)) {
            (Some(a), Some(b)) => {
                let (less, greater) = match signed {
                    true => (IntCC::SignedLessThan, IntCC::SignedGreaterThan),
                    false => (IntCC::UnsignedLessThan, IntCC::UnsignedGreaterThan),
                };
                let less = fx.b.ins().icmp(less, a, b);
                let greater = fx.b.ins().icmp(greater, a, b);
                lo.ordering(fx, less, greater)
            }
            _ => {
                let no = fx.int(types::I8, 0);
                lo.ordering(fx, no, no)
            }
        },
        "default" => match lo.owned_repr(ty) {
            Repr::Scalar(scalar) => Held::Scalar(super::body::zero(fx, scalar)),
            _ => Held::Empty,
        },
        "hash" => {
            if let Some(a) = value(0, fx, lo) {
                let width = fx.b.func.dfg.value_type(a).bytes() as u64;
                let buffer = fx.slot(8, 8);
                fx.store(a, buffer, 0);
                let hasher_ty = abi.params[1].0.ty().clone();
                write_hasher(lo, fx, &hasher_ty, params[1], buffer, width);
            }
            Held::Empty
        }
        other => panic!("the scalar conformance {other}"),
    };
    finish(lo, fx, abi, sret, held);
}

/// The conformances of a layout, by its bits value of type `bits`.
fn layout(lo: &mut Lowering, fx: &mut Fx, abi: &Abi, bits: &Ty, method: &str) {
    let (sret, params) = params(fx, abi);
    let own = match method {
        "hash" => vec![Arg::Ty(abi.params[1].0.ty().clone())],
        _ => Vec::new(),
    };
    let held = lo.call_method(fx, bits, method_interface(method), own, &params);
    finish(lo, fx, abi, sret, held);
}

/// Writes `len` bytes at an address to a hasher of a type.
pub fn write_hasher(
    lo: &mut Lowering,
    fx: &mut Fx,
    hasher_ty: &Ty,
    hasher: Held,
    bytes: Value,
    len: u64,
) {
    let view = fx.slot(16, 8);
    fx.store(bytes, view, 0);
    let len = fx.ptr(len);
    fx.store(len, view, 8);
    lo.call_method(
        fx,
        hasher_ty,
        "Hasher",
        Vec::new(),
        &[hasher, Held::Address(view)],
    );
}

/// The conformances of structs and enums, field by field, and their derived methods.
fn composite(lo: &mut Lowering, fx: &mut Fx, abi: &Abi, ty: &Ty, method: &str) {
    let (sret, params) = params(fx, abi);
    let layout = lo.layout(ty);
    let address = |held: Held| held.address();
    match (method, &layout.shape) {
        ("eq", Shape::Record(fields)) => {
            let (a, b) = (address(params[0]), address(params[1]));
            let mut result = fx.int(types::I8, 1);
            for field in fields.clone() {
                let left = fx.offset(a, field.offset);
                let right = fx.offset(b, field.offset);
                let same = lo.equal(fx, &field.ty, left, right);
                result = fx.b.ins().band(result, same);
            }
            finish(lo, fx, abi, sret, Held::Scalar(result));
        }
        ("cmp", Shape::Record(fields)) => {
            let (a, b) = (address(params[0]), address(params[1]));
            let mut result = fx.int(types::I8, 1);
            for field in fields.clone() {
                let left = fx.offset(a, field.offset);
                let right = fx.offset(b, field.offset);
                let undecided = fx.b.ins().icmp_imm_s(IntCC::Equal, result, 1);
                let order = fx.choose(
                    undecided,
                    &[types::I8],
                    |fx| vec![lo.order(fx, &field.ty, left, right)],
                    |_| vec![result],
                );
                result = order[0];
            }
            let less = fx.b.ins().icmp_imm_s(IntCC::Equal, result, 0);
            let greater = fx.b.ins().icmp_imm_s(IntCC::Equal, result, 2);
            let ordering = lo.ordering(fx, less, greater);
            finish(lo, fx, abi, sret, ordering);
        }
        ("hash", Shape::Record(fields)) => {
            let value = address(params[0]);
            let hasher_ty = abi.params[1].0.ty().clone();
            let mut sorted = fields.clone();
            sorted.sort_by(|a, b| a.name.cmp(&b.name));
            for field in sorted {
                let at = fx.offset(value, field.offset);
                let held = lo.held_at(fx, &field.ty, at);
                lo.call_method(
                    fx,
                    &field.ty,
                    "Hash",
                    vec![Arg::Ty(hasher_ty.clone())],
                    &[held, params[1]],
                );
            }
            finish(lo, fx, abi, sret, Held::Empty);
        }
        ("default", Shape::Record(fields)) => {
            let target = sret.expect("a default's address");
            for field in fields.clone() {
                let value = lo.call_method(fx, &field.ty, "Default", Vec::new(), &[]);
                let at = fx.offset(target, field.offset);
                match value {
                    Held::Scalar(value) => fx.store(value, at, 0),
                    Held::Address(source) => {
                        let field_layout = lo.layout(&field.ty);
                        lo.copy(fx, at, source, field_layout.size, field_layout.align);
                    }
                    Held::Empty => {}
                }
            }
            finish(lo, fx, abi, sret, Held::Empty);
        }
        ("eq" | "cmp" | "hash", Shape::Enum(shape)) => {
            let (a, b) = (address(params[0]), params.get(1).copied());
            let index_a = lo.variant_index(fx, ty, a);
            match method {
                "hash" => {
                    let hasher_ty = abi.params[1].0.ty().clone();
                    for (index, variant) in shape.variants.iter().enumerate() {
                        let is = fx.b.ins().icmp_imm_s(IntCC::Equal, index_a, index as i64);
                        let hasher = b.expect("a hasher");
                        fx.when(is, |fx| {
                            let name = variant.name.as_bytes();
                            let len_bytes = fx.slot(8, 8);
                            let len = fx.int(types::I64, name.len() as i64);
                            fx.store(len, len_bytes, 0);
                            write_hasher(lo, fx, &hasher_ty, hasher, len_bytes, 8);
                            let text = lo.view_literal(fx, name).address();
                            let start = fx.load(PTR, text, 0);
                            write_hasher(lo, fx, &hasher_ty, hasher, start, name.len() as u64);
                            let mut fields = variant.fields.clone();
                            fields.sort_by(|left, right| left.name.cmp(&right.name));
                            for field in fields {
                                let at = fx.offset(a, field.offset);
                                let held = lo.held_at(fx, &field.ty, at);
                                lo.call_method(
                                    fx,
                                    &field.ty,
                                    "Hash",
                                    vec![Arg::Ty(hasher_ty.clone())],
                                    &[held, hasher],
                                );
                            }
                        });
                    }
                    finish(lo, fx, abi, sret, Held::Empty);
                }
                _ => {
                    let b = address(b.expect("a second value"));
                    let index_b = lo.variant_index(fx, ty, b);
                    let same = fx.b.ins().icmp(IntCC::Equal, index_a, index_b);
                    let result = fx.b.declare_var(types::I8);
                    let initial = match method {
                        "eq" => fx.b.ins().bxor_imm_s(same, 0),
                        _ => {
                            let less = fx.b.ins().icmp(IntCC::UnsignedLessThan, index_a, index_b);
                            let two = fx.int(types::I8, 2);
                            let zero = fx.int(types::I8, 0);
                            let one = fx.int(types::I8, 1);
                            let differs = fx.b.ins().select(less, zero, two);
                            fx.b.ins().select(same, one, differs)
                        }
                    };
                    fx.b.def_var(result, initial);
                    for (index, variant) in shape.variants.iter().enumerate() {
                        let is = fx.b.ins().icmp_imm_s(IntCC::Equal, index_a, index as i64);
                        let here = fx.b.ins().band(is, same);
                        let fields = variant.fields.clone();
                        fx.when(here, |fx| {
                            for field in fields {
                                let left = fx.offset(a, field.offset);
                                let right = fx.offset(b, field.offset);
                                let current = fx.b.use_var(result);
                                let next = match method {
                                    "eq" => {
                                        let equal = lo.equal(fx, &field.ty, left, right);
                                        fx.b.ins().band(current, equal)
                                    }
                                    _ => {
                                        let undecided =
                                            fx.b.ins().icmp_imm_s(IntCC::Equal, current, 1);
                                        fx.choose(
                                            undecided,
                                            &[types::I8],
                                            |fx| vec![lo.order(fx, &field.ty, left, right)],
                                            |_| vec![current],
                                        )[0]
                                    }
                                };
                                fx.b.def_var(result, next);
                            }
                        });
                    }
                    let value = fx.b.use_var(result);
                    let held = match method {
                        "eq" => Held::Scalar(value),
                        _ => {
                            let less = fx.b.ins().icmp_imm_s(IntCC::Equal, value, 0);
                            let greater = fx.b.ins().icmp_imm_s(IntCC::Equal, value, 2);
                            lo.ordering(fx, less, greater)
                        }
                    };
                    finish(lo, fx, abi, sret, held);
                }
            }
        }
        ("eq" | "cmp" | "hash", Shape::Empty) => {
            let held = match method {
                "eq" => Held::Scalar(fx.int(types::I8, 1)),
                "cmp" => {
                    let no = fx.int(types::I8, 0);
                    lo.ordering(fx, no, no)
                }
                _ => Held::Empty,
            };
            finish(lo, fx, abi, sret, held);
        }
        ("default", Shape::Empty) => finish(lo, fx, abi, sret, Held::Empty),
        (method, shape) => panic!("the conformance {method} of {ty:?} laid out as {shape:?}"),
    }
}
