//! Interpreter values: scalars, structs and enum values by their items, arrays, views into
//! memory, references to places, function values and pointers.

use std::rc::Rc;

use crate::check::db::ItemId;
use crate::check::ty::{Args, Scalar};
use crate::nir::FnRef;

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Int(i128),
    F32(f32),
    F64(f64),
    Bool(bool),
    Char(char),
    Unit,
    /// A struct or layout value: its item and its fields in declaration order; a std type the
    /// interpreter models keeps its own fields.
    Struct(ItemId, Rc<Vec<Val>>),
    /// An enum value: its item, the index of its variant and its payload fields.
    Variant(ItemId, u32, Rc<Vec<Val>>),
    Array(Rc<Vec<Val>>),
    /// A `Slice`, `SliceMut` or `Str`: elements of an array held in memory.
    View(View),
    /// A reference to a place held in memory.
    Ref(Loc),
    Fn(Rc<FnVal>),
    Ptr(Ptr),
    /// A `Dyn[I]`: the value with the function implementing each of the interface's methods
    /// for its type, by method index.
    Dyn(Rc<DynVal>),
    /// What a place holds once moved, or before its first store.
    Absent,
}

/// A place in memory: an extent and the path of field, payload and element indexes in the value
/// it holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Loc {
    pub extent: u32,
    pub path: Vec<u32>,
}

impl Loc {
    pub fn with(&self, index: u32) -> Loc {
        let mut path = self.path.clone();
        path.push(index);
        Loc {
            extent: self.extent,
            path,
        }
    }
}

/// Elements `start..start + len` of the array at `array`.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub array: Loc,
    pub start: usize,
    pub len: usize,
}

/// A function value: a function with its type arguments, or a closure of a unit with its
/// captured values.
#[derive(Clone, Debug, PartialEq)]
pub enum FnVal {
    Function(FnRef),
    Closure {
        item: ItemId,
        index: u32,
        args: Args,
        captures: Vec<Val>,
    },
}

/// A pointer: an address and, when made from a place or view, its provenance.
#[derive(Clone, Debug, PartialEq)]
pub struct Ptr {
    pub address: u64,
    pub provenance: Option<Provenance>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Provenance {
    pub extent: u32,
    /// Whether it was made through a read access.
    pub read_only: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DynVal {
    pub value: Val,
    pub methods: Vec<FnRef>,
}

impl Val {
    pub fn int(&self) -> i128 {
        match self {
            Val::Int(value) => *value,
            Val::Char(c) => i128::from(u32::from(*c)),
            Val::Bool(value) => i128::from(*value),
            other => panic!("an integer value, not {other:?}"),
        }
    }

    pub fn bool(&self) -> bool {
        match self {
            Val::Bool(value) => *value,
            other => panic!("a bool value, not {other:?}"),
        }
    }

    pub fn fields(&self) -> &[Val] {
        match self {
            Val::Struct(_, fields) | Val::Variant(_, _, fields) => fields,
            Val::Array(elements) => elements,
            other => panic!("a value with parts, not {other:?}"),
        }
    }

    pub fn some(item: ItemId, value: Val) -> Val {
        Val::Variant(item, 0, Rc::new(vec![value]))
    }

    pub fn none(item: ItemId) -> Val {
        Val::Variant(item, 1, Rc::new(Vec::new()))
    }
}

/// Whether an integer is a value of a scalar type.
pub fn fits(value: i128, scalar: Scalar) -> bool {
    let (low, high) = scalar.range();
    (low..=high).contains(&value)
}

/// The value of a scalar type congruent to an integer modulo 2^width.
pub fn wrap(value: i128, scalar: Scalar) -> i128 {
    let bits = scalar.bits();
    let modulus = 1i128 << bits;
    let low = value.rem_euclid(modulus);
    if scalar.is_signed() && low >= modulus / 2 {
        low - modulus
    } else {
        low
    }
}
