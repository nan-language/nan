//! Modules: the directories of a package's source files, their target variants and summary files.

use std::collections::{HashMap, HashSet};

use super::{Manifest, Package, Target};
use crate::diagnostic::{Code, Diagnostic};

/// A module: the root module, or a plain directory, or the variants of one name.
pub struct Module {
    pub name: String,
    pub dirs: Vec<ModuleDir>,
}

/// A module or variant directory, relative to the workspace root.
pub struct ModuleDir {
    pub path: String,
    pub selector: Option<String>,
    /// Its first source file in byte order of path, where diagnostics about it are reported.
    pub first_file: String,
}

/// Groups the sources of a package into modules, returning them and each source's module.
pub fn assign(
    package_dir: &str,
    package_name: Option<&str>,
    sources: &[String],
) -> (Vec<Module>, Vec<usize>) {
    let mut modules: Vec<Module> = Vec::new();
    let mut indexes: HashMap<String, usize> = HashMap::new();
    let mut assigned = Vec::new();
    for source in sources {
        let dir = source.rsplit_once('/').map_or("", |(dir, _)| dir);
        let (name, selector) = if dir == package_dir {
            (package_name.unwrap_or_default().to_owned(), None)
        } else {
            let last = dir.rsplit('/').next().expect("a directory segment");
            match last.split_once('@') {
                Some((name, selector)) => (name.to_owned(), Some(selector.to_owned())),
                None => (last.to_owned(), None),
            }
        };
        let index = *indexes.entry(name.clone()).or_insert_with(|| {
            modules.push(Module {
                name,
                dirs: Vec::new(),
            });
            modules.len() - 1
        });
        let dirs = &mut modules[index].dirs;
        if !dirs.iter().any(|known| known.path == dir) {
            dirs.push(ModuleDir {
                path: dir.to_owned(),
                selector,
                first_file: source.clone(),
            });
        }
        assigned.push(index);
    }
    (modules, assigned)
}

/// The concrete targets a variant selector matches.
fn matches(selector: &str) -> Vec<Target> {
    match selector {
        "x86_64-host" | "host" => vec![Target::Host],
        "x86_64-uefi" | "uefi" => vec![Target::Uefi],
        "x86_64-bare" | "bare" => vec![Target::Bare],
        "x86_64" => vec![Target::Host, Target::Uefi, Target::Bare],
        _ => Vec::new(),
    }
}

/// Checks the variants of each module and that each module directory holds its summary file.
pub fn check(package: &Package, manifest: &Manifest) -> Vec<Diagnostic> {
    let mut found = Vec::new();
    let manifest_path = &package.files[0].path;
    let paths: HashSet<&str> = package
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    let concrete: Vec<Target> = manifest
        .targets
        .iter()
        .copied()
        .filter(|target| *target != Target::Any)
        .collect();
    for module in &package.modules {
        let variants: Vec<&ModuleDir> = module
            .dirs
            .iter()
            .filter(|dir| dir.selector.is_some())
            .collect();
        if variants.len() < module.dirs.len() {
            for variant in &variants {
                found.push(Diagnostic::at(
                    Code::ModuleVariantMixed,
                    &variant.first_file,
                    1,
                ));
            }
        }
        if !variants.is_empty() {
            for variant in &variants {
                let selector = variant
                    .selector
                    .as_deref()
                    .expect("a variant has a selector");
                if !matches(selector)
                    .iter()
                    .any(|target| concrete.contains(target))
                {
                    found.push(Diagnostic::at(
                        Code::ModuleVariantUnmatched,
                        &variant.first_file,
                        1,
                    ));
                }
            }
            for target in &concrete {
                let mut matching: Vec<&&ModuleDir> = variants
                    .iter()
                    .filter(|variant| {
                        matches(variant.selector.as_deref().expect("a selector")).contains(target)
                    })
                    .collect();
                matching.sort_by(|left, right| left.path.cmp(&right.path));
                if matching.is_empty() {
                    let line = manifest
                        .targets_line
                        .expect("a declared concrete target has a directive");
                    found.push(Diagnostic::at(
                        Code::ModuleVariantMissing,
                        manifest_path,
                        line,
                    ));
                }
                for later in matching.iter().skip(1) {
                    found.push(Diagnostic::at(
                        Code::ModuleVariantOverlap,
                        &later.first_file,
                        1,
                    ));
                }
            }
        }
        for dir in &module.dirs {
            let summary = format!("{}/{}.nan", dir.path, module.name);
            if !paths.contains(summary.as_str()) {
                let root = dir.path == package.dir;
                let diagnostic = if root {
                    Diagnostic::at(Code::DocModuleFileMissing, manifest_path, manifest.line)
                } else {
                    Diagnostic::at(Code::DocModuleFileMissing, &dir.first_file, 1)
                };
                found.push(diagnostic);
            }
        }
    }
    found.sort_by(|left, right| {
        (&left.path, left.line, left.code).cmp(&(&right.path, right.line, right.code))
    });
    found.dedup();
    found
}
