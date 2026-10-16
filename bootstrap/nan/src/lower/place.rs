//! Places and values: where a NIR place lies, how values are read from and written to it, and
//! the values that constructions, variants, failure conversions, function values and closures
//! build.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{InstBuilder, Type, Value as IrValue, types};

use super::abi::Repr;
use super::body::{Body, Held};
use super::fx::{PTR, access};
use super::layout::{Field, Shape};
use super::{Instance, trap};
use crate::check::sig::SigKind;
use crate::check::ty::{Arg, Const, Ty};
use crate::nir::{
    BlockId, Constant, FnRef, NTy, Operand, Payload, Place, Role, Value, is_variant_step,
};

/// Where a place lies.
#[derive(Clone, Debug)]
pub enum Location {
    /// The bytes of a value of `ty` at an address; the scalars of a bytes layout's fields may be
    /// unaligned and big-endian.
    Memory {
        address: IrValue,
        ty: Ty,
        unaligned: bool,
        big: bool,
    },
    /// A wrapped part of type `ty` at an address, laid out as `ty`'s wrapped type.
    Wrapped { address: IrValue, ty: Ty },
    /// A value held in a register.
    Register { value: IrValue, ty: Ty },
    /// The bits `bit..bit + width` of an integer layout held at `container`, a field of type
    /// `ty`.
    Bits {
        container: Box<Location>,
        bit: u32,
        width: u32,
        ty: Ty,
    },
}

impl Body<'_, '_, '_, '_> {
    /// The type of the value at a place.
    pub fn place_type(&mut self, place: &Place) -> Ty {
        let location = self.locate_place(place);
        self.location_type(&location)
    }

    pub fn location_type(&self, location: &Location) -> Ty {
        match location {
            Location::Memory { ty, .. }
            | Location::Wrapped { ty, .. }
            | Location::Register { ty, .. }
            | Location::Bits { ty, .. } => ty.clone(),
        }
    }

    /// Where a place lies: its root's storage, then each step's field or payload, a step
    /// reaching a reference continuing at the place it designates.
    pub fn locate_place(&mut self, place: &Place) -> Location {
        let root = place.root;
        let held = self.held(root);
        let nty = self.nty(root).clone();
        let wrapped = self.is_wrapped(root);
        let ty = nty.ty().clone();
        let mut location = match held {
            Held::Address(address) if wrapped => Location::Wrapped { address, ty },
            Held::Address(address) => Location::Memory {
                address,
                ty,
                unaligned: false,
                big: false,
            },
            Held::Scalar(value) => Location::Register { value, ty },
            Held::Empty => {
                let address = self.fx.slot(0, 1);
                Location::Memory {
                    address,
                    ty,
                    unaligned: false,
                    big: false,
                }
            }
        };
        let mut index = 0;
        while index < place.steps.len() {
            let step = &place.steps[index];
            index += 1;
            if is_variant_step(step) {
                let labels = self.payload_labelled(&location, step);
                let label = match labels {
                    true => {
                        index += 1;
                        place.steps[index - 1].clone()
                    }
                    false => String::new(),
                };
                location = self.payload_step(location, step, &label);
            } else {
                location = self.field_step(location, step);
            }
        }
        location
    }

    /// Whether a variant's payload of the enum at a location is labelled.
    fn payload_labelled(&mut self, location: &Location, variant: &str) -> bool {
        let ty = self.location_type(location);
        let Ty::Item(item, _) = &ty else {
            return false;
        };
        match &self.lo.ck.sig(*item).kind {
            SigKind::Enum { variants } => variants.iter().any(|candidate| {
                candidate.name == variant
                    && matches!(candidate.payload, crate::check::sig::Payload::Labelled(_))
            }),
            _ => false,
        }
    }

    /// The payload field of a variant of the enum at a location; in a wrapped part, a payload
    /// that is a part is reached through the address the wrapped part holds.
    fn payload_step(&mut self, location: Location, variant: &str, label: &str) -> Location {
        let (address, ty, wrapped) = match location {
            Location::Memory { address, ty, .. } => (address, ty, false),
            Location::Wrapped { address, ty } => (address, ty, true),
            other => panic!("a payload step into {other:?}"),
        };
        let field = self.lo.layouts.field(self.lo.ck, &ty, Some(variant), label);
        if wrapped {
            let held_as = self.lo.wrapped_type(&ty);
            let stored = self
                .lo
                .layouts
                .field(self.lo.ck, &held_as, Some(variant), label);
            let at = self.fx.offset(address, stored.offset);
            let address = match self.lo.is_part(&field.ty) {
                true => self.fx.load(PTR, at, 0),
                false => at,
            };
            return Location::Memory {
                address,
                ty: field.ty,
                unaligned: false,
                big: false,
            };
        }
        Location::Memory {
            address: self.fx.offset(address, field.offset),
            ty: field.ty,
            unaligned: false,
            big: false,
        }
    }

    fn field_step(&mut self, location: Location, name: &str) -> Location {
        let ty = self.location_type(&location);
        let layout = self.lo.layout(&ty);
        match (&layout.shape, location) {
            (Shape::Scalar { .. }, container) => {
                let (bit, width, field_ty) = self.lo.bit_field(&ty, name);
                match container {
                    Location::Bits {
                        container,
                        bit: outer,
                        ..
                    } => Location::Bits {
                        container,
                        bit: outer + bit,
                        width,
                        ty: field_ty,
                    },
                    container => Location::Bits {
                        container: Box::new(container),
                        bit,
                        width,
                        ty: field_ty,
                    },
                }
            }
            (Shape::Bytes { fields, big }, Location::Memory { address, .. }) => {
                let field = find(fields, name);
                Location::Memory {
                    address: self.fx.offset(address, field.offset),
                    ty: field.ty.clone(),
                    unaligned: true,
                    big: *big,
                }
            }
            (
                Shape::Record(fields),
                Location::Memory {
                    address,
                    unaligned,
                    big,
                    ..
                },
            ) => {
                let field = find(fields, name);
                Location::Memory {
                    address: self.fx.offset(address, field.offset),
                    ty: field.ty.clone(),
                    unaligned,
                    big,
                }
            }
            (shape, location) => panic!("a field step {name} into {shape:?} at {location:?}"),
        }
    }

    /// The address of the value at a place and the type it is laid out as: a wrapped part's
    /// wrapped type.
    pub fn place_storage(&mut self, place: &Place) -> (IrValue, Ty) {
        match self.locate_place(place) {
            Location::Wrapped { address, ty } => (address, self.lo.wrapped_type(&ty)),
            location => {
                let ty = self.location_type(&location);
                (self.location_address(&location), ty)
            }
        }
    }

    /// The address of a place, spilling a value held in a register.
    pub fn place_address(&mut self, place: &Place) -> IrValue {
        let location = self.locate_place(place);
        self.location_address(&location)
    }

    pub fn location_address(&mut self, location: &Location) -> IrValue {
        match location {
            Location::Memory { address, .. } | Location::Wrapped { address, .. } => *address,
            Location::Register { value, ty } => {
                let layout = self.lo.layout(ty);
                let slot = self.fx.slot(layout.size, layout.align);
                self.store_scalar(*value, slot, 0, ty);
                slot
            }
            Location::Bits { .. } => {
                let ty = self.location_type(location);
                let value = self.read_location(location);
                let layout = self.lo.layout(&ty);
                let slot = self.fx.slot(layout.size, layout.align);
                self.copy_to(slot, value, &ty);
                slot
            }
        }
    }

    /// The value at a place, copied: a scalar loaded, any other value copied to new storage.
    pub fn read_place(&mut self, place: &Place) -> Held {
        let location = self.locate_place(place);
        self.read_location(&location)
    }

    pub fn read_location(&mut self, location: &Location) -> Held {
        match location {
            Location::Register { value, .. } => Held::Scalar(*value),
            Location::Wrapped { address, ty } => {
                let layout = self.lo.layout(&self.lo.wrapped_type(ty));
                let slot = self.fx.slot(layout.size, layout.align);
                self.lo
                    .copy(self.fx, slot, *address, layout.size, layout.align);
                Held::Address(slot)
            }
            Location::Bits {
                container,
                bit,
                width,
                ty,
            } => {
                let whole = self.read_location(container).scalar();
                let field = self.extract_bits(whole, *bit, *width, ty);
                match self.lo.owned_repr(ty) {
                    Repr::Scalar(_) => Held::Scalar(field),
                    _ => Held::Empty,
                }
            }
            Location::Memory {
                address,
                ty,
                unaligned,
                big,
            } => {
                let layout = self.lo.layout(ty);
                match &layout.shape {
                    Shape::Scalar {
                        ty: scalar,
                        big: own,
                    } => {
                        Held::Scalar(self.load_with(*scalar, *address, 0, *unaligned, *big || *own))
                    }
                    Shape::Empty => Held::Empty,
                    _ => {
                        let slot = self.fx.slot(layout.size, layout.align);
                        self.lo
                            .copy(self.fx, slot, *address, layout.size, layout.align);
                        Held::Address(slot)
                    }
                }
            }
        }
    }

    /// The value at a place passed by `take`: a scalar loaded, any other value by the address of
    /// its storage, which the receiver takes over.
    pub fn take_place(&mut self, place: &Place, ty: &Ty) -> Held {
        let location = self.locate_place(place);
        match (&location, self.lo.owned_repr(ty)) {
            (Location::Memory { address, .. }, Repr::Address) => Held::Address(*address),
            _ => self.read_location(&location),
        }
    }

    /// Writes a value to a location.
    pub fn write_location(&mut self, location: &Location, held: Held, nty: &NTy) {
        match location {
            Location::Memory {
                address,
                ty,
                unaligned,
                big,
            } => {
                let layout = self.lo.layout(ty);
                match (&layout.shape, held) {
                    (Shape::Scalar { big: own, .. }, held) => {
                        let value = self.adapt(held, nty, Repr::Scalar(types::I64)).scalar();
                        self.store_with(value, *address, 0, *unaligned, *big || *own);
                    }
                    (Shape::Empty, _) => {}
                    (_, held) => {
                        let source = self.adapt(held, nty, Repr::Address).address();
                        self.lo
                            .copy(self.fx, *address, source, layout.size, layout.align);
                    }
                }
            }
            Location::Bits {
                container,
                bit,
                width,
                ..
            } => {
                let value = match held {
                    Held::Scalar(value) => value,
                    other => self.adapt(other, nty, Repr::Scalar(types::I64)).scalar(),
                };
                let whole = self.read_location(container).scalar();
                let updated = self.insert_bits(whole, value, *bit, *width);
                self.write_location(container, Held::Scalar(updated), nty);
            }
            Location::Register { .. } | Location::Wrapped { .. } => {
                panic!("a write to a value that is no place")
            }
        }
    }

    /// Writes a value of `ty` at an address.
    pub fn write_value(&mut self, address: IrValue, held: Held, ty: &Ty) {
        let location = Location::Memory {
            address,
            ty: ty.clone(),
            unaligned: false,
            big: false,
        };
        self.write_location(&location, held, &NTy::Owned(ty.clone()));
    }

    /// Copies a value of `ty` to an address.
    pub fn copy_to(&mut self, address: IrValue, held: Held, ty: &Ty) {
        self.write_value(address, held, ty);
    }

    pub fn load_scalar(&mut self, address: IrValue, offset: u64, ty: &Ty) -> IrValue {
        let layout = self.lo.layout(ty);
        let Shape::Scalar { ty: scalar, big } = layout.shape else {
            panic!("a scalar load of {ty:?}");
        };
        self.load_with(scalar, address, offset, false, big)
    }

    pub fn store_scalar(&mut self, value: IrValue, address: IrValue, offset: u64, ty: &Ty) {
        let layout = self.lo.layout(ty);
        let big = matches!(layout.shape, Shape::Scalar { big: true, .. });
        self.store_with(value, address, offset, false, big);
    }

    fn load_with(
        &mut self,
        ty: Type,
        address: IrValue,
        offset: u64,
        unaligned_access: bool,
        big: bool,
    ) -> IrValue {
        let flags = access(unaligned_access, big);
        self.fx.b.ins().load(ty, flags, address, offset as i32)
    }

    fn store_with(
        &mut self,
        value: IrValue,
        address: IrValue,
        offset: u64,
        unaligned_access: bool,
        big: bool,
    ) {
        let flags = access(unaligned_access, big);
        self.fx.b.ins().store(flags, value, address, offset as i32);
    }

    fn extract_bits(&mut self, whole: IrValue, bit: u32, width: u32, ty: &Ty) -> IrValue {
        let container = self.fx.b.func.dfg.value_type(whole);
        let shifted = if bit == 0 {
            whole
        } else {
            self.fx.b.ins().ushr_imm_s(whole, i64::from(bit))
        };
        let masked = if width >= container.bits() {
            shifted
        } else {
            self.fx
                .b
                .ins()
                .band_imm_s(shifted, ((1u128 << width) - 1) as i64)
        };
        let target = match self.lo.owned_repr(ty) {
            Repr::Scalar(target) => target,
            _ => return masked,
        };
        resize(self.fx, masked, target)
    }

    fn insert_bits(&mut self, whole: IrValue, value: IrValue, bit: u32, width: u32) -> IrValue {
        let container = self.fx.b.func.dfg.value_type(whole);
        let value = resize(self.fx, value, container);
        let mask = if width >= container.bits() {
            u128::MAX >> (128 - container.bits())
        } else {
            ((1u128 << width) - 1) << bit
        };
        let cleared = self.fx.b.ins().band_imm_s(whole, !(mask as i64));
        let shifted = if bit == 0 {
            value
        } else {
            self.fx.b.ins().ishl_imm_s(value, i64::from(bit))
        };
        let placed = self.fx.b.ins().band_imm_s(shifted, mask as i64);
        self.fx.b.ins().bor(cleared, placed)
    }

    /// Whether a value fits the ranged `bit` or `rest` field at a location, none for a field that
    /// takes every value of its type.
    pub fn bits_fit(&mut self, location: &Location, value: IrValue) -> Option<IrValue> {
        let Location::Bits { width, .. } = location else {
            return None;
        };
        let ty = self.fx.b.func.dfg.value_type(value);
        if *width >= ty.bits() {
            return None;
        }
        let high = self.fx.b.ins().ushr_imm_s(value, i64::from(*width));
        Some(self.fx.b.ins().icmp_imm_s(IntCC::Equal, high, 0))
    }

    /// A struct or layout value from its fields' operands.
    pub fn construct(
        &mut self,
        ty: &Ty,
        fields: &[(String, Operand)],
        block: BlockId,
        position: usize,
    ) -> Held {
        let layout = self.lo.layout(ty);
        if let Shape::Scalar { ty: scalar, .. } = layout.shape {
            let mut whole = self.fx.b.ins().iconst(scalar, 0);
            for (name, operand) in fields {
                let (held, nty) = self.operand(operand);
                let value = match self.adapt(held, &nty, Repr::Scalar(types::I64)) {
                    Held::Scalar(value) => value,
                    _ => continue,
                };
                let (bit, width, _) = self.lo.bit_field(ty, name);
                let location = Location::Bits {
                    container: Box::new(Location::Register {
                        value: whole,
                        ty: ty.clone(),
                    }),
                    bit,
                    width,
                    ty: nty.ty().clone(),
                };
                if let Some(fits) = self.bits_fit(&location, value)
                    && self.checked(block, position)
                {
                    let stored = vec![(operand.clone(), held, nty)];
                    let operands = trap::Operands::Values(trap::Form::Field(name.clone()), stored);
                    self.trap_unless(fits, "trap.conversion", operands);
                }
                whole = self.insert_bits(whole, value, bit, width);
            }
            return Held::Scalar(whole);
        }
        if let Shape::Empty = layout.shape {
            for (_, operand) in fields {
                self.operand(operand);
            }
            return Held::Empty;
        }
        let slot = self.fx.slot(layout.size, layout.align);
        let target = Location::Memory {
            address: slot,
            ty: ty.clone(),
            unaligned: false,
            big: false,
        };
        for (name, operand) in fields {
            let (held, nty) = self.operand(operand);
            let location = self.field_step(target.clone(), name);
            self.write_location(&location, held, &nty);
        }
        Held::Address(slot)
    }

    /// An enum value of a variant with its payload's operands; a wrapped part stores the address
    /// of each non-Copy payload.
    pub fn variant(&mut self, ty: &Ty, variant: &str, payload: &Payload, wrapped: bool) -> Held {
        let operands: Vec<&Operand> = match payload {
            Payload::None => Vec::new(),
            Payload::Single(operand) => vec![operand],
            Payload::Labelled(fields) => fields.iter().map(|(_, operand)| operand).collect(),
        };
        let held: Vec<Held> = operands
            .into_iter()
            .map(|operand| self.operand(operand).0)
            .collect();
        self.lo.variant_value(self.fx, ty, variant, &held, wrapped)
    }

    /// Failure conversion: the value unchanged, or wrapped in a from variant; a cancellation
    /// keeps its place at the top of a cancellable target.
    pub fn convert(&mut self, operand: &Operand, to: &Ty, via: Option<&str>) -> Held {
        let (held, nty) = self.operand(operand);
        let from = nty.ty().clone();
        let repr = self.lo.owned_repr(&from);
        let held = self.adapt(held, &nty, repr);
        self.lo.convert_failure(self.fx, held, &from, to, via)
    }

    /// The value a `constant` names.
    pub fn constant(&mut self, constant: &Constant, ty: &Ty) -> Held {
        match constant {
            Constant::Item(item) => self.lo.const_item(self.fx, *item, ty),
            Constant::Param(param) => {
                let arg = self.subst.get(*param).cloned();
                let value = match arg {
                    Some(Arg::Const(Const::Int(value))) => value as i64,
                    Some(Arg::Const(Const::Bool(value))) => i64::from(value),
                    other => panic!("a const parameter's value, found {other:?}"),
                };
                match self.lo.owned_repr(ty) {
                    Repr::Scalar(scalar) => Held::Scalar(self.fx.b.ins().iconst(scalar, value)),
                    _ => panic!("a const parameter of the non-scalar {ty:?}"),
                }
            }
            Constant::Register(_, index) => {
                Held::Scalar(self.fx.int(types::I64, i64::from(*index)))
            }
        }
    }

    /// A function value naming a function: the code calling it through the ABI of function
    /// values, without an environment.
    pub fn fn_value(&mut self, fn_ref: &FnRef) -> Held {
        let fn_ref = FnRef {
            callable: fn_ref.callable.clone(),
            args: self.subst.args(&fn_ref.args),
        };
        let instance = self.lo.callable_instance(&fn_ref, true);
        let own_stack = self.lo.instance_fn_ty(&instance).limits.stack;
        let glue = self.lo.instance(Instance::Value(Box::new(instance)));
        self.lo.entry_stack(glue, own_stack);
        let reference = self.lo.func_ref(self.fx, glue);
        let code = self.fx.b.ins().func_addr(PTR, reference);
        let slot = self.fx.slot(16, 8);
        self.fx.store(code, slot, 0);
        let none = self.fx.ptr(0);
        self.fx.store(none, slot, 8);
        Held::Address(slot)
    }

    /// A closure value: its code and an environment holding its captures, in this frame for a
    /// closure and in runtime memory for a task closure, which outlives the frame.
    pub fn closure(&mut self, index: u32, captures: &[Operand]) -> Held {
        let instance = Instance::Nir {
            item: self.item,
            role: Role::Closure,
            index,
            args: self.args.clone(),
        };
        let unit = self.unit.clone();
        let function = unit
            .function(Role::Closure, index)
            .expect("a closure function");
        let task = function.closure.as_ref().is_some_and(|head| head.task);
        let subst = self.subst.clone();
        let types: Vec<NTy> = function
            .types
            .iter()
            .map(|nty| super::abi::substituted(&subst, nty))
            .collect();
        let params = &function.params[..captures.len()];
        let start = if task { super::transfer::ENV_HEADER } else { 0 };
        let offsets = self.lo.capture_layout(&types, params, start);
        let size = self.lo.capture_size(&types, params, start);
        let env = if task {
            let size = self.fx.ptr(size.max(8));
            let call = self
                .lo
                .call_routine(self.fx, super::runtime::Routine::TaskEnv, &[size]);
            call[0]
        } else {
            self.fx.slot(size, 8)
        };
        for ((operand, param), offset) in captures.iter().zip(params).zip(offsets.iter().copied()) {
            let (held, nty) = self.operand(operand);
            let param_nty = types[param.value.0 as usize].clone();
            let at = self.fx.offset(env, offset);
            if param_nty.is_reference() {
                let address = self.adapt(held, &nty, Repr::Address).address();
                self.fx.store(address, at, 0);
            } else {
                let ty = param_nty.ty().clone();
                self.write_value(at, held, &ty);
            }
        }
        if task {
            let owned: Vec<(Ty, u64)> = params
                .iter()
                .zip(&offsets)
                .filter_map(|(param, offset)| match &types[param.value.0 as usize] {
                    NTy::Owned(ty) => Some((ty.clone(), *offset)),
                    _ => None,
                })
                .collect();
            let walk = super::transfer::captures(self.lo, self.fx, instance.clone(), owned);
            self.fx.store(walk, env, 0);
        }
        let own_stack = function.closure.as_ref().and_then(|head| head.limits.stack);
        let code_id = self.lo.instance(instance);
        if task {
            self.lo.entry_stack(code_id, own_stack);
        }
        let reference = self.lo.func_ref(self.fx, code_id);
        let code = self.fx.b.ins().func_addr(PTR, reference);
        let slot = self.fx.slot(16, 8);
        self.fx.store(code, slot, 0);
        self.fx.store(env, slot, 8);
        Held::Address(slot)
    }

    /// Whether a value holds a wrapped part.
    pub fn is_wrapped(&self, value: Value) -> bool {
        let nty = self.nty(value);
        !nty.is_reference() && self.lo.wraps(nty.ty()) && self.wrapped_flag(value)
    }

    /// The operand passed as a read argument: a wrapped part becomes an owned value whose
    /// payloads are copies of the parts it points at, which the callee only reads.
    pub fn plain(&mut self, held: Held, value: Option<Value>, ty: &Ty) -> Held {
        if !value.is_some_and(|value| self.is_wrapped(value)) {
            return held;
        }
        let wrapped_ty = self.lo.wrapped_type(ty);
        let layout = self.lo.layout(ty);
        let wrapped_layout = self.lo.layout(&wrapped_ty);
        let (Shape::Enum(shape), Shape::Enum(wrapped)) = (&layout.shape, &wrapped_layout.shape)
        else {
            return held;
        };
        let slot = self.fx.slot(layout.size, layout.align);
        let source = held.address();
        for (index, (variant, wrapped_variant)) in
            shape.variants.iter().zip(&wrapped.variants).enumerate()
        {
            let is = self
                .lo
                .variant_is(self.fx, source, &wrapped_ty, &variant.name);
            let lo = &mut *self.lo;
            self.fx.when(is, |fx| {
                for (field, wrapped_field) in variant.fields.iter().zip(&wrapped_variant.fields) {
                    let from = fx.offset(source, wrapped_field.offset);
                    let to = fx.offset(slot, field.offset);
                    let origin = if lo.is_part(&field.ty) {
                        fx.load(PTR, from, 0)
                    } else {
                        from
                    };
                    let field_layout = lo.layout(&field.ty);
                    lo.copy(fx, to, origin, field_layout.size, field_layout.align);
                }
                super::held::write_tag(fx, slot, &shape.tag, index);
            });
        }
        Held::Address(slot)
    }

    pub fn operand_value(&self, operand: &Operand) -> Option<Value> {
        match operand {
            Operand::Value(value) => Some(*value),
            _ => None,
        }
    }
}

/// An integer register resized to another integer type, without its sign.
pub fn resize(fx: &mut super::fx::Fx, value: IrValue, target: Type) -> IrValue {
    let ty = fx.b.func.dfg.value_type(value);
    match ty.bits().cmp(&target.bits()) {
        std::cmp::Ordering::Equal => value,
        std::cmp::Ordering::Less => fx.b.ins().uextend(target, value),
        std::cmp::Ordering::Greater => fx.b.ins().ireduce(target, value),
    }
}

fn find<'f>(fields: &'f [Field], name: &str) -> &'f Field {
    fields
        .iter()
        .find(|field| field.name == name)
        .unwrap_or_else(|| panic!("a field {name}"))
}
