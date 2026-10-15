//! Low-level items: the representation of layouts, the registers of registers blocks, the heads
//! and conventions of foreign functions, and the trusted primitives.

use super::Checker;
use super::db::ItemId;
use super::props::Property;
use super::sig::{FnSig, SigKind, int_literal};
use super::ty::{Scalar, Ty};
use crate::diagnostic::{Code, Offer};
use crate::syntax::ast::{self, Access, ItemKind, LayoutFieldKind, LayoutRepr, Mode, Tok};
use crate::workspace::Target;

/// The trusted primitives: std functions callable only inside trusted code.
const PRIMITIVES: [&str; 9] = [
    "hw::Ptr.read",
    "hw::Ptr.write",
    "hw::Ptr.offset",
    "hw::Ptr.cast",
    "hw::Ptr.copy",
    "hw::fence",
    "hw::Mmio.map",
    "hw::Ports.map",
    "hw::TrapFrame.set_ip",
];

/// The privileged intrinsics that are trusted primitives too.
const PRIVILEGED_PRIMITIVES: [&str; 3] = ["write_cr3", "read_msr", "write_msr"];

/// The calling conventions and the targets each serves.
const CONVENTIONS: [(&str, Target); 2] = [("sysv", Target::Host), ("win64", Target::Uefi)];

impl Checker<'_> {
    /// Whether a std function is a trusted primitive.
    pub fn is_trusted_primitive(&self, function: ItemId) -> bool {
        if !self.db.is_std(function) {
            return false;
        }
        let path = self.db.canonical(function);
        PRIMITIVES.contains(&path.as_str())
            || path
                .strip_prefix("hw::Ptr.")
                .is_some_and(|member| member.starts_with("atomic_"))
            || path
                .strip_prefix("hw::Ring0.")
                .is_some_and(|member| PRIVILEGED_PRIMITIVES.contains(&member))
    }

    /// The size of a type with a fixed layout: a scalar, `hw::Ptr`, a layout or an array of
    /// these.
    pub fn fixed_size(&self, ty: &Ty) -> Option<u64> {
        match ty {
            Ty::Scalar(scalar) => Some(scalar_size(*scalar)),
            Ty::Item(item, args) => {
                if self.is_std(ty, "hw::Ptr") {
                    return Some(8);
                }
                if self.is_std(ty, "core::Array") {
                    let element = args.first().and_then(super::ty::Arg::ty)?;
                    let count = match args.get(1) {
                        Some(super::ty::Arg::Const(super::ty::Const::Int(count))) => {
                            u64::try_from(*count).ok()?
                        }
                        _ => return None,
                    };
                    return self.fixed_size(element).map(|size| size * count);
                }
                self.layout_size(*item)
            }
            _ => None,
        }
    }

    /// The size in bytes of a layout's representation.
    fn layout_size(&self, item: ItemId) -> Option<u64> {
        let ItemKind::Layout(layout) = &self.db.ast(item).kind else {
            return None;
        };
        match &layout.repr {
            LayoutRepr::Integer { ty, .. } => Scalar::from_name(&ty.text).map(scalar_size),
            LayoutRepr::Bytes { size, .. } => int_literal(self.db.item(item).text(*size))
                .and_then(|size| u64::try_from(size).ok()),
        }
    }

    /// The bits of an integer layout: each field's place in the representation, or each tag's
    /// value; none for a bytes layout.
    pub fn layout_shape(&self, item: ItemId) -> Option<LayoutShape> {
        let ItemKind::Layout(layout) = &self.db.ast(item).kind else {
            return None;
        };
        if matches!(layout.repr, LayoutRepr::Bytes { .. }) {
            return None;
        }
        let data = self.db.item(item);
        let value = |token: Tok| int_literal(data.text(token)).unwrap_or(0) as u32;
        if let Some(tags) = &layout.tags {
            return Some(LayoutShape::Tags(
                tags.iter().map(|tag| u64::from(value(tag.value))).collect(),
            ));
        }
        let types: Vec<Ty> = match &self.sig(item).kind {
            SigKind::Layout { fields, .. } => fields.iter().map(|field| field.ty.clone()).collect(),
            _ => Vec::new(),
        };
        let fields = layout
            .fields
            .iter()
            .zip(&types)
            .map(|(field, ty)| match &field.kind {
                LayoutFieldKind::Bit { low, high: None } => LayoutBits {
                    low: value(*low),
                    width: 1,
                    kind: BitsKind::Bool,
                },
                LayoutFieldKind::Bit {
                    low,
                    high: Some(high),
                } => LayoutBits {
                    low: value(*low),
                    width: value(*high) - value(*low) + 1,
                    kind: BitsKind::Ranged,
                },
                LayoutFieldKind::Typed { at, .. } => LayoutBits {
                    low: value(*at),
                    width: self.field_width(ty, false).unwrap_or(0) as u32,
                    kind: BitsKind::Typed,
                },
                LayoutFieldKind::Rest => LayoutBits {
                    low: 0,
                    width: 0,
                    kind: BitsKind::Rest,
                },
            })
            .collect();
        Some(LayoutShape::Fields(fields))
    }

    /// The rules of a layout item: its representation, literals, byte order, tags and fields.
    pub fn check_layout(&self, item: ItemId) {
        let ItemKind::Layout(layout) = &self.db.ast(item).kind else {
            return;
        };
        let data = self.db.item(item);
        let text = |token: Tok| data.text(token);
        let head = self.db.ast(item).token;
        let (width, bytes, order) = match &layout.repr {
            LayoutRepr::Integer { ty, order } => match Scalar::from_name(&ty.text) {
                Some(scalar @ (Scalar::U8 | Scalar::U16 | Scalar::U32 | Scalar::U64)) => {
                    (scalar_size(scalar), false, order)
                }
                _ => {
                    self.report(item, ty.token, Code::LayoutRepr);
                    return;
                }
            },
            LayoutRepr::Bytes { size, order } => {
                if layout.tags.is_some() {
                    self.report(item, *size, Code::LayoutRepr);
                    return;
                }
                if suffixed(text(*size)) {
                    self.report(item, *size, Code::LayoutLiteral);
                    return;
                }
                match int_literal(text(*size)).and_then(|size| u64::try_from(size).ok()) {
                    Some(size) if size >= 1 => (size, true, order),
                    _ => {
                        self.report(item, *size, Code::LayoutRepr);
                        return;
                    }
                }
            }
        };
        if (width > 1) != order.is_some() {
            let fix = match (order, &layout.repr) {
                (Some(order), _) => self.fix_deleting(item, order.token, order.token),
                (None, LayoutRepr::Integer { ty, .. }) => self.fix_appending(item, ty.token, " le"),
                (None, LayoutRepr::Bytes { size, .. }) => self.fix_appending(item, *size, " le"),
            };
            self.report_fixed(
                item,
                (head, head),
                Code::LayoutEndianness,
                vec![fix],
                Offer::Each,
            );
        }
        let literals: Vec<Tok> = layout
            .tags
            .iter()
            .flatten()
            .map(|tag| tag.value)
            .chain(layout.fields.iter().flat_map(|field| match &field.kind {
                LayoutFieldKind::Bit { low, high } => {
                    vec![*low].into_iter().chain(*high).collect::<Vec<_>>()
                }
                LayoutFieldKind::Typed { at, .. } => vec![*at],
                LayoutFieldKind::Rest => Vec::new(),
            }))
            .collect();
        if let Some(literal) = literals.into_iter().find(|token| suffixed(text(*token))) {
            self.report(item, literal, Code::LayoutLiteral);
            return;
        }
        if let Some(tags) = &layout.tags {
            self.check_tags(item, tags, width * 8);
            return;
        }
        self.check_layout_fields(item, layout, width, bytes);
    }

    /// The tags of a tags layout: at least one, each a distinct value fitting `bits` bits.
    fn check_tags(&self, item: ItemId, tags: &[ast::Tag], bits: u64) {
        let data = self.db.item(item);
        if tags.is_empty() {
            self.report(item, self.db.ast(item).token, Code::LayoutTagValue);
            return;
        }
        let mut names: Vec<&str> = Vec::new();
        let mut values: Vec<i128> = Vec::new();
        for tag in tags {
            if names.contains(&tag.name.text.as_str()) {
                self.report(item, tag.name.token, Code::TypeDuplicateMember);
            }
            names.push(&tag.name.text);
            let value = int_literal(data.text(tag.value));
            let fits = value.is_some_and(|value| value >= 0 && (bits >= 64 || value < 1 << bits));
            if !fits || value.is_some_and(|value| values.contains(&value)) {
                self.report(item, tag.value, Code::LayoutTagValue);
            }
            values.extend(value);
        }
    }

    /// The fields of a layout: their forms and types for its kind, within its `width` bytes, and
    /// each bit or byte covered by exactly one.
    fn check_layout_fields(&self, item: ItemId, layout: &ast::LayoutItem, width: u64, bytes: bool) {
        let data = self.db.item(item);
        let units = if bytes { width } else { width * 8 };
        let fields = match &self.sig(item).kind {
            SigKind::Layout { fields, .. } => fields.clone(),
            _ => return,
        };
        let mut covered: Vec<Option<usize>> = vec![None; units as usize];
        let mut rests = 0;
        let mut names: Vec<&str> = Vec::new();
        for (index, (field, typed)) in layout.fields.iter().zip(&fields).enumerate() {
            if names.contains(&field.name.text.as_str()) {
                self.report(item, field.name.token, Code::TypeDuplicateMember);
            }
            names.push(&field.name.text);
            let span = match &field.kind {
                LayoutFieldKind::Rest if bytes => {
                    self.report(item, field.name.token, Code::LayoutFieldForm);
                    continue;
                }
                LayoutFieldKind::Rest => {
                    rests += 1;
                    if rests > 1 {
                        self.report(item, field.name.token, Code::LayoutOverlap);
                    }
                    continue;
                }
                LayoutFieldKind::Bit { .. } if bytes => {
                    self.report(item, field.name.token, Code::LayoutFieldForm);
                    continue;
                }
                LayoutFieldKind::Bit { low, high } => {
                    let low = int_literal(data.text(*low)).unwrap_or(0) as u64;
                    let high =
                        high.map_or(low, |high| int_literal(data.text(high)).unwrap_or(0) as u64);
                    if high <= low && field_ranged(&field.kind) {
                        self.report(item, field.name.token, Code::LayoutFieldForm);
                        continue;
                    }
                    (low, high)
                }
                LayoutFieldKind::Typed { at, .. } => {
                    let Some(size) = self.field_width(&typed.ty, bytes) else {
                        if !typed.ty.has_error() {
                            self.report(item, field.name.token, Code::LayoutFieldType);
                        }
                        continue;
                    };
                    let at = int_literal(data.text(*at)).unwrap_or(0) as u64;
                    (at, at + size - 1)
                }
            };
            if span.1 >= units {
                self.report(item, field.name.token, Code::LayoutOutOfRange);
                continue;
            }
            for unit in span.0..=span.1 {
                if covered[unit as usize].is_some() {
                    self.report(item, field.name.token, Code::LayoutOverlap);
                    break;
                }
                covered[unit as usize] = Some(index);
            }
        }
        if rests == 0 && covered.iter().any(Option::is_none) {
            let head = self.db.ast(item).token;
            let fixes = match &layout.repr {
                LayoutRepr::Integer { .. } => self
                    .member_added(item, "reserved: rest")
                    .into_iter()
                    .collect(),
                LayoutRepr::Bytes { .. } => Vec::new(),
            };
            self.report_fixed(
                item,
                (head, head),
                Code::LayoutUncovered,
                fixes,
                Offer::Each,
            );
        }
    }

    /// The width a typed layout field occupies: bits in an integer layout, for an unsigned
    /// integer type or integer layout; bytes in a bytes layout, for an integer scalar, a layout
    /// or an array of these.
    fn field_width(&self, ty: &Ty, bytes: bool) -> Option<u64> {
        if !bytes {
            return match ty {
                Ty::Scalar(scalar @ (Scalar::U8 | Scalar::U16 | Scalar::U32 | Scalar::U64)) => {
                    Some(scalar_size(*scalar) * 8)
                }
                Ty::Item(item, _) => match &self.db.ast(*item).kind {
                    ItemKind::Layout(layout)
                        if matches!(layout.repr, LayoutRepr::Integer { .. }) =>
                    {
                        self.layout_size(*item).map(|size| size * 8)
                    }
                    _ => None,
                },
                _ => None,
            };
        }
        let allowed = |ty: &Ty| match ty {
            Ty::Scalar(scalar) => scalar.is_integer(),
            Ty::Item(item, _) => matches!(self.db.ast(*item).kind, ItemKind::Layout(_)),
            _ => false,
        };
        let element = if self.is_std(ty, "core::Array") {
            ty.type_arg(0)?
        } else {
            ty
        };
        allowed(element).then(|| self.fixed_size(ty)).flatten()
    }

    /// The rules of a registers item: register types, literals, alignment, strides and overlaps.
    pub fn check_registers(&self, item: ItemId) {
        let ItemKind::Registers(registers) = &self.db.ast(item).kind else {
            return;
        };
        let data = self.db.item(item);
        let SigKind::Registers { registers: typed } = &self.sig(item).kind else {
            return;
        };
        let mut placed: Vec<(u64, u64, Access)> = Vec::new();
        let mut names: Vec<&str> = Vec::new();
        for (register, sig) in registers.registers.iter().zip(typed) {
            let token = register.name.token;
            if names.contains(&register.name.text.as_str()) {
                self.report(item, token, Code::TypeDuplicateMember);
            }
            names.push(&register.name.text);
            if sig.ty.has_error() {
                continue;
            }
            let array = self.is_std(&sig.ty, "core::Array");
            let element = if array {
                sig.ty.type_arg(0).cloned().unwrap_or(Ty::Error)
            } else {
                sig.ty.clone()
            };
            let size = self
                .fixed_size(&element)
                .filter(|size| matches!(size, 1 | 2 | 4 | 8));
            let Some(size) = size.filter(|_| self.has(&element, Property::AnyBits)) else {
                self.report(item, token, Code::RegisterType);
                continue;
            };
            let literals = std::iter::once(register.at).chain(register.stride);
            if literals.clone().any(|literal| suffixed(data.text(literal))) {
                self.report(item, token, Code::RegisterLiteral);
                continue;
            }
            let offset = int_literal(data.text(register.at)).unwrap_or(0) as u64;
            if !offset.is_multiple_of(size) {
                self.report(item, token, Code::RegisterMisaligned);
                continue;
            }
            let stride = register
                .stride
                .map(|stride| int_literal(data.text(stride)).unwrap_or(0) as u64);
            let count = match sig.ty.args().get(1) {
                Some(super::ty::Arg::Const(super::ty::Const::Int(count))) if array => *count as u64,
                _ => 1,
            };
            match stride {
                Some(stride) if array && stride != 0 && stride % size == 0 => {}
                None if !array => {}
                _ => {
                    self.report(item, token, Code::RegisterStride);
                    continue;
                }
            }
            let end = offset + stride.unwrap_or(size) * (count - 1) + size;
            let overlapping = placed.iter().any(|(start, stop, access)| {
                let disjoint = end <= *start || offset >= *stop;
                let split = matches!(
                    (access, register.access),
                    (Access::Read, Access::Write) | (Access::Write, Access::Read)
                );
                !disjoint && !split
            });
            if overlapping {
                self.report(item, token, Code::RegisterOverlap);
            }
            placed.push((offset, end, register.access));
        }
    }

    /// The head of a foreign fn: read parameters of integer and float scalars, pointers and
    /// AnyBits layouts, such a result or `Unit` or `Never`, one identifier as its name, and no
    /// generic parameters or `fails`.
    pub fn check_foreign(&self, item: ItemId, function: &ast::FnItem, sig: &FnSig) {
        let token = self.db.ast(item).token;
        let passable = |ty: &Ty| match ty {
            Ty::Scalar(scalar) => scalar.is_number(),
            Ty::Item(item, _) => {
                self.is_std(ty, "hw::Ptr")
                    || matches!(self.db.ast(*item).kind, ItemKind::Layout(_))
                        && self.has(ty, Property::AnyBits)
            }
            _ => ty.has_error(),
        };
        let signature = function.label.owner.is_none()
            && sig.generics.is_empty()
            && sig.fails.is_none()
            && sig
                .params
                .iter()
                .all(|param| param.mode == Mode::Read && passable(&param.ty))
            && (passable(&sig.result) || matches!(sig.result, Ty::Unit | Ty::Never));
        if !signature {
            self.report(item, token, Code::TrustedForeignSignature);
            return;
        }
        if let Some(convention) = &function.convention {
            self.check_convention(item, convention.token, &convention.value);
        }
    }

    /// The head of a landing fn: read parameters of integer scalars, pointers, AnyBits layouts,
    /// Untrusted integers and arrays of these, such a result, `Unit`, `Never` or its target's
    /// root grant, and no generic parameters, `fails` or `uses wait`.
    pub fn check_landing(&self, item: ItemId, sig: &FnSig) {
        let raw = |ty: &Ty| -> bool {
            let element = if self.is_std(ty, "core::Array") {
                ty.type_arg(0).cloned().unwrap_or(Ty::Error)
            } else {
                ty.clone()
            };
            let untrusted_integer = self.is_std(&element, "core::Untrusted")
                && element
                    .type_arg(0)
                    .and_then(Ty::scalar)
                    .is_some_and(Scalar::is_integer);
            untrusted_integer || self.raw_value(&element)
        };
        let grant = ["host::World", "boot::Machine", "boot::Kernel"]
            .iter()
            .any(|path| self.is_std(&sig.result, path));
        let result =
            self.raw_value(&sig.result) || matches!(sig.result, Ty::Unit | Ty::Never) || grant;
        let signature = sig.generics.is_empty()
            && sig.fails.is_none()
            && !sig.wait
            && sig
                .params
                .iter()
                .all(|param| param.mode == Mode::Read && raw(&param.ty))
            && result;
        if !signature {
            self.report(item, self.db.ast(item).token, Code::HwLandingSignature);
        }
    }

    /// Whether a type is an integer scalar, a pointer or an AnyBits layout.
    fn raw_value(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Scalar(scalar) => scalar.is_integer(),
            Ty::Item(item, _) => {
                self.is_std(ty, "hw::Ptr")
                    || matches!(self.db.ast(*item).kind, ItemKind::Layout(_))
                        && self.has(ty, Property::AnyBits)
            }
            _ => ty.has_error(),
        }
    }

    /// A completion operation whose buffer type holds an `hw::Dma` outside an `Untrusted`.
    pub fn check_dma_completion(&self, item: ItemId, sig: &FnSig) {
        let completion = sig.wait && self.is_std(&sig.result, "task::Completion");
        let Some(buffer) = sig.result.type_arg(0).filter(|_| completion) else {
            return;
        };
        if self.exposes_dma(buffer) {
            self.report(item, self.db.ast(item).token, Code::HwDmaCompletion);
        }
    }

    /// Whether a type holds an `hw::Dma` other than inside an `Untrusted`.
    fn exposes_dma(&self, ty: &Ty) -> bool {
        if self.is_std(ty, "hw::Dma") {
            return true;
        }
        if self.is_std(ty, "core::Untrusted") {
            return false;
        }
        ty.args()
            .iter()
            .filter_map(super::ty::Arg::ty)
            .any(|arg| self.exposes_dma(arg))
    }

    /// The rules an instantiation of a low-level std type breaks: a register window or register
    /// of something other than a registers block, a port window over an 8-byte register, and a
    /// DMA buffer or user address of a type without a fixed layout.
    pub fn lowlevel_problem(&self, ty: &Ty) -> Option<Code> {
        let path = [
            "hw::Mmio",
            "hw::Ports",
            "hw::Register",
            "hw::Dma",
            "hw::DmaShared",
            "hw::UserAddr",
        ]
        .into_iter()
        .find(|path| self.is_std(ty, path))?;
        let argument = ty.type_arg(0)?;
        if argument.has_error() || matches!(argument, Ty::Param(_)) {
            return None;
        }
        match path {
            "hw::Mmio" | "hw::Ports" | "hw::Register" => {
                let Ty::Registers(block) = argument else {
                    return Some(Code::RegisterWindowType);
                };
                let wide = path == "hw::Ports"
                    && matches!(&self.sig(*block).kind, SigKind::Registers { registers }
                    if registers.iter().any(|register| {
                        let element = if self.is_std(&register.ty, "core::Array") { register.ty.type_arg(0).cloned().unwrap_or(Ty::Error) } else { register.ty.clone() };
                        self.fixed_size(&element) == Some(8)
                    }));
                wide.then_some(Code::RegisterPortWidth)
            }
            _ => self
                .fixed_size(argument)
                .is_none()
                .then_some(Code::HwLayoutUnfixed),
        }
    }

    /// A registers block written as the type of a value, which it has none of.
    pub fn check_value_type(&self, item: ItemId, ty: &Ty, token: Tok) {
        if let Ty::Registers(_) = ty {
            self.report(item, token, Code::RegisterWindowType);
        }
    }

    /// The types of an item's values: its parameters and result, fields and payloads, const
    /// type and registry entry, none a registers block.
    pub fn check_value_types(&self, item: ItemId) {
        let token = self.db.ast(item).token;
        let sig = self.sig(item);
        let types: Vec<(Ty, Tok)> = match &sig.kind {
            SigKind::Fn(head) => head
                .params
                .iter()
                .map(|param| (param.ty.clone(), param.token))
                .chain([(head.result.clone(), token)])
                .collect(),
            SigKind::Struct { fields } => fields
                .iter()
                .map(|field| (field.ty.clone(), field.token))
                .collect(),
            SigKind::Enum { variants } => variants
                .iter()
                .flat_map(|variant| {
                    let types: Vec<Ty> = match &variant.payload {
                        super::sig::Payload::None => Vec::new(),
                        super::sig::Payload::Single(ty) => vec![ty.clone()],
                        super::sig::Payload::Labelled(fields) => {
                            fields.iter().map(|(_, ty)| ty.clone()).collect()
                        }
                    };
                    types.into_iter().map(move |ty| (ty, variant.token))
                })
                .collect(),
            SigKind::Const { ty } => vec![(ty.clone(), token)],
            SigKind::Registry { entry } => vec![(entry.clone(), token)],
            _ => Vec::new(),
        };
        for (ty, at) in types {
            self.check_value_type(item, &ty, at);
        }
    }

    /// A convention clause: a known convention serving every target its package declares.
    pub fn check_convention(&self, item: ItemId, token: Tok, convention: &str) {
        let Some((_, target)) = CONVENTIONS.iter().find(|(name, _)| *name == convention) else {
            self.report(item, token, Code::TrustedConventionUnknown);
            return;
        };
        let package = self.db.package(self.db.package_of(item));
        let declared = package
            .source
            .manifest
            .iter()
            .flat_map(|manifest| &manifest.targets);
        if declared.into_iter().any(|declared| declared != target) {
            self.report(item, token, Code::TrustedConventionTarget);
        }
    }
}

fn scalar_size(scalar: Scalar) -> u64 {
    match scalar {
        Scalar::U8 | Scalar::I8 | Scalar::Bool => 1,
        Scalar::U16 | Scalar::I16 => 2,
        Scalar::U32 | Scalar::I32 | Scalar::F32 | Scalar::Char => 4,
        Scalar::U64 | Scalar::I64 | Scalar::F64 | Scalar::Usize | Scalar::Isize => 8,
    }
}

/// Whether an `INT` token's text carries a literal suffix.
fn suffixed(text: &str) -> bool {
    let body = text.trim_start_matches("0x").trim_start_matches("0b");
    body.contains(['u', 'i']) || (!text.starts_with("0x") && body.contains('f'))
}

/// Whether a layout field is a ranged `bit i..=j`.
fn field_ranged(kind: &LayoutFieldKind) -> bool {
    matches!(kind, LayoutFieldKind::Bit { high: Some(_), .. })
}

/// The bits of an integer layout.
pub enum LayoutShape {
    /// Its fields in declaration order.
    Fields(Vec<LayoutBits>),
    /// Its tags' values, in declaration order.
    Tags(Vec<u64>),
}

/// Where a field of an integer layout lies: its lowest bit and its width, both 0 for `rest`.
pub struct LayoutBits {
    pub low: u32,
    pub width: u32,
    pub kind: BitsKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BitsKind {
    /// `bit i`, a `bool`.
    Bool,
    /// `bit i..=j`.
    Ranged,
    /// `T at i`, an unsigned integer or integer layout.
    Typed,
    /// `rest`, the bits no other field covers.
    Rest,
}
