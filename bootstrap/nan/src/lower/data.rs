//! Read-only data: the bytes of text and byte-string literals, the values of consts and
//! contributions laid out as their types are, and registry tables; and the writable data of
//! zeros a machine image reserves for its support routines. An image writes no data at run time
//! but the support routines'.

use std::collections::HashMap;
use std::rc::Rc;

use cranelift_codegen::ir::{InstBuilder, Value, types};
use cranelift_module::{DataDescription, DataId, Linkage, Module};

use super::body::Held;
use super::fx::{Fx, PTR};
use super::layout::{Shape, Tag};
use super::{Instance, Lowering};
use crate::check::db::ItemId;
use crate::check::sig::SigKind;
use crate::check::ty::{Arg, Const, Scalar, Ty};
use crate::interp::{ConstValue, FnVal, Interpreter, Val};
use crate::syntax::ast::ItemKind;

#[derive(Default)]
pub struct Data {
    /// Literal bytes and the view records of literals, by their bytes.
    bytes: HashMap<Vec<u8>, DataId>,
    views: HashMap<Vec<u8>, DataId>,
    /// The value of each const item and contribution, by item and type.
    consts: HashMap<(ItemId, Ty), DataId>,
    /// The table of each registry.
    tables: HashMap<ItemId, DataId>,
    next: u32,
}

/// The bytes of a value with the addresses it holds: each a function's or data's, at an offset.
struct Image {
    bytes: Vec<u8>,
    functions: Vec<(u32, Instance)>,
    data: Vec<(u32, DataId, i64)>,
}

impl Lowering<'_, '_> {
    /// A read-only data object holding bytes, named after `kind`.
    fn define_data(&mut self, kind: &str, image: Image, align: u64) -> DataId {
        let name = format!("_NSdata_{kind}{}", self.data.next);
        self.data.next += 1;
        let id = self
            .module
            .declare_data(&name, Linkage::Local, false, false)
            .expect("a data object");
        let mut description = DataDescription::new();
        description.define(image.bytes.into_boxed_slice());
        description.set_align(align.max(1));
        for (offset, instance) in image.functions {
            let function = self.instance(instance);
            let reference = self.module.declare_func_in_data(function, &mut description);
            description.write_function_addr(offset, reference);
        }
        for (offset, target, addend) in image.data {
            let reference = self.module.declare_data_in_data(target, &mut description);
            description.write_data_addr(offset, reference, addend);
        }
        self.module
            .define_data(id, &description)
            .expect("defining data");
        id
    }

    /// A read-only data object holding bytes, each relocation an offset in it at which the
    /// address of `target` plus an addend is written.
    pub fn relocated_data(
        &mut self,
        kind: &str,
        bytes: Vec<u8>,
        relocations: &[(u32, u64)],
        target: DataId,
    ) -> DataId {
        let data = relocations
            .iter()
            .map(|(offset, addend)| (*offset, target, *addend as i64))
            .collect();
        let image = Image {
            bytes,
            functions: Vec::new(),
            data,
        };
        self.define_data(kind, image, 8)
    }

    /// The writable data object of zeros a machine image reserves under a name, declared once.
    pub fn reserved_data(&mut self, name: &'static str, size: u64, align: u64) -> DataId {
        if let Some(id) = self.reserved.get(name) {
            return *id;
        }
        let id = self
            .module
            .declare_data(name, Linkage::Local, true, false)
            .unwrap_or_else(|error| panic!("declaring {name}: {error}"));
        let mut description = DataDescription::new();
        description.define_zeroinit(size as usize);
        description.set_align(align);
        self.module
            .define_data(id, &description)
            .unwrap_or_else(|error| panic!("defining {name}: {error}"));
        self.reserved.insert(name, id);
        id
    }

    /// The address of the writable data object of zeros a machine image reserves under a name.
    pub fn reserved(&mut self, fx: &mut Fx, name: &'static str, size: u64, align: u64) -> Value {
        let id = self.reserved_data(name, size, align);
        self.data_address(fx, id)
    }

    /// The address of a data object of the image.
    pub fn data_address(&mut self, fx: &mut Fx, id: DataId) -> Value {
        let global = self.data_ref(fx, id);
        fx.b.ins().symbol_value(PTR, global)
    }

    /// The data object holding bytes.
    pub fn bytes_data(&mut self, bytes: &[u8]) -> DataId {
        if let Some(id) = self.data.bytes.get(bytes) {
            return *id;
        }
        let id = self.define_data(
            "bytes",
            Image {
                bytes: if bytes.is_empty() {
                    vec![0]
                } else {
                    bytes.to_vec()
                },
                functions: Vec::new(),
                data: Vec::new(),
            },
            1,
        );
        self.data.bytes.insert(bytes.to_vec(), id);
        id
    }

    /// A text or byte-string literal: a view record in read-only data.
    pub fn view_literal(&mut self, fx: &mut Fx, bytes: &[u8]) -> Held {
        let id = match self.data.views.get(bytes) {
            Some(id) => *id,
            None => {
                let target = self.bytes_data(bytes);
                let mut record = vec![0; 16];
                record[8..].copy_from_slice(&(bytes.len() as u64).to_le_bytes());
                let id = self.define_data(
                    "view",
                    Image {
                        bytes: record,
                        functions: Vec::new(),
                        data: vec![(0, target, 0)],
                    },
                    8,
                );
                self.data.views.insert(bytes.to_vec(), id);
                id
            }
        };
        let global = self.data_ref(fx, id);
        Held::Address(fx.b.ins().symbol_value(PTR, global))
    }

    /// The value of a const item or contribution, or a registry's table as a slice.
    pub fn const_item(&mut self, fx: &mut Fx, item: ItemId, ty: &Ty) -> Held {
        if matches!(self.ck.db.ast(item).kind, ItemKind::Registry(_)) {
            let table = self.table(item);
            let global = self.data_ref(fx, table);
            return Held::Address(fx.b.ins().symbol_value(PTR, global));
        }
        let value = self
            .ck
            .const_value(item)
            .unwrap_or_else(|| panic!("the value of {}", self.ck.db.canonical(item)));
        let layout = self.layout(ty);
        match &layout.shape {
            Shape::Scalar { ty: scalar, .. } => {
                let bits = self.scalar_bits(ty, &value.value);
                Held::Scalar(match *scalar {
                    types::F32 => fx.b.ins().f32const(f32::from_bits(bits as u32)),
                    types::F64 => fx.b.ins().f64const(f64::from_bits(bits as u64)),
                    _ => fx.b.ins().iconst(*scalar, bits as i64),
                })
            }
            Shape::Empty => Held::Empty,
            _ => {
                let key = (item, ty.clone());
                let id = match self.data.consts.get(&key) {
                    Some(id) => *id,
                    None => {
                        let id = self.value_data(&value, ty);
                        self.data.consts.insert(key, id);
                        id
                    }
                };
                let global = self.data_ref(fx, id);
                Held::Address(fx.b.ins().symbol_value(PTR, global))
            }
        }
    }

    /// A data object holding a const value of a type.
    fn value_data(&mut self, value: &Rc<ConstValue>, ty: &Ty) -> DataId {
        let layout = self.layout(ty);
        let mut image = Image {
            bytes: vec![0; layout.size.max(1) as usize],
            functions: Vec::new(),
            data: Vec::new(),
        };
        let mut extents = HashMap::new();
        self.write_val(value, &value.value, ty, 0, &mut image, &mut extents);
        self.define_data("const", image, layout.align)
    }

    /// Writes a value of a type at an offset of an image.
    fn write_val(
        &mut self,
        constant: &Rc<ConstValue>,
        value: &Val,
        ty: &Ty,
        offset: u64,
        image: &mut Image,
        extents: &mut HashMap<(u32, Ty), DataId>,
    ) {
        let layout = self.layout(ty);
        match (&layout.shape, value) {
            (Shape::Empty, _) => {}
            (Shape::Scalar { ty: scalar, big }, value) => {
                let bits = self.scalar_bits(ty, value);
                let size = scalar.bytes() as usize;
                let at = offset as usize;
                let bytes = bits.to_le_bytes();
                let target = &mut image.bytes[at..at + size];
                target.copy_from_slice(&bytes[..size]);
                if *big {
                    target.reverse();
                }
            }
            (Shape::Array { element, .. }, Val::Array(elements)) => {
                let element_layout = self.layout(element);
                for (index, element_value) in elements.iter().enumerate() {
                    let at = offset + index as u64 * element_layout.size;
                    self.write_val(constant, element_value, element, at, image, extents);
                }
            }
            (Shape::Record(_), Val::View(view)) => {
                let element = self.view_element(ty);
                let element_layout = self.layout(&element);
                let key = (view.array.extent, element.clone());
                let target = match extents.get(&key) {
                    Some(id) => *id,
                    None => {
                        let id = self.extent_data(constant, view.array.extent, &element);
                        extents.insert(key, id);
                        id
                    }
                };
                assert!(
                    view.array.path.is_empty(),
                    "a const view into a part of its data"
                );
                let addend = view.start as u64 * element_layout.size;
                image.data.push((offset as u32, target, addend as i64));
                let len = (view.len as u64).to_le_bytes();
                let at = offset as usize + 8;
                image.bytes[at..at + 8].copy_from_slice(&len);
            }
            (Shape::Record(fields), Val::Struct(_, values)) => {
                let fields = fields.clone();
                for (field, field_value) in fields.iter().zip(values.iter()) {
                    self.write_val(
                        constant,
                        field_value,
                        &field.ty,
                        offset + field.offset,
                        image,
                        extents,
                    );
                }
            }
            (Shape::Record(_), Val::Fn(function)) => {
                let instance = match function.as_ref() {
                    FnVal::Function(fn_ref) => self.callable_instance(fn_ref, true),
                    FnVal::Closure { .. } => panic!("a closure in a const"),
                };
                image
                    .functions
                    .push((offset as u32, Instance::Value(Box::new(instance))));
            }
            (Shape::Enum(shape), Val::Variant(_, variant, payload)) => {
                let index = *variant as usize;
                let fields = shape.variants[index].fields.clone();
                for (field, field_value) in fields.iter().zip(payload.iter()) {
                    self.write_val(
                        constant,
                        field_value,
                        &field.ty,
                        offset + field.offset,
                        image,
                        extents,
                    );
                }
                match &shape.tag {
                    Tag::Index(tag) => {
                        let size = tag.bytes() as usize;
                        let at = offset as usize;
                        image.bytes[at..at + size]
                            .copy_from_slice(&(index as u64).to_le_bytes()[..size]);
                    }
                    Tag::Packed { niche, .. } => {
                        if let Some(value) = shape.tag.value_of(index) {
                            let size = niche.ty.bytes() as usize;
                            let at = (offset + niche.offset) as usize;
                            let mut bytes = (value << niche.bit).to_le_bytes()[..size].to_vec();
                            if niche.big {
                                bytes.reverse();
                            }
                            image.bytes[at..at + size].copy_from_slice(&bytes);
                        }
                    }
                }
            }
            (Shape::Bytes { fields, .. }, Val::Struct(_, values)) => {
                let fields = fields.clone();
                for (field, field_value) in fields.iter().zip(values.iter()) {
                    self.write_val(
                        constant,
                        field_value,
                        &field.ty,
                        offset + field.offset,
                        image,
                        extents,
                    );
                }
            }
            (shape, value) => panic!("a const value {value:?} laid out as {shape:?}"),
        }
    }

    /// The element type of a view type: a slice's element, or `u8` for a text.
    fn view_element(&self, ty: &Ty) -> Ty {
        if self.ck.is_std(ty, "core::Str") {
            return Ty::Scalar(Scalar::U8);
        }
        ty.type_arg(0).cloned().expect("a view's element type")
    }

    /// A data object holding an extent of a const value: an array of elements of a type.
    fn extent_data(&mut self, constant: &Rc<ConstValue>, extent: u32, element: &Ty) -> DataId {
        let elements = constant.data[extent as usize].fields().to_vec();
        let element_layout = self.layout(element);
        let mut image = Image {
            bytes: vec![0; (element_layout.size * elements.len() as u64).max(1) as usize],
            functions: Vec::new(),
            data: Vec::new(),
        };
        let mut extents = HashMap::new();
        for (index, value) in elements.iter().enumerate() {
            let at = index as u64 * element_layout.size;
            self.write_val(constant, value, element, at, &mut image, &mut extents);
        }
        self.define_data("extent", image, element_layout.align)
    }

    /// The bits of a scalar const value.
    fn scalar_bits(&mut self, ty: &Ty, value: &Val) -> u128 {
        match value {
            Val::Int(value) => *value as u128,
            Val::Bool(value) => u128::from(*value),
            Val::Char(c) => u128::from(u32::from(*c)),
            Val::F32(value) => u128::from(value.to_bits()),
            Val::F64(value) => u128::from(value.to_bits()),
            Val::Ptr(pointer) => u128::from(pointer.address),
            Val::Struct(item, fields) => self.layout_bits(*item, fields),
            Val::Variant(item, index, _) => {
                let name = match &self.ck.sig(*item).kind {
                    SigKind::Layout { tags, .. } => tags[*index as usize].clone(),
                    SigKind::Enum { variants } => variants[*index as usize].name.clone(),
                    _ => panic!("a variant of no enum"),
                };
                u128::from(self.tag_value(ty, &name))
            }
            Val::Unit => 0,
            other => panic!("the bits of {other:?}"),
        }
    }

    /// The bits of an integer layout value from its fields.
    fn layout_bits(&mut self, item: ItemId, fields: &[Val]) -> u128 {
        let ty = Ty::item(item, Vec::new());
        let SigKind::Layout {
            fields: declared, ..
        } = &self.ck.sig(item).kind
        else {
            panic!("bits of a non-layout struct");
        };
        let declared = declared.clone();
        let mut bits = 0u128;
        for (field, value) in declared.iter().zip(fields) {
            let (low, mask, field_ty) = self.bit_field(&ty, &field.name);
            let value = self.scalar_bits(&field_ty, value);
            bits |= (value & u128::from(mask)) << low;
        }
        bits
    }

    /// The table of a registry: its admitted contributions in key order, as a slice record.
    fn table(&mut self, registry: ItemId) -> DataId {
        if let Some(id) = self.data.tables.get(&registry) {
            return *id;
        }
        let SigKind::Registry { entry } = &self.ck.sig(registry).kind else {
            panic!("a registry's entry type");
        };
        let entry = entry.clone();
        let entries = self.table_entries(registry);
        let layout = self.layout(&entry);
        let mut image = Image {
            bytes: vec![0; (layout.size * entries.len() as u64).max(1) as usize],
            functions: Vec::new(),
            data: Vec::new(),
        };
        for (index, value) in entries.iter().enumerate() {
            let mut extents = HashMap::new();
            let at = index as u64 * layout.size;
            self.write_val(value, &value.value, &entry, at, &mut image, &mut extents);
        }
        let array = self.define_data("table", image, layout.align);
        let mut record = vec![0; 16];
        record[8..].copy_from_slice(&(entries.len() as u64).to_le_bytes());
        let id = self.define_data(
            "registry",
            Image {
                bytes: record,
                functions: Vec::new(),
                data: vec![(0, array, 0)],
            },
            8,
        );
        self.data.tables.insert(registry, id);
        id
    }

    /// The values of a registry's admitted contributions in table order: by canonical path, each
    /// inserted before the first entry whose key is greater.
    pub(super) fn table_entries(&mut self, registry: ItemId) -> Vec<Rc<ConstValue>> {
        let ItemKind::Registry(declared) = &self.ck.db.ast(registry).kind else {
            panic!("a registry item");
        };
        let key_name = declared.key.text.clone();
        let SigKind::Registry {
            entry: Ty::Item(entry, _),
        } = &self.ck.sig(registry).kind
        else {
            panic!("a registry of a struct");
        };
        let SigKind::Struct { fields } = &self.ck.sig(*entry).kind else {
            panic!("a registry of a struct");
        };
        let key = fields
            .iter()
            .position(|field| field.name == key_name)
            .expect("the key field");
        let mut contributions: Vec<(String, ItemId)> = self
            .roots
            .iter()
            .flat_map(|root| self.ck.contributions(*root))
            .filter(|contribution| contribution.admitted && contribution.registry == registry)
            .map(|contribution| (self.ck.db.canonical(contribution.item), contribution.item))
            .collect();
        contributions.sort();
        contributions.dedup();
        let mut interpreter = Interpreter::new(self.ck);
        let mut table: Vec<Rc<ConstValue>> = Vec::new();
        for (_, item) in contributions {
            let Some(value) = self.ck.const_value(item) else {
                continue;
            };
            let at = table
                .iter()
                .position(|known| interpreter.field_order(known, &value, key).is_gt())
                .unwrap_or(table.len());
            table.insert(at, value);
        }
        table
    }
}

/// The argument of a const generic parameter.
pub fn const_arg(arg: &Arg) -> i128 {
    match arg {
        Arg::Const(Const::Int(value)) => *value,
        Arg::Const(Const::Bool(value)) => i128::from(*value),
        other => panic!("a const argument, found {other:?}"),
    }
}
