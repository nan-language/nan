//! Values at addresses: loading and storing them as their layouts hold them, and building and
//! inspecting enum variants, for bodies and for the code lowering writes itself.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::Lowering;
use super::body::Held;
use super::fx::{Fx, access};
use super::layout::{Niche, Shape, Tag};
use crate::check::ty::Ty;

impl Lowering<'_, '_> {
    /// The value of a type at an address, held as its type is: a scalar loaded.
    pub fn held_at(&mut self, fx: &mut Fx, ty: &Ty, address: Value) -> Held {
        let layout = self.layout(ty);
        match layout.shape {
            Shape::Scalar { ty: scalar, big } => {
                Held::Scalar(fx.b.ins().load(scalar, access(false, big), address, 0))
            }
            Shape::Empty => Held::Empty,
            _ => Held::Address(address),
        }
    }

    /// Stores a value of a type at an address.
    pub fn store_held(&mut self, fx: &mut Fx, address: Value, held: Held, ty: &Ty) {
        let layout = self.layout(ty);
        match (&layout.shape, held) {
            (Shape::Empty, _) | (_, Held::Empty) => {}
            (Shape::Scalar { big, .. }, Held::Scalar(value)) => {
                fx.b.ins().store(access(false, *big), value, address, 0);
            }
            (Shape::Scalar { ty: scalar, big }, Held::Address(source)) => {
                let value = fx.b.ins().load(*scalar, access(false, *big), source, 0);
                fx.b.ins().store(access(false, *big), value, address, 0);
            }
            (_, Held::Address(source)) => {
                if source != address {
                    self.copy(fx, address, source, layout.size, layout.align);
                }
            }
            (shape, Held::Scalar(_)) => panic!("a scalar stored as {shape:?}"),
        }
    }

    /// A value of enum `ty` of a variant holding `payload`, one value per payload field in
    /// order. In a wrapped value, a part's payload is given and stored as its address.
    pub fn variant_value(
        &mut self,
        fx: &mut Fx,
        ty: &Ty,
        variant: &str,
        payload: &[Held],
        wrapped: bool,
    ) -> Held {
        let layout_ty = if wrapped {
            self.wrapped_type(ty)
        } else {
            ty.clone()
        };
        let layout = self.layout(&layout_ty);
        match &layout.shape {
            Shape::Scalar { ty: scalar, .. } => {
                let value = self.tag_value(ty, variant);
                Held::Scalar(fx.b.ins().iconst(*scalar, value as i64))
            }
            Shape::Empty => Held::Empty,
            Shape::Enum(shape) => {
                let index = variant_position(shape, variant);
                let slot = fx.slot(layout.size, layout.align);
                let fields = shape.variants[index].fields.clone();
                for (field, held) in fields.iter().zip(payload) {
                    let at = fx.offset(slot, field.offset);
                    let real = self
                        .layouts
                        .field(self.ck, ty, Some(variant), &field.name)
                        .ty;
                    if wrapped && self.is_part(&real) {
                        fx.store(held.address(), at, 0);
                    } else {
                        self.store_held(fx, at, *held, &field.ty);
                    }
                }
                write_tag(fx, slot, &shape.tag, index);
                Held::Address(slot)
            }
            shape => panic!("a variant of {ty:?} laid out as {shape:?}"),
        }
    }

    /// Whether the enum value of `ty` at an address is a variant.
    pub fn variant_is(&mut self, fx: &mut Fx, address: Value, ty: &Ty, variant: &str) -> Value {
        let layout = self.layout(ty);
        match &layout.shape {
            Shape::Scalar { ty: scalar, big } => {
                let value = fx.b.ins().load(*scalar, access(false, *big), address, 0);
                let expected = self.tag_value(ty, variant);
                fx.b.ins().icmp_imm_s(IntCC::Equal, value, expected as i64)
            }
            Shape::Enum(shape) => {
                let index = variant_position(shape, variant);
                match &shape.tag {
                    Tag::Index(tag) => {
                        let value = fx.load(*tag, address, 0);
                        fx.b.ins().icmp_imm_s(IntCC::Equal, value, index as i64)
                    }
                    Tag::Packed {
                        niche,
                        values,
                        payload,
                    } => {
                        let bits = niche_bits(fx, address, niche);
                        if index == *payload {
                            let mut holds = fx.int(types::I8, 1);
                            for value in values {
                                let differs =
                                    fx.b.ins().icmp_imm_s(IntCC::NotEqual, bits, *value as i64);
                                holds = fx.b.ins().band(holds, differs);
                            }
                            holds
                        } else {
                            let value = shape.tag.value_of(index).expect("a niche value");
                            fx.b.ins().icmp_imm_s(IntCC::Equal, bits, value as i64)
                        }
                    }
                }
            }
            _ => fx.int(types::I8, 1),
        }
    }

    /// The index of the variant of the enum value of `ty` at an address, as a 64-bit integer.
    pub fn variant_index(&mut self, fx: &mut Fx, ty: &Ty, address: Value) -> Value {
        let layout = self.layout(ty);
        match &layout.shape {
            Shape::Scalar { ty: scalar, big } => {
                let value = fx.b.ins().load(*scalar, access(false, *big), address, 0);
                let value = super::body::widen(fx, value);
                let Ty::Item(item, _) = ty else {
                    return value;
                };
                let names = self.variant_names(*item);
                let mut index = fx.int(types::I64, 0);
                for (position, name) in names.iter().enumerate() {
                    let expected = self.tag_value(ty, name);
                    let is = fx.b.ins().icmp_imm_s(IntCC::Equal, value, expected as i64);
                    let candidate = fx.int(types::I64, position as i64);
                    index = fx.b.ins().select(is, candidate, index);
                }
                index
            }
            Shape::Enum(shape) => match &shape.tag {
                Tag::Index(tag) => {
                    let value = fx.load(*tag, address, 0);
                    super::body::widen(fx, value)
                }
                Tag::Packed { payload, niche, .. } => {
                    let bits = niche_bits(fx, address, niche);
                    let mut index = fx.int(types::I64, *payload as i64);
                    for variant in 0..shape.variants.len() {
                        let Some(value) =
                            shape.tag.value_of(variant).filter(|_| variant != *payload)
                        else {
                            continue;
                        };
                        let is = fx.b.ins().icmp_imm_s(IntCC::Equal, bits, value as i64);
                        let candidate = fx.int(types::I64, variant as i64);
                        index = fx.b.ins().select(is, candidate, index);
                    }
                    index
                }
            },
            _ => fx.int(types::I64, 0),
        }
    }

    /// The address of a variant's payload field in an enum value of `ty` at an address.
    pub fn payload_address(
        &mut self,
        fx: &mut Fx,
        ty: &Ty,
        variant: &str,
        label: &str,
        address: Value,
    ) -> Value {
        let offset = self.layouts.field(self.ck, ty, Some(variant), label).offset;
        fx.offset(address, offset)
    }

    /// The names of an enum's variants in order.
    pub fn variant_names(&self, item: crate::check::db::ItemId) -> Vec<String> {
        match &self.ck.sig(item).kind {
            crate::check::sig::SigKind::Enum { variants } => variants
                .iter()
                .map(|variant| variant.name.clone())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Writes `Some(value)` when `present` is nonzero, else `None`, as a value of `option` at
    /// an address.
    pub fn write_option(
        &mut self,
        fx: &mut Fx,
        option: &Ty,
        address: Value,
        present: Value,
        value: Held,
    ) {
        let payload = option
            .type_arg(0)
            .cloned()
            .expect("an option's payload type");
        self.write_option_with(fx, option, address, present, |lo, fx, at| {
            lo.store_held(fx, at, value, &payload);
        });
    }

    /// Writes an option laid out as `layout_ty` at an address: `Some` with its payload written
    /// by `payload` at the payload's address when `present` is nonzero, else `None`.
    pub fn write_option_with(
        &mut self,
        fx: &mut Fx,
        layout_ty: &Ty,
        address: Value,
        present: Value,
        payload: impl FnOnce(&mut Self, &mut Fx, Value),
    ) {
        let layout = self.layout(layout_ty);
        let Shape::Enum(shape) = &layout.shape else {
            panic!("an option laid out as {:?}", layout.shape);
        };
        let offset = shape.variants[0]
            .fields
            .first()
            .map_or(0, |field| field.offset);
        fx.when(present, |fx| {
            let at = fx.offset(address, offset);
            payload(self, fx, at);
        });
        match &shape.tag {
            Tag::Index(tag) => {
                let none = fx.b.ins().iconst(*tag, 1);
                let some = fx.b.ins().iconst(*tag, 0);
                let chosen = fx.b.ins().select(present, some, none);
                fx.store(chosen, address, 0);
            }
            Tag::Packed { .. } => {
                let absent = fx.b.ins().bxor_imm_s(present, 1);
                fx.when(absent, |fx| write_tag(fx, address, &shape.tag, 1));
            }
        }
    }

    /// A failure of type `from` converted to the failure type `to`: unchanged, wrapped in the
    /// from variant `via`, or a cancellation turned into the cancellation of `to` holding the
    /// same origin.
    pub fn convert_failure(
        &mut self,
        fx: &mut Fx,
        held: Held,
        from: &Ty,
        to: &Ty,
        via: Option<&str>,
    ) -> Held {
        if from == to {
            return held;
        }
        let wrap = |lo: &mut Self, fx: &mut Fx, held: Held| -> Held {
            match via {
                Some(variant) => lo.variant_value(fx, to, variant, &[held], false),
                None => held,
            }
        };
        if self.cancellation_variant(to).is_none() {
            return wrap(self, fx, held);
        }
        let Some((is_cancellation, origin)) = self.cancellation_origin(fx, from, held) else {
            return wrap(self, fx, held);
        };
        let layout = self.layout(to);
        let slot = fx.slot(layout.size, layout.align);
        fx.choose_in(
            self,
            is_cancellation,
            &[],
            |lo, fx| {
                let built = lo.cancellation(fx, to, origin);
                lo.store_held(fx, slot, built, to);
                Vec::new()
            },
            |lo, fx| {
                let wrapped = wrap(lo, fx, held);
                lo.store_held(fx, slot, wrapped, to);
                Vec::new()
            },
        );
        self.held_at(fx, to, slot)
    }

    /// The cancellation of a cancellable failure type holding the origin `origin`.
    pub fn cancellation(&mut self, fx: &mut Fx, fails: &Ty, origin: Value) -> Held {
        let cancelled_ty = self.std_ty("task", "Cancelled", Vec::new());
        let cancelled = fx.slot(8, 8);
        fx.store(origin, cancelled, 0);
        match self.cancellation_variant(fails) {
            Some(Some(variant)) => {
                self.variant_value(fx, fails, &variant, &[Held::Address(cancelled)], false)
            }
            _ => self.held_at(fx, &cancelled_ty, cancelled),
        }
    }

    /// For a failure of a cancellable type held one way or another: whether it is a
    /// cancellation, and the origin it holds then; none for a type that is not cancellable.
    pub fn cancellation_origin(
        &mut self,
        fx: &mut Fx,
        fails: &Ty,
        held: Held,
    ) -> Option<(Value, Value)> {
        match self.cancellation_variant(fails)? {
            None => {
                let yes = fx.int(types::I8, 1);
                Some((yes, fx.load(super::fx::PTR, held.address(), 0)))
            }
            Some(variant) => {
                let address = held.address();
                let is = self.variant_is(fx, address, fails, &variant);
                let payload = self.payload_address(fx, fails, &variant, "", address);
                let zero = fx.ptr(0);
                let origin = fx.choose(
                    is,
                    &[super::fx::PTR],
                    |fx| vec![fx.load(super::fx::PTR, payload, 0)],
                    |_| vec![zero],
                )[0];
                Some((is, origin))
            }
        }
    }

    /// Writes the tag of a variant into an enum value at an address whose payload is in place.
    pub fn write_variant_tag(&mut self, fx: &mut Fx, ty: &Ty, address: Value, variant: &str) {
        let layout = self.layout(ty);
        match &layout.shape {
            Shape::Scalar { ty: scalar, big } => {
                let value = self.tag_value(ty, variant);
                let tag = fx.b.ins().iconst(*scalar, value as i64);
                fx.b.ins().store(access(false, *big), tag, address, 0);
            }
            Shape::Enum(shape) => {
                let index = variant_position(shape, variant);
                write_tag(fx, address, &shape.tag, index);
            }
            _ => {}
        }
    }

    /// `None` of an option type.
    pub fn none_value(&mut self, fx: &mut Fx, option: &Ty) -> Held {
        self.variant_value(fx, option, "None", &[], false)
    }
}

fn variant_position(shape: &super::layout::EnumShape, variant: &str) -> usize {
    shape
        .variants
        .iter()
        .position(|candidate| candidate.name == variant)
        .unwrap_or_else(|| panic!("the variant {variant}"))
}

/// Writes the tag or niche value of the variant at `index` into an enum value at an address.
pub fn write_tag(fx: &mut Fx, address: Value, tag: &Tag, index: usize) {
    match tag {
        Tag::Index(ty) => {
            let value = fx.b.ins().iconst(*ty, index as i64);
            fx.store(value, address, 0);
        }
        Tag::Packed { niche, .. } => {
            if let Some(value) = tag.value_of(index) {
                let constant = fx.b.ins().iconst(niche.ty, (value << niche.bit) as i64);
                fx.b.ins().store(
                    access(false, niche.big),
                    constant,
                    address,
                    niche.offset as i32,
                );
            }
        }
    }
}

/// The bits of a niche in a value at an address, as their own integer.
fn niche_bits(fx: &mut Fx, address: Value, niche: &Niche) -> Value {
    let whole = fx.b.ins().load(
        niche.ty,
        access(false, niche.big),
        address,
        niche.offset as i32,
    );
    if niche.bit == 0 && niche.width == niche.ty.bits() {
        return whole;
    }
    let shifted = fx.b.ins().ushr_imm_s(whole, i64::from(niche.bit));
    fx.b.ins()
        .band_imm_s(shifted, ((1u128 << niche.width) - 1) as i64)
}
