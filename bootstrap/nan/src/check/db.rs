//! The item database of one check under one target: the workspace's and std's packages, their
//! modules with the variants the target selects, their items lowered to abstract syntax, the names
//! each module declares, the functions named after each type and the tombstones of retired forms.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};

use super::ty::Scalar;
use crate::syntax::ast;
use crate::syntax::file::SourceFile;
use crate::workspace::{Closure, File, Member, Package, Target, Workspace};

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
    /// Whether it is a std package.
    pub std: bool,
    /// Whether it is a std package the toolchain carries, which no check reports on, rather than
    /// one of the workspace.
    pub carried: bool,
    /// Whether it is checked without being reported on: a release of the local store, or a
    /// workspace package another closure checks.
    pub quiet: bool,
    /// The closure member it is; none for a carried std package.
    pub member: Option<Member>,
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
    pub ast: &'a ast::Item,
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
    pub workspace: &'a Workspace,
    pub target: Target,
    pub packages: Vec<PackageData<'a>>,
    pub modules: Vec<ModuleData<'a>>,
    pub items: Vec<ItemData<'a>>,
    /// The package each std alias names.
    pub std: HashMap<&'static str, PackageId>,
    /// The functions named after each type, by member name.
    pub members: HashMap<TypeKey, BTreeMap<String, Vec<ItemId>>>,
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
    noted: HashSet<(ItemId, ItemId)>,
    /// The items read while checking each line of a subject's text, line 0 standing for its
    /// reads outside every line checked, in order of first read.
    on_lines: HashMap<(ItemId, u32), Vec<ItemId>>,
    noted_on_lines: HashSet<(ItemId, u32, ItemId)>,
}

impl<'a> Db<'a> {
    /// The database of a closure's packages and std under `target`: the workspace packages it
    /// checks, its other members quiet, and each std package no member is named like, carried.
    pub fn build(
        workspace: &'a Workspace,
        closure: &Closure,
        std: &'a [Package],
        target: Target,
    ) -> Db<'a> {
        let mut db = Db {
            workspace,
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
        let mut members: Vec<Member> = closure.members.values().copied().collect();
        members.sort();
        for member in &members {
            let package = workspace.member(*member);
            if package.manifest.is_none() {
                continue;
            }
            let quiet = !matches!(member, Member::Source(index) if closure.checked.contains(index));
            db.add_package(package, Some(*member), quiet);
        }
        for package in std {
            if package.manifest.is_some()
                && !closure
                    .members
                    .contains_key(package.name().unwrap_or_default())
            {
                db.add_package(package, None, false);
            }
        }
        for (index, package) in db.packages.iter().enumerate() {
            let alias = crate::stdlib::PACKAGES
                .iter()
                .find(|alias| **alias == package.name);
            if let Some(alias) = alias.filter(|_| package.std) {
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
                        let named = package
                            .member
                            .and_then(|member| closure.aliases.get(&(member, dep.alias.clone())));
                        let found = named.and_then(|named| {
                            db.packages
                                .iter()
                                .position(|candidate| candidate.member == Some(*named))
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

    /// Adds a package: a closure's member, or a carried std package without one.
    fn add_package(&mut self, package: &'a Package, member: Option<Member>, quiet: bool) {
        let carried = member.is_none();
        let id = PackageId(self.packages.len() as u32);
        let name = package
            .name()
            .expect("a package with a manifest")
            .to_owned();
        let std = crate::stdlib::is_std(&name);
        let mut modules = Vec::new();
        let mut root = None;
        for (index, module) in package.modules.iter().enumerate() {
            let variants = module.dirs.iter().any(|dir| dir.selector.is_some());
            let selected: Vec<&str> = if variants {
                let matching = module.dirs.iter().find(|dir| {
                    dir.selector
                        .as_deref()
                        .is_some_and(|selector| self.target.selected_by(selector))
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
                if in_module {
                    data.files.push(file);
                }
            }
            let history = package
                .history
                .iter()
                .filter(|file| file.module == Some(index));
            for file in data.files.clone().into_iter().chain(history) {
                let lowered = file.lowered();
                for (item_index, item) in file.syntax.items.iter().enumerate() {
                    if item.abandoned {
                        data.abandoned |= item.tree.is_some();
                        continue;
                    }
                    let Some(lowered) = &lowered[item_index] else {
                        continue;
                    };
                    let item_id = ItemId(self.items.len() as u32);
                    if let Some(name) = lowered.name().filter(|_| !is_member(lowered)) {
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
            std,
            carried,
            quiet,
            member,
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
        self.items[id.0 as usize].ast
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

    /// Whether the queries' reads are being recorded.
    pub fn recording(&self) -> bool {
        self.reads.borrow().is_some()
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
        if reads.noted.insert((subject, item)) {
            reads.read.entry(subject).or_default().push(item);
        }
        let line = line.unwrap_or(0);
        if reads.noted_on_lines.insert((subject, line, item)) {
            reads
                .on_lines
                .entry((subject, line))
                .or_default()
                .push(item);
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

    /// Whether an item is a tombstone of its package's history.
    pub fn in_history(&self, item: ItemId) -> bool {
        self.items[item.0 as usize].file.history
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

    /// Whether an item belongs to a std package the toolchain carries.
    pub fn is_carried(&self, item: ItemId) -> bool {
        self.package(self.package_of(item)).carried
    }

    /// Whether an item belongs to a package the check reports on: neither carried nor quiet.
    pub fn is_reported(&self, item: ItemId) -> bool {
        let package = self.package(self.package_of(item));
        !package.carried && !package.quiet
    }

    pub fn ast(&self, item: ItemId) -> &ast::Item {
        self.item(item).ast
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

    /// The canonical path of a named item, or of a tombstone's old form.
    pub fn canonical(&self, item: ItemId) -> String {
        if let ast::ItemKind::Retired(retired) = &self.ast(item).kind {
            return self
                .old_form(self.items[item.0 as usize].module, retired)
                .unwrap_or_default();
        }
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
