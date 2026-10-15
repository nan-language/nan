//! The rules over a package's modules and items taken together: the cycles its modules' paths
//! form, the items its variant directories export, and the consts and contributions whose
//! values reach themselves.

use std::collections::{BTreeMap, BTreeSet};

use super::Checker;
use super::db::{ItemId, ModuleId, PackageId};
use crate::diagnostic::Code;
use crate::syntax::ast::{self, ItemKind, Visibility};

impl Checker<'_> {
    /// The rules over a package once each of its items is checked.
    pub fn check_package_whole(&self, package: PackageId) {
        self.check_module_cycles(package);
        self.check_variants(package);
        self.check_summaries(package);
        self.check_variant_docs(package);
        self.check_unused(package);
    }

    /// Each set of two or more modules depending on one another, reported once on the first
    /// path from the set's first module, by name, to another module of the set; std's module
    /// graphs may hold cycles.
    fn check_module_cycles(&self, package: PackageId) {
        if self.db.package(package).std {
            return;
        }
        let modules = &self.db.package(package).modules;
        let mut edges: BTreeMap<ModuleId, Vec<(ModuleId, ItemId, u32)>> = BTreeMap::new();
        for &module in modules {
            for &item in &self.db.module(module).items {
                for (target, line) in self.module_paths(item, package) {
                    if target != module {
                        edges.entry(module).or_default().push((target, item, line));
                    }
                }
            }
        }
        let reaches = |from: ModuleId| {
            let mut seen = BTreeSet::from([from]);
            let mut pending = vec![from];
            while let Some(current) = pending.pop() {
                for (target, _, _) in edges.get(&current).into_iter().flatten() {
                    if seen.insert(*target) {
                        pending.push(*target);
                    }
                }
            }
            seen
        };
        let mut reported: BTreeSet<ModuleId> = BTreeSet::new();
        let mut by_name: Vec<ModuleId> = modules.clone();
        by_name.sort_by_key(|module| self.db.module(*module).name.clone());
        for &first in &by_name {
            if reported.contains(&first) {
                continue;
            }
            let set: BTreeSet<ModuleId> = reaches(first)
                .into_iter()
                .filter(|other| *other == first || reaches(*other).contains(&first))
                .collect();
            if set.len() < 2 {
                continue;
            }
            reported.extend(set.iter().copied());
            let path = edges
                .get(&first)
                .into_iter()
                .flatten()
                .filter(|(target, _, _)| set.contains(target))
                .min_by_key(|(_, item, line)| (self.db.item(*item).file.path.clone(), *line));
            if let Some((_, item, line)) = path {
                self.report_line(*item, *line, Code::ModuleCycle);
            }
        }
    }

    /// The modules of a package an item names by a path, with each path's line: a first segment
    /// naming a module, or the package's name naming its root.
    fn module_paths(&self, item: ItemId, package: PackageId) -> Vec<(ModuleId, u32)> {
        let data = self.db.item(item);
        let syntax = &data.file.syntax;
        let span = &syntax.items[data.index];
        syntax
            .path_starts(span.first..span.end)
            .filter_map(|index| {
                let module = self.db.module_named(package, syntax.text(index))?;
                Some((module, syntax.tokens[index].line))
            })
            .collect()
    }

    /// The variant directories of each module export the same `pub` and `pkg` items, impl lines
    /// and tombstones; one missing from a variant is reported on it in the first directory, by
    /// path, holding it.
    fn check_variants(&self, package: PackageId) {
        let source = self.db.package(package).source;
        for (index, module) in source.modules.iter().enumerate() {
            if module.dirs.iter().all(|dir| dir.selector.is_none()) {
                continue;
            }
            let mut dirs: Vec<&str> = module.dirs.iter().map(|dir| dir.path.as_str()).collect();
            dirs.sort();
            let mut exported: BTreeMap<String, Vec<(&str, &str, u32)>> = BTreeMap::new();
            for file in source
                .files
                .iter()
                .filter(|file| file.module == Some(index))
            {
                let Some((dir, _)) = file.path.rsplit_once('/') else {
                    continue;
                };
                for syntax in &file.syntax.items {
                    let Some(tree) = &syntax.tree else {
                        continue;
                    };
                    let Some(item) = ast::lower(&file.syntax, tree) else {
                        continue;
                    };
                    if let Some(identity) = exported_identity(&item) {
                        let line = file.syntax.tokens[item.token as usize].line;
                        exported
                            .entry(identity)
                            .or_default()
                            .push((dir, file.path.as_str(), line));
                    }
                }
            }
            for holders in exported.values() {
                let everywhere = dirs
                    .iter()
                    .all(|dir| holders.iter().any(|(holder, _, _)| holder == dir));
                if everywhere {
                    continue;
                }
                if let Some((_, path, line)) = holders.iter().min() {
                    self.report_file(path, *line, Code::ModuleVariantMismatch);
                }
            }
        }
    }
}

/// The identity under which a variant directory exports an item: a `pub` or `pkg` item's name,
/// an impl line's target and interface, or a tombstone's old form.
pub fn exported_identity(item: &ast::Item) -> Option<String> {
    let text = |path: &ast::Path| path.written();
    match &item.kind {
        ItemKind::Impl(implementation) => Some(format!(
            "impl {}: {}",
            text(&implementation.target),
            text(&implementation.interface.path)
        )),
        ItemKind::Retired(retired) => Some(format!(
            "retired {}{}",
            text(&retired.path),
            retired
                .member
                .as_ref()
                .map_or(String::new(), |member| format!(".{}", member.text))
        )),
        _ if item.visibility != Visibility::Private => item.name(),
        _ => None,
    }
}
