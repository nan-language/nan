//! Schema trees: the description of a type's encoded structure, and the schema hash, the hash of
//! the `mem::wire` encoding of a tree's root node.

use std::collections::BTreeMap;

use super::Checker;
use super::db::{ItemId, PackageId, TypeKey};
use super::sig::{Payload, SigKind};
use super::ty::{Arg, Const, Scalar, Subst, Ty};
use crate::encoding::uvar;
use crate::hash::Hash;
use crate::syntax::ast::{ItemKind, RetiredNew, Visibility};

/// A node of a schema tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Bool,
    Char,
    Text,
    Bytes,
    Unsigned(u64),
    Signed(u64),
    Float(u64),
    BoundedText(u64),
    Sequence {
        element: Box<Node>,
        length: Option<u64>,
        capacity: Option<u64>,
        unique: bool,
    },
    Map {
        key: Box<Node>,
        value: Box<Node>,
    },
    Record {
        open: bool,
        fields: Vec<Field>,
    },
    Variant {
        open: bool,
        variants: Vec<Variant>,
    },
    Back(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub key: String,
    pub aliases: Vec<String>,
    pub defaulted: bool,
    pub value: Node,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variant {
    pub key: String,
    pub aliases: Vec<String>,
    pub payload: Option<Node>,
}

impl Node {
    /// Whether this tree, a type's new one, accepts an old one: nodes of one kind whose parts
    /// accept the old parts, a record matching each old field by key or alias, keeping one only
    /// when open and defaulting each new one, and a variant node matching each old variant.
    pub fn accepts(&self, old: &Node) -> bool {
        match (self, old) {
            (Node::Unsigned(new), Node::Unsigned(old))
            | (Node::Signed(new), Node::Signed(old))
            | (Node::Float(new), Node::Float(old))
            | (Node::Back(new), Node::Back(old)) => new == old,
            (Node::BoundedText(new), Node::BoundedText(old)) => new >= old,
            (
                Node::Sequence {
                    element,
                    length,
                    capacity,
                    unique,
                },
                Node::Sequence {
                    element: old_element,
                    length: old_length,
                    capacity: old_capacity,
                    unique: old_unique,
                },
            ) => {
                element.accepts(old_element)
                    && length == old_length
                    && capacity
                        .is_none_or(|capacity| old_capacity.is_some_and(|old| capacity >= old))
                    && (!unique || *old_unique)
            }
            (
                Node::Map { key, value },
                Node::Map {
                    key: old_key,
                    value: old_value,
                },
            ) => key.accepts(old_key) && value.accepts(old_value),
            (
                Node::Record { open, fields },
                Node::Record {
                    fields: old_fields, ..
                },
            ) => {
                let matching = |field: &Field, key: &str| {
                    field.key == key || field.aliases.iter().any(|alias| alias == key)
                };
                let kept = old_fields.iter().all(|old| {
                    match fields.iter().find(|field| matching(field, &old.key)) {
                        Some(field) => field.value.accepts(&old.value),
                        None => *open,
                    }
                });
                let defaulted = fields.iter().all(|field| {
                    field.defaulted || old_fields.iter().any(|old| matching(field, &old.key))
                });
                kept && defaulted
            }
            (
                Node::Variant { variants, .. },
                Node::Variant {
                    variants: old_variants,
                    ..
                },
            ) => old_variants.iter().all(|old| {
                variants.iter().any(|variant| {
                    let matched = variant.key == old.key || variant.aliases.contains(&old.key);
                    matched
                        && match (&variant.payload, &old.payload) {
                            (None, None) => true,
                            (Some(new), Some(old)) => new.accepts(old),
                            _ => false,
                        }
                })
            }),
            (new, old) => std::mem::discriminant(new) == std::mem::discriminant(old),
        }
    }

    /// The node's `mem::wire` encoding: a variant keyed by its name, with a record of its fields
    /// as payload when it has fields.
    pub fn wire(&self) -> Vec<u8> {
        let (name, fields): (&str, Vec<(&str, Vec<u8>)>) = match self {
            Node::Bool => ("Bool", Vec::new()),
            Node::Char => ("Char", Vec::new()),
            Node::Text => ("Text", Vec::new()),
            Node::Bytes => ("Bytes", Vec::new()),
            Node::Unsigned(width) => ("Unsigned", vec![("width", unsigned(*width))]),
            Node::Signed(width) => ("Signed", vec![("width", unsigned(*width))]),
            Node::Float(width) => ("Float", vec![("width", unsigned(*width))]),
            Node::BoundedText(capacity) => ("BoundedText", vec![("capacity", unsigned(*capacity))]),
            Node::Sequence {
                element,
                length,
                capacity,
                unique,
            } => (
                "Sequence",
                vec![
                    ("capacity", option(capacity.map(unsigned))),
                    ("element", element.wire()),
                    ("length", option(length.map(unsigned))),
                    ("unique", vec![u8::from(*unique)]),
                ],
            ),
            Node::Map { key, value } => ("Map", vec![("key", key.wire()), ("value", value.wire())]),
            Node::Record { open, fields } => {
                let mut sorted: Vec<&Field> = fields.iter().collect();
                sorted.sort_by(|left, right| left.key.cmp(&right.key));
                let fields = sequence(sorted.iter().map(|field| {
                    record(vec![
                        (
                            "aliases",
                            sequence(field.aliases.iter().map(|alias| text(alias))),
                        ),
                        ("defaulted", vec![u8::from(field.defaulted)]),
                        ("key", text(&field.key)),
                        ("value", field.value.wire()),
                    ])
                }));
                (
                    "Record",
                    vec![("fields", fields), ("open", vec![u8::from(*open)])],
                )
            }
            Node::Variant { open, variants } => {
                let mut sorted: Vec<&Variant> = variants.iter().collect();
                sorted.sort_by(|left, right| left.key.cmp(&right.key));
                let variants = sequence(sorted.iter().map(|variant| {
                    record(vec![
                        (
                            "aliases",
                            sequence(variant.aliases.iter().map(|alias| text(alias))),
                        ),
                        ("key", text(&variant.key)),
                        ("payload", option(variant.payload.as_ref().map(Node::wire))),
                    ])
                }));
                (
                    "Variant",
                    vec![("open", vec![u8::from(*open)]), ("variants", variants)],
                )
            }
            Node::Back(depth) => ("Back", vec![("depth", unsigned(*depth))]),
        };
        let mut out = text(name);
        match fields.is_empty() {
            true => out.push(0),
            false => {
                let payload = record(fields);
                out.push(1);
                uvar(&mut out, payload.len() as u128);
                out.extend(payload);
            }
        }
        out
    }

    pub fn hash(&self) -> Hash {
        Hash::of(&self.wire())
    }
}

fn unsigned(value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    uvar(&mut out, u128::from(value));
    out
}

fn text(value: &str) -> Vec<u8> {
    let mut out = unsigned(value.len() as u64);
    out.extend(value.as_bytes());
    out
}

fn sequence(elements: impl Iterator<Item = Vec<u8>>) -> Vec<u8> {
    let elements: Vec<Vec<u8>> = elements.collect();
    let mut out = unsigned(elements.len() as u64);
    for element in elements {
        out.extend(element);
    }
    out
}

/// A record: its field count, then per field its key, its value's length and its value; the
/// fields given in ascending byte order of keys.
fn record(fields: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    let mut out = unsigned(fields.len() as u64);
    for (key, value) in fields {
        out.extend(text(key));
        uvar(&mut out, value.len() as u128);
        out.extend(value);
    }
    out
}

/// An `Option` value as `mem::wire` encodes it: the variant `None`, or `Some` with its payload.
fn option(value: Option<Vec<u8>>) -> Vec<u8> {
    match value {
        None => {
            let mut out = text("None");
            out.push(0);
            out
        }
        Some(payload) => {
            let mut out = text("Some");
            out.push(1);
            uvar(&mut out, payload.len() as u128);
            out.extend(payload);
            out
        }
    }
}

/// A walk making a schema tree: the struct and enum instantiations enclosing the node being
/// made, each with the number of `Record` and `Variant` nodes made since it, and the
/// instantiations found of packages other than `outside`, while outside every other found one.
#[derive(Default)]
struct Walk {
    enclosing: Vec<(Ty, u64)>,
    outside: Option<PackageId>,
    found: Vec<Ty>,
}

impl Checker<'_> {
    /// The schema tree of a type implementing `Encode`.
    pub fn schema(&self, ty: &Ty) -> Node {
        self.schema_within(ty, &mut Walk::default())
    }

    /// The struct and enum instantiations of packages other than `package` that the schema tree
    /// of a type makes nodes for, each outside every other.
    pub fn foreign_schemas(&self, ty: &Ty, package: PackageId) -> Vec<Ty> {
        let mut walk = Walk {
            outside: Some(package),
            ..Walk::default()
        };
        self.schema_within(ty, &mut walk);
        walk.found
    }

    /// Each non-generic `pub` struct or enum of a package implementing `Encode`, by canonical
    /// path.
    pub fn exported_encoded(&self, package: PackageId) -> BTreeMap<String, Ty> {
        let mut found = BTreeMap::new();
        let Some(encode) = self.db.core_item("Encode") else {
            return found;
        };
        for &module in &self.db.package(package).modules {
            for &item in &self.db.module(module).items {
                let ast = self.db.ast(item);
                let typed = matches!(ast.kind, ItemKind::Struct(_) | ItemKind::Enum(_));
                if !typed
                    || ast.visibility != Visibility::Pub
                    || !self.sig(item).generics.is_empty()
                {
                    continue;
                }
                let ty = Ty::item(item, Vec::new());
                if self.implements(&ty, encode)
                    && let Some(path) = self.canonical_path(item)
                {
                    found.insert(path, ty);
                }
            }
        }
        found
    }

    /// The types whose schema trees a package's code root records, by canonical path: its
    /// exported types implementing `Encode` and each component state it declares, the target of
    /// an impl line naming `sys::Component`.
    pub fn recorded_schemas(&self, package: PackageId) -> BTreeMap<String, Ty> {
        let mut found = self.exported_encoded(package);
        for module in &self.db.package(package).modules {
            for item in &self.db.module(*module).items {
                if let SigKind::Impl {
                    target: Some(TypeKey::Item(target)),
                    interface: Some(bound),
                    ..
                } = &self.sig(*item).kind
                    && self.db.canonical(bound.interface) == "sys::Component"
                {
                    found.insert(self.db.canonical(*target), Ty::item(*target, Vec::new()));
                }
            }
        }
        found
    }

    /// The schema of a type within a walk.
    fn schema_within(&self, ty: &Ty, walk: &mut Walk) -> Node {
        let width = |scalar: Scalar| u64::from(scalar.bits());
        match ty {
            Ty::Scalar(Scalar::Bool) => return Node::Bool,
            Ty::Scalar(Scalar::Char) => return Node::Char,
            Ty::Scalar(scalar) if scalar.is_float() => return Node::Float(width(*scalar)),
            Ty::Scalar(scalar) if scalar.is_signed() => return Node::Signed(width(*scalar)),
            Ty::Scalar(scalar) => return Node::Unsigned(width(*scalar)),
            Ty::Unit => {
                return Node::Record {
                    open: false,
                    fields: Vec::new(),
                };
            }
            _ => {}
        }
        let Ty::Item(item, args) = ty else {
            return Node::Record {
                open: false,
                fields: Vec::new(),
            };
        };
        let arg = |index: usize| {
            args.get(index)
                .and_then(Arg::ty)
                .cloned()
                .unwrap_or(Ty::Unit)
        };
        let count = |index: usize| match args.get(index) {
            Some(Arg::Const(Const::Int(value))) => Some(*value as u64),
            _ => None,
        };
        let path = self.db.canonical(*item);
        match path.as_str() {
            "text::String" => return Node::Text,
            "mem::Bytes" => return Node::Bytes,
            "text::Fixed" => return Node::BoundedText(count(0).unwrap_or_default()),
            "mem::Box" | "mem::Shared" | "core::Untrusted" => {
                return self.schema_within(&arg(0), walk);
            }
            "mem::Id" => return Node::Unsigned(32),
            "core::Array" | "mem::Bounded" | "mem::Set" | "mem::List" => {
                let element = Box::new(self.schema_within(&arg(0), walk));
                return Node::Sequence {
                    element,
                    length: (path == "core::Array").then(|| count(1)).flatten(),
                    capacity: (path == "mem::Bounded").then(|| count(1)).flatten(),
                    unique: path == "mem::Set",
                };
            }
            "mem::Map" => {
                return Node::Map {
                    key: Box::new(self.schema_within(&arg(0), walk)),
                    value: Box::new(self.schema_within(&arg(1), walk)),
                };
            }
            _ => {}
        }
        if let Some(at) = walk.enclosing.iter().position(|(known, _)| known == ty) {
            let depth = walk.enclosing[at..]
                .iter()
                .map(|(_, made)| made)
                .sum::<u64>();
            return Node::Back(depth);
        }
        let outside = walk.outside;
        if outside.is_some_and(|package| self.db.package_of(*item) != package) {
            if !walk.found.contains(ty) {
                walk.found.push(ty.clone());
            }
            walk.outside = None;
        }
        let sig = self.sig(*item);
        let subst = Subst::of(sig.generics.iter().map(|def| def.param), args);
        let open = self.db.ast(*item).is_open();
        walk.enclosing.push((ty.clone(), 1));
        let node = match &sig.kind {
            SigKind::Struct { fields } => Node::Record {
                open,
                fields: fields
                    .iter()
                    .filter(|field| !field.is_kept())
                    .map(|field| {
                        let value_ty = subst.ty(&field.ty);
                        let defaulted = open && self.implements_default(&value_ty);
                        Field {
                            key: field.name.clone(),
                            aliases: self.key_aliases_of(*item, &field.name),
                            defaulted,
                            value: self.nested(&value_ty, walk),
                        }
                    })
                    .collect(),
            },
            SigKind::Enum { variants } => Node::Variant {
                open,
                variants: variants
                    .iter()
                    .map(|variant| Variant {
                        key: variant.name.clone(),
                        aliases: self.key_aliases_of(*item, &variant.name),
                        payload: match &variant.payload {
                            Payload::None => None,
                            Payload::Single(payload) => Some(self.nested(&subst.ty(payload), walk)),
                            Payload::Labelled(fields) => Some(Node::Record {
                                open: false,
                                fields: fields
                                    .iter()
                                    .map(|(label, payload)| Field {
                                        key: label.clone(),
                                        aliases: Vec::new(),
                                        defaulted: false,
                                        value: self.nested(&subst.ty(payload), walk),
                                    })
                                    .collect(),
                            }),
                        },
                    })
                    .collect(),
            },
            _ => Node::Record {
                open,
                fields: Vec::new(),
            },
        };
        walk.enclosing.pop();
        walk.outside = outside;
        node
    }

    /// The node of a constituent, counting the node made for each enclosing instantiation.
    fn nested(&self, ty: &Ty, walk: &mut Walk) -> Node {
        let node = self.schema_within(ty, walk);
        if matches!(node, Node::Record { .. } | Node::Variant { .. })
            && let Some((_, made)) = walk.enclosing.last_mut()
        {
            *made += 1;
        }
        node
    }

    fn implements_default(&self, ty: &Ty) -> bool {
        self.db
            .core_item("Default")
            .is_some_and(|interface| self.implements(ty, interface))
    }

    /// The aliases of a field's or variant's key: the old names of the `preserves` tombstones of
    /// the type's package retiring a member of the type into it.
    fn key_aliases_of(&self, owner: ItemId, member: &str) -> Vec<String> {
        let target = format!("{}.{member}", self.db.canonical(owner));
        let mut aliases: Vec<String> = Vec::new();
        for (old, tombstones) in &self.db.retired {
            let Some(old_member) = old.strip_prefix(&format!("{}.", self.db.canonical(owner)))
            else {
                continue;
            };
            let renamed = tombstones.iter().any(|tombstone| {
                matches!(&self.db.ast(*tombstone).kind, ItemKind::Retired(retired)
                    if matches!(&retired.new, RetiredNew::Rewrite { expr, preserves: true }
                        if self.rewrite_names(*tombstone, expr, &target)))
            });
            if renamed {
                aliases.push(old_member.to_owned());
            }
        }
        aliases.sort();
        aliases
    }

    /// Whether a tombstone's rewrite names a member by its canonical path.
    fn rewrite_names(
        &self,
        tombstone: ItemId,
        expr: &crate::syntax::ast::Expr,
        target: &str,
    ) -> bool {
        let data = self.db.item(tombstone);
        let written: Vec<&str> = (expr.token..=expr.last)
            .map(|token| data.text(token))
            .collect();
        let written = written.concat();
        let Some((owner, member)) = written.rsplit_once('.') else {
            return false;
        };
        let segments: Vec<&str> = owner.split("::").collect();
        self.canonically(data.module, &segments)
            .is_some_and(|path| format!("{path}.{member}") == target)
    }
}

#[cfg(test)]
mod tests;
