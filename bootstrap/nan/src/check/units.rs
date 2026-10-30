//! The units of a package in the canonical NIR encoding, as its code root's NIR stream holds
//! them: each unit's path, kind, targets, visibility, head, declaration, properties, functions,
//! digests under each target, premises, statuses and spans; and the encoding of const values.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::Checker;
use super::db::{ItemId, PackageId};
use super::digest::Digest;
use super::props::Props;
use super::reversibility::Class;
use super::sig::{FnSig, GenericDef, Payload, SigKind};
use super::testing::Target as TestTarget;
use super::ty::{Arg, Param, Scalar, Ty};
use crate::encoding::{Record, bytes, list, uvar};
use crate::interp::{ConstValue, FnVal, Val};
use crate::nir::encode::{Encoder, Names, literal_value};
use crate::nir::{Callable, Function, Literal, Op, Role, Span, Unit};
use crate::prover::Status;
use crate::syntax::ast::{
    self, Access, ExprKind, FnBody, FnModifier, ItemKind, LayoutFieldKind, LayoutRepr, Tok,
    TypeModifier, Visibility,
};
use crate::workspace::Target;

/// A package's NIR stream, with the lines its spans start on.
pub struct Stream {
    pub bytes: Vec<u8>,
    /// The first line of each span, by file path as spans write it.
    pub lines: BTreeMap<String, BTreeSet<u32>>,
}

/// A unit encoded but for its targets and digests, with what those need.
struct Encoded {
    path: String,
    /// Parts 1, 2, 4 to 8 and 10 to 12, by key.
    parts: Vec<(u64, Vec<u8>)>,
    /// The digest of each function part 9 covers, by its role and index.
    covered: Vec<(Role, u32)>,
    lines: BTreeMap<String, BTreeSet<u32>>,
}

/// The encoded functions of a unit under one target.
type Targeted = (Target, Vec<Vec<u8>>);

/// The NIR stream of the package named `package` from its checkers, one per target it is
/// checked under, in target order.
pub fn stream(checkers: &[(Target, &Checker)], package: &str) -> Stream {
    let mut units: Vec<(Encoded, Vec<Targeted>)> = Vec::new();
    for (target, ck) in checkers {
        let Some(id) = ck.package_named(package) else {
            continue;
        };
        for item in ck.package_items(id) {
            let Some(encoded) = ck.encoded_unit(item) else {
                continue;
            };
            let digests: Vec<Vec<u8>> = encoded
                .covered
                .iter()
                .map(|(role, index)| ck.digest_record(item, *role, *index))
                .collect();
            match units
                .iter_mut()
                .find(|(known, _)| known.path == encoded.path && known.parts == encoded.parts)
            {
                Some((_, targets)) => targets.push((*target, digests)),
                None => units.push((encoded, vec![(*target, digests)])),
            }
        }
    }
    units.sort_by(|(left, left_targets), (right, right_targets)| {
        (&left.path, left_targets[0].0.name()).cmp(&(&right.path, right_targets[0].0.name()))
    });
    let mut lines: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    let mut out = vec![0x4E, 0x49, 0x52, 0x00];
    uvar(&mut out, 1);
    list(
        &mut out,
        units.into_iter(),
        |out, (encoded, mut targets)| {
            targets.sort_by_key(|(target, _)| target.name());
            for (path, held) in encoded.lines {
                lines.entry(path).or_default().extend(held);
            }
            let mut record = Record::default();
            let mut parts = encoded.parts.into_iter().peekable();
            for key in 1..=12u64 {
                match key {
                    3 => {
                        let mut content = Vec::new();
                        list(&mut content, targets.iter(), |out, (target, _)| {
                            bytes(out, target.name().as_bytes());
                        });
                        record.field(3, content);
                    }
                    9 => {
                        if targets
                            .first()
                            .is_some_and(|(_, digests)| !digests.is_empty())
                        {
                            record.field(9, digest_pairs(&targets));
                        }
                    }
                    _ => {
                        if let Some((_, content)) = parts.next_if(|(part, _)| *part == key) {
                            record.field(key, content);
                        }
                    }
                }
            }
            out.extend(record.finish());
        },
    );
    Stream { bytes: out, lines }
}

/// Part 9: for each covered function, the pairs of a target name and its digest under it.
fn digest_pairs(targets: &[(Target, Vec<Vec<u8>>)]) -> Vec<u8> {
    let count = targets.first().map_or(0, |(_, digests)| digests.len());
    let mut out = Vec::new();
    list(&mut out, 0..count, |out, function| {
        list(out, targets.iter(), |out, (target, digests)| {
            bytes(out, target.name().as_bytes());
            out.extend(&digests[function]);
        });
    });
    out
}

fn number(out: &mut Vec<u8>, value: u64) {
    uvar(out, u128::from(value));
}

fn text(out: &mut Vec<u8>, value: &str) {
    bytes(out, value.as_bytes());
}

fn content(write: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut out = Vec::new();
    write(&mut out);
    out
}

/// How the encoding of an item's unit names what the item reaches.
pub struct ItemNames<'c, 'a> {
    ck: &'c Checker<'a>,
    positions: HashMap<Param, u64>,
}

impl<'c, 'a> ItemNames<'c, 'a> {
    pub fn new(ck: &'c Checker<'a>, item: ItemId) -> ItemNames<'c, 'a> {
        ItemNames {
            ck,
            positions: ck.generic_positions(item),
        }
    }
}

impl Names for ItemNames<'_, '_> {
    fn path(&self, item: ItemId) -> String {
        self.ck
            .canonical_path(item)
            .unwrap_or_else(|| self.ck.db.canonical(item))
    }

    fn method(&self, interface: ItemId, method: u32) -> String {
        match &self.ck.sig(interface).kind {
            SigKind::Interface { methods } => methods[method as usize].name.clone(),
            _ => panic!("a method of an interface"),
        }
    }

    fn register(&self, block: ItemId, register: u32) -> String {
        match &self.ck.sig(block).kind {
            SigKind::Registers { registers } => registers[register as usize].name.clone(),
            _ => panic!("a register of a registers block"),
        }
    }

    fn param(&self, param: Param) -> u64 {
        *self
            .positions
            .get(&param)
            .unwrap_or_else(|| panic!("a generic parameter of the unit: {param:?}"))
    }

    fn const_types(&self, callable: &Callable) -> Vec<Option<Scalar>> {
        let generics: Vec<GenericDef> = match callable {
            Callable::Item { item, .. } => match self.ck.fn_sig(*item) {
                Some(head) => head.generics,
                None => self.ck.sig(*item).generics.clone(),
            },
            Callable::Method { interface, method } => match &self.ck.sig(*interface).kind {
                SigKind::Interface { methods } => methods[*method as usize].sig.generics.clone(),
                _ => Vec::new(),
            },
            Callable::Derived { owner, .. } => self.ck.sig(*owner).generics.clone(),
            Callable::Builtin(_) => Vec::new(),
        };
        generics.iter().map(|def| def.constant).collect()
    }

    fn by_reference(&self, ty: &Ty) -> bool {
        !self.ck.nir_copy(ty) && !self.ck.nir_view(ty)
    }
}

/// An item kind's position in the item kind list.
fn kind_position(kind: &ItemKind) -> u64 {
    match kind {
        ItemKind::Fn(_) => 0,
        ItemKind::Struct(_) => 1,
        ItemKind::Enum(_) => 2,
        ItemKind::Interface(_) => 3,
        ItemKind::Impl(_) => 4,
        ItemKind::Layout(_) => 5,
        ItemKind::Registers(_) => 6,
        ItemKind::Const(_) => 7,
        ItemKind::ConstAssert(_) => 8,
        ItemKind::Registry(_) => 9,
        ItemKind::Contribute(_) => 10,
        ItemKind::Test(_) => 11,
        ItemKind::Retired(_) => 12,
        ItemKind::Package => 13,
    }
}

/// A visibility as an optional word of the `visibility` rule.
fn visibility(out: &mut Vec<u8>, visibility: Visibility) {
    match visibility {
        Visibility::Private => number(out, 0),
        Visibility::Pub => {
            number(out, 1);
            number(out, 0);
        }
        Visibility::Pkg => {
            number(out, 1);
            number(out, 1);
        }
    }
}

fn status_position(status: Status) -> u64 {
    match status {
        Status::Proven => 0,
        Status::Assumed => 1,
        Status::Checked => 2,
        Status::Open => 3,
    }
}

impl<'a> Checker<'a> {
    /// The workspace or release package of a name, other than the carried std.
    pub(super) fn package_named(&self, name: &str) -> Option<PackageId> {
        self.db
            .packages
            .iter()
            .position(|package| package.name == name && !package.carried)
            .map(|index| PackageId(index as u32))
    }

    /// The items of a package whose units its NIR stream holds: all but tests whose target is
    /// an item of the package.
    fn package_items(&self, package: PackageId) -> Vec<ItemId> {
        let mut items = Vec::new();
        for &module in &self.db.package(package).modules {
            for &item in &self.db.module(module).items {
                if self.db.in_history(item) {
                    continue;
                }
                if let ItemKind::Test(test) = &self.db.ast(item).kind {
                    let target = match self.test_target(item, test) {
                        Some(TestTarget::Function(target) | TestTarget::Derived(target)) => {
                            Some(target)
                        }
                        None => None,
                    };
                    if target.is_some_and(|target| self.db.package_of(target) == package) {
                        continue;
                    }
                }
                items.push(item);
            }
        }
        items
    }

    /// The position of each generic parameter among a unit's: `Self` in an interface, those of
    /// the type or interface the item is a member of, then its own, an interface method's own
    /// after its interface's.
    fn generic_positions(&self, item: ItemId) -> HashMap<Param, u64> {
        let mut positions = HashMap::new();
        let mut place = |defs: &[GenericDef], from: u64| {
            for (offset, def) in defs.iter().enumerate() {
                positions.insert(def.param, from + offset as u64);
            }
        };
        match &self.db.ast(item).kind {
            ItemKind::Fn(_) => {
                if let Some(head) = self.fn_sig(item) {
                    place(&head.generics, 0);
                }
            }
            ItemKind::Interface(_) => {
                let sig = self.sig(item);
                let own: Vec<GenericDef> = sig
                    .generics
                    .iter()
                    .filter(|def| !def.param.is_self())
                    .cloned()
                    .collect();
                place(&own, 1);
                if let SigKind::Interface { methods } = &sig.kind {
                    for method in methods {
                        place(method.sig.own_generics(), 1 + own.len() as u64);
                    }
                }
                positions.insert(
                    Param {
                        owner: item,
                        index: Param::SELF,
                    },
                    0,
                );
            }
            _ => place(&self.sig(item).generics.clone(), 0),
        }
        positions
    }

    /// An item's unit encoded but for its targets and digests; none for an item without one.
    fn encoded_unit(&self, item: ItemId) -> Option<Encoded> {
        let unit = self.unit(item)?;
        let path = self.canonical_path(item)?;
        let names = ItemNames::new(self, item);
        let encoder = Encoder { names: &names };
        let ast = self.db.ast(item);
        let mut parts = vec![
            (1, content(|out| text(out, &path))),
            (2, content(|out| number(out, kind_position(&ast.kind)))),
            (4, content(|out| visibility(out, ast.visibility))),
        ];
        let head = self.head_record(item, &encoder);
        if !head.is_empty() {
            parts.push((5, head));
        }
        let declaration = self.declaration_record(item, &encoder);
        if !declaration.is_empty() {
            parts.push((6, declaration));
        }
        let properties = self.properties_record(item);
        if !properties.is_empty() {
            parts.push((7, properties));
        }
        if !unit.functions.is_empty() {
            parts.push((
                8,
                content(|out| {
                    list(out, unit.functions.iter(), |out, function| {
                        out.extend(encoder.function(function));
                    });
                }),
            ));
        }
        let premises = self.premise_texts(item, &unit);
        if !premises.is_empty() {
            parts.push((
                10,
                content(|out| list(out, premises.iter(), |out, premise| text(out, premise))),
            ));
        }
        let statuses = self.obligations(item);
        if !statuses.is_empty() {
            parts.push((
                11,
                content(|out| {
                    list(out, statuses.iter(), |out, site| {
                        number(out, status_position(site.status));
                    });
                }),
            ));
        }
        let mut lines = BTreeMap::new();
        if !unit.functions.is_empty() {
            parts.push((12, self.spans(item, &unit, &mut lines)));
        }
        let covered = unit
            .functions
            .iter()
            .filter(|function| matches!(function.role, Role::Body | Role::Closure | Role::Default))
            .map(|function| (function.role, function.index))
            .collect();
        Some(Encoded {
            path,
            parts,
            covered,
            lines,
        })
    }

    /// Part 5: the item's modifiers, its generic parameters with their bounds, and a function's
    /// head and whether consumers unfold its body.
    fn head_record(&self, item: ItemId, encoder: &Encoder) -> Vec<u8> {
        let ast = self.db.ast(item);
        let mut head = Record::default();
        let mut modifiers: Vec<u64> = ast
            .type_modifiers
            .iter()
            .map(|(modifier, _)| match modifier {
                TypeModifier::Closed => 0,
                TypeModifier::Open => 1,
                TypeModifier::Linear => 2,
                TypeModifier::View => 3,
            })
            .collect();
        if let ItemKind::Fn(function) = &ast.kind
            && let Some((modifier, _)) = function.modifier
        {
            modifiers.push(match modifier {
                FnModifier::Proj => 4,
                FnModifier::Interrupt => 5,
                FnModifier::Landing => 6,
                FnModifier::Foreign => 7,
                FnModifier::Validator => 8,
            });
        }
        if !modifiers.is_empty() {
            head.field(1, content(|out| list(out, modifiers.into_iter(), number)));
        }
        let generics: Vec<GenericDef> = match &ast.kind {
            ItemKind::Fn(_) => self
                .fn_sig(item)
                .map(|head| head.own_generics().to_vec())
                .unwrap_or_default(),
            _ => self
                .sig(item)
                .generics
                .iter()
                .filter(|def| !def.param.is_self())
                .cloned()
                .collect(),
        };
        if !generics.is_empty() {
            head.field(2, self.generics_list(&generics, encoder));
        }
        if let ItemKind::Fn(function) = &ast.kind
            && let Some(sig) = self.fn_sig(item)
        {
            encoder.fn_head(&mut head, &sig.fn_ty());
            let transparent =
                matches!(function.body, FnBody::Expr(_)) && self.transparent_seen(item, None);
            head.field(9, content(|out| number(out, u64::from(transparent))));
        }
        head.finish_if_any()
    }

    /// Generic parameters: each a record of its name, then its bounds, or a const parameter's
    /// type.
    fn generics_list(&self, generics: &[GenericDef], encoder: &Encoder) -> Vec<u8> {
        content(|out| {
            list(out, generics.iter(), |out, def| {
                let mut param = Record::default();
                param.field(1, content(|out| text(out, &def.name)));
                match def.constant {
                    Some(scalar) => {
                        param.field(3, content(|out| encoder.ty(out, &Ty::Scalar(scalar))));
                    }
                    None if !def.bounds.is_empty() => param.field(
                        2,
                        content(|out| {
                            list(out, def.bounds.iter(), |out, bound| {
                                encoder.bound(out, bound);
                            });
                        }),
                    ),
                    None => {}
                }
                out.extend(param.finish());
            });
        })
    }

    /// Part 6: what the item declares beyond its head.
    fn declaration_record(&self, item: ItemId, encoder: &Encoder) -> Vec<u8> {
        let ast = self.db.ast(item);
        let sig = self.sig(item);
        let mut declaration = Record::default();
        match (&ast.kind, &sig.kind) {
            (ItemKind::Struct(_), SigKind::Struct { fields }) => {
                declaration.field(
                    1,
                    content(|out| {
                        list(out, fields.iter(), |out, field| {
                            let mut element = Record::default();
                            element.field(1, content(|out| visibility(out, field.visibility)));
                            element.field(2, content(|out| text(out, &field.name)));
                            element.field(3, content(|out| encoder.ty(out, &field.ty)));
                            out.extend(element.finish());
                        });
                    }),
                );
            }
            (ItemKind::Enum(_), SigKind::Enum { variants }) => {
                declaration.field(
                    2,
                    content(|out| {
                        list(out, variants.iter(), |out, variant| {
                            let mut element = Record::default();
                            element.field(1, content(|out| text(out, &variant.name)));
                            element.field(
                                2,
                                content(|out| match &variant.payload {
                                    Payload::None => number(out, 0),
                                    Payload::Single(ty) => {
                                        number(out, 1);
                                        encoder.ty(out, ty);
                                    }
                                    Payload::Labelled(fields) => {
                                        number(out, 2);
                                        list(out, fields.iter(), |out, (label, ty)| {
                                            text(out, label);
                                            encoder.ty(out, ty);
                                        });
                                    }
                                }),
                            );
                            if variant.from {
                                element.field(3, content(|out| number(out, 1)));
                            }
                            out.extend(element.finish());
                        });
                    }),
                );
            }
            (ItemKind::Layout(layout), SigKind::Layout { fields, .. }) => {
                declaration.field(3, self.layout_record(layout, fields, encoder, item));
            }
            (ItemKind::Registers(block), SigKind::Registers { registers }) => {
                declaration.field(
                    4,
                    content(|out| {
                        list(
                            out,
                            block.registers.iter().zip(registers),
                            |out, (written, register)| {
                                let mut element = Record::default();
                                element.field(1, content(|out| text(out, &register.name)));
                                element.field(2, content(|out| encoder.ty(out, &register.ty)));
                                element.field(
                                    3,
                                    content(|out| number(out, self.int(item, written.at))),
                                );
                                if let Some(stride) = written.stride {
                                    element.field(
                                        4,
                                        content(|out| number(out, self.int(item, stride))),
                                    );
                                }
                                element.field(
                                    5,
                                    content(|out| {
                                        number(
                                            out,
                                            match register.access {
                                                Access::Read => 0,
                                                Access::ReadWrite => 1,
                                                Access::Write => 2,
                                                Access::WriteOneClear => 3,
                                            },
                                        );
                                    }),
                                );
                                out.extend(element.finish());
                            },
                        );
                    }),
                );
            }
            (ItemKind::Interface(interface), SigKind::Interface { methods }) => {
                declaration.field(
                    5,
                    content(|out| {
                        list(
                            out,
                            interface.methods.iter().zip(methods),
                            |out, (written, method)| {
                                let mut element = Record::default();
                                element.field(1, content(|out| text(out, &method.name)));
                                element.field(
                                    2,
                                    self.method_head_record(written.proj, &method.sig, encoder),
                                );
                                element.field(
                                    3,
                                    content(|out| number(out, u64::from(method.default))),
                                );
                                out.extend(element.finish());
                            },
                        );
                    }),
                );
            }
            (
                ItemKind::Impl(_),
                SigKind::Impl {
                    target_ty,
                    interface,
                    ..
                },
            ) => {
                let mut line = Record::default();
                line.field(1, content(|out| encoder.ty(out, target_ty)));
                if let Some(bound) = interface {
                    line.field(2, content(|out| encoder.bound(out, bound)));
                }
                declaration.field(6, line.finish());
            }
            (ItemKind::Registry(registry), SigKind::Registry { entry }) => {
                let mut record = Record::default();
                record.field(1, content(|out| encoder.ty(out, entry)));
                record.field(2, content(|out| text(out, &registry.key.text)));
                if !registry.unique.is_empty() {
                    record.field(
                        3,
                        content(|out| {
                            list(out, registry.unique.iter(), |out, name| {
                                text(out, &name.text)
                            });
                        }),
                    );
                }
                declaration.field(7, record.finish());
            }
            (ItemKind::Contribute(contribution), _) => {
                let mut record = Record::default();
                let registry = self
                    .canonically(
                        self.db.module_of(item),
                        &contribution
                            .registry
                            .segments
                            .iter()
                            .map(|name| name.text.as_str())
                            .collect::<Vec<_>>(),
                    )
                    .unwrap_or_else(|| contribution.registry.written());
                record.field(1, content(|out| text(out, &registry)));
                record.field(2, content(|out| text(out, &contribution.name.text)));
                declaration.field(8, record.finish());
            }
            (ItemKind::Const(_), SigKind::Const { ty }) => {
                let mut record = Record::default();
                record.field(1, content(|out| encoder.ty(out, ty)));
                if let Some(value) = self.const_value(item) {
                    record.field(2, self.value_bytes(ty, &value, encoder));
                }
                declaration.field(9, record.finish());
            }
            (ItemKind::Test(test), _) => {
                let target = match self.test_target(item, test) {
                    Some(TestTarget::Function(function)) => self.canonical_path(function),
                    Some(TestTarget::Derived(owner)) => test
                        .member
                        .as_ref()
                        .map(|member| format!("{}.{}", self.db.canonical(owner), member.text)),
                    None => None,
                };
                if let Some(target) = target {
                    declaration.field(10, content(|out| text(out, &target)));
                }
            }
            _ => {}
        }
        declaration.finish_if_any()
    }

    fn layout_record(
        &self,
        layout: &ast::LayoutItem,
        fields: &[super::sig::FieldSig],
        encoder: &Encoder,
        item: ItemId,
    ) -> Vec<u8> {
        let mut record = Record::default();
        record.field(
            1,
            content(|out| match &layout.repr {
                LayoutRepr::Bytes { size, order } => {
                    number(out, 0);
                    number(out, self.int(item, *size));
                    order_word(out, order.as_ref());
                }
                LayoutRepr::Integer { ty, order } => {
                    number(out, 1);
                    text(out, &ty.text);
                    order_word(out, order.as_ref());
                }
            }),
        );
        if !layout.fields.is_empty() {
            record.field(
                2,
                content(|out| {
                    list(out, layout.fields.iter(), |out, field| {
                        let mut element = Record::default();
                        element.field(1, content(|out| text(out, &field.name.text)));
                        element.field(
                            2,
                            content(|out| match &field.kind {
                                LayoutFieldKind::Bit { low, high } => {
                                    number(out, 0);
                                    number(out, self.int(item, *low));
                                    match high {
                                        Some(high) => {
                                            number(out, 1);
                                            number(out, self.int(item, *high));
                                        }
                                        None => number(out, 0),
                                    }
                                }
                                LayoutFieldKind::Rest => number(out, 1),
                                LayoutFieldKind::Typed { at, .. } => {
                                    number(out, 2);
                                    let ty = fields
                                        .iter()
                                        .find(|sig| sig.name == field.name.text)
                                        .map_or(Ty::Error, |sig| sig.ty.clone());
                                    encoder.ty(out, &ty);
                                    number(out, self.int(item, *at));
                                }
                            }),
                        );
                        out.extend(element.finish());
                    });
                }),
            );
        }
        if let Some(tags) = &layout.tags {
            record.field(
                3,
                content(|out| {
                    list(out, tags.iter(), |out, tag| {
                        text(out, &tag.name.text);
                        number(out, self.int(item, tag.value));
                    });
                }),
            );
        }
        record.finish()
    }

    /// The head of an interface method, as part 5 writes a function's.
    fn method_head_record(&self, proj: bool, sig: &FnSig, encoder: &Encoder) -> Vec<u8> {
        let mut head = Record::default();
        if proj {
            head.field(1, content(|out| list(out, [4u64].into_iter(), number)));
        }
        if !sig.own_generics().is_empty() {
            head.field(2, self.generics_list(sig.own_generics(), encoder));
        }
        encoder.fn_head(&mut head, &sig.fn_ty());
        head.finish()
    }

    /// Part 7: a declared type's structural properties, or an interface's Dyn-usability.
    fn properties_record(&self, item: ItemId) -> Vec<u8> {
        let mut record = Record::default();
        match &self.db.ast(item).kind {
            ItemKind::Struct(_) | ItemKind::Enum(_) | ItemKind::Layout(_) => {
                let Props {
                    linear,
                    view,
                    copy,
                    send,
                    inert,
                    any_bits,
                } = self.definition_props(item);
                for (key, holds) in [linear, view, copy, send, inert, any_bits]
                    .into_iter()
                    .enumerate()
                {
                    record.field(key as u64 + 1, content(|out| number(out, u64::from(holds))));
                }
            }
            ItemKind::Interface(_) => {
                record.field(
                    7,
                    content(|out| number(out, u64::from(self.dyn_usable(item)))),
                );
            }
            _ => {}
        }
        record.finish_if_any()
    }

    /// The premise of each trusted block or expression of the item, by premise index.
    fn premise_texts(&self, item: ItemId, unit: &Unit) -> Vec<String> {
        let mut premises: HashMap<Tok, String> = HashMap::new();
        let mut collect = |expr: &ast::Expr| {
            if let ExprKind::Trusted { premise, .. } = &expr.kind {
                premises.insert(expr.token, premise.value.clone());
            }
        };
        match &self.db.ast(item).kind {
            ItemKind::Fn(function) => match &function.body {
                FnBody::Block(block) => block.visit(&mut collect),
                FnBody::Expr(expr) => expr.visit(&mut collect),
                FnBody::None => {}
            },
            ItemKind::Test(test) => test.body.visit(&mut collect),
            ItemKind::Const(constant) => constant.value.visit(&mut collect),
            ItemKind::ConstAssert(condition) => condition.visit(&mut collect),
            ItemKind::Contribute(contribution) => contribution.value.visit(&mut collect),
            _ => {}
        }
        unit.premises
            .iter()
            .map(|token| premises.get(token).cloned().unwrap_or_default())
            .collect()
    }

    /// Part 12: for each function, its span, its parameters' spans with their names, then each
    /// block's instructions' spans, a slot's with its name, and its terminator's span.
    fn spans(
        &self,
        item: ItemId,
        unit: &Unit,
        lines: &mut BTreeMap<String, BTreeSet<u32>>,
    ) -> Vec<u8> {
        let data = self.db.item(item);
        let package = self.db.package(self.db.package_of(item));
        let within = data
            .file
            .path
            .strip_prefix(&format!("{}/", package.source.dir))
            .unwrap_or(&data.file.path);
        let path = format!("{}/{within}", package.name);
        let syntax = data.syntax();
        let mut span = |out: &mut Vec<u8>, span: Span| {
            let first = syntax.tokens[span.first as usize];
            let last = syntax.tokens[span.last as usize];
            let line_start = |line: u32| syntax.lines[line as usize - 1].start;
            text(out, &path);
            number(out, u64::from(first.line));
            number(out, u64::from(first.start - line_start(first.line)));
            number(out, u64::from(last.line));
            number(out, u64::from(last.end - line_start(last.line)));
            lines.entry(path.clone()).or_default().insert(first.line);
        };
        content(|out| {
            list(out, unit.functions.iter(), |out, function: &Function| {
                span(out, function.span);
                list(out, function.params.iter(), |out, param| {
                    span(out, param.span);
                    text(out, &param.name);
                });
                list(out, function.blocks.iter(), |out, block| {
                    list(out, block.insts.iter(), |out, inst| {
                        span(out, inst.span);
                        if let Op::Slot {
                            name: Some(name), ..
                        } = &inst.op
                        {
                            text(out, name);
                        }
                    });
                    span(out, block.term_span);
                });
            });
        })
    }

    /// The digest of a covered function under this checker's target, as part 9 writes it.
    fn digest_record(&self, item: ItemId, role: Role, index: u32) -> Vec<u8> {
        let digest: Digest = self.function_digest(item, role, index);
        let may_trap = self.function_may_trap(item, role, index);
        let mut record = Record::default();
        let facts = [
            digest.wait,
            digest.alloc,
            digest.device,
            digest.privileged,
            digest.sync,
            may_trap,
            digest.terminates,
            digest.float,
        ];
        for (key, holds) in facts.into_iter().enumerate() {
            record.field(key as u64 + 1, content(|out| number(out, u64::from(holds))));
        }
        for (key, set) in [(9, &digest.capabilities), (10, &digest.validators)] {
            record.field(
                key,
                content(|out| match set {
                    Some(set) => {
                        number(out, 0);
                        let mut paths: Vec<String> =
                            set.iter().map(|item| self.db.canonical(*item)).collect();
                        paths.sort();
                        list(out, paths.iter(), |out, path| text(out, path));
                    }
                    None => number(out, 1),
                }),
            );
        }
        if digest.class != Class::Traceless {
            record.field(11, content(|out| digest.class.encode(out)));
        }
        record.finish()
    }

    /// The value of an `INT` token of an item's file.
    fn int(&self, item: ItemId, token: Tok) -> u64 {
        let digits: String = self
            .db
            .item(item)
            .text(token)
            .chars()
            .filter(|c| *c != '_')
            .collect();
        let (radix, digits) = match digits.strip_prefix("0x") {
            Some(hex) => (16, hex.to_owned()),
            None => match digits.strip_prefix("0b") {
                Some(binary) => (2, binary.to_owned()),
                None => (10, digits),
            },
        };
        let end = digits
            .find(|c: char| !c.is_digit(radix))
            .unwrap_or(digits.len());
        u64::from_str_radix(&digits[..end], radix).unwrap_or_default()
    }

    /// A const value in the canonical encoding, following its type.
    pub fn value_bytes(&self, ty: &Ty, value: &ConstValue, encoder: &Encoder) -> Vec<u8> {
        content(|out| self.write_value(out, ty, &value.value, value, encoder))
    }

    fn write_value(
        &self,
        out: &mut Vec<u8>,
        ty: &Ty,
        value: &Val,
        whole: &ConstValue,
        encoder: &Encoder,
    ) {
        match (ty, value) {
            (Ty::Scalar(scalar), Val::Int(int)) => {
                literal_value(out, &Literal::Int(*int, *scalar));
            }
            (_, Val::F32(float)) => {
                literal_value(
                    out,
                    &Literal::Float(u64::from(float.to_bits()), Scalar::F32),
                );
            }
            (_, Val::F64(float)) => {
                literal_value(out, &Literal::Float(float.to_bits(), Scalar::F64));
            }
            (_, Val::Bool(flag)) => number(out, u64::from(*flag)),
            (_, Val::Char(c)) => number(out, u64::from(*c)),
            (_, Val::Unit) => {}
            (Ty::Item(owner, args), Val::Struct(_, fields)) => {
                let sig = self.sig(*owner);
                let subst = super::ty::Subst::of(sig.generics.iter().map(|def| def.param), args);
                let types: Vec<Ty> = match &sig.kind {
                    SigKind::Struct { fields } | SigKind::Layout { fields, .. } => {
                        fields.iter().map(|field| subst.ty(&field.ty)).collect()
                    }
                    _ => Vec::new(),
                };
                for (field, value) in fields
                    .iter()
                    .enumerate()
                    .map(|(at, value)| (types.get(at), value))
                {
                    self.write_value(out, field.unwrap_or(&Ty::Unit), value, whole, encoder);
                }
            }
            (Ty::Item(owner, args), Val::Variant(_, variant, payload)) => {
                number(out, u64::from(*variant));
                let sig = self.sig(*owner);
                let subst = super::ty::Subst::of(sig.generics.iter().map(|def| def.param), args);
                let types: Vec<Ty> = match &sig.kind {
                    SigKind::Enum { variants } => match &variants[*variant as usize].payload {
                        Payload::None => Vec::new(),
                        Payload::Single(ty) => vec![subst.ty(ty)],
                        Payload::Labelled(fields) => {
                            fields.iter().map(|(_, ty)| subst.ty(ty)).collect()
                        }
                    },
                    _ => Vec::new(),
                };
                for (at, value) in payload.iter().enumerate() {
                    self.write_value(
                        out,
                        types.get(at).unwrap_or(&Ty::Unit),
                        value,
                        whole,
                        encoder,
                    );
                }
            }
            (Ty::Item(_, args), Val::Array(elements)) => {
                let element = args.first().and_then(Arg::ty).cloned().unwrap_or(Ty::Unit);
                list(out, elements.iter(), |out, value| {
                    self.write_value(out, &element, value, whole, encoder);
                });
            }
            (Ty::Item(_, args), Val::View(view)) => {
                let elements = whole.elements(view);
                match args.first().and_then(Arg::ty) {
                    Some(element) => list(out, elements.iter(), |out, value| {
                        self.write_value(out, element, value, whole, encoder);
                    }),
                    None => {
                        let text: Vec<u8> =
                            elements.iter().map(|value| value.int() as u8).collect();
                        bytes(out, &text);
                    }
                }
            }
            (_, Val::Fn(function)) => match &**function {
                FnVal::Function(callee) => encoder.fn_ref(out, callee),
                FnVal::Closure { .. } => panic!("a closure as a const value"),
            },
            (_, other) => panic!("a const value of an unencodable kind: {other:?}"),
        }
    }
}

fn order_word(out: &mut Vec<u8>, order: Option<&ast::Name>) {
    match order.map(|name| name.text.as_str()) {
        Some("le") => {
            number(out, 1);
            number(out, 0);
        }
        Some(_) => {
            number(out, 1);
            number(out, 1);
        }
        None => number(out, 0),
    }
}

trait FinishIfAny {
    fn finish_if_any(self) -> Vec<u8>;
}

impl FinishIfAny for Record {
    /// The record's bytes, or nothing for a record without fields.
    fn finish_if_any(self) -> Vec<u8> {
        let bytes = self.finish();
        if bytes == [0] { Vec::new() } else { bytes }
    }
}
