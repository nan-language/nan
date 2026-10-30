//! Derived code: the impl lines and implementing functions that the derives of `Eq`, `Ord`, `Hash`,
//! `Default` and `Render` write for a struct or enum, in canonical format.

use super::Checker;
use super::db::ItemId;
use super::identity::Hashes;
use crate::format::normal::{Known, Reach, forms};
use crate::syntax::ast::{self, ItemKind, Visibility};
use crate::syntax::file::parse;

/// A member of a struct or enum the derived code reaches: a field, or a variant with its payload.
enum Member {
    Field { name: String, ty: String },
    Variant { name: String, payload: Payload },
}

enum Payload {
    None,
    Single,
    /// The labels, in declaration order.
    Labelled(Vec<String>),
}

impl Payload {
    /// The pattern binding a variant's payload, each value named with `prefix`.
    fn pattern(&self, variant: &str, prefix: &str) -> String {
        match self {
            Payload::None => variant.to_owned(),
            Payload::Single => format!("{variant}({prefix})"),
            Payload::Labelled(labels) => {
                let fields: Vec<String> = labels
                    .iter()
                    .map(|label| format!("{label}: {prefix}_{label}"))
                    .collect();
                format!("{variant}({})", fields.join(", "))
            }
        }
    }

    /// The names a pattern of [`Payload::pattern`] binds, labels in `order`.
    fn bound(&self, prefix: &str, sorted: bool) -> Vec<String> {
        match self {
            Payload::None => Vec::new(),
            Payload::Single => vec![prefix.to_owned()],
            Payload::Labelled(labels) => {
                let mut labels = labels.clone();
                if sorted {
                    labels.sort();
                }
                labels
                    .iter()
                    .map(|label| format!("{prefix}_{label}"))
                    .collect()
            }
        }
    }
}

/// The type a derived item is written for.
struct Derived {
    /// Its name as its module writes it, and its type with its parameters.
    name: String,
    own: String,
    /// Its type parameters.
    params: Vec<String>,
    visibility: &'static str,
    members: Vec<Member>,
}

/// An item a derive writes: its canonical path, text and hashes.
pub struct DerivedItem {
    pub path: String,
    pub text: String,
    pub hashes: Hashes,
}

impl Checker<'_> {
    /// The items a struct's or enum's derives write, with the paths and hashes their texts have
    /// in its module.
    pub fn derived(&self, item: ItemId) -> Vec<DerivedItem> {
        let module = self.db.module_of(item);
        let owner = self.db.canonical(item);
        let resolve = |segments: &[&str]| self.canonically(module, segments);
        self.derived_items(item)
            .into_iter()
            .map(|text| {
                let file = parse(text.clone().into_bytes());
                let syntax = file
                    .items
                    .iter()
                    .find(|item| item.tree.is_some())
                    .expect("derived code parses");
                let path = match text.strip_prefix("impl ") {
                    Some(line) => {
                        let interface = line.trim_end().rsplit(": ").next().unwrap_or_default();
                        format!(
                            "{}::impl[{owner}: core::{interface}]",
                            self.db.module_path(module)
                        )
                    }
                    None => {
                        let label = text.trim_end().rsplit(' ').next().unwrap_or_default();
                        let method = label.rsplit('.').next().unwrap_or_default();
                        format!("{owner}.{method}")
                    }
                };
                let known = Known {
                    resolve: &resolve,
                    own: (!text.starts_with("impl ")).then(|| path.clone()),
                    transparent: false,
                    fields: Reach::Module,
                    facts: None,
                };
                let hashes = Hashes::of(forms(&file, syntax, &known));
                DerivedItem { path, text, hashes }
            })
            .collect()
    }

    /// The texts of the items a struct's or enum's derives write, for each derive in its clause's
    /// order: the impl line, then the implementing function.
    fn derived_items(&self, item: ItemId) -> Vec<String> {
        let data = self.db.item(item);
        let (name, generics, derives) = match &data.ast.kind {
            ItemKind::Struct(structure) => {
                (&structure.name, &structure.generics, &structure.derives)
            }
            ItemKind::Enum(enumeration) => (
                &enumeration.name,
                &enumeration.generics,
                &enumeration.derives,
            ),
            _ => return Vec::new(),
        };
        let written = |ty: &ast::TypeExpr| self.written(item, ty.token, ty.last);
        let members = match &data.ast.kind {
            ItemKind::Struct(structure) => structure
                .fields()
                .map(|field| Member::Field {
                    name: field.name.text.clone(),
                    ty: written(&field.ty),
                })
                .collect(),
            ItemKind::Enum(enumeration) => enumeration
                .variants
                .iter()
                .map(|variant| Member::Variant {
                    name: format!("{}.{}", name.text, variant.name.text),
                    payload: match &variant.payload {
                        None => Payload::None,
                        Some(fields) if fields.iter().all(|field| field.label.is_some()) => {
                            Payload::Labelled(
                                fields
                                    .iter()
                                    .filter_map(|field| field.label.as_ref().map(written))
                                    .collect(),
                            )
                        }
                        Some(_) => Payload::Single,
                    },
                })
                .collect(),
            _ => Vec::new(),
        };
        let params: Vec<String> = generics
            .iter()
            .map(|param| param.name.text.clone())
            .collect();
        let derived = Derived {
            own: if params.is_empty() {
                name.text.clone()
            } else {
                format!("{}[{}]", name.text, params.join(", "))
            },
            name: name.text.clone(),
            params,
            visibility: match data.ast.visibility {
                Visibility::Pub => "pub ",
                Visibility::Pkg => "pkg ",
                Visibility::Private => "",
            },
            members,
        };
        let mut items = Vec::new();
        for entry in derives {
            let function = match entry.text.as_str() {
                "Eq" => derived.eq(),
                "Ord" => derived.cmp(),
                "Hash" => derived.hash(),
                "Default" => derived.default(),
                "Render" => derived.render(),
                _ => continue,
            };
            items.push(derived.impl_line(&entry.text));
            items.push(function);
        }
        items
    }
}

impl Derived {
    fn impl_line(&self, interface: &str) -> String {
        let params: Vec<String> = self
            .params
            .iter()
            .map(|param| format!("{param}: {interface}"))
            .collect();
        if params.is_empty() {
            format!("impl {}: {interface}\n", self.name)
        } else {
            format!("impl {}[{}]: {interface}\n", self.name, params.join(", "))
        }
    }

    /// A function named after the type, with its head after the name and its body lines.
    fn function(&self, method: &str, head: &str, body: &[String]) -> String {
        let mut text = format!("{}fn {}.{method}{head} {{\n", self.visibility, self.name);
        for line in body {
            text.push_str("    ");
            text.push_str(line);
            text.push('\n');
        }
        text.push_str(&format!("}} {}.{method}\n", self.name));
        text
    }

    fn fields(&self) -> impl Iterator<Item = (&str, &str)> {
        self.members.iter().filter_map(|member| match member {
            Member::Field { name, ty } => Some((name.as_str(), ty.as_str())),
            Member::Variant { .. } => None,
        })
    }

    fn variants(&self) -> impl Iterator<Item = (&str, &Payload)> {
        self.members.iter().filter_map(|member| match member {
            Member::Variant { name, payload } => Some((name.as_str(), payload)),
            Member::Field { .. } => None,
        })
    }

    fn is_enum(&self) -> bool {
        self.variants().next().is_some()
    }

    /// The arms of a `match` over the variants, each written by `arm`, closed by `else` when
    /// another variant remains.
    fn arms(&self, arm: impl Fn(&str, &Payload) -> Vec<String>) -> Vec<String> {
        self.variants()
            .flat_map(|(variant, payload)| arm(variant, payload))
            .collect()
    }

    fn eq(&self) -> String {
        let head = format!(
            "(self, other: {}) -> bool limits pure, notrap, terminates",
            self.own
        );
        let body = if self.is_enum() {
            let several = self.variants().count() > 1;
            let mut body = vec!["return match self {".to_owned()];
            body.extend(self.arms(|variant, payload| {
                let compared: Vec<String> = payload
                    .bound("left", false)
                    .iter()
                    .zip(payload.bound("right", false))
                    .map(|(left, right)| format!("{left} == {right}"))
                    .collect();
                let equal = if compared.is_empty() {
                    "true".to_owned()
                } else {
                    compared.join(" and ")
                };
                let mut arm = vec![
                    format!("    {} => match other {{", payload.pattern(variant, "left")),
                    format!("        {} => {equal}", payload.pattern(variant, "right")),
                ];
                if several {
                    arm.push("        else => false".to_owned());
                }
                arm.push("    }".to_owned());
                arm
            }));
            body.push("}".to_owned());
            body
        } else {
            let compared: Vec<String> = self
                .fields()
                .map(|(name, _)| format!("self.{name} == other.{name}"))
                .collect();
            if compared.is_empty() {
                vec!["return true".to_owned()]
            } else {
                vec![format!("return {}", compared.join(" and "))]
            }
        };
        self.function("eq", &head, &body)
    }

    fn cmp(&self) -> String {
        let head = format!(
            "(self, other: {}) -> core::Ordering limits pure, notrap, terminates",
            self.own
        );
        let mut body = Vec::new();
        if self.is_enum() {
            for (side, value) in [("self_position", "self"), ("other_position", "other")] {
                body.push(format!("let {side} = match {value} {{"));
                for (position, (variant, payload)) in self.variants().enumerate() {
                    let ignored = match payload {
                        Payload::None => variant.to_owned(),
                        Payload::Single => format!("{variant}(_)"),
                        Payload::Labelled(labels) => {
                            let fields: Vec<String> =
                                labels.iter().map(|label| format!("{label}: _")).collect();
                            format!("{variant}({})", fields.join(", "))
                        }
                    };
                    body.push(format!("    {ignored} => {position}u32"));
                }
                body.push("}".to_owned());
            }
            body.push("if self_position != other_position {".to_owned());
            body.push("    return self_position.cmp(other_position)".to_owned());
            body.push("}".to_owned());
            let several = self.variants().count() > 1;
            body.push("return match self {".to_owned());
            body.extend(self.arms(|variant, payload| {
                let pairs: Vec<(String, String)> = payload
                    .bound("left", false)
                    .into_iter()
                    .zip(payload.bound("right", false))
                    .collect();
                let Some(((last_left, last_right), earlier)) = pairs.split_last() else {
                    return vec![format!("    {variant} => core::Ordering.Equal")];
                };
                let mut arm = vec![format!(
                    "    {} => match other {{",
                    payload.pattern(variant, "left")
                )];
                let right = payload.pattern(variant, "right");
                if earlier.is_empty() {
                    arm.push(format!("        {right} => {last_left}.cmp({last_right})"));
                } else {
                    arm.push(format!("        {right} => {{"));
                    for (left, right) in earlier {
                        arm.push(format!("            let by_{left} = {left}.cmp({right})"));
                        arm.push(format!(
                            "            if by_{left} != core::Ordering.Equal {{"
                        ));
                        arm.push(format!("                return by_{left}"));
                        arm.push("            }".to_owned());
                    }
                    arm.push(format!("            return {last_left}.cmp({last_right})"));
                    arm.push("        }".to_owned());
                }
                if several {
                    arm.push("        else => core::Ordering.Equal".to_owned());
                }
                arm.push("    }".to_owned());
                arm
            }));
            body.push("}".to_owned());
        } else {
            let fields: Vec<&str> = self.fields().map(|(name, _)| name).collect();
            for (index, name) in fields.iter().enumerate() {
                if index + 1 == fields.len() {
                    body.push(format!("return self.{name}.cmp(other.{name})"));
                    continue;
                }
                body.push(format!("let by_{name} = self.{name}.cmp(other.{name})"));
                body.push(format!("if by_{name} != core::Ordering.Equal {{"));
                body.push(format!("    return by_{name}"));
                body.push("}".to_owned());
            }
            if fields.is_empty() {
                body.push("return core::Ordering.Equal".to_owned());
            }
        }
        self.function("cmp", &head, &body)
    }

    fn hash(&self) -> String {
        let head = "[H: core::Hasher](self, mut hasher: H) limits pure, notrap, terminates";
        let mut body = Vec::new();
        if self.is_enum() {
            body.push("match self {".to_owned());
            body.extend(self.arms(|variant, payload| {
                let key = variant.rsplit('.').next().unwrap_or(variant);
                let write = format!("mut hasher.write({})", name_bytes(key));
                let hashed = payload.bound("value", true);
                if hashed.is_empty() {
                    return vec![format!(
                        "    {} => {write}",
                        payload.pattern(variant, "value")
                    )];
                }
                let mut arm = vec![
                    format!("    {} => {{", payload.pattern(variant, "value")),
                    format!("        {write}"),
                ];
                arm.extend(
                    hashed
                        .iter()
                        .map(|value| format!("        {value}.hash(mut hasher)")),
                );
                arm.push("    }".to_owned());
                arm
            }));
            body.push("}".to_owned());
        } else {
            let mut names: Vec<&str> = self.fields().map(|(name, _)| name).collect();
            names.sort();
            body.extend(
                names
                    .iter()
                    .map(|name| format!("self.{name}.hash(mut hasher)")),
            );
        }
        self.function("hash", head, &body)
    }

    fn default(&self) -> String {
        let head = format!("() -> {} limits pure, terminates", self.own);
        let values: Vec<String> = self
            .fields()
            .map(|(name, ty)| format!("{name}: {ty}.default()"))
            .collect();
        let body = vec![format!("return {}({})", self.name, values.join(", "))];
        self.function("default", &head, &body)
    }

    fn render(&self) -> String {
        let head = "(self, mut out: core::Renderer) limits pure, terminates";
        let mut body = Vec::new();
        if self.is_enum() {
            body.push("match self {".to_owned());
            body.extend(self.arms(|variant, payload| {
                let write = |text: &str| format!("mut out.write(\"{text}\")");
                match payload {
                    Payload::None => vec![format!("    {variant} => {}", write(variant))],
                    Payload::Single => vec![
                        format!("    {} => {{", payload.pattern(variant, "value")),
                        format!("        {}", write(&format!("{variant}("))),
                        "        mut out.write_nested(value)".to_owned(),
                        format!("        {}", write(")")),
                        "    }".to_owned(),
                    ],
                    Payload::Labelled(labels) => {
                        let mut arm =
                            vec![format!("    {} => {{", payload.pattern(variant, "value"))];
                        for (index, label) in labels.iter().enumerate() {
                            let opener = if index == 0 {
                                format!("{variant}(")
                            } else {
                                ", ".to_owned()
                            };
                            arm.push(format!("        {}", write(&format!("{opener}{label}: "))));
                            arm.push(format!("        mut out.write_nested(value_{label})"));
                        }
                        arm.push(format!("        {}", write(")")));
                        arm.push("    }".to_owned());
                        arm
                    }
                }
            }));
            body.push("}".to_owned());
        } else {
            let fields: Vec<&str> = self.fields().map(|(name, _)| name).collect();
            if fields.is_empty() {
                body.push(format!("mut out.write(\"{}()\")", self.name));
            }
            for (index, name) in fields.iter().enumerate() {
                let opener = if index == 0 {
                    format!("{}(", self.name)
                } else {
                    ", ".to_owned()
                };
                body.push(format!("mut out.write(\"{opener}{name}: \")"));
                body.push(format!("mut out.write_nested(self.{name})"));
                if index + 1 == fields.len() {
                    body.push("mut out.write(\")\")".to_owned());
                }
            }
        }
        self.function("render", head, &body)
    }
}

/// A byte-string literal holding a name's byte length in 8 bytes little-endian, then its bytes.
fn name_bytes(name: &str) -> String {
    let length: String = (name.len() as u64)
        .to_le_bytes()
        .iter()
        .map(|byte| format!("\\x{byte:02X}"))
        .collect();
    format!("b\"{length}{name}\"")
}
