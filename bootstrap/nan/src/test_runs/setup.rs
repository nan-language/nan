//! Setup forms: the values a failing case's parameters took, rebuilt from its draws by the rules
//! the generators follow, each written as the statements binding it in the test's module; and
//! the regression-test edit, which inserts them in a parameterless test of the same target.

use crate::check::Checker;
use crate::check::db::{ItemId, ModuleId};
use crate::check::sig::{Payload, SigKind};
use crate::check::ty::{Arg, Const, Scalar, Ty};
use crate::diagnostic::{Fix, Replacement};
use crate::syntax::ast::{ItemKind, Mode, ParamKind};

/// The depth of enclosing generator calls from which collections draw no elements and enums
/// only their variants that do not hold themselves.
const DEEP: u32 = 8;

/// A generated value, as its draws rebuild it.
enum Part {
    Int(i128),
    Bool(bool),
    Char(u32),
    /// A float's bits and its type.
    Float(u64, Scalar),
    Unit,
    Struct(Ty, Vec<(String, Part)>),
    /// A variant of an enum type, its payload with each field's label.
    Variant(Ty, String, Vec<(Option<String>, Part)>),
    Array(Ty, Vec<Part>),
    /// A collection's elements, or a map's entries as pairs.
    Elements(Ty, Vec<Part>),
    Entries(Ty, Vec<(Part, Part)>),
    Text(Ty, String),
    Boxed(Box<Part>),
    Untrusted(Box<Part>),
    Id(Ty, i128),
    /// A value whose generator is not one whose draws rebuild it.
    Opaque(Ty),
}

/// The values a case's draws gave, consumed in order; past the recorded ones, zero.
struct Draws<'d> {
    given: &'d [i128],
    at: usize,
}

impl Draws<'_> {
    fn next(&mut self) -> i128 {
        let value = self.given.get(self.at).copied().unwrap_or(0);
        self.at += 1;
        value
    }
}

/// Rebuilds a value of a type from the draws its generator made under `depth` enclosing
/// generator calls.
fn rebuild(ck: &Checker, ty: &Ty, draws: &mut Draws, depth: u32) -> Part {
    let inner = depth + 1;
    match ty {
        Ty::Unit => Part::Unit,
        Ty::Scalar(Scalar::Bool) => Part::Bool(draws.next() != 0),
        Ty::Scalar(Scalar::Char) => {
            let value = draws.next() as u32;
            Part::Char(if value >= 0xD800 {
                value + 0x800
            } else {
                value
            })
        }
        Ty::Scalar(scalar @ (Scalar::F32 | Scalar::F64)) => {
            Part::Float(draws.next() as u64, *scalar)
        }
        Ty::Scalar(_) => Part::Int(draws.next()),
        Ty::Item(item, args) => {
            let arg = |index: usize| args.get(index).and_then(Arg::ty).cloned();
            let count = |index: usize| match args.get(index) {
                Some(Arg::Const(Const::Int(count))) => *count as usize,
                _ => 0,
            };
            let elements = |element: &Ty, draws: &mut Draws| {
                let len = draws.next() as usize;
                (0..len)
                    .map(|_| rebuild(ck, element, draws, inner))
                    .collect::<Vec<Part>>()
            };
            match ck.db.canonical(*item).as_str() {
                "core::Array" => {
                    let element = arg(0).expect("an element type");
                    let parts = (0..count(1))
                        .map(|_| rebuild(ck, &element, draws, inner))
                        .collect();
                    Part::Array(ty.clone(), parts)
                }
                "core::Untrusted" => {
                    let contents = arg(0).expect("an Untrusted type argument");
                    Part::Untrusted(Box::new(rebuild(ck, &contents, draws, inner)))
                }
                "mem::Id" => Part::Id(ty.clone(), draws.next()),
                "mem::Box" => {
                    let contents = arg(0).expect("a boxed type");
                    Part::Boxed(Box::new(rebuild(ck, &contents, draws, inner)))
                }
                "mem::List" | "mem::Set" | "mem::Bounded" => {
                    let element = arg(0).expect("an element type");
                    Part::Elements(ty.clone(), elements(&element, draws))
                }
                "mem::Bytes" => {
                    Part::Elements(ty.clone(), elements(&Ty::Scalar(Scalar::U8), draws))
                }
                "text::String" => {
                    let chars = elements(&Ty::Scalar(Scalar::Char), draws);
                    Part::Text(ty.clone(), text_of(&chars))
                }
                "text::Fixed" => {
                    let len = draws.next() as usize;
                    let text = (0..len)
                        .map(|_| char::from_u32(draws.next() as u32).unwrap_or(' '))
                        .collect();
                    Part::Text(ty.clone(), text)
                }
                "mem::Map" | "mem::HashMap" => {
                    if ck.db.canonical(*item) == "mem::HashMap" {
                        draws.next();
                    }
                    let (key, value) = (arg(0).expect("a key type"), arg(1).expect("a value type"));
                    let len = draws.next() as usize;
                    let entries = (0..len)
                        .map(|_| {
                            let key = rebuild(ck, &key, draws, inner);
                            (key, rebuild(ck, &value, draws, inner))
                        })
                        .collect();
                    Part::Entries(ty.clone(), entries)
                }
                _ => derived(ck, ty, *item, draws, depth),
            }
        }
        _ => Part::Opaque(ty.clone()),
    }
}

/// Rebuilds a value a derived generator built: a struct's fields in declaration order, or an
/// enum's variant drawn among the eligible ones and its payload.
fn derived(ck: &Checker, ty: &Ty, item: ItemId, draws: &mut Draws, depth: u32) -> Part {
    let generate = ck.db.core_item("Generate");
    if !generate.is_some_and(|generate| ck.derives(item, generate)) {
        return Part::Opaque(ty.clone());
    }
    let inner = depth + 1;
    match &ck.sig(item).kind {
        SigKind::Struct { fields } => {
            let fields = fields
                .iter()
                .filter(|field| !field.is_kept())
                .map(|field| {
                    let field_ty = ck.field_type(ty, &field.name).unwrap_or(Ty::Unit);
                    (field.name.clone(), rebuild(ck, &field_ty, draws, inner))
                })
                .collect();
            Part::Struct(ty.clone(), fields)
        }
        SigKind::Enum { variants } => {
            let eligible: Vec<usize> = (0..variants.len())
                .filter(|index| {
                    depth < DEEP || !holds_itself(ck, ty, &variants[*index].payload, item)
                })
                .collect();
            let position = draws.next() as usize;
            let index = eligible.get(position).copied().unwrap_or(0);
            let variant = &variants[index];
            let payload = payload_types(ck, ty, &variant.payload)
                .into_iter()
                .map(|(label, payload_ty)| (label, rebuild(ck, &payload_ty, draws, inner)))
                .collect();
            Part::Variant(ty.clone(), variant.name.clone(), payload)
        }
        _ => Part::Opaque(ty.clone()),
    }
}

/// The types of a variant's payload for an instantiation of its enum, with their labels.
fn payload_types(ck: &Checker, ty: &Ty, payload: &Payload) -> Vec<(Option<String>, Ty)> {
    let Ty::Item(item, args) = ty else {
        return Vec::new();
    };
    let sig = ck.sig(*item);
    let subst = crate::check::ty::Subst::of(sig.generics.iter().map(|def| def.param), args);
    match payload {
        Payload::None => Vec::new(),
        Payload::Single(payload) => vec![(None, subst.ty(payload))],
        Payload::Labelled(fields) => fields
            .iter()
            .map(|(label, field)| (Some(label.clone()), subst.ty(field)))
            .collect(),
    }
}

/// Whether a variant's payload holds its enum as a constituent type at any depth.
fn holds_itself(ck: &Checker, ty: &Ty, payload: &Payload, enumeration: ItemId) -> bool {
    payload_types(ck, ty, payload)
        .iter()
        .any(|(_, payload)| reaches(ck, payload, enumeration, &mut Vec::new()))
}

/// Whether a type is an item or holds it as a constituent type.
fn reaches(ck: &Checker, ty: &Ty, target: ItemId, seen: &mut Vec<ItemId>) -> bool {
    let Ty::Item(item, args) = ty else {
        return false;
    };
    if *item == target {
        return true;
    }
    if seen.contains(item) {
        return false;
    }
    seen.push(*item);
    let sig = ck.sig(*item);
    let subst = crate::check::ty::Subst::of(sig.generics.iter().map(|def| def.param), args);
    args.iter()
        .filter_map(Arg::ty)
        .any(|arg| reaches(ck, arg, target, seen))
        || ck
            .constituents(*item)
            .iter()
            .any(|constituent| reaches(ck, &subst.ty(constituent), target, seen))
}

/// The text of rebuilt chars.
fn text_of(chars: &[Part]) -> String {
    chars
        .iter()
        .filter_map(|part| match part {
            Part::Char(code) => char::from_u32(*code),
            _ => None,
        })
        .collect()
}

/// The statements a test's module writes to bind values, nested parts needing statements first
/// bound to `n_1`, `n_2` and so on.
struct Writer<'c> {
    ck: &'c Checker<'c>,
    module: ModuleId,
    statements: Vec<String>,
    names: usize,
}

impl Writer<'_> {
    fn show(&self, ty: &Ty) -> String {
        self.ck.show(ty, self.module)
    }

    /// The setup form of a value for a name: `let n: T = e`, or `var n: T = e` with the
    /// statements after it when it needs them or `var` is asked.
    fn bind(&mut self, name: &str, ty: &Ty, part: &Part, var: bool) {
        let mut after = Vec::new();
        let value = match part {
            Part::Elements(collection, elements) => {
                let empty = self.empty(collection);
                for element in elements {
                    let written = self.expr(element);
                    after.push(self.pushed(collection, name, &written));
                }
                empty
            }
            Part::Entries(map, entries) => {
                let empty = self.empty(map);
                for (key, value) in entries {
                    let key = self.expr(key);
                    let value = self.expr(value);
                    after.push(format!("discard mut {name}.insert({key}, value: {value})"));
                }
                empty
            }
            Part::Array(array, elements) => {
                let written: Vec<String> =
                    elements.iter().map(|element| self.expr(element)).collect();
                let most = most_frequent(&written).unwrap_or_default();
                for (index, element) in written.iter().enumerate() {
                    if *element != most {
                        after.push(format!("{name}[{index}] = {element}"));
                    }
                }
                format!("{}.filled({most})", self.show(array))
            }
            other => self.expr(other),
        };
        let keyword = if var || !after.is_empty() {
            "var"
        } else {
            "let"
        };
        self.statements
            .push(format!("{keyword} {name}: {} = {value}", self.show(ty)));
        self.statements.extend(after);
    }

    /// A collection's `empty` call.
    fn empty(&self, collection: &Ty) -> String {
        let written = self.show(collection);
        match self.ck.std_path(collection).as_deref() {
            Some("mem::HashMap") => format!("{written}.empty(0, alloc: testing::world().alloc)"),
            Some("mem::Bounded") => format!("{written}.empty()"),
            _ => format!("{written}.empty(testing::world().alloc)"),
        }
    }

    /// The statement adding an element to a collection: a `push`, or an `insert` into a set,
    /// whose result is discarded.
    fn pushed(&self, collection: &Ty, name: &str, element: &str) -> String {
        match self.ck.std_path(collection).as_deref() {
            Some("mem::Set") => format!("discard mut {name}.insert({element})"),
            _ => format!("mut {name}.push({element})"),
        }
    }

    /// The expression of a value, a nested part needing statements bound first.
    fn expr(&mut self, part: &Part) -> String {
        match part {
            Part::Int(value) => grouped(*value),
            Part::Bool(value) => value.to_string(),
            Part::Char(code) => format!(
                "'{}'",
                escaped(&char::from_u32(*code).unwrap_or(' ').to_string(), '\'')
            ),
            Part::Float(bits, scalar) => {
                format!("{}.from_bits({})", scalar.name(), hexadecimal(*bits))
            }
            Part::Unit => "Unit".to_owned(),
            Part::Struct(ty, fields) => match self.constructible(ty) {
                true => {
                    let fields: Vec<String> = fields
                        .iter()
                        .map(|(name, part)| format!("{name}: {}", self.expr(part)))
                        .collect();
                    format!("{}({})", self.show(ty), fields.join(", "))
                }
                false => self.todo(ty),
            },
            Part::Variant(ty, variant, payload) => {
                let fields: Vec<String> = payload
                    .iter()
                    .map(|(label, part)| match label {
                        Some(label) => format!("{label}: {}", self.expr(part)),
                        None => self.expr(part),
                    })
                    .collect();
                let core = matches!(variant.as_str(), "Some" | "None" | "Ok" | "Err")
                    && self
                        .ck
                        .std_path(ty)
                        .is_some_and(|path| path == "core::Option" || path == "core::Result");
                let head = match core {
                    true => variant.clone(),
                    false => format!("{}.{variant}", self.show(ty)),
                };
                match fields.is_empty() {
                    true => head,
                    false => format!("{head}({})", fields.join(", ")),
                }
            }
            Part::Text(ty, text) => {
                let literal = format!("\"{}\"", escaped(text, '"'));
                match self.ck.std_path(ty).as_deref() {
                    Some("text::Fixed") => format!("{}.of({literal})", self.show(ty)),
                    _ => format!("text::String.of({literal}, alloc: testing::world().alloc)"),
                }
            }
            Part::Boxed(contents) => {
                let contents = self.expr(contents);
                format!("mem::Heap.of(testing::world().alloc).boxed({contents})")
            }
            Part::Untrusted(contents) => format!("Untrusted.of({})", self.expr(contents)),
            Part::Id(ty, index) => format!("{}.of({})", self.show(ty), grouped(*index)),
            Part::Opaque(ty) => self.todo(ty),
            Part::Elements(ty, _) | Part::Entries(ty, _) | Part::Array(ty, _) => {
                self.names += 1;
                let name = format!("n_{}", self.names);
                self.bind(&name, ty, part, false);
                match self.ck.has(ty, crate::check::props::Property::Copy) {
                    true => name,
                    false => format!("take {name}"),
                }
            }
        }
    }

    fn todo(&self, ty: &Ty) -> String {
        let path = match ty {
            Ty::Item(item, _) => self.ck.db.canonical(*item),
            other => self.show(other),
        };
        format!("todo \"{path}\"")
    }

    /// Whether the module can write a construction of a struct: it sees the struct and every
    /// field, and the struct is its package's or not open.
    fn constructible(&self, ty: &Ty) -> bool {
        let Ty::Item(item, _) = ty else {
            return false;
        };
        let db = &self.ck.db;
        let ast = db.ast(*item);
        let own = db.package_of(*item) == db.module(self.module).package;
        let ItemKind::Struct(structure) = &ast.kind else {
            return false;
        };
        db.visible(self.module, *item)
            && (own || !ast.is_open())
            && structure
                .fields()
                .all(|field| db.field_visible(self.module, *item, field.visibility))
    }
}

/// The first most frequent of written elements.
fn most_frequent(written: &[String]) -> Option<String> {
    let mut best: Option<(&String, usize)> = None;
    for element in written {
        let count = written.iter().filter(|other| *other == element).count();
        if best.is_none_or(|(_, most)| count > most) {
            best = Some((element, count));
        }
    }
    best.map(|(element, _)| element.clone())
}

/// A decimal integer literal in canonical format: `_` between each group of 3 digits from the
/// right once there are 4 digits or more.
fn grouped(value: i128) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && digits.len() >= 4 && (digits.len() - index).is_multiple_of(3) {
            out.push('_');
        }
        out.push(digit);
    }
    if value < 0 { format!("-{out}") } else { out }
}

/// A hexadecimal literal in canonical format: uppercase digits, `_` between each group of 4 from
/// the right once there are more than 4.
fn hexadecimal(bits: u64) -> String {
    let digits = format!("{bits:X}");
    let mut out = String::from("0x");
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && digits.len() > 4 && (digits.len() - index) % 4 == 0 {
            out.push('_');
        }
        out.push(digit);
    }
    out
}

/// A text's contents in a literal of a quote: `\` before `\` and the quote, and each char
/// outside U+0020 to U+007E as `\u{H}`.
fn escaped(text: &str, quote: char) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            ' '..='~' => out.push(c),
            c => out.push_str(&format!("\\u{{{:X}}}", c as u32)),
        }
    }
    out
}

/// The setup forms of a failing case's parameters, rebuilt from the values its draws gave, and
/// the regression-test edit: a test named `<name>_regression_<k>`, `k` the smallest positive
/// number no item of the module uses, inserted after the failing test, holding the setup forms
/// and then the failing test's statements.
pub fn regression(ck: &Checker, test: ItemId, given: &[i128]) -> (Vec<String>, Fix) {
    let data = ck.db.item(test);
    let ItemKind::Test(item) = &data.ast.kind else {
        panic!("a test item");
    };
    let module = data.module;
    let body = ck.typed(test).expect("a test's typed body");
    let mut writer = Writer {
        ck,
        module,
        statements: Vec::new(),
        names: 0,
    };
    let mut draws = Draws { given, at: 0 };
    let params = item.params.iter().flatten();
    for (param, local) in params.zip(&body.params) {
        let ty = body.local(*local).ty.clone();
        let part = rebuild(ck, &ty, &mut draws, 0);
        let ParamKind::Named { name, .. } = &param.kind else {
            continue;
        };
        writer.bind(&name.text, &ty, &part, param.mode == Mode::Take);
    }
    let taken: Vec<String> = ck
        .db
        .module(module)
        .items
        .iter()
        .filter_map(|other| ck.db.ast(*other).name())
        .collect();
    let name = &item.name.text;
    let label = (1..)
        .map(|k| format!("{name}_regression_{k}"))
        .find(|label| !taken.contains(label))
        .expect("a free regression test name");
    let target = item
        .target
        .segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join("::");
    let target = match &item.member {
        Some(member) => format!("{target}.{}", member.text),
        None => target,
    };
    let syntax = data.syntax();
    let lines = syntax.item_lines(&syntax.items[data.index]);
    let opener = data.line(item.body.token);
    let mut text = format!("\ntest {label} for {target} {{\n");
    for statement in &writer.statements {
        text.push_str(&format!("    {statement}\n"));
    }
    for line in opener + 1..*lines.end() {
        text.push_str(&String::from_utf8_lossy(syntax.line_text(line)));
        text.push('\n');
    }
    text.push_str(&format!("}} {label}\n"));
    let end = syntax
        .lines
        .get(*lines.end() as usize)
        .map_or(syntax.source.len() as u32, |next| next.start);
    let fix = Fix(vec![(
        data.file.path.clone(),
        Replacement {
            start: end,
            end,
            text,
        },
    )]);
    (writer.statements, fix)
}
