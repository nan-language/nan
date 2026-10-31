//! Interpreter memory: the extents of a run, each holding a value at an address, and the
//! interpreter layout that sizes them.

use std::rc::Rc;

use super::value::{Loc, Val};
use crate::check::Checker;
use crate::check::sig::{Payload, SigKind};
use crate::check::ty::{Scalar, Subst, Ty};

/// The address of a run's first extent.
const FIRST_ADDRESS: u64 = 0x1_0000_0000;

/// The granule extent addresses are multiples of, and the least gap after an extent.
const GRANULE: u64 = 65_536;

pub struct Extent {
    pub value: Val,
    pub writable: bool,
    pub live: bool,
    /// Whether it holds literal data or the data of a const, which a const's value may view.
    pub data: bool,
    pub address: u64,
    pub size: u64,
}

#[derive(Default)]
pub struct Memory {
    pub extents: Vec<Extent>,
}

impl Memory {
    /// A new live extent holding a value, at the next address past the last extent.
    pub fn alloc(&mut self, value: Val, writable: bool, size: u64) -> u32 {
        self.add(value, writable, false, size)
    }

    /// A new read-only extent of literal or const data.
    pub fn alloc_data(&mut self, value: Val, size: u64) -> u32 {
        self.add(value, false, true, size)
    }

    fn add(&mut self, value: Val, writable: bool, data: bool, size: u64) -> u32 {
        let address = match self.extents.last() {
            Some(last) => (last.address + last.size + GRANULE).div_ceil(GRANULE) * GRANULE,
            None => FIRST_ADDRESS,
        };
        self.extents.push(Extent {
            value,
            writable,
            live: true,
            data,
            address,
            size,
        });
        self.extents.len() as u32 - 1
    }

    pub fn extent(&self, id: u32) -> &Extent {
        &self.extents[id as usize]
    }

    pub fn kill(&mut self, id: u32) {
        self.extents[id as usize].live = false;
    }

    /// The value at a place.
    pub fn get(&self, loc: &Loc) -> &Val {
        let mut value = &self.extents[loc.extent as usize].value;
        for index in &loc.path {
            value = &value.fields()[*index as usize];
        }
        value
    }

    /// Stores a value at a place.
    pub fn set(&mut self, loc: &Loc, stored: Val) {
        let mut value = &mut self.extents[loc.extent as usize].value;
        for index in &loc.path {
            value = match value {
                Val::Struct(_, fields) | Val::Variant(_, _, fields) | Val::Array(fields) => {
                    &mut Rc::make_mut(fields)[*index as usize]
                }
                other => panic!("a place inside {other:?}"),
            };
        }
        *value = stored;
    }
}

/// The size and alignment of a type in the interpreter layout.
pub fn layout(ck: &Checker, ty: &Ty) -> (u64, u64) {
    match ty {
        Ty::Scalar(scalar) => {
            let size = match scalar {
                Scalar::Bool | Scalar::U8 | Scalar::I8 => 1,
                Scalar::U16 | Scalar::I16 => 2,
                Scalar::U32 | Scalar::I32 | Scalar::F32 | Scalar::Char => 4,
                _ => 8,
            };
            (size, size)
        }
        Ty::Unit | Ty::Never | Ty::Error | Ty::Registers(_) | Ty::Param(_) => (0, 1),
        Ty::Fn(_) | Ty::Dyn(_) => (16, 8),
        Ty::Item(item, args) => {
            let path = ck.db.canonical(*item);
            if matches!(
                path.as_str(),
                "core::Slice" | "core::SliceMut" | "core::Str"
            ) {
                return (16, 8);
            }
            if path == "core::Array" {
                let element = args.first().and_then(|arg| arg.ty()).cloned();
                let count = match args.get(1) {
                    Some(crate::check::ty::Arg::Const(crate::check::ty::Const::Int(count))) => {
                        *count as u64
                    }
                    _ => 0,
                };
                let (size, align) = element.map_or((0, 1), |element| layout(ck, &element));
                return (size * count, align);
            }
            let sig = ck.sig(*item);
            let subst = Subst::of(sig.generics.iter().map(|def| def.param), args);
            match &sig.kind {
                SigKind::Struct { fields } | SigKind::Layout { fields, .. } => {
                    let types: Vec<Ty> = fields.iter().map(|field| subst.ty(&field.ty)).collect();
                    place_fields(ck, 0, 1, &types)
                }
                SigKind::Enum { variants } | SigKind::Protocol { variants, .. } => {
                    let mut end = 4;
                    let mut align = 4;
                    for variant in variants {
                        let types: Vec<Ty> = match &variant.payload {
                            Payload::None => Vec::new(),
                            Payload::Single(ty) => vec![subst.ty(ty)],
                            Payload::Labelled(fields) => {
                                fields.iter().map(|(_, ty)| subst.ty(ty)).collect()
                            }
                        };
                        let (size, field_align) = place_fields(ck, 4, 4, &types);
                        end = end.max(size);
                        align = align.max(field_align);
                    }
                    (end.div_ceil(align) * align, align)
                }
                _ => (0, 1),
            }
        }
    }
}

/// The size and alignment of fields placed in order from `start`, each at the lowest offset
/// that is a multiple of its alignment.
fn place_fields(ck: &Checker, start: u64, align: u64, types: &[Ty]) -> (u64, u64) {
    let mut end = start;
    let mut align = align;
    for ty in types {
        let (size, field_align) = layout(ck, ty);
        end = end.div_ceil(field_align.max(1)) * field_align.max(1) + size;
        align = align.max(field_align);
    }
    (end.div_ceil(align) * align, align)
}
