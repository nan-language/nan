//! Paths: what a path written in a module names, under the four path forms and the core
//! vocabulary, the visibility of items, and the linking of functions named after a type.

use super::db::{Db, ItemId, ModuleId, TypeKey};
use super::ty::Scalar;
use crate::diagnostic::Code;
use crate::syntax::ast::{ItemKind, Path, RetiredItem, Tok, Visibility};

/// What a path names at item level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Named {
    Item(ItemId),
    /// A module, a package's root module named by its alias or package name included.
    Module(ModuleId),
    Scalar(Scalar),
    Unit,
    Never,
    Dyn,
    /// `Some`, `None`, `Ok` or `Err`: a variant of a core enum.
    Variant(ItemId, usize),
}

/// A path that names nothing legal: the code and the segment it is reported at, or no code for a
/// path through a `dep` alias whose package is outside the workspace, which reports nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathError {
    pub code: Option<Code>,
    pub token: Tok,
}

/// The core vocabulary names that are items of `core`.
const CORE_ITEMS: [&str; 19] = [
    "Option",
    "Result",
    "Array",
    "Slice",
    "SliceMut",
    "Str",
    "Untrusted",
    "Eq",
    "Ord",
    "Hash",
    "Copy",
    "Default",
    "Render",
    "Encode",
    "Generate",
    "Send",
    "Inert",
    "AnyBits",
    "Any",
];

/// The core vocabulary names that are no item: the scalars, `Unit`, `Never`, `Dyn` and the
/// variants of `Option` and `Result`.
const CORE_NAMES: [&str; 7] = ["Unit", "Never", "Dyn", "Some", "None", "Ok", "Err"];

/// The core vocabulary names.
pub fn core_vocabulary() -> impl Iterator<Item = &'static str> {
    Scalar::ALL
        .iter()
        .map(|scalar| scalar.name())
        .chain(CORE_NAMES)
        .chain(CORE_ITEMS)
}

/// Whether a name is in the core vocabulary.
pub fn is_core_vocabulary(name: &str) -> bool {
    Scalar::from_name(name).is_some() || CORE_NAMES.contains(&name) || CORE_ITEMS.contains(&name)
}

/// Whether a path is reserved: one of two or more segments starting with `ui`, or starting with
/// `sys::infer`, `sys::load` or `core::simd`.
fn reserved(path: &Path) -> bool {
    let words: Vec<&str> = path
        .segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect();
    matches!(
        words[..],
        ["ui", _, ..] | ["sys", "infer" | "load", ..] | ["core", "simd", ..]
    )
}

impl Db<'_> {
    /// A core vocabulary name as the checker reads it.
    pub fn core_name(&self, name: &str) -> Option<Named> {
        if let Some(scalar) = Scalar::from_name(name) {
            return Some(Named::Scalar(scalar));
        }
        match name {
            "Unit" => return Some(Named::Unit),
            "Never" => return Some(Named::Never),
            "Dyn" => return Some(Named::Dyn),
            "Some" | "None" => {
                let option = self.core_item("Option")?;
                return Some(Named::Variant(option, usize::from(name == "None")));
            }
            "Ok" | "Err" => {
                let result = self.core_item("Result")?;
                return Some(Named::Variant(result, usize::from(name == "Err")));
            }
            _ => {}
        }
        CORE_ITEMS
            .contains(&name)
            .then(|| self.core_item(name))
            .flatten()
            .map(Named::Item)
    }

    /// What a path written in `from` names, ignoring locals and generic parameters.
    pub fn resolve(&self, from: ModuleId, path: &Path) -> Result<Named, PathError> {
        let segments = &path.segments;
        let unknown = |index: usize| PathError {
            code: Some(Code::NameUnknown),
            token: segments[index].token,
        };
        if reserved(path) {
            return Err(PathError {
                code: Some(Code::NameReserved),
                token: segments[0].token,
            });
        }
        let package = self.module(from).package;
        let first = segments[0].text.as_str();
        if segments.len() == 1 {
            if let Some(item) = self.lookup(from, first) {
                return Ok(Named::Item(item));
            }
            if let Some(named) = self.core_name(first) {
                return Ok(named);
            }
            let module = self.module_named(package, first).or_else(|| {
                self.alias(package, first)
                    .and_then(|aliased| self.package(aliased).root)
            });
            return module.map(Named::Module).ok_or(unknown(0));
        }
        if (self.lookup(from, first).is_some() || self.core_name(first).is_some())
            && self.module_named(package, first).is_none()
            && self.alias(package, first).is_none()
        {
            return Err(PathError {
                code: Some(Code::NameColonForMember),
                token: segments[1].token,
            });
        }
        let (module, rest) = match self.module_named(package, first) {
            Some(module) => (module, 1),
            None => {
                let Some(aliased) = self.alias(package, first) else {
                    let code = (!self.unavailable(package, first)).then_some(Code::NameUnknown);
                    return Err(PathError {
                        code,
                        token: segments[0].token,
                    });
                };
                let is_std = self.package(package).std;
                if first == "core" && is_core_vocabulary(&segments[1].text) && !is_std {
                    return Err(PathError {
                        code: Some(Code::NameRedundantPath),
                        token: segments[0].token,
                    });
                }
                if segments.len() == 2 {
                    let root = self.package(aliased).root.ok_or(unknown(0))?;
                    (root, 1)
                } else {
                    let below = &segments[1].text;
                    let Some(module) = self.module_named(aliased, below) else {
                        return Err(unknown(1));
                    };
                    if self.module(module).root && !is_std {
                        return Err(PathError {
                            code: Some(Code::NameRedundantPath),
                            token: segments[1].token,
                        });
                    }
                    (module, 2)
                }
            }
        };
        if module == from && !self.package(package).std {
            return Err(PathError {
                code: Some(Code::NameRedundantPath),
                token: segments[0].token,
            });
        }
        let name = &segments[rest];
        let Some(item) = self.lookup(module, &name.text) else {
            return Err(unknown(rest));
        };
        if rest + 1 < segments.len() {
            return Err(PathError {
                code: Some(Code::NameColonForMember),
                token: segments[rest + 1].token,
            });
        }
        Ok(Named::Item(item))
    }

    /// Whether a field of a type, declared with `visibility`, is visible from `from`: the lesser
    /// of its own and its type's visibility.
    pub fn field_visible(&self, from: ModuleId, owner: ItemId, visibility: Visibility) -> bool {
        let module = self.item(owner).module;
        let field = match visibility {
            Visibility::Pub => true,
            Visibility::Pkg => self.module(module).package == self.module(from).package,
            Visibility::Private => module == from,
        };
        field && self.visible(from, owner)
    }

    /// Whether `item` may be named from `from`.
    pub fn visible(&self, from: ModuleId, item: ItemId) -> bool {
        let data = self.item(item);
        let declared = match data.ast.visibility {
            Visibility::Pub => true,
            Visibility::Pkg => self.module(data.module).package == self.module(from).package,
            Visibility::Private => data.module == from,
        };
        declared
            && match self.owners.get(&item) {
                Some(TypeKey::Item(owner)) => self.visible(from, *owner),
                _ => true,
            }
    }

    /// The type a function's label names it after, resolved from its module.
    pub fn owner_key(&self, module: ModuleId, owner: &Path) -> Option<TypeKey> {
        match self.resolve(module, owner).ok()? {
            Named::Item(item) => matches!(
                self.ast(item).kind,
                ItemKind::Struct(_) | ItemKind::Enum(_) | ItemKind::Layout(_)
            )
            .then_some(TypeKey::Item(item)),
            Named::Scalar(scalar) => Some(TypeKey::Scalar(scalar)),
            Named::Unit => Some(TypeKey::Unit),
            _ => None,
        }
    }

    /// The canonical path a path written in `from` spells, whether or not it names an item: its
    /// last segment in the module or aliased package its other segments name, a module of the
    /// package counting even without files.
    pub fn spelled(&self, from: ModuleId, path: &Path) -> Option<String> {
        let package = self.module(from).package;
        let words: Vec<&str> = path
            .segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect();
        let (module, name) = match words[..] {
            [name] => (self.module_path(from), name),
            [first, name] => match self.module_named(package, first) {
                Some(module) => (self.module_path(module), name),
                None => match self.alias(package, first) {
                    Some(aliased) => (self.package(aliased).name.clone(), name),
                    None => (format!("{}::{first}", self.package(package).name), name),
                },
            },
            [first, module, name] => {
                let aliased = self.alias(package, first)?;
                (format!("{}::{module}", self.package(aliased).name), name)
            }
            _ => return None,
        };
        Some(format!("{module}::{name}"))
    }

    /// The canonical path of a tombstone's old form, read in its module.
    pub fn old_form(&self, module: ModuleId, retired: &RetiredItem) -> Option<String> {
        let Some(member) = &retired.member else {
            return self.spelled(module, &retired.path);
        };
        let owner = match self.owner_key(module, &retired.path) {
            Some(key) => self.type_path(key),
            None => self.spelled(module, &retired.path)?,
        };
        Some(format!("{owner}.{}", member.text))
    }

    /// Links each function named `Type.name` to its type, and each tombstone to the canonical
    /// path of its old form.
    pub fn link(&mut self) {
        let mut tombstones = Vec::new();
        for (index, data) in self.items.iter().enumerate() {
            if let ItemKind::Retired(retired) = &data.ast.kind
                && let Some(form) = self.old_form(data.module, retired)
            {
                tombstones.push((ItemId(index as u32), form));
            }
        }
        for (item, form) in tombstones {
            self.retired.entry(form).or_default().push(item);
        }
        self.link_members();
    }

    /// Records the type each function named `Type.name` is named after.
    fn link_members(&mut self) {
        let mut links = Vec::new();
        for (index, data) in self.items.iter().enumerate() {
            let ItemKind::Fn(function) = &data.ast.kind else {
                continue;
            };
            let Some(owner) = &function.label.owner else {
                continue;
            };
            if let Some(key) = self.owner_key(data.module, owner) {
                links.push((ItemId(index as u32), key, function.label.name.text.clone()));
            }
        }
        for (item, key, name) in links {
            self.owners.insert(item, key);
            self.members
                .entry(key)
                .or_default()
                .entry(name)
                .or_default()
                .push(item);
        }
    }
}
