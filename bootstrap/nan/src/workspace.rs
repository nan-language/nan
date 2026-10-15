//! The workspace a command runs in: its packages, their manifests, modules and source files.

mod manifest;
mod modules;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub use manifest::{Authority, Manifest, Target};
pub use modules::Module;

use crate::diagnostic::{Code, Diagnostic, Reason};
use crate::syntax::ast;
use crate::syntax::file::{SourceFile, parse, parse_std};
use crate::syntax::grammar::Rule;
use crate::syntax::items::Item;

pub struct Workspace {
    pub root: PathBuf,
    pub packages: Vec<Package>,
    /// Source files that no package holds.
    pub stray: Vec<String>,
    /// The package and file indexes of each file, by path.
    paths: HashMap<String, (usize, usize)>,
    /// The package directories and files the walk found.
    layout: Vec<Found>,
}

pub struct Package {
    /// The package directory, relative to the workspace root.
    pub dir: String,
    pub manifest: Option<Manifest>,
    /// `package.nan` first, then the source files in byte order of path.
    pub files: Vec<File>,
    pub modules: Vec<Module>,
    /// The names of the structs, enums and layouts each module declares, by module index.
    types: Vec<HashSet<String>>,
    pub diagnostics: Vec<Diagnostic>,
}

pub struct File {
    /// The path relative to the workspace root.
    pub path: String,
    /// The index of the file's module, none for `package.nan`.
    pub module: Option<usize>,
    pub syntax: SourceFile,
    /// Its items lowered, by index, once a check has read them; none for an item without one.
    lowered: OnceLock<Vec<Option<ast::Item>>>,
}

impl File {
    fn new(path: String, module: Option<usize>, syntax: SourceFile) -> File {
        File {
            path,
            module,
            syntax,
            lowered: OnceLock::new(),
        }
    }

    /// Its items lowered, by index: none for an abandoned item or one that lowers to nothing.
    pub fn lowered(&self) -> &[Option<ast::Item>] {
        self.lowered.get_or_init(|| {
            self.syntax
                .items
                .iter()
                .map(|item| {
                    let tree = item.tree.as_ref().filter(|_| !item.abandoned)?;
                    ast::lower(&self.syntax, tree)
                })
                .collect()
        })
    }
}

/// The workspace root of a command run in `current`: the parent of the outermost directory at or
/// above it holding a `package.nan`, or else `current`.
pub fn root(current: &Path) -> PathBuf {
    match current
        .ancestors()
        .filter(|dir| is_manifest(&dir.join("package.nan")))
        .last()
    {
        Some(outermost) => outermost.parent().unwrap_or(outermost).to_path_buf(),
        None => current.to_path_buf(),
    }
}

/// Whether a directory under `root`, outside every name starting with `.`, holds a `package.nan`.
pub fn holds_package(root: &Path) -> io::Result<bool> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.')
            || !fs::symlink_metadata(&path)?.is_dir()
        {
            continue;
        }
        if is_manifest(&path.join("package.nan")) || holds_package(&path)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn is_manifest(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
}

/// The files of one package directory found by the walk.
#[derive(Clone)]
struct Found {
    dir: String,
    sources: Vec<String>,
    links: Vec<String>,
}

impl Workspace {
    /// Loads the workspace at `root`, recording in `read` each file it reads.
    pub fn load(root: PathBuf, read: &mut Vec<String>) -> io::Result<Workspace> {
        let mut layout = Vec::new();
        let mut stray = Vec::new();
        walk(&root, "", None, &mut layout, &mut stray)?;
        let mut source = |path: &str| {
            read.push(path.to_owned());
            fs::read(root.join(path))
        };
        Workspace::build(root.clone(), layout, stray, &mut source)
    }

    /// The workspace with the files of `changed` holding their new sources, read from memory.
    pub fn with_sources(&self, changed: &HashMap<String, Vec<u8>>) -> Workspace {
        let mut source = |path: &str| {
            Ok(changed.get(path).cloned().unwrap_or_else(|| {
                let (_, file) = self.file(path).expect("a fix changes workspace files");
                file.syntax.source.clone()
            }))
        };
        Workspace::build(
            self.root.clone(),
            self.layout.clone(),
            self.stray.clone(),
            &mut source,
        )
        .expect("sources in memory read without failing")
    }

    /// The workspace of in-memory files, each given by its path relative to `root`: each file
    /// belongs to the package of the nearest directory at or above it holding a `package.nan`.
    pub fn of_files(root: PathBuf, files: &[(&str, &[u8])]) -> Workspace {
        let mut layout: Vec<Found> = files
            .iter()
            .filter_map(|(path, _)| path.strip_suffix("/package.nan"))
            .map(|dir| Found {
                dir: dir.to_owned(),
                sources: Vec::new(),
                links: Vec::new(),
            })
            .collect();
        let mut stray = Vec::new();
        for (path, _) in files {
            let holder = layout
                .iter_mut()
                .filter(|found| path.starts_with(&format!("{}/", found.dir)))
                .max_by_key(|found| found.dir.len());
            match holder {
                Some(found) if *path != format!("{}/package.nan", found.dir) => {
                    found.sources.push((*path).to_owned());
                }
                Some(_) => {}
                None => stray.push((*path).to_owned()),
            }
        }
        let mut source = |path: &str| -> io::Result<Vec<u8>> {
            let (_, bytes) = files
                .iter()
                .find(|(known, _)| *known == path)
                .expect("a file of the workspace");
            Ok(bytes.to_vec())
        };
        Workspace::build(root, layout, stray, &mut source)
            .expect("in-memory files read without failing")
    }

    fn build(
        root: PathBuf,
        layout: Vec<Found>,
        stray: Vec<String>,
        source: &mut dyn FnMut(&str) -> io::Result<Vec<u8>>,
    ) -> io::Result<Workspace> {
        let packages: Vec<Package> = layout
            .iter()
            .cloned()
            .map(|found| Package::load(found, source))
            .collect::<io::Result<_>>()?;
        let paths = packages
            .iter()
            .enumerate()
            .flat_map(|(package_index, package)| {
                package
                    .files
                    .iter()
                    .enumerate()
                    .map(move |(file_index, file)| (file.path.clone(), (package_index, file_index)))
            })
            .collect();
        Ok(Workspace {
            root,
            packages,
            stray,
            paths,
            layout,
        })
    }

    /// The loading diagnostics: a stray file's, and every package's.
    pub fn diagnostics(&self) -> impl Iterator<Item = Diagnostic> + '_ {
        let stray = self
            .stray
            .iter()
            .map(|path| Diagnostic::at(Code::ManifestMissing, path, 1));
        stray.chain(
            self.packages
                .iter()
                .flat_map(|package| package.diagnostics.iter().cloned()),
        )
    }

    pub fn files(&self) -> impl Iterator<Item = (&Package, &File)> {
        self.packages
            .iter()
            .flat_map(|package| package.files.iter().map(move |file| (package, file)))
    }

    pub fn file(&self, path: &str) -> Option<(&Package, &File)> {
        let &(package, file) = self.paths.get(path)?;
        let package = &self.packages[package];
        Some((package, &package.files[file]))
    }

    /// The path of the item holding a line of a file, if that item has one this implementation
    /// computes.
    pub fn item_at(&self, path: &str, line: u32) -> Option<ItemPath> {
        let (package, file) = self.file(path)?;
        let item = file.syntax.item_at(line)?;
        package.item_path(
            file,
            Item {
                file: &file.syntax,
                tree: item.tree.as_ref()?,
            },
        )
    }
}

/// The canonical path of an item: its package, its module unless it is the root module, and its
/// name, which a member's type precedes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemPath {
    pub package: String,
    pub module: Option<String>,
    pub name: Option<String>,
}

impl ItemPath {
    pub fn canonical(&self) -> String {
        [
            Some(&self.package),
            self.module.as_ref(),
            self.name.as_ref(),
        ]
        .into_iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join("::")
    }
}

/// Collects the package directories and source files under `dir`, skipping names that start with
/// `.`. `package` is the index in `found` of the package holding `dir`.
fn walk(
    root: &Path,
    dir: &str,
    package: Option<usize>,
    found: &mut Vec<Found>,
    stray: &mut Vec<String>,
) -> io::Result<()> {
    let absolute = root.join(dir);
    let package = if !dir.is_empty() && is_manifest(&absolute.join("package.nan")) {
        found.push(Found {
            dir: dir.to_owned(),
            sources: Vec::new(),
            links: Vec::new(),
        });
        Some(found.len() - 1)
    } else {
        package
    };
    let mut names: Vec<String> = fs::read_dir(&absolute)?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<io::Result<_>>()?;
    names.sort();
    for name in names {
        let path = if dir.is_empty() {
            name.clone()
        } else {
            format!("{dir}/{name}")
        };
        let metadata = fs::symlink_metadata(root.join(&path))?;
        if let Some(index) = package {
            let in_package = &path[found[index].dir.len() + 1..];
            if metadata.is_symlink() && !name.starts_with('.') {
                found[index].links.push(in_package.to_owned());
                continue;
            }
            if in_package == ".nan" {
                links_in_machine_dir(root, &path, in_package, &mut found[index].links)?;
            }
        }
        if name.starts_with('.') || metadata.is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            walk(root, &path, package, found, stray)?;
        } else if name.ends_with(".nan")
            && !(name == "package.nan" && package.is_some_and(|index| found[index].dir == dir))
        {
            match package {
                Some(index) => found[index].sources.push(path),
                None => stray.push(path),
            }
        }
    }
    Ok(())
}

/// The symbolic links among `.nan`, `.nan/api` and the machine files of a package.
fn links_in_machine_dir(
    root: &Path,
    path: &str,
    in_package: &str,
    links: &mut Vec<String>,
) -> io::Result<()> {
    let is_link = |path: &str| {
        fs::symlink_metadata(root.join(path)).is_ok_and(|metadata| metadata.is_symlink())
    };
    if is_link(path) {
        links.push(in_package.to_owned());
        return Ok(());
    }
    if !root.join(path).is_dir() {
        return Ok(());
    }
    for machine in ["lock.nan", "history.nan", "api"] {
        if is_link(&format!("{path}/{machine}")) {
            links.push(format!("{in_package}/{machine}"));
        }
    }
    let api = root.join(path).join("api");
    if api.is_dir() && !is_link(&format!("{path}/api")) {
        let mut names: Vec<String> = fs::read_dir(api)?
            .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
            .collect::<io::Result<_>>()?;
        names.sort();
        for name in names.into_iter().filter(|name| name.ends_with(".nan")) {
            if is_link(&format!("{path}/api/{name}")) {
                links.push(format!("{in_package}/api/{name}"));
            }
        }
    }
    Ok(())
}

impl Package {
    pub fn name(&self) -> Option<&str> {
        self.manifest
            .as_ref()
            .map(|manifest| manifest.name.as_str())
    }

    /// The canonical path of a module of the package, its name absent for the root module.
    pub fn module_path(&self, module: usize) -> Option<ItemPath> {
        let package = self.name()?.to_owned();
        let name = &self.modules[module].name;
        let module = (self.modules[module]
            .dirs
            .iter()
            .all(|dir| dir.path != self.dir))
        .then(|| name.clone());
        Some(ItemPath {
            package,
            module,
            name: None,
        })
    }

    /// The canonical path of an item of `file`, when it is named and the path of a function named
    /// after a type follows from the package's own modules.
    fn item_path(&self, file: &File, item: Item) -> Option<ItemPath> {
        let Some(module) = file.module else {
            let is_manifest = item
                .kind()
                .is_some_and(|(rule, _)| rule == Rule::PackageItem);
            let package = self.name()?.to_owned();
            return is_manifest.then_some(ItemPath {
                package,
                module: None,
                name: None,
            });
        };
        let mut path = self.module_path(module)?;
        let name = item.name()?;
        let Some((owner, _)) = name.rsplit_once('.') else {
            path.name = Some(name);
            return Some(path);
        };
        match owner.split_once("::") {
            None if self.types[module].contains(owner) => path.name = Some(name),
            Some((owner_module, _)) => {
                let index = self
                    .modules
                    .iter()
                    .position(|candidate| candidate.name == owner_module)?;
                path = self.module_path(index)?;
                path.name = Some(name[owner_module.len() + 2..].to_owned());
            }
            None => return None,
        }
        Some(path)
    }

    fn load(
        mut found: Found,
        source: &mut dyn FnMut(&str) -> io::Result<Vec<u8>>,
    ) -> io::Result<Package> {
        found.sources.sort();
        let manifest_path = format!("{}/package.nan", found.dir);
        let syntax = parse(source(&manifest_path)?);
        let mut diagnostics = Vec::new();
        let manifest = Manifest::read(&syntax, &manifest_path, &mut diagnostics);
        let mut files = vec![File::new(manifest_path.clone(), None, syntax)];
        if let Some(manifest) = &manifest {
            for link in &found.links {
                diagnostics.push(Diagnostic {
                    because: vec![Reason::note(link)],
                    ..Diagnostic::at(Code::BuildSymbolicLink, &manifest_path, manifest.line)
                });
            }
        }
        let package_name = manifest.as_ref().map(|manifest| manifest.name.clone());
        let (modules, assigned) =
            modules::assign(&found.dir, package_name.as_deref(), &found.sources);
        let std = package_name
            .as_deref()
            .is_some_and(|name| crate::stdlib::PACKAGES.contains(&name));
        if manifest.as_ref().is_none_or(|manifest| manifest.supported) {
            for (path, module) in found.sources.iter().zip(assigned) {
                let source = source(path)?;
                let syntax = if std {
                    parse_std(source)
                } else {
                    parse(source)
                };
                files.push(File::new(path.clone(), Some(module), syntax));
            }
        }
        let types = type_names(&files, modules.len());
        let mut package = Package {
            dir: found.dir,
            manifest,
            files,
            modules,
            types,
            diagnostics,
        };
        package.check_misplaced_manifests();
        if let Some(manifest) = package
            .manifest
            .as_ref()
            .filter(|manifest| manifest.supported)
        {
            let found = modules::check(&package, manifest);
            package.diagnostics.extend(found);
        }
        Ok(package)
    }

    /// Reports every `package` item outside `package.nan`, and every item of `package.nan` but
    /// its manifest when it has one.
    fn check_misplaced_manifests(&mut self) {
        for file in &self.files {
            let manifest_file = file.module.is_none();
            if manifest_file && self.manifest.is_none() {
                continue;
            }
            let mut manifest_seen = false;
            for item in file.syntax.items.iter().filter(|item| !item.abandoned) {
                let Some(tree) = &item.tree else { continue };
                let view = Item {
                    file: &file.syntax,
                    tree,
                };
                let is_manifest = view
                    .kind()
                    .is_some_and(|(rule, _)| rule == Rule::PackageItem);
                if manifest_file && is_manifest && !manifest_seen {
                    manifest_seen = true;
                } else if manifest_file || is_manifest {
                    let line = view.head_line().expect("a parsed item has a head");
                    self.diagnostics.push(Diagnostic::at(
                        Code::ManifestMisplaced,
                        &file.path,
                        line,
                    ));
                }
            }
        }
    }
}

/// The names of the structs, enums and layouts each module of `files` declares.
fn type_names(files: &[File], modules: usize) -> Vec<HashSet<String>> {
    let mut types = vec![HashSet::new(); modules];
    for file in files {
        let Some(module) = file.module else { continue };
        for tree in file
            .syntax
            .items
            .iter()
            .filter_map(|item| item.tree.as_ref())
        {
            let item = Item {
                file: &file.syntax,
                tree,
            };
            let is_type = item.kind().is_some_and(|(rule, _)| {
                matches!(rule, Rule::StructItem | Rule::EnumItem | Rule::LayoutItem)
            });
            types[module].extend(item.name().filter(|_| is_type));
        }
    }
    types
}

#[cfg(test)]
pub(crate) mod tests;
