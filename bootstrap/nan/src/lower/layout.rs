//! Type layout: the size, alignment and parts of every type an image's code uses. Fixed layouts,
//! empty types and niche packing follow the specification; the representation of every other
//! type is the ABI this lowering chooses.

use std::collections::HashMap;
use std::rc::Rc;

use cranelift_codegen::ir::{Type, types};

use crate::check::Checker;
use crate::check::db::ItemId;
use crate::check::sig::{Payload, SigKind};
use crate::check::ty::{Arg, Const, Scalar, Subst, Ty};
use crate::syntax::ast::{ItemKind, LayoutFieldKind, LayoutRepr};

/// The machine form of a type.
#[derive(Debug)]
pub struct Layout {
    pub size: u64,
    pub align: u64,
    pub shape: Shape,
    /// The invalid bit patterns an enclosing packed enum may use, none when it has none.
    pub niche: Option<Niche>,
}

#[derive(Debug)]
pub enum Shape {
    /// A value held in a register: a scalar, `hw::Ptr` or an integer layout, whose memory form
    /// is big-endian when `big` holds.
    Scalar {
        ty: Type,
        big: bool,
    },
    /// No bytes.
    Empty,
    /// Fields at offsets.
    Record(Vec<Field>),
    Enum(EnumShape),
    Array {
        element: Ty,
        count: u64,
    },
    /// A bytes layout: fields at byte offsets, whatever their alignment.
    Bytes {
        fields: Vec<Field>,
        big: bool,
    },
}

#[derive(Clone, Debug)]
pub struct Field {
    pub name: String,
    pub ty: Ty,
    pub offset: u64,
}

#[derive(Debug)]
pub struct EnumShape {
    pub variants: Vec<Variant>,
    pub tag: Tag,
}

#[derive(Debug)]
pub struct Variant {
    pub name: String,
    /// The payload's fields: one unnamed for `V(T)`, the labels of a labelled payload.
    pub fields: Vec<Field>,
}

#[derive(Debug)]
pub enum Tag {
    /// The variant's index at offset 0.
    Index(Type),
    /// A packed enum: the variant `payload` is its payload; each other variant is the niche
    /// value at its position in `values`.
    Packed {
        payload: usize,
        niche: Niche,
        values: Vec<u128>,
    },
}

impl Tag {
    /// The niche value of a unit variant of a packed enum.
    pub fn value_of(&self, variant: usize) -> Option<u128> {
        match self {
            Tag::Index(_) => None,
            Tag::Packed {
                payload, values, ..
            } => {
                let position = if variant < *payload {
                    variant
                } else {
                    variant.checked_sub(1)?
                };
                (variant != *payload).then(|| values[position])
            }
        }
    }
}

/// Invalid bit patterns of a value: those of the bits `bit..bit + width` of the integer of type
/// `ty` at `offset` that its kind leaves invalid, past the `used` first ones.
#[derive(Clone, Debug)]
pub struct Niche {
    pub offset: u64,
    pub ty: Type,
    pub big: bool,
    pub bit: u32,
    pub width: u32,
    pub used: u128,
    pub kind: NicheKind,
}

#[derive(Clone, Debug)]
pub enum NicheKind {
    /// The values from `first`, `count` of them.
    Range { first: u128, count: u128 },
    /// Every value of the width but these.
    Except(Rc<[u128]>),
}

impl Niche {
    /// The invalid patterns left.
    pub fn count(&self) -> u128 {
        let total = match &self.kind {
            NicheKind::Range { count, .. } => *count,
            NicheKind::Except(valid) => (1u128 << self.width) - valid.len() as u128,
        };
        total - self.used
    }

    /// The `n`th invalid pattern left.
    pub fn nth(&self, n: u128) -> u128 {
        let n = n + self.used;
        match &self.kind {
            NicheKind::Range { first, .. } => first + n,
            NicheKind::Except(valid) => {
                let mut seen = 0;
                let mut value = 0;
                loop {
                    if !valid.contains(&value) {
                        if seen == n {
                            return value;
                        }
                        seen += 1;
                    }
                    value += 1;
                }
            }
        }
    }

    fn at(&self, offset: u64) -> Niche {
        Niche {
            offset: self.offset + offset,
            ..self.clone()
        }
    }

    fn after(&self, used: u128) -> Niche {
        Niche {
            used: self.used + used,
            ..self.clone()
        }
    }
}

/// The std base types this lowering gives a representation, by canonical path.
const VIEW_PAIRS: [&str; 6] = [
    "core::Slice",
    "core::SliceMut",
    "core::Str",
    "core::SliceCursor",
    "core::SliceCursorMut",
    "core::CharCursor",
];

/// The base types holding one pointer to state the runtime keeps.
const POINTERS: [&str; 17] = [
    "core::Renderer",
    "core::Choices",
    "mem::Budget",
    "mem::Arena",
    "mem::Shared",
    "mem::OnceMap",
    "text::Interner",
    "task::Cown",
    "task::Sender",
    "task::Receiver",
    "task::Join",
    "task::Scope",
    "sys::Revoker",
    "sys::Minter",
    "sys::Cx",
    "sys::Stepper",
    "sys::Trap",
];

/// The base types that are generational handles, as capability kinds are.
const HANDLES: [&str; 13] = [
    "mem::Alloc",
    "mem::Heap",
    "mem::Handle",
    "task::Executor",
    "task::Account",
    "sys::Fs",
    "sys::Net",
    "sys::Clock",
    "sys::WallClock",
    "sys::Random",
    "sys::Log",
    "sys::Stdio",
    "sys::Outbox",
];

/// The base types holding storage, its length, its capacity and its allocator.
const VECTORS: [&str; 5] = [
    "mem::List",
    "mem::Bytes",
    "mem::Map",
    "mem::Set",
    "text::String",
];

/// The cursors owning the storage of a vector, with their position after it.
const OWNING_CURSORS: [&str; 4] = [
    "mem::ListCursor",
    "mem::MapIntoCursor",
    "mem::HashMapIntoCursor",
    "mem::SetIntoCursor",
];

/// The cursors viewing the entries of a map or set: the address of the next and the count
/// left.
const VIEW_CURSORS: [&str; 5] = [
    "mem::MapCursor",
    "mem::MapCursorMut",
    "mem::HashMapCursor",
    "mem::HashMapCursorMut",
    "mem::SetCursor",
];

/// The cursors of stores: the store's address and the next index.
const INDEX_CURSORS: [&str; 2] = ["mem::ArenaCursor", "mem::PoolCursor"];

/// The resources a platform opens: a capability table handle and the holding of the platform's
/// descriptor.
const RESOURCES: [&str; 3] = ["sys::fs::File", "sys::net::Listener", "sys::net::Conn"];

pub const POINTER: u64 = 8;

/// The layouts of one final closure, computed once per type.
pub struct Layouts {
    computed: HashMap<Ty, Rc<Layout>>,
}

impl Layouts {
    pub fn new() -> Layouts {
        Layouts {
            computed: HashMap::new(),
        }
    }

    pub fn of(&mut self, ck: &Checker, ty: &Ty) -> Rc<Layout> {
        if let Some(layout) = self.computed.get(ty) {
            return layout.clone();
        }
        let layout = Rc::new(self.compute(ck, ty));
        self.computed.insert(ty.clone(), layout.clone());
        layout
    }

    fn compute(&mut self, ck: &Checker, ty: &Ty) -> Layout {
        match ty {
            Ty::Scalar(scalar) => scalar_layout(*scalar, ck.db.target.is_machine()),
            Ty::Unit | Ty::Never => empty(),
            Ty::Fn(_) => pointers(2, None),
            Ty::Dyn(_) => pointers(2, Some(0)),
            Ty::Item(item, args) => self.item(ck, *item, args),
            Ty::Param(_) | Ty::Registers(_) | Ty::Error => {
                panic!("a layout of the unlowerable type {ty:?}")
            }
        }
    }

    fn item(&mut self, ck: &Checker, item: ItemId, args: &[Arg]) -> Layout {
        if ck.db.is_std(item) {
            let path = ck.db.canonical(item);
            if let Some(layout) = self.base(ck, &path, args) {
                return layout;
            }
        }
        if let ItemKind::Layout(layout) = &ck.db.ast(item).kind {
            return self.layout_item(ck, item, &layout.repr);
        }
        if ck.is_kind(item) {
            return handle();
        }
        let sig = ck.sig(item);
        let subst = Subst::of(sig.generics.iter().map(|def| def.param), args);
        match &sig.kind {
            SigKind::Struct { fields } => {
                let types: Vec<(String, Ty)> = fields
                    .iter()
                    .map(|field| (field.name.clone(), subst.ty(&field.ty)))
                    .collect();
                self.record(ck, &types)
            }
            SigKind::Enum { variants } | SigKind::Protocol { variants, .. } => {
                let variants: Vec<(String, Vec<(String, Ty)>)> = variants
                    .iter()
                    .map(|variant| {
                        let fields = match &variant.payload {
                            Payload::None => Vec::new(),
                            Payload::Single(ty) => vec![(String::new(), subst.ty(ty))],
                            Payload::Labelled(fields) => fields
                                .iter()
                                .map(|(name, ty)| (name.clone(), subst.ty(ty)))
                                .collect(),
                        };
                        (variant.name.clone(), fields)
                    })
                    .collect();
                self.enumeration(ck, variants)
            }
            _ => empty(),
        }
    }

    /// The layout the lowering gives a std base type, none for a type std declares in full.
    fn base(&mut self, ck: &Checker, path: &str, args: &[Arg]) -> Option<Layout> {
        let ty_arg = |index: usize| args.get(index).and_then(Arg::ty).cloned();
        let word = Ty::Scalar(Scalar::Usize);
        let count = |index: usize| match args.get(index) {
            Some(Arg::Const(Const::Int(count))) => *count as u64,
            _ => 0,
        };
        if VIEW_PAIRS.contains(&path) {
            let niche = matches!(path, "core::Slice" | "core::SliceMut" | "core::Str");
            return Some(pointers(2, niche.then_some(0)));
        }
        if POINTERS.contains(&path) {
            return Some(pointers(1, None));
        }
        if HANDLES.contains(&path) {
            return Some(handle());
        }
        if VECTORS.contains(&path) {
            return Some(words(&["data", "len", "capacity", "alloc"]));
        }
        if OWNING_CURSORS.contains(&path) {
            return Some(words(&["data", "len", "capacity", "alloc", "position"]));
        }
        if VIEW_CURSORS.contains(&path) {
            return Some(pointers(2, None));
        }
        if INDEX_CURSORS.contains(&path) {
            return Some(words(&["store", "position"]));
        }
        if path == "mem::HashMap" {
            return Some(words(&[
                "data", "len", "capacity", "alloc", "seed", "index", "slots",
            ]));
        }
        if path == "mem::Pool" {
            return Some(words(&[
                "data", "len", "capacity", "alloc", "count", "free", "first", "last",
            ]));
        }
        if RESOURCES.contains(&path) {
            return Some(Layout {
                size: 16,
                align: 8,
                shape: Shape::Record(vec![
                    field("entry", Ty::Scalar(Scalar::U64), 0),
                    field("holding", Ty::Scalar(Scalar::U64), 8),
                ]),
                niche: None,
            });
        }
        Some(match path {
            "core::Array" => {
                let element = ty_arg(0).expect("an element type");
                let count = count(1);
                let layout = self.of(ck, &element);
                Layout {
                    size: layout.size * count,
                    align: layout.align,
                    niche: layout.niche.clone().filter(|_| count > 0),
                    shape: Shape::Array { element, count },
                }
            }
            "core::ArrayCursor" => {
                let element = ty_arg(0).expect("an element type");
                let array = Ty::item(
                    ck.db.core_item("Array").expect("core::Array"),
                    vec![Arg::Ty(element), args[1].clone()],
                );
                self.record(
                    ck,
                    &[
                        ("position".to_owned(), Ty::Scalar(Scalar::Usize)),
                        ("array".to_owned(), array),
                    ],
                )
            }
            "core::Untrusted" | "core::Secret" => {
                let inner = ty_arg(0).expect("a wrapped type argument");
                return Some(self.transparent(ck, &inner));
            }
            "hw::Ptr" => Layout {
                size: 8,
                align: 8,
                shape: Shape::Scalar {
                    ty: types::I64,
                    big: false,
                },
                niche: None,
            },
            // A register value is its register's index in its block, which a volatile call reads.
            "hw::Register" => Layout {
                size: 8,
                align: 8,
                shape: Shape::Scalar {
                    ty: types::I64,
                    big: false,
                },
                niche: None,
            },
            "mem::Box" => match ty_arg(0) {
                Some(Ty::Dyn(_)) => pointers(2, Some(0)),
                _ => pointers(1, Some(0)),
            },
            "mem::Id" => Layout {
                size: 4,
                align: 4,
                shape: Shape::Record(vec![field("index", Ty::Scalar(Scalar::U32), 0)]),
                niche: None,
            },
            "mem::Entry" => pointers(2, None),
            "mem::BoundedCursor" => {
                let bounded = Ty::item(
                    ck.db.std_item("mem", "Bounded").expect("mem::Bounded"),
                    args.to_vec(),
                );
                self.record(
                    ck,
                    &[
                        ("position".to_owned(), Ty::Scalar(Scalar::Usize)),
                        ("bounded".to_owned(), bounded),
                    ],
                )
            }
            "mem::Bounded" => {
                let element = ty_arg(0).expect("an element type");
                let array = Ty::item(
                    ck.db.core_item("Array").expect("core::Array"),
                    vec![Arg::Ty(element), args[1].clone()],
                );
                self.record(
                    ck,
                    &[
                        ("len".to_owned(), Ty::Scalar(Scalar::Usize)),
                        ("items".to_owned(), array),
                    ],
                )
            }
            "text::Fixed" => {
                let bytes = Ty::item(
                    ck.db.core_item("Array").expect("core::Array"),
                    vec![Arg::Ty(Ty::Scalar(Scalar::U8)), args[0].clone()],
                );
                self.record(
                    ck,
                    &[
                        ("len".to_owned(), Ty::Scalar(Scalar::Usize)),
                        ("bytes".to_owned(), bytes),
                    ],
                )
            }
            "task::Cancelled" => pointers(1, None),
            path if path.starts_with("core::atomic::") => {
                let scalar = match path.trim_start_matches("core::atomic::Atomic") {
                    "U8" => Scalar::U8,
                    "U16" => Scalar::U16,
                    "U32" => Scalar::U32,
                    "U64" => Scalar::U64,
                    "Usize" => Scalar::Usize,
                    _ => Scalar::Bool,
                };
                let inner = scalar_layout(scalar, ck.db.target.is_machine());
                Layout {
                    size: inner.size,
                    align: inner.align,
                    shape: Shape::Record(vec![field("value", Ty::Scalar(scalar), 0)]),
                    niche: None,
                }
            }
            "task::Poisoned" | "sys::Domain" | "host::Runner" | "hw::Anchor" | "hw::IrqContext" => {
                empty()
            }
            // A machine root grant: the handles of its root allocator, executor, clock and wall
            // clock, and whether its regions, its `hw::Ring0` and its modules were taken;
            // `boot::Machine` carries them into the `boot::Kernel` its `exit_services` returns.
            "boot::Kernel" | "boot::Machine" => self.record(
                ck,
                &[
                    ("alloc".to_owned(), Ty::Scalar(Scalar::U64)),
                    ("exec".to_owned(), Ty::Scalar(Scalar::U64)),
                    ("clock".to_owned(), Ty::Scalar(Scalar::U64)),
                    ("wall".to_owned(), Ty::Scalar(Scalar::U64)),
                    ("regions".to_owned(), Ty::Scalar(Scalar::Bool)),
                    ("ring0".to_owned(), Ty::Scalar(Scalar::Bool)),
                    ("modules".to_owned(), Ty::Scalar(Scalar::Bool)),
                ],
            ),
            // A physical frame, a page-table root owning one, and a user address are one
            // address; an installed table's token holds the table's box.
            "hw::PhysFrame" | "hw::PageTableRoot" | "hw::UserAddr" => {
                self.record(ck, &[("address".to_owned(), Ty::Scalar(Scalar::U64))])
            }
            "hw::IdtInstalled" => pointers(1, Some(0)),
            // An exception's trap frame: the address of the state its seam saved, and a page
            // fault's faulting address.
            "hw::TrapFrame" => self.record(
                ck,
                &[
                    ("saved".to_owned(), Ty::Scalar(Scalar::U64)),
                    ("fault".to_owned(), Ty::Scalar(Scalar::U64)),
                ],
            ),
            // An interrupt descriptor table, as words.
            "hw::Idt" => {
                let words = super::machine::IDT_BYTES / 8;
                let gates = Ty::item(
                    ck.db.core_item("Array").expect("core::Array"),
                    vec![
                        Arg::Ty(Ty::Scalar(Scalar::U64)),
                        Arg::Const(Const::Int(i128::from(words))),
                    ],
                );
                self.record(ck, &[("gates".to_owned(), gates)])
            }
            // A region of physical memory: its base and its length in bytes.
            "hw::Region" => self.record(
                ck,
                &[
                    ("base".to_owned(), Ty::Scalar(Scalar::U64)),
                    ("len".to_owned(), Ty::Scalar(Scalar::U64)),
                ],
            ),
            // A register window and a simulated register block each hold one pointer: the sim
            // block its window reaches, or on a machine target the mapped address or port.
            "hw::Mmio" | "hw::Ports" | "hw::sim::Registers" => pointers(1, None),
            // A DMA buffer holds its box, whose contents never move.
            "hw::Dma" | "hw::DmaShared" => pointers(1, Some(0)),
            // A device behind an IRQ-safe lock is the address of its record, and a per-CPU value
            // that of its storage.
            "hw::IrqShared" | "hw::PerCpu" => pointers(1, None),
            // A spin lock is its lock word, then its value.
            "hw::SpinLock" => {
                let value = ty_arg(0).expect("a wrapped type");
                self.record(
                    ck,
                    &[("lock".to_owned(), word), ("value".to_owned(), value)],
                )
            }
            // A one-slot mailbox: its lock word, whether it holds a value, the waiting task and
            // the value.
            "hw::Signal" => {
                let value = ty_arg(0).expect("a signalled type");
                self.record(
                    ck,
                    &[
                        ("lock".to_owned(), word.clone()),
                        ("stored".to_owned(), Ty::Scalar(Scalar::Bool)),
                        ("waiter".to_owned(), word),
                        ("value".to_owned(), value),
                    ],
                )
            }
            // A domain run's abandoner: its lock word, whether it was triggered, and the record
            // of the domain it runs.
            "sys::Abandoner" => self.record(
                ck,
                &[
                    ("lock".to_owned(), word.clone()),
                    ("triggered".to_owned(), word.clone()),
                    ("domain".to_owned(), word),
                ],
            ),
            _ => return None,
        })
    }

    /// The layout of a type wrapping another without changing its representation.
    fn transparent(&mut self, ck: &Checker, inner: &Ty) -> Layout {
        let layout = self.of(ck, inner);
        Layout {
            size: layout.size,
            align: layout.align,
            shape: match &layout.shape {
                Shape::Scalar { ty, big } => Shape::Scalar { ty: *ty, big: *big },
                Shape::Empty => Shape::Empty,
                _ => Shape::Record(vec![field("value", inner.clone(), 0)]),
            },
            niche: layout.niche.clone(),
        }
    }

    /// Fields laid out in declaration order, each at the next offset its alignment allows.
    pub fn record(&mut self, ck: &Checker, types: &[(String, Ty)]) -> Layout {
        let mut offset = 0u64;
        let mut align = 1u64;
        let mut fields = Vec::new();
        let mut niche: Option<Niche> = None;
        let mut nonempty = false;
        for (name, ty) in types {
            let layout = self.of(ck, ty);
            offset = offset.next_multiple_of(layout.align);
            align = align.max(layout.align);
            nonempty |= !matches!(layout.shape, Shape::Empty);
            if let Some(found) = &layout.niche
                && niche
                    .as_ref()
                    .is_none_or(|best| found.count() > best.count())
            {
                niche = Some(found.at(offset));
            }
            fields.push(field(name, ty.clone(), offset));
            offset += layout.size;
        }
        Layout {
            size: offset.next_multiple_of(align),
            align,
            shape: if nonempty {
                Shape::Record(fields)
            } else {
                Shape::Empty
            },
            niche,
        }
    }

    fn enumeration(&mut self, ck: &Checker, variants: Vec<(String, Vec<(String, Ty)>)>) -> Layout {
        let payloads: Vec<Layout> = variants
            .iter()
            .map(|(_, fields)| self.record(ck, fields))
            .collect();
        let carrying: Vec<usize> = payloads
            .iter()
            .enumerate()
            .filter(|(_, payload)| !matches!(payload.shape, Shape::Empty))
            .map(|(index, _)| index)
            .collect();
        let units = variants.len() as u128 - 1;
        if let [payload] = carrying[..]
            && let Some(niche) = &payloads[payload].niche
            && niche.count() >= units
        {
            let values = (0..units).map(|n| niche.nth(n)).collect();
            let layout = &payloads[payload];
            let shape = EnumShape {
                variants: variants
                    .into_iter()
                    .zip(&payloads)
                    .map(|((name, declared), payload)| Variant {
                        name,
                        fields: variant_fields(&declared, payload, 0),
                    })
                    .collect(),
                tag: Tag::Packed {
                    payload,
                    niche: niche.clone(),
                    values,
                },
            };
            return Layout {
                size: layout.size,
                align: layout.align,
                niche: Some(niche.after(units)),
                shape: Shape::Enum(shape),
            };
        }
        let tag = match variants.len() {
            0..=256 => types::I8,
            257..=65_536 => types::I16,
            _ => types::I32,
        };
        let tag_size = u64::from(tag.bytes());
        let payload_align = payloads
            .iter()
            .map(|payload| payload.align)
            .max()
            .unwrap_or(1);
        let start = tag_size.next_multiple_of(payload_align);
        let align = payload_align.max(tag_size);
        let largest = payloads
            .iter()
            .map(|payload| payload.size)
            .max()
            .unwrap_or(0);
        let shape = EnumShape {
            variants: variants
                .into_iter()
                .zip(&payloads)
                .map(|((name, declared), payload)| Variant {
                    name,
                    fields: variant_fields(&declared, payload, start),
                })
                .collect(),
            tag: Tag::Index(tag),
        };
        Layout {
            size: (start + largest).next_multiple_of(align),
            align,
            shape: Shape::Enum(shape),
            niche: None,
        }
    }

    fn layout_item(&mut self, ck: &Checker, item: ItemId, repr: &LayoutRepr) -> Layout {
        let ItemKind::Layout(layout) = &ck.db.ast(item).kind else {
            unreachable!("a layout item");
        };
        let data = ck.db.item(item);
        let number = |token| {
            crate::check::sig::int_literal(data.text(token)).expect("a layout literal") as u64
        };
        match repr {
            LayoutRepr::Integer { ty, order } => {
                let scalar = Scalar::from_name(&ty.text).expect("a layout representation");
                let big = order.as_ref().is_some_and(|order| order.text == "be");
                let base = scalar_layout(scalar, ck.db.target.is_machine());
                let Shape::Scalar { ty: repr, .. } = base.shape else {
                    unreachable!("an integer representation");
                };
                let width = scalar.bits();
                let niche = match ck.layout_shape(item) {
                    Some(crate::check::lowlevel::LayoutShape::Tags(values)) => Some(Niche {
                        offset: 0,
                        ty: repr,
                        big,
                        bit: 0,
                        width,
                        used: 0,
                        kind: NicheKind::Except(
                            values.iter().map(|value| u128::from(*value)).collect(),
                        ),
                    }),
                    _ => self.layout_fields_niche(ck, item, layout, repr, big),
                };
                Layout {
                    size: base.size,
                    align: base.align,
                    shape: Shape::Scalar { ty: repr, big },
                    niche,
                }
            }
            LayoutRepr::Bytes { size, order } => {
                let big = order.as_ref().is_some_and(|order| order.text == "be");
                let types = match &ck.sig(item).kind {
                    SigKind::Layout { fields, .. } => fields
                        .iter()
                        .map(|field| (field.name.clone(), field.ty.clone()))
                        .collect::<Vec<_>>(),
                    _ => Vec::new(),
                };
                let mut fields = Vec::new();
                let mut niche: Option<Niche> = None;
                for (declared, (name, ty)) in layout.fields.iter().zip(types) {
                    let LayoutFieldKind::Typed { at, .. } = &declared.kind else {
                        continue;
                    };
                    let offset = number(*at);
                    let field_layout = self.of(ck, &ty);
                    if let Some(found) = &field_layout.niche
                        && niche
                            .as_ref()
                            .is_none_or(|best| found.count() > best.count())
                    {
                        niche = Some(found.at(offset));
                    }
                    fields.push(field(&name, ty, offset));
                }
                Layout {
                    size: number(*size),
                    align: 1,
                    shape: Shape::Bytes { fields, big },
                    niche,
                }
            }
        }
    }

    /// The niche of an integer layout of fields: that of its field of a layout or array type
    /// with the most invalid patterns, at that field's bits.
    fn layout_fields_niche(
        &mut self,
        ck: &Checker,
        item: ItemId,
        layout: &crate::syntax::ast::LayoutItem,
        repr: Type,
        big: bool,
    ) -> Option<Niche> {
        let types = match &ck.sig(item).kind {
            SigKind::Layout { fields, .. } => fields
                .iter()
                .map(|field| field.ty.clone())
                .collect::<Vec<_>>(),
            _ => return None,
        };
        let data = ck.db.item(item);
        let mut best: Option<Niche> = None;
        for (declared, ty) in layout.fields.iter().zip(types) {
            let LayoutFieldKind::Typed { at, .. } = &declared.kind else {
                continue;
            };
            let Some(found) = self.of(ck, &ty).niche.clone() else {
                continue;
            };
            let bit =
                crate::check::sig::int_literal(data.text(*at)).expect("a bit position") as u32;
            if best
                .as_ref()
                .is_none_or(|known| found.count() > known.count())
            {
                best = Some(Niche {
                    offset: 0,
                    ty: repr,
                    big,
                    bit: bit + found.bit,
                    ..found
                });
            }
        }
        best
    }

    /// The offset and type of a struct field or of a field of a variant's payload.
    pub fn field(&mut self, ck: &Checker, ty: &Ty, variant: Option<&str>, name: &str) -> Field {
        let layout = self.of(ck, ty);
        let found = match (&layout.shape, variant) {
            (Shape::Record(fields), None) | (Shape::Bytes { fields, .. }, None) => {
                fields.iter().find(|field| field.name == name)
            }
            (Shape::Enum(shape), Some(variant)) => shape
                .variants
                .iter()
                .find(|candidate| candidate.name == variant)
                .and_then(|variant| variant.fields.iter().find(|field| field.name == name)),
            _ => None,
        };
        found
            .cloned()
            .unwrap_or_else(|| panic!("no field {name} of {variant:?} in {ty:?}"))
    }
}

/// The fields of a variant's payload laid out as `payload` from `start`; a payload holding no
/// bytes keeps its declared fields, all at `start`.
fn variant_fields(declared: &[(String, Ty)], payload: &Layout, start: u64) -> Vec<Field> {
    match &payload.shape {
        Shape::Record(fields) => fields
            .iter()
            .map(|field| Field {
                offset: field.offset + start,
                ..field.clone()
            })
            .collect(),
        _ => declared
            .iter()
            .map(|(name, ty)| field(name, ty.clone(), start))
            .collect(),
    }
}

fn field(name: &str, ty: Ty, offset: u64) -> Field {
    Field {
        name: name.to_owned(),
        ty,
        offset,
    }
}

/// Words of 8 bytes with the given names.
fn words(names: &[&str]) -> Layout {
    Layout {
        size: 8 * names.len() as u64,
        align: 8,
        shape: Shape::Record(
            names
                .iter()
                .enumerate()
                .map(|(index, name)| field(name, Ty::Scalar(Scalar::U64), 8 * index as u64))
                .collect(),
        ),
        niche: None,
    }
}

/// A generational handle: a slot index and its generation.
fn handle() -> Layout {
    Layout {
        size: 8,
        align: 4,
        shape: Shape::Record(vec![
            field("index", Ty::Scalar(Scalar::U32), 0),
            field("generation", Ty::Scalar(Scalar::U32), 4),
        ]),
        niche: None,
    }
}

fn empty() -> Layout {
    Layout {
        size: 0,
        align: 1,
        shape: Shape::Empty,
        niche: None,
    }
}

/// `count` pointers, the one at index `null` having the zero address as its niche.
fn pointers(count: u64, null: Option<u64>) -> Layout {
    Layout {
        size: count * POINTER,
        align: POINTER,
        shape: Shape::Record(
            (0..count)
                .map(|index| {
                    field(
                        &format!("word{index}"),
                        Ty::Scalar(Scalar::Usize),
                        index * 8,
                    )
                })
                .collect(),
        ),
        niche: null.map(|index| Niche {
            offset: index * POINTER,
            ty: types::I64,
            big: false,
            bit: 0,
            width: 64,
            used: 0,
            kind: NicheKind::Range { first: 0, count: 1 },
        }),
    }
}

/// The type of a scalar's values. Code for a machine target uses no floating-point register, so
/// there a float is held as its bit pattern.
pub fn scalar_type(scalar: Scalar, machine: bool) -> Type {
    match scalar {
        Scalar::U8 | Scalar::I8 | Scalar::Bool => types::I8,
        Scalar::U16 | Scalar::I16 => types::I16,
        Scalar::U32 | Scalar::I32 | Scalar::Char => types::I32,
        Scalar::U64 | Scalar::I64 | Scalar::Usize | Scalar::Isize => types::I64,
        Scalar::F32 if machine => types::I32,
        Scalar::F64 if machine => types::I64,
        Scalar::F32 => types::F32,
        Scalar::F64 => types::F64,
    }
}

fn scalar_layout(scalar: Scalar, machine: bool) -> Layout {
    let ty = scalar_type(scalar, machine);
    let size = u64::from(ty.bytes());
    let niche = match scalar {
        Scalar::Bool => Some((2, 254)),
        Scalar::Char => Some((0x11_0000, (1u128 << 32) - 0x11_0000)),
        _ => None,
    };
    Layout {
        size,
        align: size,
        shape: Shape::Scalar { ty, big: false },
        niche: niche.map(|(first, count)| Niche {
            offset: 0,
            ty,
            big: false,
            bit: 0,
            width: scalar.bits(),
            used: 0,
            kind: NicheKind::Range { first, count },
        }),
    }
}
