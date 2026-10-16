//! The merge driver: three versions of a file merged into one, a source file per item and per
//! order-free member, the lock, history and API snapshots by their own rules, any other file as
//! one unit. A conflict leaves a conflict block in place of the conflicting text.

use std::collections::BTreeMap;

use crate::machine::api::{Entry, Snapshot};
use crate::machine::history::History;
use crate::machine::lock::Lock;
use crate::syntax::ast::{self, ItemKind};
use crate::syntax::file::{SourceFile, parse};
use crate::syntax::lexer::lex;

/// A merged file: its content, none when it is deleted, and the first line of each conflict
/// block it holds.
pub struct Merged {
    pub content: Option<Vec<u8>>,
    pub conflicts: Vec<u32>,
}

/// The kind of a file, by its path within its package.
enum Kind {
    Source,
    Lock,
    History,
    Api,
    Other,
}

fn kind(path: &str) -> Kind {
    if path.ends_with("/.nan/lock.nan") {
        Kind::Lock
    } else if path.ends_with("/.nan/history.nan") {
        Kind::History
    } else if path.contains("/.nan/api/") && path.ends_with(".nan") {
        Kind::Api
    } else if path.ends_with(".nan") && !path.contains("/.nan/") {
        Kind::Source
    } else {
        Kind::Other
    }
}

/// Merges three versions of the file at `path`, an absent version counting as one.
pub fn merge(
    path: &str,
    base: Option<&[u8]>,
    ours: Option<&[u8]>,
    theirs: Option<&[u8]>,
) -> Merged {
    if ours == theirs {
        return whole(ours);
    }
    if base == ours {
        return whole(theirs);
    }
    if base == theirs {
        return whole(ours);
    }
    let (Some(ours), Some(theirs)) = (ours, theirs) else {
        return conflict(ours.unwrap_or_default(), theirs.unwrap_or_default());
    };
    match kind(path) {
        Kind::Source
            if [base, Some(ours), Some(theirs)]
                .into_iter()
                .flatten()
                .all(lexes) =>
        {
            source(base.unwrap_or_default(), ours, theirs)
        }
        Kind::Source => conflict(ours, theirs),
        Kind::Lock => lock(ours, theirs),
        Kind::History => history(base, ours, theirs),
        Kind::Api => snapshot(base, ours, theirs),
        Kind::Other => conflict(ours, theirs),
    }
}

fn lexes(text: &[u8]) -> bool {
    lex(text).diagnostics.is_empty()
}

fn whole(content: Option<&[u8]>) -> Merged {
    Merged {
        content: content.map(<[u8]>::to_vec),
        conflicts: Vec::new(),
    }
}

/// The conflict block of two versions of a text: ours', then theirs'.
fn block(ours: &str, theirs: &str) -> String {
    let mut text = String::from("<<<<<<< ours\n");
    for (version, marker) in [(ours, "=======\n"), (theirs, ">>>>>>> theirs\n")] {
        text.push_str(version);
        if !version.is_empty() && !version.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(marker);
    }
    text
}

fn conflict(ours: &[u8], theirs: &[u8]) -> Merged {
    let text = block(
        &String::from_utf8_lossy(ours),
        &String::from_utf8_lossy(theirs),
    );
    Merged {
        content: Some(text.into_bytes()),
        conflicts: vec![1],
    }
}

/// A unit of a source file: the file summary, or an item with the comment lines before it, by
/// identity, with its text.
#[derive(Clone, Debug, PartialEq)]
struct Unit {
    identity: String,
    text: String,
    /// The item's syntax kind, for members: an enum, a struct, a manifest, or another item.
    shape: Shape,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Enum,
    Struct,
    Ordered,
    Manifest,
    Other,
}

/// The units of a source file, in order.
fn units(text: &[u8]) -> Vec<Unit> {
    let file = parse(text.to_vec());
    let line = |number: u32| String::from_utf8_lossy(file.line_text(number)).into_owned();
    let mut found = Vec::new();
    let mut taken = 0u32;
    if let Some(summary) = file.summary {
        let at = file.tokens[summary].line;
        found.push(Unit {
            identity: "summary".to_owned(),
            text: line(at),
            shape: Shape::Other,
        });
        taken = at;
    }
    for item in &file.items {
        let lines = file.item_lines(item);
        if *lines.end() <= taken || item.first >= item.end {
            continue;
        }
        let real = item.tree.is_some() && !item.abandoned;
        let mut first = *lines.start();
        while first > taken + 1 && is_comment(&line(first - 1)) {
            first -= 1;
        }
        let text: Vec<String> = (first..=*lines.end()).map(line).collect();
        let (identity, shape) = match (real, item.tree.as_ref()) {
            (true, Some(tree)) => identify(&file, tree),
            _ => (format!("line {}", line(*lines.start())), Shape::Other),
        };
        if !real
            && text
                .iter()
                .all(|line| line.trim().is_empty() || is_comment(line))
        {
            taken = *lines.end();
            continue;
        }
        found.push(Unit {
            identity,
            text: text.join("\n"),
            shape,
        });
        taken = *lines.end();
    }
    found
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//") && !line.starts_with("//!")
}

/// An item's identity and shape: its name, or an impl line's target and interface, a
/// tombstone's old form, a const assert's condition, the manifest.
fn identify(file: &SourceFile, tree: &crate::syntax::tree::Tree) -> (String, Shape) {
    let Some(item) = ast::lower(file, tree) else {
        return (String::new(), Shape::Other);
    };
    let span = |first: ast::Tok, last: ast::Tok| -> String {
        (first..=last)
            .map(|token| file.text(token as usize))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let derives_ordered = |derives: &[ast::Name]| {
        derives
            .iter()
            .any(|name| matches!(name.text.as_str(), "Ord" | "Render" | "Generate"))
    };
    match &item.kind {
        ItemKind::Package => ("package".to_owned(), Shape::Manifest),
        ItemKind::Impl(line) => (
            format!(
                "impl {}: {}",
                line.target.written(),
                line.interface.path.written()
            ),
            Shape::Other,
        ),
        ItemKind::Retired(retired) => {
            let member = retired
                .member
                .as_ref()
                .map(|member| format!(".{}", member.text))
                .unwrap_or_default();
            (
                format!("retired {}{member}", retired.path.written()),
                Shape::Other,
            )
        }
        ItemKind::ConstAssert(condition) => (
            format!("assert {}", span(condition.token, condition.last)),
            Shape::Other,
        ),
        ItemKind::Enum(enumeration) => (
            item.name().unwrap_or_default(),
            match derives_ordered(&enumeration.derives) {
                true => Shape::Ordered,
                false => Shape::Enum,
            },
        ),
        ItemKind::Struct(structure) => (
            item.name().unwrap_or_default(),
            match derives_ordered(&structure.derives) {
                true => Shape::Ordered,
                false => Shape::Struct,
            },
        ),
        _ => (item.name().unwrap_or_default(), Shape::Other),
    }
}

/// The text a three-way merge of one unit takes, or none for a conflict: equal versions, or the
/// version changed on one side only.
fn three<'t, T: PartialEq>(
    base: Option<&'t T>,
    ours: Option<&'t T>,
    theirs: Option<&'t T>,
) -> Option<Option<&'t T>> {
    if ours == theirs || base == theirs {
        Some(ours)
    } else if base == ours {
        Some(theirs)
    } else {
        None
    }
}

/// Places the keys only `theirs` holds among `ours`': each after the nearest preceding key in
/// `theirs` that the result holds, after those ours added there, or first.
fn ordered(ours: &[String], theirs: &[String], kept: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut result: Vec<String> = ours.iter().filter(|key| kept(key)).cloned().collect();
    for (position, key) in theirs.iter().enumerate() {
        if result.contains(key) || !kept(key) {
            continue;
        }
        let anchor = theirs[..position]
            .iter()
            .rev()
            .find(|earlier| result.contains(earlier));
        let at = match anchor {
            Some(anchor) => {
                let mut at = result
                    .iter()
                    .position(|known| known == anchor)
                    .expect("held")
                    + 1;
                while at < result.len() && !theirs.contains(&result[at]) {
                    at += 1;
                }
                at
            }
            None => 0,
        };
        result.insert(at, key.clone());
    }
    result
}

fn source(base: &[u8], ours: &[u8], theirs: &[u8]) -> Merged {
    let (base, ours, theirs) = (units(base), units(ours), units(theirs));
    let by = |units: &[Unit]| -> BTreeMap<String, Unit> {
        units
            .iter()
            .map(|unit| (unit.identity.clone(), unit.clone()))
            .collect()
    };
    let (base_by, ours_by, theirs_by) = (by(&base), by(&ours), by(&theirs));
    let order = ordered(
        &ours
            .iter()
            .map(|unit| unit.identity.clone())
            .collect::<Vec<_>>(),
        &theirs
            .iter()
            .map(|unit| unit.identity.clone())
            .collect::<Vec<_>>(),
        &|_| true,
    );
    let mut texts: Vec<String> = Vec::new();
    let mut conflicts = 0;
    for identity in order {
        let (b, o, t) = (
            base_by.get(&identity),
            ours_by.get(&identity),
            theirs_by.get(&identity),
        );
        let text = |unit: Option<&Unit>| unit.map(|unit| unit.text.clone());
        match three(text(b).as_ref(), text(o).as_ref(), text(t).as_ref()) {
            Some(Some(taken)) => texts.push(taken.clone()),
            Some(None) => {}
            None => match (b, o, t) {
                (Some(b), Some(o), Some(t)) => match members(b, o, t) {
                    Some(merged) => texts.push(merged),
                    None => {
                        conflicts += 1;
                        texts.push(block(&o.text, &t.text).trim_end().to_owned());
                    }
                },
                _ => {
                    conflicts += 1;
                    let o = o.map_or(String::new(), |unit| unit.text.clone());
                    let t = t.map_or(String::new(), |unit| unit.text.clone());
                    texts.push(block(&o, &t).trim_end().to_owned());
                }
            },
        }
    }
    let mut text = texts.join("\n\n");
    text.push('\n');
    let conflicts = conflict_lines(&text).into_iter().take(conflicts).collect();
    Merged {
        content: Some(text.into_bytes()),
        conflicts,
    }
}

/// The first line of each conflict block of a text.
pub fn conflict_lines(text: &str) -> Vec<u32> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| *line == "<<<<<<< ours")
        .map(|(index, _)| index as u32 + 1)
        .collect()
}

/// An item's member lines: its lines between the head ending in `{` and the closing line, each
/// member with the doc comment lines before it and the lines continuing it.
struct Members {
    head: Vec<String>,
    members: Vec<(String, String)>,
    tail: Vec<String>,
}

fn split(unit: &Unit) -> Option<Members> {
    let lines: Vec<&str> = unit.text.lines().collect();
    let open = lines.iter().position(|line| line.ends_with('{'))?;
    let close = lines.iter().rposition(|line| line.starts_with('}'))?;
    if close <= open {
        return None;
    }
    let mut members: Vec<(String, Vec<&str>)> = Vec::new();
    let mut docs: Vec<&str> = Vec::new();
    for line in &lines[open + 1..close] {
        let content = line.trim_start();
        let indent = line.len() - content.len();
        if content.starts_with("///") && indent == 4 {
            docs.push(line);
            continue;
        }
        if indent > 4
            && let Some((_, held)) = members.last_mut()
        {
            held.push(line);
            continue;
        }
        let key = member_key(unit.shape, content)?;
        let mut held = std::mem::take(&mut docs);
        held.push(line);
        members.push((key, held));
    }
    if !docs.is_empty() {
        return None;
    }
    Some(Members {
        head: lines[..=open]
            .iter()
            .map(|line| (*line).to_owned())
            .collect(),
        members: members
            .into_iter()
            .map(|(key, held)| (key, held.join("\n")))
            .collect(),
        tail: lines[close..]
            .iter()
            .map(|line| (*line).to_owned())
            .collect(),
    })
}

/// The key a member matches by: an invariant line or another directive by its text, a `dep`
/// directive by alias, an `allow` directive by authority, `language`, `targets` and `entry` by
/// word, a field or variant by name.
fn member_key(shape: Shape, line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    let first = words.next()?;
    Some(match shape {
        Shape::Manifest => match first {
            "language" | "targets" | "entry" => first.to_owned(),
            "dep" => format!("dep {}", words.next()?),
            "allow" => {
                let authority: Vec<&str> = words.take_while(|word| *word != "in").collect();
                format!("allow {}", authority.join(" "))
            }
            _ => line.to_owned(),
        },
        Shape::Struct if first == "invariant" => line.to_owned(),
        Shape::Struct | Shape::Enum => {
            let named = match first {
                "pub" | "pkg" => words.next()?,
                _ => first,
            };
            named
                .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .next()?
                .to_owned()
        }
        Shape::Ordered | Shape::Other => return None,
    })
}

/// An item both sides changed, merged per order-free member when its text outside them changed
/// on one side only; none for a conflict.
fn members(base: &Unit, ours: &Unit, theirs: &Unit) -> Option<String> {
    let (b, o, t) = (split(base)?, split(ours)?, split(theirs)?);
    let outside = |members: &Members| (members.head.clone(), members.tail.clone());
    let kept_outside = three(Some(&outside(&b)), Some(&outside(&o)), Some(&outside(&t)))??.clone();
    let by = |members: &Members| -> BTreeMap<String, String> {
        members.members.iter().cloned().collect()
    };
    let (base_by, ours_by, theirs_by) = (by(&b), by(&o), by(&t));
    let keys = |members: &Members| -> Vec<String> {
        members.members.iter().map(|(key, _)| key.clone()).collect()
    };
    let mut merged: BTreeMap<String, String> = BTreeMap::new();
    let mut all: Vec<&String> = base_by
        .keys()
        .chain(ours_by.keys())
        .chain(theirs_by.keys())
        .collect();
    all.sort();
    all.dedup();
    for key in all {
        let taken = match three(base_by.get(key), ours_by.get(key), theirs_by.get(key)) {
            Some(taken) => taken.cloned(),
            None => Some(listed(
                base_by.get(key).map(String::as_str),
                ours_by.get(key)?,
                theirs_by.get(key)?,
            )?),
        };
        if let Some(text) = taken {
            merged.insert(key.clone(), text);
        }
    }
    let order = ordered(&keys(&o), &keys(&t), &|key| merged.contains_key(key));
    let mut lines = kept_outside.0;
    lines.extend(order.iter().map(|key| merged[key].clone()));
    lines.extend(kept_outside.1);
    Some(lines.join("\n"))
}

/// A `targets` or `allow` directive both sides changed, its names merged as a set: the base's
/// names neither side removed and the names either side added, in byte order.
fn listed(base: Option<&str>, ours: &str, theirs: &str) -> Option<String> {
    let names = |line: &str| -> Vec<String> {
        let list = match line.trim_start().strip_prefix("targets ") {
            Some(list) => list,
            None => line.split_once(" in ").map_or("", |(_, list)| list),
        };
        list.split(", ").map(str::to_owned).collect()
    };
    let prefix = |line: &str| -> Option<String> {
        let trimmed = line.trim_start();
        if trimmed.starts_with("targets ") {
            return Some(line[..line.len() - trimmed.len()].to_owned() + "targets ");
        }
        trimmed.starts_with("allow ").then(|| {
            line.split_once(" in ")
                .map(|(head, _)| format!("{head} in "))
        })?
    };
    let start = prefix(ours)?;
    let (base_names, ours_names, theirs_names) = (
        base.map(names).unwrap_or_default(),
        names(ours),
        names(theirs),
    );
    let mut merged: Vec<String> = base_names
        .iter()
        .filter(|name| ours_names.contains(name) && theirs_names.contains(name))
        .cloned()
        .collect();
    for name in ours_names.iter().chain(&theirs_names) {
        if !base_names.contains(name) && !merged.contains(name) {
            merged.push(name.clone());
        }
    }
    merged.sort();
    Some(format!("{start}{}", merged.join(", ")))
}

/// The lock of the version with the later snapshot, ours when neither is later.
fn lock(ours: &[u8], theirs: &[u8]) -> Merged {
    let snapshot = |bytes: &[u8]| Lock::read(bytes).map(|lock| lock.snapshot).ok();
    match (snapshot(ours), snapshot(theirs)) {
        (Some(ours_at), Some(theirs_at)) if theirs_at > ours_at => whole(Some(theirs)),
        (None, Some(_)) => whole(Some(theirs)),
        _ => whole(Some(ours)),
    }
}

/// The union of two histories' tombstones, ours' winning for equal identities, with the store
/// name and head of the version that changed them from the base, ours' when both did.
fn history(base: Option<&[u8]>, ours: &[u8], theirs: &[u8]) -> Merged {
    let read = |bytes: &[u8]| History::read(bytes).ok();
    let (Some(o), Some(t)) = (read(ours), read(theirs)) else {
        return conflict(ours, theirs);
    };
    let b = base.and_then(read);
    let changed = |side: &History| {
        b.as_ref()
            .is_none_or(|base| base.name != side.name || base.head != side.head)
    };
    let (name, head) = match (changed(&o), changed(&t)) {
        (false, true) => (t.name.clone(), t.head),
        _ => (o.name.clone(), o.head),
    };
    let identity = |tombstone: &str| -> String {
        tombstone
            .split(" => ")
            .next()
            .unwrap_or(tombstone)
            .to_owned()
    };
    let mut modules: BTreeMap<String, Vec<String>> = o.modules.iter().cloned().collect();
    for (module, tombstones) in &t.modules {
        let held = modules.entry(module.clone()).or_default();
        for tombstone in tombstones {
            if !held
                .iter()
                .any(|known| identity(known) == identity(tombstone))
            {
                held.push(tombstone.clone());
            }
        }
        held.sort();
    }
    let merged = History {
        name,
        head,
        modules: modules.into_iter().collect(),
    };
    Merged {
        content: merged.bytes(),
        conflicts: Vec::new(),
    }
}

/// Two snapshots merged per entry as items merge, an entry both sides changed differently
/// taking ours' records and theirs' `pending` records.
fn snapshot(base: Option<&[u8]>, ours: &[u8], theirs: &[u8]) -> Merged {
    let read = |bytes: &[u8]| Snapshot::read(bytes).ok();
    let (Some(o), Some(t)) = (read(ours), read(theirs)) else {
        return conflict(ours, theirs);
    };
    let b = base.and_then(read).unwrap_or_default();
    let by = |snapshot: &Snapshot| -> BTreeMap<String, Entry> {
        snapshot
            .entries
            .iter()
            .map(|entry| (entry.path.clone(), entry.clone()))
            .collect()
    };
    let (base_by, ours_by, theirs_by) = (by(&b), by(&o), by(&t));
    let mut paths: Vec<&String> = base_by
        .keys()
        .chain(ours_by.keys())
        .chain(theirs_by.keys())
        .collect();
    paths.sort();
    paths.dedup();
    let mut entries = Vec::new();
    for path in paths {
        let taken = match three(base_by.get(path), ours_by.get(path), theirs_by.get(path)) {
            Some(taken) => taken.cloned(),
            None => ours_by.get(path).map(|ours| Entry {
                pending: theirs_by
                    .get(path)
                    .map_or_else(|| ours.pending.clone(), |theirs| theirs.pending.clone()),
                ..ours.clone()
            }),
        };
        entries.extend(taken);
    }
    let merged = Snapshot { entries };
    Merged {
        content: merged.bytes(),
        conflicts: Vec::new(),
    }
}

#[cfg(test)]
mod tests;
