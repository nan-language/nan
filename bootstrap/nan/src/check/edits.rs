//! The edits of the editing commands: an item's identity, and the replacements renaming or moving
//! an item or member across the workspace.

use std::ops::RangeInclusive;

use super::db::ItemId;
use super::references::{Move, Target, removing};
use super::{Checker, check_targets, checked, checked_closures, unchecked};
use crate::diagnostic::{Fix, Replacement};
use crate::syntax::ast::ItemKind;
use crate::workspace::Workspace;

/// The identity of the item holding a line of a workspace file: its name, or an impl line's
/// canonical path, a tombstone's old form or a const assert's item hash.
pub fn identity(workspace: &Workspace, path: &str, line: u32) -> Option<String> {
    let std = crate::stdlib::packages();
    let (source, _) = workspace.file(path)?;
    let closure = workspace.closure_of(source)?;
    let checker = unchecked(workspace, closure, std, *check_targets(source).first()?);
    let db = &checker.db;
    let item = (0..db.items.len())
        .map(|index| ItemId(index as u32))
        .find(|item| {
            let data = db.item(*item);
            data.file.path == path
                && data
                    .syntax()
                    .item_lines(&data.syntax().items[data.index])
                    .contains(&line)
        })?;
    let ast = db.ast(item);
    match &ast.kind {
        ItemKind::Impl(_) => checker.canonical_path(item),
        ItemKind::Retired(_) => checker.own_path(item),
        ItemKind::ConstAssert(_) => Some(checker.hashes(item).item.full()),
        _ => ast.name(),
    }
}

/// The edit renaming the item or member declared on lines of a workspace file, or named by a
/// location's line, to `name`: each
/// span writing its name rewritten, the rename tombstone it needs, or without one the old paths
/// of the tombstones retiring members of a type it renames, and each rename tombstone it then
/// takes removed.
pub fn renamed(
    workspace: &Workspace,
    path: &str,
    lines: &RangeInclusive<u32>,
    located: Option<u32>,
    name: &str,
) -> Option<Fix> {
    let std = crate::stdlib::packages();
    let mut spans = std::collections::BTreeSet::new();
    let mut tombstone = None;
    let mut retaken = Vec::new();
    let mut found = false;
    for (_, checker) in checked_closures(workspace, std) {
        let Some(target) = checker.target_at(path, lines, located) else {
            continue;
        };
        found = true;
        spans.extend(checker.name_spans(&target));
        let written = checker.rename_tombstone(&target, name);
        if written.is_none() {
            spans.extend(checker.member_tombstone_spans(&target));
        }
        tombstone = tombstone.or(written);
        if retaken.is_empty() {
            retaken = checker.retaking(
                checker.db.package_of(target.item()),
                &checker.target_form(&target),
                &checker.renamed_form(&target, name),
            );
        }
    }
    if !found {
        return None;
    }
    let mut replacements: Vec<(String, Replacement)> = spans
        .into_iter()
        .map(|span| {
            (
                span.path,
                Replacement {
                    start: span.start,
                    end: span.end,
                    text: name.to_owned(),
                },
            )
        })
        .collect();
    replacements.extend(tombstone);
    for removed in retaken {
        removing(&mut replacements, removed);
    }
    Some(Fix(replacements))
}

impl Checker<'_> {
    /// The canonical path of a target.
    fn target_form(&self, target: &Target) -> String {
        match target {
            Target::Item(item) => self.db.canonical(*item),
            Target::Field(owner, member)
            | Target::Variant(owner, member)
            | Target::Method(owner, member) => format!("{}.{member}", self.db.canonical(*owner)),
        }
    }

    /// The canonical path of a target once renamed to `name`.
    fn renamed_form(&self, target: &Target, name: &str) -> String {
        let db = &self.db;
        match target {
            Target::Item(item) => match db.owners.get(item) {
                Some(key) => format!("{}.{name}", db.type_path(*key)),
                None => format!("{}::{name}", db.module_path(db.module_of(*item))),
            },
            Target::Field(owner, _) | Target::Variant(owner, _) | Target::Method(owner, _) => {
                format!("{}.{name}", db.canonical(*owner))
            }
        }
    }

    /// The rename tombstone renaming a target to `name` writes after the item it retires, or the
    /// type holding the member: none unless the package's history head has an item or member
    /// visible to consumers at the target's canonical path, or the target is a field or variant
    /// of a type deriving `Encode`.
    fn rename_tombstone(&self, target: &Target, name: &str) -> Option<(String, Replacement)> {
        let db = &self.db;
        let (old, new, keeps_key) = match target {
            Target::Item(item) => {
                let old = db.ast(*item).name()?;
                let new = match old.rsplit_once('.') {
                    Some((owner, _)) => format!("{owner}.{name}"),
                    None => name.to_owned(),
                };
                (old, new, false)
            }
            Target::Field(owner, member)
            | Target::Variant(owner, member)
            | Target::Method(owner, member) => {
                let derives = match &db.ast(*owner).kind {
                    ItemKind::Struct(structure) => &structure.derives[..],
                    ItemKind::Enum(enumeration) => &enumeration.derives[..],
                    _ => &[],
                };
                let owner_name = db.ast(*owner).name()?;
                (
                    format!("{owner_name}.{member}"),
                    format!("{owner_name}.{name}"),
                    derives.iter().any(|entry| entry.text == "Encode"),
                )
            }
        };
        let item = target.item();
        if !keeps_key && !self.head_exports_form(db.package_of(item), &self.target_form(target)) {
            return None;
        }
        let data = db.item(item);
        let syntax = data.syntax();
        let last = *syntax.item_lines(&syntax.items[data.index]).end();
        let at = syntax
            .lines
            .get(last as usize)
            .map_or(syntax.source.len() as u32, |next| next.start);
        Some((
            data.file.path.clone(),
            Replacement {
                start: at,
                end: at,
                text: format!("\nretired {old} => {new} preserves\n"),
            },
        ))
    }
}

/// The edit moving the item declared on lines of a workspace file, or named by a location's line,
/// into the module of its package named `module`: none when the lines declare nothing, an error
/// when the move is invalid.
pub fn moved(
    workspace: &Workspace,
    path: &str,
    lines: &RangeInclusive<u32>,
    located: Option<u32>,
    module: &str,
) -> Option<Result<Move, ()>> {
    let std = crate::stdlib::packages();
    let (source, _) = workspace.file(path)?;
    let closure = workspace.closure_of(source)?;
    let checker = checked(workspace, closure, std, *check_targets(source).first()?);
    let target = checker.target_at(path, lines, located)?;
    Some(checker.moving(&target, module).ok_or(()))
}
