//! What consumers see of an item, as `nan show --api` prints it: its doc comments, its interface
//! text with the status of each ensures clause, the impl lines naming it, the properties of a
//! struct or enum and the digest of a function.

use super::Checker;
use super::db::{ItemId, ModuleId, TypeKey};
use super::props::{Property, Props};
use super::reversibility::Class;
use super::sig::SigKind;
use super::tir::ClauseRole;
use crate::nir::Role;
use crate::output;
use crate::prover::Status;
use crate::syntax::ast::{ItemKind, TypeModifier, Visibility};

impl Checker<'_> {
    /// Whether consumers see an item: a `pub` item, or an impl line whose impl target and
    /// interface are `pub`.
    pub fn consumers_see(&self, item: ItemId) -> bool {
        let ast = self.db.ast(item);
        if !matches!(ast.kind, ItemKind::Impl(_)) {
            return ast.visibility == Visibility::Pub;
        }
        let SigKind::Impl {
            target, interface, ..
        } = &self.sig(item).kind
        else {
            return false;
        };
        let public = |item: ItemId| self.db.ast(item).visibility == Visibility::Pub;
        let target = match target {
            Some(TypeKey::Item(target)) => public(*target),
            Some(_) => true,
            None => false,
        };
        target
            && interface
                .as_ref()
                .is_some_and(|bound| public(bound.interface))
    }

    /// Whether code in a module sees an impl line: its impl target and interface are visible
    /// there.
    fn seen_from(&self, viewer: super::db::ModuleId, line: ItemId) -> bool {
        let SigKind::Impl {
            target, interface, ..
        } = &self.sig(line).kind
        else {
            return false;
        };
        let target = match target {
            Some(TypeKey::Item(target)) => self.db.visible(viewer, *target),
            Some(_) => true,
            None => false,
        };
        target
            && interface
                .as_ref()
                .is_some_and(|bound| self.db.visible(viewer, bound.interface))
    }

    /// The lines of an item's `api` record before its digest: its doc comments when `docs`
    /// holds, its interface text as consumers or code in the module `viewer` see it, each ensures
    /// clause followed by its status, the impl lines naming it that consumers or that code see
    /// and, for a struct or enum, its properties.
    pub fn api_lines(&self, item: ItemId, docs: bool, viewer: Option<ModuleId>) -> Vec<String> {
        let data = self.db.item(item);
        let syntax = data.syntax();
        let mut lines: Vec<String> = Vec::new();
        if docs {
            let first = *syntax.item_lines(&syntax.items[data.index]).start();
            lines.extend(
                (first..data.head_line()).map(|line| output::printable(syntax.line_text(line))),
            );
        }
        let statuses = self.ensures_statuses(item);
        lines.extend(statused(self.interface_text(item, viewer), &statuses));
        let mut impls: Vec<ItemId> = (0..self.db.items.len())
            .map(|index| ItemId(index as u32))
            .filter(|&other| {
                matches!(self.db.ast(other).kind, ItemKind::Impl(_))
                    && viewer.map_or_else(
                        || self.consumers_see(other),
                        |viewer| self.seen_from(viewer, other),
                    )
                    && match &self.sig(other).kind {
                        SigKind::Impl {
                            target, interface, ..
                        } => {
                            *target == Some(TypeKey::Item(item))
                                || interface
                                    .as_ref()
                                    .is_some_and(|bound| bound.interface == item)
                        }
                        _ => false,
                    }
            })
            .collect();
        impls.sort_by_key(|other| {
            (
                &self.db.item(*other).file.path,
                self.db.item(*other).head_line(),
            )
        });
        for other in impls {
            lines.extend(self.interface_text(other, None));
        }
        if matches!(data.ast.kind, ItemKind::Struct(_) | ItemKind::Enum(_)) {
            let props = self.consumer_props(item);
            let seen: Vec<&str> = [
                (Property::Linear, "linear"),
                (Property::View, "view"),
                (Property::Copy, "Copy"),
                (Property::Send, "Send"),
                (Property::Inert, "Inert"),
                (Property::AnyBits, "AnyBits"),
            ]
            .into_iter()
            .filter(|(property, _)| props.has(*property))
            .map(|(_, name)| name)
            .collect();
            lines.push(format!("properties: {}", or_none(&seen, ", ")));
        }
        lines
    }

    /// The properties consumers see of a struct or enum: those of a closed type as computed, and
    /// of an open one its head's and those its `derives` clause lists.
    pub fn consumer_props(&self, item: ItemId) -> Props {
        let ast = self.db.ast(item);
        let props = self.definition_props(item);
        if !ast.has_modifier(TypeModifier::Open) {
            return props;
        }
        let derived = |name: &str| match &ast.kind {
            ItemKind::Struct(structure) => structure.derives.iter().any(|entry| entry.text == name),
            ItemKind::Enum(enumeration) => {
                enumeration.derives.iter().any(|entry| entry.text == name)
            }
            _ => false,
        };
        Props {
            send: derived("Send"),
            inert: derived("Inert"),
            any_bits: derived("AnyBits"),
            ..props
        }
    }

    /// The status of each ensures clause of a function with a body, in clause order: the first
    /// of open, assumed, checked and proven among those of its obligations.
    fn ensures_statuses(&self, item: ItemId) -> Vec<&'static str> {
        let Some(body) = self
            .typed(item)
            .filter(|_| self.db.ast(item).kind.is_fn_with_body())
        else {
            return Vec::new();
        };
        let obligations = self.obligations(item);
        body.clauses
            .iter()
            .filter(|clause| clause.role == ClauseRole::Ensures)
            .enumerate()
            .map(|(index, clause)| {
                let statuses: Vec<Status> = obligations
                    .iter()
                    .filter(|site| site.role == Role::Body && site.clause == Some(index as u32))
                    .map(|site| site.status)
                    .collect();
                match () {
                    _ if clause.disputed || statuses.contains(&Status::Open) => "open",
                    _ if statuses.contains(&Status::Assumed) => "assumed",
                    _ if statuses.contains(&Status::Checked) => "checked",
                    _ => "proven",
                }
            })
            .collect()
    }

    /// A function's digest as its digest line writes it after `digest: `.
    pub fn digest_text(&self, item: ItemId) -> String {
        let digest = self.digest(item);
        let mut effects: Vec<String> = [
            ("wait", digest.wait),
            ("alloc", digest.alloc),
            ("sync", digest.sync),
            ("float", digest.float),
        ]
        .into_iter()
        .filter(|(_, holds)| *holds)
        .map(|(name, _)| name.to_owned())
        .collect();
        if digest.class != Class::Traceless {
            effects.push(digest.class.text());
        }
        let effects: Vec<&str> = effects.iter().map(String::as_str).collect();
        let mut authority: Vec<String> = [("device", digest.device), ("priv", digest.privileged)]
            .into_iter()
            .filter(|(_, holds)| *holds)
            .map(|(name, _)| name.to_owned())
            .collect();
        match (&digest.capabilities, &digest.validators) {
            (Some(capabilities), Some(validators)) => {
                for set in [capabilities, validators] {
                    let mut paths: Vec<String> =
                        set.iter().map(|item| self.db.canonical(*item)).collect();
                    paths.sort();
                    authority.extend(paths);
                }
            }
            _ => authority.push("any".to_owned()),
        }
        let authority: Vec<&str> = authority.iter().map(String::as_str).collect();
        let terminates = if digest.terminates {
            "terminates"
        } else {
            "may-not-terminate"
        };
        let may_trap = if self.may_trap(item) { "yes" } else { "no" };
        format!(
            "effects {} | authority {} | may-trap {may_trap} | {terminates}",
            or_none(&effects, " "),
            or_none(&authority, " ")
        )
    }
}

fn or_none(entries: &[&str], separator: &str) -> String {
    if entries.is_empty() {
        "none".to_owned()
    } else {
        entries.join(separator)
    }
}

/// Interface text lines with each ensures clause followed by its status, `checked` for a clause
/// of a function without a body.
fn statused(mut lines: Vec<String>, statuses: &[&str]) -> Vec<String> {
    let depth = |line: &str| line.len() - line.trim_start().len();
    let mut clause = 0;
    let mut at = 0;
    while at < lines.len() {
        let text = lines[at].trim_start();
        let disputed = text.starts_with("todo ") && text.contains(" ensures ");
        if text.starts_with("ensures ") || disputed {
            let mut end = at;
            while lines
                .get(end + 1)
                .is_some_and(|next| depth(next) > depth(&lines[at]))
            {
                end += 1;
            }
            let status = match (statuses.get(clause), disputed) {
                (Some(status), _) => status,
                (None, true) => "open",
                (None, false) => "checked",
            };
            lines[end].push_str(&format!(" [{status}]"));
            clause += 1;
            at = end;
        }
        at += 1;
    }
    lines
}
