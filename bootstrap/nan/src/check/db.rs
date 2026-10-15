//! The item database of one check under one target: the workspace's and std's packages, their
//! modules with the variants the target selects, their items lowered to abstract syntax, the names
//! each module declares, the functions named after each type and the tombstones of retired forms.

use std::cell::RefCell;
use std::collections::HashMap;

use super::ty::Scalar;
use crate::syntax::ast;
use crate::syntax::file::SourceFile;
use crate::workspace::{File, Package, Target, Workspace};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PackageId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModuleId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemId(pub u32);

/// What a function named `Type.name` is named after.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TypeKey {
    Scalar(Scalar),
    Unit,
    Item(ItemId),
}

pub struct PackageData<'a> {
    pub source: &'a Package,
    pub name: String,
    pub std: bool,
    /// Its modules, by their index in the package.
    pub modules: Vec<ModuleId>,
    pub root: Option<ModuleId>,
    /// The package each of its `dep` aliases names, when it lies in the workspace.
    pub deps: HashMap<String, Option<PackageId>>,
}

pub struct ModuleData<'a> {
    pub package: PackageId,
    pub name: String,
    pub root: bool,
    /// Its items in byte order of file path, then position.
    pub items: Vec<ItemId>,
    /// The items each name declares, functions named after a type excepted.
    pub names: HashMap<String, Vec<ItemId>>,
    /// Whether one of its files holds an item the parse abandoned.
    pub abandoned: bool,
    /// Its source files of the variant the target selects, in byte order of path.
    pub files: Vec<&'a File>,
}

pub struct ItemData<'a> {
    pub module: ModuleId,
    pub file: &'a File,
    /// Its index among the items of its file.
    pub index: usize,
    pub ast: ast::Item,
}

impl ItemData<'_> {
    pub fn syntax(&self) -> &SourceFile {
        &self.file.syntax
    }

    /// The line of one of its tokens.
    pub fn line(&self, token: ast::Tok) -> u32 {
        self.file.syntax.tokens[token as usize].line
    }

    pub fn text(&self, token: ast::Tok) -> &str {
        self.file.syntax.text(token as usize)
    }

    pub fn head_line(&self) -> u32 {
        self.line(self.ast.token)
    }
}

pub struct Db<'a> {
    pub target: Target,
    pub packages: Vec<PackageData<'a>>,
    pub modules: Vec<ModuleData<'a>>,
    pub items: Vec<ItemData<'a>>,
    /// The package each std alias names.
    pub std: HashMap<&'static str, PackageId>,
    /// The functions named after each type, by member name.
    pub members: HashMap<TypeKey, HashMap<String, Vec<ItemId>>>,
    /// The type each function named after one is named after.
    pub owners: HashMap<ItemId, TypeKey>,
    /// The tombstones, by the canonical path of their old form.
    pub retired: HashMap<String, Vec<ItemId>>,
    /// What the queries read, once recording starts.
    reads: RefCell<Option<Reads>>,
}

/// The items whose facts each subject's queries read.
#[derive(Default)]
struct Reads {
    /// The items the queries being computed are about, innermost last, each with the line of its
    /// text being checked; none for a rule relating several items.
    subjects: Vec<(Option<ItemId>, Option<u32>)>,
    /// The items each subject's queries read, each once, in order of first read.
    read: HashMap<ItemId, Vec<ItemId>>,
    /// The items read while checking each line of a subject's text, line 0 standing for its
    /// reads outside every line checked, in order of first read.
    on_lines: HashMap<(ItemId, u32), Vec<ItemId>>,
}

/// Whether a variant selector matches a target.
fn selects(selector: &str, target: Target) -> bool {
    match selector {
        "x86_64-host" | "host" => target == Target::Host,
        "x86_64-uefi" | "uefi" => target == Target::Uefi,
        "x86_64-bare" | "bare" => target == Target::Bare,
        "x86_64" => target != Target::Any,
        _ => false,
    }
}

impl<'a> Db<'a> {
    /// The database of the workspace's packages and std under `target`.
    pub fn build(workspace: &'a Workspace, std: &'a [Package], target: Target) -> Db<'a> {
        let mut db = Db {
            target,
            packages: Vec::new(),
            modules: Vec::new(),
            items: Vec::new(),
            std: HashMap::new(),
            members: HashMap::new(),
            owners: HashMap::new(),
            retired: HashMap::new(),
            reads: RefCell::new(None),
        };
        let sources = workspace
            .packages
            .iter()
            .map(|package| (package, false))
            .chain(std.iter().map(|package| (package, true)));
        for (package, is_std) in sources {
            if package.manifest.is_none() {
                continue;
            }
            db.add_package(package, is_std);
        }
        for (index, package) in db.packages.iter().enumerate() {
            if package.std {
                let alias = crate::stdlib::PACKAGES
                    .iter()
                    .find(|alias| **alias == package.name)
                    .expect("a std package is named like its alias");
                db.std.insert(alias, PackageId(index as u32));
            }
        }
        let deps: Vec<HashMap<String, Option<PackageId>>> = db
            .packages
            .iter()
            .map(|package| {
                let manifest = package.source.manifest.as_ref().expect("a manifest");
                manifest
                    .deps
                    .iter()
                    .map(|dep| {
                        let dir = dep.source.as_deref().filter(|_| dep.path);
                        let found = dir.and_then(|dir| {
                            db.packages
                                .iter()
                                .position(|candidate| !candidate.std && candidate.source.dir == dir)
                        });
                        (
                            dep.alias.clone(),
                            found.map(|index| PackageId(index as u32)),
                        )
                    })
                    .collect()
            })
            .collect();
        for (package, deps) in db.packages.iter_mut().zip(deps) {
            package.deps = deps;
        }
        db
    }

    fn add_package(&mut self, package: &'a Package, is_std: bool) {
        let id = PackageId(self.packages.len() as u32);
        let name = package
            .name()
            .expect("a package with a manifest")
            .to_owned();
        let mut modules = Vec::new();
        let mut root = None;
        for (index, module) in package.modules.iter().enumerate() {
            let variants = module.dirs.iter().any(|dir| dir.selector.is_some());
            let selected: Vec<&str> = if variants {
                let matching = module.dirs.iter().find(|dir| {
                    dir.selector
                        .as_deref()
                        .is_some_and(|selector| selects(selector, self.target))
                });
                let chosen = matching.or_else(|| module.dirs.iter().min_by_key(|dir| &dir.path));
                chosen.map(|dir| dir.path.as_str()).into_iter().collect()
            } else {
                module.dirs.iter().map(|dir| dir.path.as_str()).collect()
            };
            let is_root = module.dirs.iter().any(|dir| dir.path == package.dir);
            let module_id = ModuleId(self.modules.len() as u32);
            if is_root {
                root = Some(module_id);
            }
            let mut data = ModuleData {
                package: id,
                name: module.name.clone(),
                root: is_root,
                items: Vec::new(),
                names: HashMap::new(),
                abandoned: false,
                files: Vec::new(),
            };
            for file in &package.files {
                let in_module = file.module == Some(index)
                    && file
                        .path
                        .rsplit_once('/')
                        .is_some_and(|(dir, _)| selected.contains(&dir));
                if !in_module {
                    continue;
                }
                data.files.push(file);
                for (item_index, item) in file.syntax.items.iter().enumerate() {
                    if item.abandoned {
                        data.abandoned |= item.tree.is_some();
                        continue;
                    }
                    let Some(tree) = &item.tree else { continue };
                    let Some(lowered) = ast::lower(&file.syntax, tree) else {
                        continue;
                    };
                    let item_id = ItemId(self.items.len() as u32);
                    if let Some(name) = lowered.name().filter(|_| !is_member(&lowered)) {
                        data.names.entry(name).or_default().push(item_id);
                    }
                    data.items.push(item_id);
                    self.items.push(ItemData {
                        module: module_id,
                        file,
                        index: item_index,
                        ast: lowered,
                    });
                }
            }
            self.modules.push(data);
            modules.push(module_id);
        }
        self.packages.push(PackageData {
            source: package,
            name,
            std: is_std,
            modules,
            root,
            deps: HashMap::new(),
        });
    }

    pub fn item(&self, id: ItemId) -> &ItemData<'a> {
        self.note(id);
        &self.items[id.0 as usize]
    }

    /// An item's syntax read for its name and kind alone, which the names of its module show.
    pub fn declaration(&self, id: ItemId) -> &ast::Item {
        &self.items[id.0 as usize].ast
    }

    /// The item of this database parsed from the same item of the same file as another
    /// database's.
    pub fn same(&self, other: &ItemData) -> Option<ItemId> {
        self.items
            .iter()
            .position(|data| std::ptr::eq(data.file, other.file) && data.index == other.index)
            .map(|position| ItemId(position as u32))
    }

    /// Starts recording what the queries read.
    pub fn record(&self) {
        *self.reads.borrow_mut() = Some(Reads::default());
    }

    /// Records that the current subject reads an item's facts.
    pub fn note(&self, item: ItemId) {
        let mut reads = self.reads.borrow_mut();
        let Some(reads) = reads.as_mut() else {
            return;
        };
        let Some((Some(subject), line)) = reads.subjects.last().copied() else {
            return;
        };
        if subject == item {
            return;
        }
        let read = reads.read.entry(subject).or_default();
        if !read.contains(&item) {
            read.push(item);
        }
        let on_line = reads
            .on_lines
            .entry((subject, line.unwrap_or(0)))
            .or_default();
        if !on_line.contains(&item) {
            on_line.push(item);
        }
    }

    /// Computes a query about `subject`, or about no item, its reads recorded as that subject's.
    pub fn about<T>(&self, subject: Option<ItemId>, compute: impl FnOnce() -> T) -> T {
        if let Some(reads) = self.reads.borrow_mut().as_mut() {
            reads.subjects.push((subject, None));
        }
        let computed = compute();
        if let Some(reads) = self.reads.borrow_mut().as_mut() {
            reads.subjects.pop();
        }
        computed
    }

    /// Computes the checking of a line of the current subject's text, its reads recorded also as
    /// that line's.
    pub fn on_line<T>(&self, line: u32, compute: impl FnOnce() -> T) -> T {
        let enclosing = self
            .reads
            .borrow_mut()
            .as_mut()
            .and_then(|reads| reads.subjects.last_mut())
            .map(|(_, current)| current.replace(line));
        let computed = compute();
        if let Some(enclosing) = enclosing
            && let Some((_, current)) = self
                .reads
                .borrow_mut()
                .as_mut()
                .and_then(|reads| reads.subjects.last_mut())
        {
            *current = enclosing;
        }
        computed
    }

    /// The items read while checking a line of a subject's text, or outside every line checked
    /// for line 0, in order of first read.
    pub fn read_on(&self, subject: ItemId, line: u32) -> Vec<ItemId> {
        self.reads
            .borrow()
            .as_ref()
            .and_then(|reads| reads.on_lines.get(&(subject, line)).cloned())
            .unwrap_or_default()
    }

    /// The lines of a subject's text whose checking was recorded, 0 aside.
    pub fn lines_read(&self, subject: ItemId) -> Vec<u32> {
        self.reads.borrow().as_ref().map_or_else(Vec::new, |reads| {
            reads
                .on_lines
                .keys()
                .filter(|(item, line)| *item == subject && *line > 0)
                .map(|(_, line)| *line)
                .collect()
        })
    }

    /// The items a subject's queries read, in order of first read.
    pub fn read_by(&self, subject: ItemId) -> Vec<ItemId> {
        self.reads
            .borrow()
            .as_ref()
            .and_then(|reads| reads.read.get(&subject).cloned())
            .unwrap_or_default()
    }

    pub fn module(&self, id: ModuleId) -> &ModuleData<'a> {
        &self.modules[id.0 as usize]
    }

    pub fn package(&self, id: PackageId) -> &PackageData<'a> {
        &self.packages[id.0 as usize]
    }

    pub fn module_of(&self, item: ItemId) -> ModuleId {
        self.item(item).module
    }

    pub fn package_of(&self, item: ItemId) -> PackageId {
        self.module(self.module_of(item)).package
    }

    pub fn is_std(&self, item: ItemId) -> bool {
        self.package(self.package_of(item)).std
    }

    pub fn ast(&self, item: ItemId) -> &ast::Item {
        &self.item(item).ast
    }

    /// The first item of a module declaring `name`, functions named after a type excepted.
    pub fn lookup(&self, module: ModuleId, name: &str) -> Option<ItemId> {
        self.module(module)
            .names
            .get(name)
            .and_then(|items| items.first().copied())
    }

    /// The module of a package with that name, the root module under the package's name.
    pub fn module_named(&self, package: PackageId, name: &str) -> Option<ModuleId> {
        let data = self.package(package);
        if name == data.name {
            return data.root;
        }
        data.modules
            .iter()
            .copied()
            .find(|module| !self.module(*module).root && self.module(*module).name == name)
    }

    /// The package an alias names in `package`: a `dep` alias, or a std alias.
    pub fn alias(&self, package: PackageId, name: &str) -> Option<PackageId> {
        match self.package(package).deps.get(name) {
            Some(found) => *found,
            None => self.std.get(name).copied(),
        }
    }

    /// Whether a `dep` alias of `package` names a package outside the workspace.
    pub fn unavailable(&self, package: PackageId, name: &str) -> bool {
        self.package(package).deps.get(name) == Some(&None)
    }

    pub fn core(&self) -> PackageId {
        self.std["core"]
    }

    /// The core item of that name.
    pub fn core_item(&self, name: &str) -> Option<ItemId> {
        let root = self.package(self.core()).root?;
        self.lookup(root, name)
    }

    /// The item of a std module, given as its alias and module path, of that name.
    pub fn std_item(&self, module: &str, name: &str) -> Option<ItemId> {
        let (alias, below) = match module.split_once("::") {
            Some((alias, below)) => (alias, Some(below)),
            None => (module, None),
        };
        let package = *self.std.get(alias)?;
        let module = match below {
            Some(below) => self.module_named(package, below)?,
            None => self.package(package).root?,
        };
        self.lookup(module, name)
    }

    /// The functions named after a type with a member name.
    pub fn members(&self, key: TypeKey, name: &str) -> &[ItemId] {
        self.members
            .get(&key)
            .and_then(|members| members.get(name))
            .map_or(&[], Vec::as_slice)
    }

    /// The module path as tool output writes it: the package name, then the module's name below
    /// the root.
    pub fn module_path(&self, module: ModuleId) -> String {
        let data = self.module(module);
        let package = &self.package(data.package).name;
        if data.root {
            package.clone()
        } else {
            format!("{package}::{}", data.name)
        }
    }

    /// The canonical path of a named item.
    pub fn canonical(&self, item: ItemId) -> String {
        let name = self.declaration(item).name().unwrap_or_default();
        match self.owners.get(&item) {
            Some(key) => {
                let member = name
                    .rsplit_once('.')
                    .map_or(name.as_str(), |(_, member)| member);
                format!("{}.{member}", self.type_path(*key))
            }
            None => format!(
                "{}::{name}",
                self.module_path(self.items[item.0 as usize].module)
            ),
        }
    }

    /// The canonical path of a type that functions are named after.
    pub fn type_path(&self, key: TypeKey) -> String {
        match key {
            TypeKey::Item(item) => self.canonical(item),
            TypeKey::Scalar(scalar) => format!("core::{}", scalar.name()),
            TypeKey::Unit => "core::Unit".to_owned(),
        }
    }
}

/// Whether an item is a function named after a type.
pub fn is_member(item: &ast::Item) -> bool {
    matches!(&item.kind, ast::ItemKind::Fn(function) if function.label.owner.is_some())
}
