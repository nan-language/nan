//! The edits of the editing commands: an item's identity, and the replacements renaming or moving
//! an item or member across the workspace.

use std::ops::RangeInclusive;

use super::db::ItemId;
use super::references::{Move, Target};
use super::{check_targets, checked, targets, unchecked};
use crate::diagnostic::{Fix, Replacement};
use crate::syntax::ast::ItemKind;
use crate::workspace::Workspace;

/// The identity of the item holding a line of a workspace file: its name, or an impl line's
/// canonical path, a tombstone's old form or a const assert's item hash.
pub fn identity(workspace: &Workspace, path: &str, line: u32) -> Option<String> {
    let std = crate::stdlib::packages();
    let (source, _) = workspace.file(path)?;
    let checker = unchecked(workspace, std, *check_targets(source).first()?);
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

/// The edit renaming the item or member declared on lines of a workspace file to `name`: each
/// span writing its name rewritten, and for a field or variant of a type deriving `Encode` the
/// rename tombstone after the type.
pub fn renamed(
    workspace: &Workspace,
    path: &str,
    lines: &RangeInclusive<u32>,
    name: &str,
) -> Option<Fix> {
    let std = crate::stdlib::packages();
    let mut spans = std::collections::BTreeSet::new();
    let mut tombstone = None;
    let mut found = false;
    for target in targets(workspace) {
        let checker = checked(workspace, std, target);
        let Some(target) = checker.target_at(path, lines) else {
            continue;
        };
        found = true;
        spans.extend(checker.name_spans(&target));
        if let Target::Field(owner, old) | Target::Variant(owner, old) = &target {
            let derives_encode = match &checker.db.ast(*owner).kind {
                ItemKind::Struct(structure) => {
                    structure.derives.iter().any(|entry| entry.text == "Encode")
                }
                ItemKind::Enum(enumeration) => enumeration
                    .derives
                    .iter()
                    .any(|entry| entry.text == "Encode"),
                _ => false,
            };
            if derives_encode {
                let data = checker.db.item(*owner);
                let syntax = data.syntax();
                let last = *syntax.item_lines(&syntax.items[data.index]).end();
                let at = syntax
                    .lines
                    .get(last as usize)
                    .map_or(syntax.source.len() as u32, |next| next.start);
                let owner_name = data.ast.name().unwrap_or_default();
                tombstone = Some((
                    data.file.path.clone(),
                    at,
                    format!("\nretired {owner_name}.{old} => {owner_name}.{name} preserves\n"),
                ));
            }
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
    if let Some((path, at, text)) = tombstone {
        replacements.push((
            path,
            Replacement {
                start: at,
                end: at,
                text,
            },
        ));
    }
    Some(Fix(replacements))
}

/// The edit moving the item declared on lines of a workspace file into the module of its package
/// named `module`: none when the lines declare nothing, an error when the move is invalid.
pub fn moved(
    workspace: &Workspace,
    path: &str,
    lines: &RangeInclusive<u32>,
    module: &str,
) -> Option<Result<Move, ()>> {
    let std = crate::stdlib::packages();
    let (source, _) = workspace.file(path)?;
    let checker = checked(workspace, std, *check_targets(source).first()?);
    let target = checker.target_at(path, lines)?;
    Some(checker.moving(&target, module).ok_or(()))
}
