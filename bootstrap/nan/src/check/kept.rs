//! The snapshots of checks the build cache keeps: for a closure under a target, what checking a
//! state gave each workspace item and the facts it derived of every item. A check of a state
//! differing from a snapshot's in function bodies alone checks the items whose bodies changed and
//! those whose own work read them, takes every other item's outcome from the snapshot, and keeps
//! that result only when the items it checked give the facts the snapshot holds; otherwise it
//! checks the state whole.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use super::db::ItemId;
use super::digest::Digest;
use super::reversibility::Class as Reversibility;
use super::unused::FieldLabel;
use super::{Checker, Found, checked_packages, unchecked};
use crate::cache::Cache;
use crate::diagnostic::{Class, Code, Diagnostic, Fix, Offer, Reason, Replacement, Role};
use crate::encoding::{bytes, uvar};
use crate::hash::Hash;
use crate::syntax::file::{ItemSyntax, SourceFile};
use crate::syntax::grammar::{Punct, Rule};
use crate::syntax::items::Item;
use crate::syntax::token::TokenKind;
use crate::workspace::{Authority, Closure, Member, Package, Target, Workspace};

/// What the build cache keeps of a check and gives back to it.
#[derive(Default)]
pub struct Kept {
    /// The items whose outcomes a snapshot gives: their own work reports nothing.
    restored: RefCell<HashSet<ItemId>>,
    /// The reads of another item's body, typed, lowered, elaborated or with its obligations
    /// decided, by an item's own work: reader, then read.
    reads: RefCell<BTreeSet<(ItemId, ItemId)>>,
    /// The reads of the items another item names by an item's own work: reader, then read.
    names_reads: RefCell<BTreeSet<(ItemId, ItemId)>>,
    /// The fields of structs each item's own work reads: reader, struct, field.
    fields: RefCell<BTreeSet<(ItemId, ItemId, String)>>,
    /// The `label: value` naming each field of a struct, in order: struct, field, label.
    labels: RefCell<Vec<(ItemId, String, FieldLabel)>>,
    labelled: RefCell<HashSet<(ItemId, String, FieldLabel)>>,
    /// The authorities an item's module performs through the item, in order.
    performs: RefCell<Vec<(ItemId, Authority)>>,
    /// Whether judging each item alone rejected it.
    alone: RefCell<HashMap<ItemId, bool>>,
    /// The green entries the check computed.
    greens: RefCell<HashMap<ItemId, Greened>>,
    /// The state checked and the snapshot restored from, once planned.
    plan: RefCell<Option<Plan>>,
}

impl Kept {
    /// Records whether judging an item alone rejected it.
    pub fn judged_alone(&self, item: ItemId, rejected: bool) {
        self.alone.borrow_mut().entry(item).or_insert(rejected);
    }

    /// Records a `label: value` naming a struct's field, once, giving whether it was new.
    pub fn labelled(&self, owner: ItemId, field: &str, label: FieldLabel) -> bool {
        let key = (owner, field.to_owned(), label);
        if !self.labelled.borrow_mut().insert(key.clone()) {
            return false;
        }
        self.labels.borrow_mut().push(key);
        true
    }

    /// Records an authority an item's module performs through it.
    pub fn performed(&self, item: ItemId, authority: &Authority) {
        let mut performs = self.performs.borrow_mut();
        if !performs.contains(&(item, authority.clone())) {
            performs.push((item, authority.clone()));
        }
    }
}

/// An item's api lines and interface hash, as its last-green entry holds them.
#[derive(Clone)]
pub struct Greened {
    pub api: Vec<String>,
    pub interface: Option<Hash>,
}

/// The state of a closure under a target as its snapshot names it.
#[derive(Clone)]
struct State {
    /// The hash of every input of the check, which names its snapshot.
    key: Hash,
    /// The name of the cache record holding the key of the latest snapshot of the closure under
    /// the target in this workspace, narrowed to the package its check is narrowed to.
    latest: String,
    /// The hash of every input but the source files of the packages checked.
    context: Hash,
    /// The source files of the packages checked, by path, with the hash of their contents.
    files: Vec<(String, Hash)>,
}

/// The state a check checks, and the snapshot it restores from.
struct Plan {
    state: State,
    restored: Option<Restoring>,
}

/// A snapshot a check restores from: its key, the outcomes it restores, at the positions of the
/// state checked, the reads and green entries it keeps, and its files.
struct Restoring {
    key: Hash,
    outcomes: HashMap<ItemId, Outcome>,
    reads: ReadsBy,
    names_reads: ReadsBy,
    greens: HashMap<ItemId, Greened>,
    files: Vec<SnapshotFile>,
}

/// The positions `start..end` of one kind in a file.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stretch {
    start: u32,
    end: u32,
}

/// A function or test body, its statements up to the `}` closing it: the hash of its bytes and
/// where it lies in bytes, lines and tokens, each from the end of what precedes it to the start
/// of the `}`.
#[derive(Clone, Copy, PartialEq, Eq)]
struct BodyAt {
    hash: Hash,
    bytes: Stretch,
    lines: Stretch,
    tokens: Stretch,
}

/// The shape of a source file: the hash of its text outside function bodies, and each item's
/// body, by its index among the file's items.
#[derive(Clone)]
struct FileShape {
    skeleton: Hash,
    bodies: Vec<Option<BodyAt>>,
}

/// A file a snapshot's state checked, with its contents' hash and its shape.
#[derive(Clone)]
struct SnapshotFile {
    path: String,
    content: Hash,
    shape: FileShape,
}

/// The facts of an item a snapshot seeds a check with: its digest, its dependencies through its
/// typed body and its may-trap fact, each once computed.
struct Seed {
    item: ItemId,
    digest: Option<Digest>,
    body: Option<Option<Vec<ItemId>>>,
    traps: Option<bool>,
}

/// A snapshot as read.
struct Snapshot {
    context: Hash,
    items: usize,
    files: Vec<SnapshotFile>,
    seeds: Vec<Seed>,
    reads: ReadsBy,
    names_reads: ReadsBy,
    greens: HashMap<ItemId, Greened>,
    outcomes: HashMap<ItemId, Outcome>,
}

/// Reads of other items by items' own work, by reader: each reader with the items it reads, in
/// order, as a snapshot encodes them, each the distance from the one before.
#[derive(Default)]
struct ReadsBy(Vec<(ItemId, Vec<u8>)>);

impl ReadsBy {
    /// The reads of a set of them, reader then read.
    fn of(reads: &BTreeSet<(ItemId, ItemId)>) -> ReadsBy {
        let mut by: Vec<(ItemId, Vec<u8>)> = Vec::new();
        let mut last = 0;
        for (reader, read) in reads {
            if by.last().is_none_or(|(held, _)| held != reader) {
                by.push((*reader, Vec::new()));
                last = 0;
            }
            let (_, encoded) = by.last_mut().expect("a reader");
            put_number(encoded, read.0 - last);
            last = read.0;
        }
        ReadsBy(by)
    }

    /// Each read, reader then read; none when one does not decode.
    fn pairs(&self) -> Option<Vec<(ItemId, ItemId)>> {
        let mut pairs = Vec::new();
        for (reader, encoded) in &self.0 {
            let mut cursor = Cursor {
                bytes: encoded,
                at: 0,
            };
            let mut read = 0u32;
            while cursor.at < encoded.len() {
                read = read.checked_add(cursor.small()?)?;
                pairs.push((*reader, ItemId(read)));
            }
        }
        Some(pairs)
    }

    /// The reads of the readers not marked.
    fn unmarked(&self, marked: &[bool]) -> ReadsBy {
        ReadsBy(
            (self.0.iter())
                .filter(|(reader, _)| !marked[reader.0 as usize])
                .cloned()
                .collect(),
        )
    }

    /// The reads of both, whose readers differ.
    fn joined(&self, other: ReadsBy) -> ReadsBy {
        let mut by: Vec<(ItemId, Vec<u8>)> = self.0.iter().cloned().chain(other.0).collect();
        by.sort_by_key(|(reader, _)| *reader);
        ReadsBy(by)
    }

    fn put(&self, out: &mut Vec<u8>) {
        put_number(out, self.0.len() as u64);
        for (reader, encoded) in &self.0 {
            put_item(out, *reader);
            bytes(out, encoded);
        }
    }
}

/// What a snapshot keeps of an item's own work: whether judging it alone, and the check, rejected
/// it; the diagnostics and those of unused code it found; the fields it read and labelled, the
/// authorities it performed and the consts it needed; the items it names; a const's interface
/// hash.
#[derive(Clone)]
struct Outcome {
    alone: bool,
    rejected: bool,
    found: Vec<Diagnostic>,
    unused: Vec<Diagnostic>,
    fields: Vec<(ItemId, String)>,
    labels: Vec<(ItemId, String, FieldLabel)>,
    performs: Vec<Authority>,
    needs: Vec<ItemId>,
    names: Option<Vec<ItemId>>,
    value: Option<Hash>,
}

/// Whether judging an item alone rejected it, whether the check did, and a const's interface
/// hash, as a snapshot's outcome holds them.
struct OutcomeFacts {
    alone: bool,
    rejected: bool,
    value: Option<Hash>,
}

/// How the positions of a file outside its changed bodies moved between two states of it: its
/// bodies in both, in order.
struct FileMoves {
    path: String,
    old: Vec<BodyAt>,
    new: Vec<BodyAt>,
}

/// The kinds of positions in a file.
#[derive(Clone, Copy)]
enum Position {
    Byte,
    Line,
    Token,
}

impl BodyAt {
    fn stretch(&self, kind: Position) -> Stretch {
        match kind {
            Position::Byte => self.bytes,
            Position::Line => self.lines,
            Position::Token => self.tokens,
        }
    }
}

impl FileMoves {
    /// Where a position of the earlier state of a file lies in the later one; none for one
    /// inside a changed body.
    fn moved(&self, value: u32, kind: Position) -> Option<u32> {
        let mut offset: i64 = 0;
        for (old_body, new_body) in self.old.iter().zip(&self.new) {
            let (old, new) = (old_body.stretch(kind), new_body.stretch(kind));
            if value <= old.start {
                break;
            }
            if value < old.end {
                if old_body.hash != new_body.hash {
                    return None;
                }
                offset = i64::from(new.start) - i64::from(old.start);
                break;
            }
            offset = i64::from(new.end) - i64::from(old.end);
        }
        u32::try_from(i64::from(value) + offset).ok()
    }
}

/// How the positions of the files whose bodies changed moved.
struct Moves(Vec<FileMoves>);

impl Moves {
    /// Where a position of a file of the earlier state lies in the later one; none for one
    /// inside a changed body.
    fn moved(&self, path: &str, value: u32, kind: Position) -> Option<u32> {
        match self.0.iter().find(|file| file.path == path) {
            Some(file) => file.moved(value, kind),
            None => Some(value),
        }
    }

    fn span(&self, path: &str, span: &mut Option<(u32, u32)>) -> bool {
        let Some((start, end)) = *span else {
            return true;
        };
        let (Some(start), Some(end)) = (
            self.moved(path, start, Position::Byte),
            self.moved(path, end, Position::Byte),
        ) else {
            return false;
        };
        *span = Some((start, end));
        true
    }

    /// Moves a diagnostic's positions to the state checked; whether none lies in a changed body.
    fn diagnostic(&self, diagnostic: &mut Diagnostic) -> bool {
        let Some(path) = diagnostic.path.clone() else {
            return true;
        };
        if let Some(line) = diagnostic.line {
            let Some(moved) = self.moved(&path, line, Position::Line) else {
                return false;
            };
            diagnostic.line = Some(moved);
        }
        if !self.span(&path, &mut diagnostic.span) || !self.span(&path, &mut diagnostic.judged) {
            return false;
        }
        if let Some(at) = diagnostic.failing {
            let Some(moved) = self.moved(&path, at, Position::Byte) else {
                return false;
            };
            diagnostic.failing = Some(moved);
        }
        for reason in &mut diagnostic.because {
            let Some((at, line)) = &mut reason.at else {
                continue;
            };
            let Some(moved) = self.moved(at, *line, Position::Line) else {
                return false;
            };
            *line = moved;
        }
        for Fix(replacements) in &mut diagnostic.fixes {
            for (at, replacement) in replacements {
                let (Some(start), Some(end)) = (
                    self.moved(at, replacement.start, Position::Byte),
                    self.moved(at, replacement.end, Position::Byte),
                ) else {
                    return false;
                };
                replacement.start = start;
                replacement.end = end;
            }
        }
        true
    }

    /// An outcome of the snapshot's state at the positions of the state checked; none when one
    /// of its positions lies in a changed body.
    fn outcome(&self, path: &str, mut outcome: Outcome) -> Option<Outcome> {
        if self.0.is_empty() {
            return Some(outcome);
        }
        for diagnostic in outcome.found.iter_mut().chain(&mut outcome.unused) {
            if !self.diagnostic(diagnostic) {
                return None;
            }
        }
        for (_, _, label) in &mut outcome.labels {
            label.first = self.moved(path, label.first, Position::Token)?;
            label.last = self.moved(path, label.last, Position::Token)?;
        }
        Some(outcome)
    }
}

/// The body of an item of a file.
fn item_body(file: &SourceFile, item: &ItemSyntax) -> Option<BodyAt> {
    let tree = item.tree.as_ref().filter(|_| !item.abandoned)?;
    let (rule, mut holder) = Item { file, tree }.kind()?;
    match rule {
        Rule::FnItem => holder = tree.find(holder, Rule::FnEnd)?,
        Rule::TestItem => {}
        _ => return None,
    }
    let body = tree.find(holder, Rule::FnBody)?;
    let (head, _) = tree.span(body)?;
    let close = tree
        .child_tokens(body)
        .find(|token| file.tokens[*token as usize].kind == TokenKind::Punct(Punct::RBrace))?;
    if head as usize <= item.first || close < head {
        return None;
    }
    let (before, closer) = (
        &file.tokens[head as usize - 1],
        &file.tokens[close as usize],
    );
    if closer.start < before.end {
        return None;
    }
    Some(BodyAt {
        hash: Hash::of(&file.source[before.end as usize..closer.start as usize]),
        bytes: Stretch {
            start: before.end,
            end: closer.start,
        },
        lines: Stretch {
            start: before.line,
            end: closer.line,
        },
        tokens: Stretch {
            start: head - 1,
            end: close,
        },
    })
}

/// A source file's shape.
fn file_shape(file: &SourceFile) -> FileShape {
    let mut skeleton = Vec::new();
    let mut bodies = Vec::new();
    let mut from = 0;
    for item in &file.items {
        let found = item_body(file, item);
        bodies.push(found);
        if let Some(body) = found {
            bytes(&mut skeleton, &file.source[from..body.bytes.start as usize]);
            from = body.bytes.end as usize;
        }
    }
    bytes(&mut skeleton, &file.source[from..]);
    FileShape {
        skeleton: Hash::of(&skeleton),
        bodies,
    }
}

/// How positions moved between two shapes of a file with the same text outside bodies; none
/// for shapes that differ there.
fn file_moves(path: &str, old: &FileShape, new: &FileShape) -> Option<FileMoves> {
    if new.skeleton != old.skeleton || new.bodies.len() != old.bodies.len() {
        return None;
    }
    let mut moves = FileMoves {
        path: path.to_owned(),
        old: Vec::new(),
        new: Vec::new(),
    };
    for (before, after) in old.bodies.iter().zip(&new.bodies) {
        match (before, after) {
            (Some(before), Some(after)) => {
                moves.old.push(*before);
                moves.new.push(*after);
            }
            (None, None) => {}
            _ => return None,
        }
    }
    Some(moves)
}

const SNAPSHOT_HEADER: &str = "nan kept 2";

fn put_flag(out: &mut Vec<u8>, value: bool) {
    uvar(out, u128::from(value));
}

fn put_text(out: &mut Vec<u8>, value: &str) {
    bytes(out, value.as_bytes());
}

fn put_hash(out: &mut Vec<u8>, value: &Hash) {
    bytes(out, &value.0);
}

fn put_number(out: &mut Vec<u8>, value: impl Into<u128>) {
    uvar(out, value.into());
}

fn put_item(out: &mut Vec<u8>, item: ItemId) {
    put_number(out, item.0);
}

fn put_items<'i>(out: &mut Vec<u8>, items: impl ExactSizeIterator<Item = &'i ItemId>) {
    put_number(out, items.len() as u64);
    for item in items {
        put_item(out, *item);
    }
}

fn put_optional<T>(out: &mut Vec<u8>, value: Option<T>, put: impl FnOnce(&mut Vec<u8>, T)) {
    put_flag(out, value.is_some());
    if let Some(value) = value {
        put(out, value);
    }
}

fn put_span(out: &mut Vec<u8>, span: Option<(u32, u32)>) {
    put_optional(out, span, |out, (start, end)| {
        put_number(out, start);
        put_number(out, end);
    });
}

/// The roles of reasons, by their number in a snapshot.
const ROLES: [Role; 12] = [
    Role::Declared,
    Role::Bound,
    Role::Frozen,
    Role::Held,
    Role::Moved,
    Role::Used,
    Role::Accessed,
    Role::Called,
    Role::Raised,
    Role::Command,
    Role::Note,
    Role::Diagnostic,
];

const OFFERS: [Offer; 3] = [Offer::Each, Offer::First, Offer::Unique];

/// A diagnostic, every part of it.
fn put_diagnostic(out: &mut Vec<u8>, diagnostic: &Diagnostic) {
    put_text(out, diagnostic.code.name());
    for text in [
        &diagnostic.path,
        &diagnostic.item,
        &diagnostic.expected,
        &diagnostic.found,
    ] {
        put_optional(out, text.as_deref(), put_text);
    }
    put_optional(out, diagnostic.line, put_number);
    put_number(out, diagnostic.because.len() as u64);
    for reason in &diagnostic.because {
        let role = ROLES.iter().position(|role| *role == reason.role);
        put_number(out, role.expect("a role of reasons") as u64);
        put_text(out, &reason.subject);
        put_optional(out, reason.at.as_ref(), |out, (path, line)| {
            put_text(out, path);
            put_number(out, *line);
        });
    }
    put_span(out, diagnostic.span);
    put_span(out, diagnostic.judged);
    put_optional(out, diagnostic.failing, put_number);
    put_number(out, diagnostic.fixes.len() as u64);
    for Fix(replacements) in &diagnostic.fixes {
        put_number(out, replacements.len() as u64);
        for (path, replacement) in replacements {
            put_text(out, path);
            put_number(out, replacement.start);
            put_number(out, replacement.end);
            put_text(out, &replacement.text);
        }
    }
    let offer = OFFERS.iter().position(|offer| *offer == diagnostic.offer);
    put_number(out, offer.expect("an offer") as u64);
    put_optional(out, diagnostic.envelope.as_ref(), |out, (origin, lines)| {
        put_text(out, origin);
        put_number(out, lines.len() as u64);
        for line in lines {
            put_text(out, line);
        }
    });
}

fn put_authority(out: &mut Vec<u8>, authority: &Authority) {
    let number: u8 = match authority {
        Authority::Device => 0,
        Authority::Priv => 1,
        Authority::Trusted => 2,
        Authority::Validate => 3,
        Authority::Contribute(_) => 4,
        Authority::Capability => 5,
        Authority::Confined => 6,
    };
    put_number(out, number);
    if let Authority::Contribute(path) = authority {
        put_text(out, path);
    }
}

fn put_item_set(out: &mut Vec<u8>, set: Option<&BTreeSet<ItemId>>) {
    put_optional(out, set, |out, set| put_items(out, set.iter()));
}

fn put_digest(out: &mut Vec<u8>, digest: &Digest) {
    for flag in [
        digest.wait,
        digest.alloc,
        digest.device,
        digest.privileged,
        digest.sync,
        digest.terminates,
        digest.float,
    ] {
        put_flag(out, flag);
    }
    put_item_set(out, digest.capabilities.as_ref());
    put_item_set(out, digest.validators.as_ref());
    match digest.class {
        Reversibility::Traceless => put_number(out, 0u8),
        Reversibility::Recorded => put_number(out, 1u8),
        Reversibility::Restorable => put_number(out, 2u8),
        Reversibility::Irreversible(kinds) => {
            put_number(out, 3u8);
            put_number(out, kinds);
        }
    }
}

fn put_shape(out: &mut Vec<u8>, shape: &FileShape) {
    put_hash(out, &shape.skeleton);
    put_number(out, shape.bodies.len() as u64);
    for body in &shape.bodies {
        put_optional(out, body.as_ref(), |out, body| {
            put_hash(out, &body.hash);
            for stretch in [body.bytes, body.lines, body.tokens] {
                put_number(out, stretch.start);
                put_number(out, stretch.end);
            }
        });
    }
}

fn put_green(out: &mut Vec<u8>, green: &Greened) {
    put_number(out, green.api.len() as u64);
    for line in &green.api {
        put_text(out, line);
    }
    put_optional(out, green.interface.as_ref(), put_hash);
}

fn put_diagnostics(out: &mut Vec<u8>, diagnostics: &[Diagnostic]) {
    put_number(out, diagnostics.len() as u64);
    for diagnostic in diagnostics {
        put_diagnostic(out, diagnostic);
    }
}

fn put_outcome(out: &mut Vec<u8>, outcome: &Outcome) {
    put_flag(out, outcome.alone);
    put_flag(out, outcome.rejected);
    put_diagnostics(out, &outcome.found);
    put_diagnostics(out, &outcome.unused);
    put_number(out, outcome.fields.len() as u64);
    for (owner, name) in &outcome.fields {
        put_item(out, *owner);
        put_text(out, name);
    }
    put_number(out, outcome.labels.len() as u64);
    for (owner, name, label) in &outcome.labels {
        put_item(out, *owner);
        put_text(out, name);
        put_number(out, label.first);
        put_number(out, label.last);
        put_flag(out, label.simple);
    }
    put_number(out, outcome.performs.len() as u64);
    for authority in &outcome.performs {
        put_authority(out, authority);
    }
    put_items(out, outcome.needs.iter());
    put_optional(out, outcome.names.as_ref(), |out, names| {
        put_items(out, names.iter());
    });
    put_optional(out, outcome.value.as_ref(), put_hash);
}

/// The bytes of a snapshot and the position read to; a read past their end, or of a part
/// malformed, fails the whole read.
struct Cursor<'b> {
    bytes: &'b [u8],
    at: usize,
}

impl<'b> Cursor<'b> {
    fn number(&mut self) -> Option<u64> {
        let (mut value, mut shift) = (0u64, 0);
        loop {
            let byte = *self.bytes.get(self.at)?;
            self.at += 1;
            if shift > 56 {
                return None;
            }
            value |= u64::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
            shift += 7;
        }
    }

    fn small(&mut self) -> Option<u32> {
        u32::try_from(self.number()?).ok()
    }

    fn count(&mut self) -> Option<usize> {
        usize::try_from(self.number()?)
            .ok()
            .filter(|count| *count <= self.bytes.len())
    }

    fn flag(&mut self) -> Option<bool> {
        Some(self.number()? == 1)
    }

    fn chunk(&mut self) -> Option<&'b [u8]> {
        let length = self.count()?;
        let chunk = self.bytes.get(self.at..self.at.checked_add(length)?)?;
        self.at += length;
        Some(chunk)
    }

    fn text(&mut self) -> Option<String> {
        String::from_utf8(self.chunk()?.to_vec()).ok()
    }

    fn hash(&mut self) -> Option<Hash> {
        Some(Hash(self.chunk()?.try_into().ok()?))
    }

    fn item(&mut self) -> Option<ItemId> {
        Some(ItemId(self.small()?))
    }

    fn list<T>(&mut self, mut each: impl FnMut(&mut Self) -> Option<T>) -> Option<Vec<T>> {
        let count = self.count()?;
        (0..count).map(|_| each(self)).collect()
    }

    fn optional<T>(&mut self, each: impl FnOnce(&mut Self) -> Option<T>) -> Option<Option<T>> {
        match self.flag()? {
            true => Some(Some(each(self)?)),
            false => Some(None),
        }
    }

    fn span(&mut self) -> Option<Option<(u32, u32)>> {
        self.optional(|cursor| Some((cursor.small()?, cursor.small()?)))
    }

    fn diagnostic(&mut self) -> Option<Diagnostic> {
        let code = Code::from_name(&self.text()?)?;
        let path = self.optional(Self::text)?;
        let item = self.optional(Self::text)?;
        let expected = self.optional(Self::text)?;
        let found = self.optional(Self::text)?;
        let line = self.optional(Self::small)?;
        let because = self.list(|cursor| {
            Some(Reason {
                role: *ROLES.get(cursor.count()?)?,
                subject: cursor.text()?,
                at: cursor.optional(|cursor| Some((cursor.text()?, cursor.small()?)))?,
            })
        })?;
        let span = self.span()?;
        let judged = self.span()?;
        let failing = self.optional(Self::small)?;
        let fixes = self.list(|cursor| {
            Some(Fix(cursor.list(|cursor| {
                Some((
                    cursor.text()?,
                    Replacement {
                        start: cursor.small()?,
                        end: cursor.small()?,
                        text: cursor.text()?,
                    },
                ))
            })?))
        })?;
        let offer = *OFFERS.get(self.count()?)?;
        let envelope = self.optional(|cursor| Some((cursor.text()?, cursor.list(Self::text)?)))?;
        Some(Diagnostic {
            code,
            path,
            line,
            item,
            expected,
            found,
            because,
            span,
            judged,
            failing,
            fixes,
            offer,
            envelope,
        })
    }

    fn authority(&mut self) -> Option<Authority> {
        Some(match self.number()? {
            0 => Authority::Device,
            1 => Authority::Priv,
            2 => Authority::Trusted,
            3 => Authority::Validate,
            4 => Authority::Contribute(self.text()?),
            5 => Authority::Capability,
            6 => Authority::Confined,
            _ => return None,
        })
    }

    fn items(&mut self) -> Option<Vec<ItemId>> {
        self.list(Self::item)
    }

    fn item_set(&mut self) -> Option<Option<BTreeSet<ItemId>>> {
        self.optional(|cursor| Some(cursor.items()?.into_iter().collect()))
    }

    fn digest(&mut self) -> Option<Digest> {
        let mut flags = [false; 7];
        for flag in &mut flags {
            *flag = self.flag()?;
        }
        let [wait, alloc, device, privileged, sync, terminates, float] = flags;
        let capabilities = self.item_set()?;
        let validators = self.item_set()?;
        let class = match self.number()? {
            0 => Reversibility::Traceless,
            1 => Reversibility::Recorded,
            2 => Reversibility::Restorable,
            3 => Reversibility::Irreversible(u8::try_from(self.number()?).ok()?),
            _ => return None,
        };
        Some(Digest {
            wait,
            alloc,
            device,
            privileged,
            sync,
            terminates,
            float,
            capabilities,
            validators,
            class,
        })
    }

    fn shape(&mut self) -> Option<FileShape> {
        let skeleton = self.hash()?;
        let bodies = self.list(|cursor| {
            cursor.optional(|cursor| {
                let hash = cursor.hash()?;
                let mut stretch = || {
                    Some(Stretch {
                        start: cursor.small()?,
                        end: cursor.small()?,
                    })
                };
                Some(BodyAt {
                    hash,
                    bytes: stretch()?,
                    lines: stretch()?,
                    tokens: stretch()?,
                })
            })
        })?;
        Some(FileShape { skeleton, bodies })
    }

    fn reads_by(&mut self) -> Option<ReadsBy> {
        let by = self.list(|cursor| Some((cursor.item()?, cursor.chunk()?.to_vec())))?;
        Some(ReadsBy(by))
    }

    fn green(&mut self) -> Option<Greened> {
        Some(Greened {
            api: self.list(Self::text)?,
            interface: self.optional(Self::hash)?,
        })
    }

    fn outcome(&mut self, item: ItemId) -> Option<Outcome> {
        let alone = self.flag()?;
        let rejected = self.flag()?;
        let found = self.list(Self::diagnostic)?;
        let unused = self.list(Self::diagnostic)?;
        let fields = self.list(|cursor| Some((cursor.item()?, cursor.text()?)))?;
        let labels = self.list(|cursor| {
            Some((
                cursor.item()?,
                cursor.text()?,
                FieldLabel {
                    item,
                    first: cursor.small()?,
                    last: cursor.small()?,
                    simple: cursor.flag()?,
                },
            ))
        })?;
        let performs = self.list(Self::authority)?;
        let needs = self.items()?;
        let names = self.optional(Self::items)?;
        let value = self.optional(Self::hash)?;
        Some(Outcome {
            alone,
            rejected,
            found,
            unused,
            fields,
            labels,
            performs,
            needs,
            names,
            value,
        })
    }

    fn snapshot(&mut self) -> Option<Snapshot> {
        if self.text()? != SNAPSHOT_HEADER {
            return None;
        }
        let context = self.hash()?;
        let items = self.small()? as usize;
        let files = self.list(|cursor| {
            Some(SnapshotFile {
                path: cursor.text()?,
                content: cursor.hash()?,
                shape: cursor.shape()?,
            })
        })?;
        let seeds = self.list(|cursor| {
            Some(Seed {
                item: cursor.item().filter(|item| (item.0 as usize) < items)?,
                digest: cursor.optional(Self::digest)?,
                body: cursor.optional(|cursor| cursor.optional(Self::items))?,
                traps: cursor.optional(Self::flag)?,
            })
        })?;
        let reads = self.reads_by()?;
        let names_reads = self.reads_by()?;
        let greens = self.list(|cursor| Some((cursor.item()?, cursor.green()?)))?;
        let outcomes = self.list(|cursor| {
            let item = cursor.item()?;
            let mut held = Cursor {
                bytes: cursor.chunk()?,
                at: 0,
            };
            let outcome = held.outcome(item)?;
            (held.at == held.bytes.len()).then_some((item, outcome))
        })?;
        (self.at == self.bytes.len()).then_some(Snapshot {
            context,
            items,
            files,
            seeds,
            reads,
            names_reads,
            greens: greens.into_iter().collect(),
            outcomes: outcomes.into_iter().collect(),
        })
    }
}

/// The strongly connected components of a graph over item indexes: for each node the least node
/// of its component, and whether its component holds a cycle.
#[derive(PartialEq, Eq)]
struct Components {
    least: Vec<u32>,
    cyclic: Vec<bool>,
}

fn components(edges: &[Vec<u32>]) -> Components {
    let count = edges.len();
    let mut search = Tarjan {
        number: vec![None; count],
        low: vec![0; count],
        on_stack: vec![false; count],
        stack: Vec::new(),
        frames: Vec::new(),
        counter: 0,
        found: Components {
            least: vec![0; count],
            cyclic: vec![false; count],
        },
    };
    for root in 0..count {
        if search.number[root].is_some() {
            continue;
        }
        search.enter(root);
        while let Some((node, next)) = search.frames.last().copied() {
            if let Some(&target) = edges[node].get(next) {
                search.frames.last_mut().expect("a frame").1 += 1;
                let target = target as usize;
                match search.number.get(target) {
                    Some(None) => search.enter(target),
                    Some(Some(reached)) if search.on_stack[target] => {
                        search.low[node] = search.low[node].min(*reached);
                    }
                    _ => {}
                }
                continue;
            }
            search.frames.pop();
            if let Some(&(parent, _)) = search.frames.last() {
                search.low[parent] = search.low[parent].min(search.low[node]);
            }
            if Some(search.low[node]) == search.number[node] {
                search.close(node, edges[node].contains(&(node as u32)));
            }
        }
    }
    search.found
}

/// The state of Tarjan's search over item indexes.
struct Tarjan {
    number: Vec<Option<u32>>,
    low: Vec<u32>,
    on_stack: Vec<bool>,
    stack: Vec<u32>,
    /// The nodes whose edges the search walks, each with the next to walk.
    frames: Vec<(usize, usize)>,
    counter: u32,
    found: Components,
}

impl Tarjan {
    fn enter(&mut self, node: usize) {
        self.number[node] = Some(self.counter);
        self.low[node] = self.counter;
        self.counter += 1;
        self.stack.push(node as u32);
        self.on_stack[node] = true;
        self.frames.push((node, 0));
    }

    /// Pops the component whose root is `node`, cyclic when it has several members or
    /// `looping`, its root depending on itself.
    fn close(&mut self, node: usize, looping: bool) {
        let mut members = Vec::new();
        while let Some(member) = self.stack.pop() {
            self.on_stack[member as usize] = false;
            members.push(member);
            if member as usize == node {
                break;
            }
        }
        let cyclic = members.len() > 1 || looping;
        let least = *members.iter().min().expect("a component's member");
        for member in members {
            self.found.least[member as usize] = least;
            self.found.cyclic[member as usize] = cyclic;
        }
    }
}

/// The dependencies through typed bodies a snapshot holds, by item index.
fn snapshot_edges(snapshot: &Snapshot) -> Vec<Vec<u32>> {
    let mut edges = vec![Vec::new(); snapshot.items];
    for seed in &snapshot.seeds {
        if let Some(Some(listed)) = &seed.body {
            edges[seed.item.0 as usize] = listed.iter().map(|item| item.0).collect();
        }
    }
    edges
}

/// The items a check of a snapshot's state checks again from those it checks first: those
/// whose own work read the body of one checked again, closed under that relation and under the
/// cycles of the snapshot's dependencies; by item index. None when its reads do not decode.
fn rechecked(snapshot: &Snapshot, first: &[ItemId], old: &Components) -> Option<Vec<bool>> {
    let mut readers: HashMap<u32, Vec<u32>> = HashMap::new();
    for (reader, read) in snapshot.reads.pairs()? {
        readers.entry(read.0).or_default().push(reader.0);
    }
    let mut cycles: HashMap<u32, Vec<u32>> = HashMap::new();
    for index in 0..snapshot.items {
        if old.cyclic[index] {
            cycles
                .entry(old.least[index])
                .or_default()
                .push(index as u32);
        }
    }
    let mut marked = vec![false; snapshot.items];
    let mut pending: Vec<u32> = Vec::new();
    for item in first {
        if !marked[item.0 as usize] {
            marked[item.0 as usize] = true;
            pending.push(item.0);
        }
    }
    while let Some(node) = pending.pop() {
        let mut next: Vec<u32> = readers.get(&node).cloned().unwrap_or_default();
        if old.cyclic[node as usize] {
            next.extend(cycles.get(&old.least[node as usize]).into_iter().flatten());
        }
        for reached in next {
            if let Some(mark) = marked.get_mut(reached as usize)
                && !*mark
            {
                *mark = true;
                pending.push(reached);
            }
        }
    }
    Some(marked)
}

/// Whether diagnostics that the reported ones order alike come from the own work of one item,
/// or rules relating items, each: then their order does not depend on when that work ran.
fn owned_apart(found: &[Found]) -> bool {
    let mut owners = HashMap::new();
    found.iter().all(|Found { owner, diagnostic }| {
        let key = (&diagnostic.path, diagnostic.line, diagnostic.code);
        *owners.entry(key).or_insert(*owner) == *owner
    })
}

/// A file's path and the hash of its contents.
fn put_file(out: &mut Vec<u8>, path: &str, content: &[u8]) {
    put_text(out, path);
    put_hash(out, &Hash::of(content));
}

/// The key and bytes of the snapshot of a state, or else of the latest state checked of its
/// closure under its target in its workspace; none when the build cache holds neither.
fn latest_snapshot(cache: &Cache, state: &State) -> Option<(Hash, Vec<u8>)> {
    if let Ok(Ok(Some(bytes))) = cache.entry(&state.key) {
        return Some((state.key, bytes));
    }
    let named = cache.record(&state.latest).ok()?.ok()??;
    let key = Cursor {
        bytes: &named,
        at: 0,
    }
    .hash()?;
    Some((key, cache.entry(&key).ok()?.ok()??))
}

/// The checker of a closure under a target, narrowed to the workspace package at an index, if
/// any, once checked: from the build cache's snapshot of the state, or of the latest state
/// checked when the two differ in function bodies alone and checking what those reach gives the
/// facts that snapshot holds; otherwise by a full check.
pub(super) fn kept_checked<'a>(
    workspace: &'a Workspace,
    closure: &Closure,
    std: &'a [Arc<Package>],
    target: Target,
    narrowed: Option<usize>,
) -> Checker<'a> {
    let fresh = || {
        let mut checker = unchecked(workspace, closure, std, target);
        checker.narrowed = narrowed;
        checker
    };
    let checker = fresh();
    let state = checker.check_state(closure);
    let found = Cache::open()
        .ok()
        .and_then(Result::ok)
        .and_then(|cache| latest_snapshot(&cache, &state));
    let Some((key, bytes)) = found else {
        checker.wholly_checked(state);
        return checker;
    };
    if checker.check_restored(state.clone(), key, &bytes) {
        return checker;
    }
    drop(checker);
    let checker = fresh();
    checker.wholly_checked(state);
    checker
}

impl Checker<'_> {
    /// Whether a snapshot gives the outcome of an item.
    pub(super) fn is_restored(&self, item: Option<ItemId>) -> bool {
        item.is_some_and(|item| self.kept.restored.borrow().contains(&item))
    }

    /// The item whose own work is computed, when it reads another item's body or names as its
    /// own read.
    fn reader(&self, item: ItemId) -> Option<ItemId> {
        (self.traced.get()).filter(|reader| *reader != item && !self.untraced.get())
    }

    /// Records that the item whose own work is computed reads another item's body.
    pub(crate) fn trace_read(&self, item: ItemId) {
        if let Some(reader) = self.reader(item) {
            self.kept.reads.borrow_mut().insert((reader, item));
        }
    }

    /// Records that the item whose own work is computed reads the items another item names.
    pub(super) fn trace_names_read(&self, item: ItemId) {
        if let Some(reader) = self.reader(item) {
            self.kept.names_reads.borrow_mut().insert((reader, item));
        }
    }

    /// Computes what a restored check takes from the items' heads or compares: what it reads of
    /// another item's body or names is not the traced item's read.
    pub(crate) fn untraced<T>(&self, compute: impl FnOnce() -> T) -> T {
        let outer = self.untraced.replace(true);
        let computed = compute();
        self.untraced.set(outer);
        computed
    }

    /// Records that the item whose own work is computed reads a struct's field.
    pub(super) fn kept_field_read(&self, owner: ItemId, field: &str) {
        if let Some(reader) = self.traced.get() {
            self.kept
                .fields
                .borrow_mut()
                .insert((reader, owner, field.to_owned()));
        }
    }

    /// An item's api lines and interface hash, as its last-green entry holds them: those the
    /// snapshot restored from holds, or those computed, kept for the next snapshot.
    pub(super) fn greened(&self, item: ItemId) -> Greened {
        let restored = self.kept.plan.borrow().as_ref().and_then(|plan| {
            let restoring = plan.restored.as_ref()?;
            restoring.greens.get(&item).cloned()
        });
        if let Some(green) = restored {
            return green;
        }
        if let Some(green) = self.kept.greens.borrow().get(&item) {
            return green.clone();
        }
        let green = Greened {
            api: self.api_lines(item, true, None),
            interface: self.hashes(item).interface,
        };
        self.kept.greens.borrow_mut().insert(item, green.clone());
        green
    }

    /// The state of the checker's closure under its target: its source files with their hashes,
    /// and the hash of its other inputs, the toolchain's, the target, its other packages' files
    /// and releases, the local store's index and the database's items.
    fn check_state(&self, closure: &Closure) -> State {
        let workspace = self.db.workspace;
        let toolchain = crate::toolchain::hash(1);
        let checked: Vec<usize> = checked_packages(self)
            .into_iter()
            .map(|(index, _)| index)
            .collect();
        let mut context = Vec::new();
        put_text(&mut context, "kept 1");
        put_hash(&mut context, &toolchain);
        put_text(&mut context, self.db.target.name());
        put_number(&mut context, self.db.items.len() as u64);
        let mut latest = Vec::new();
        put_hash(&mut latest, &toolchain);
        put_text(&mut latest, self.db.target.name());
        put_text(&mut latest, &workspace.root.to_string_lossy());
        if let Some(index) = self.narrowed {
            put_text(&mut latest, &workspace.packages[index].dir);
        }
        let mut files = Vec::new();
        for (name, member) in &closure.members {
            put_text(&mut context, name);
            put_text(&mut latest, name);
            match *member {
                Member::Release(index) => put_hash(&mut context, &workspace.releases[index].root),
                Member::Source(index) => {
                    let package = &workspace.packages[index];
                    if !checked.contains(&index) {
                        for (path, content) in super::root::package_files(package) {
                            put_file(&mut context, &path, &content);
                        }
                        continue;
                    }
                    for (path, content) in package.machine.files() {
                        put_file(&mut context, path, content);
                    }
                    for file in &package.files {
                        files.push((file.path.clone(), file.content));
                    }
                }
            }
        }
        let index = Cache::open()
            .ok()
            .and_then(Result::ok)
            .and_then(|cache| crate::store::Store::new(cache.store()).index().ok()?.ok())
            .unwrap_or_default();
        for (number, root) in index {
            put_number(&mut context, number);
            put_hash(&mut context, &root);
        }
        let context = Hash::of(&context);
        let mut keyed = Vec::new();
        put_hash(&mut keyed, &context);
        for (path, content) in &files {
            put_text(&mut keyed, path);
            put_hash(&mut keyed, content);
        }
        State {
            key: Hash::of(&keyed),
            latest: format!("latest-{}", Hash::of(&latest).full()),
            context,
            files,
        }
    }

    /// The items whose outcomes a snapshot keeps: those of the packages the checker checks.
    fn outcome_items(&self) -> Vec<ItemId> {
        checked_packages(self)
            .into_iter()
            .flat_map(|(_, package)| &self.db.package(package).modules)
            .flat_map(|module| self.db.module(*module).items.iter().copied())
            .collect()
    }

    /// The interface hash of a const whose value is known, which holds that value.
    fn const_hash(&self, item: ItemId) -> Option<Hash> {
        let known = matches!(
            self.db.ast(item).kind,
            crate::syntax::ast::ItemKind::Const(_)
        ) && self.const_values.borrow().contains_key(&item);
        known.then(|| self.hashes(item).interface).flatten()
    }

    /// The facts of an item a snapshot seeds a later check with, when the check computed one.
    fn seed_of(&self, item: ItemId) -> Option<Seed> {
        let seed = Seed {
            item,
            digest: self
                .digests
                .borrow()
                .done
                .get(&item)
                .map(|done| (**done).clone()),
            body: self
                .body_dependencies
                .borrow()
                .get(&item)
                .map(|found| found.as_ref().map(|listed| listed.to_vec())),
            traps: self.may_traps.borrow().get(&item).copied(),
        };
        (seed.digest.is_some() || seed.body.is_some() || seed.traps.is_some()).then_some(seed)
    }

    /// The outcome of each item's own work in the checker, by item.
    fn outcomes(&self, items: &[ItemId]) -> HashMap<ItemId, Outcome> {
        let mut outcomes: HashMap<ItemId, Outcome> = items
            .iter()
            .map(|item| {
                let outcome = Outcome {
                    alone: self.kept.alone.borrow().get(item).copied().unwrap_or(false),
                    rejected: self.rejections.borrow().contains(item),
                    found: Vec::new(),
                    unused: Vec::new(),
                    fields: Vec::new(),
                    labels: Vec::new(),
                    performs: Vec::new(),
                    needs: self
                        .needs
                        .borrow()
                        .get(item)
                        .map(|needed| needed.iter().copied().collect())
                        .unwrap_or_default(),
                    names: self
                        .named
                        .borrow()
                        .get(item)
                        .map(|named| named.iter().copied().collect()),
                    value: self.const_hash(*item),
                };
                (*item, outcome)
            })
            .collect();
        for found in self.found.borrow().iter() {
            if let Some(outcome) = found.owner.and_then(|owner| outcomes.get_mut(&owner)) {
                outcome.found.push(found.diagnostic.clone());
            }
        }
        for found in self.unused.borrow().iter() {
            if let Some(outcome) = found.owner.and_then(|owner| outcomes.get_mut(&owner)) {
                outcome.unused.push(found.diagnostic.clone());
            }
        }
        for (reader, owner, name) in self.kept.fields.borrow().iter() {
            if let Some(outcome) = outcomes.get_mut(reader) {
                outcome.fields.push((*owner, name.clone()));
            }
        }
        for (owner, name, label) in self.kept.labels.borrow().iter() {
            if let Some(outcome) = outcomes.get_mut(&label.item) {
                outcome.labels.push((*owner, name.clone(), *label));
            }
        }
        for (item, authority) in self.kept.performs.borrow().iter() {
            if let Some(outcome) = outcomes.get_mut(item) {
                outcome.performs.push(authority.clone());
            }
        }
        outcomes
    }

    /// The snapshot of the state the checker checked.
    fn snapshot(&self, plan: &Plan) -> Vec<u8> {
        let workspace = self.db.workspace;
        let mut out = Vec::new();
        put_text(&mut out, SNAPSHOT_HEADER);
        put_hash(&mut out, &plan.state.context);
        put_number(&mut out, self.db.items.len() as u64);
        put_number(&mut out, plan.state.files.len() as u64);
        for (index, (path, content)) in plan.state.files.iter().enumerate() {
            put_text(&mut out, path);
            put_hash(&mut out, content);
            let kept = plan
                .restored
                .as_ref()
                .and_then(|restoring| restoring.files.get(index))
                .filter(|file| file.path == *path && file.content == *content);
            match kept {
                Some(file) => put_shape(&mut out, &file.shape),
                None => {
                    let (_, file) = workspace.file(path).expect("a file of the state checked");
                    put_shape(&mut out, &file_shape(&file.syntax));
                }
            }
        }
        let seeds: Vec<Seed> = (0..self.db.items.len())
            .filter_map(|index| self.seed_of(ItemId(index as u32)))
            .collect();
        put_number(&mut out, seeds.len() as u64);
        for seed in &seeds {
            put_item(&mut out, seed.item);
            put_optional(&mut out, seed.digest.as_ref(), put_digest);
            put_optional(&mut out, seed.body.as_ref(), |out, body| {
                put_optional(out, body.as_ref(), |out, items| {
                    put_items(out, items.iter())
                });
            });
            put_optional(&mut out, seed.traps, put_flag);
        }
        let restored = plan.restored.as_ref();
        let fresh = |reads: &BTreeSet<(ItemId, ItemId)>| {
            ReadsBy::of(
                &(reads.iter().copied())
                    .filter(|(reader, read)| {
                        !self.is_restored(Some(*reader))
                            && self.db.is_reported(*reader)
                            && self.db.is_reported(*read)
                    })
                    .collect(),
            )
        };
        let reads = fresh(&self.kept.reads.borrow());
        match restored {
            Some(restoring) => restoring.reads.joined(reads).put(&mut out),
            None => reads.put(&mut out),
        }
        let names_reads = fresh(&self.kept.names_reads.borrow());
        match restored {
            Some(restoring) => restoring.names_reads.joined(names_reads).put(&mut out),
            None => names_reads.put(&mut out),
        }
        let computed = self.kept.greens.borrow();
        let mut greens: Vec<(&ItemId, &Greened)> = restored
            .into_iter()
            .flat_map(|restoring| &restoring.greens)
            .chain(computed.iter())
            .collect();
        greens.sort_by_key(|(item, _)| **item);
        greens.dedup_by_key(|(item, _)| **item);
        put_number(&mut out, greens.len() as u64);
        for (item, green) in greens {
            put_item(&mut out, *item);
            put_green(&mut out, green);
        }
        let items = self.outcome_items();
        let held = |item: &ItemId| restored.and_then(|restoring| restoring.outcomes.get(item));
        let unheld: Vec<ItemId> = items
            .iter()
            .copied()
            .filter(|item| held(item).is_none())
            .collect();
        let mut fresh = self.outcomes(&unheld);
        put_number(&mut out, items.len() as u64);
        for item in items {
            put_item(&mut out, item);
            let mut encoded = Vec::new();
            match held(&item) {
                Some(outcome) => put_outcome(&mut encoded, outcome),
                None => put_outcome(&mut encoded, &fresh.remove(&item).expect("an outcome")),
            }
            bytes(&mut out, &encoded);
        }
        out
    }

    /// The items whose bodies differ between a snapshot's state and the state checked, and how
    /// positions moved; none when the states differ elsewhere, in any other input, outside the
    /// bodies of a file, or in a body the database holds no item of.
    fn changed_bodies(&self, snapshot: &Snapshot, state: &State) -> Option<(Vec<ItemId>, Moves)> {
        let workspace = self.db.workspace;
        if snapshot.context != state.context
            || snapshot.items != self.db.items.len()
            || snapshot.files.len() != state.files.len()
        {
            return None;
        }
        let mut changed: Vec<(&str, usize)> = Vec::new();
        let mut moves = Vec::new();
        for ((path, content), old) in state.files.iter().zip(&snapshot.files) {
            if old.path != *path {
                return None;
            }
            if old.content == *content {
                continue;
            }
            let shape = file_shape(&workspace.file(path)?.1.syntax);
            let moved = file_moves(path, &old.shape, &shape)?;
            for (position, (new, old)) in shape.bodies.iter().zip(&old.shape.bodies).enumerate() {
                if new.map(|body| body.hash) != old.map(|body| body.hash) {
                    changed.push((path, position));
                }
            }
            moves.push(moved);
        }
        let mut located: HashMap<(&str, usize), ItemId> = HashMap::new();
        if !changed.is_empty() {
            for (index, data) in self.db.items.iter().enumerate() {
                if !data.file.history {
                    located.insert((&data.file.path, data.index), ItemId(index as u32));
                }
            }
        }
        let items = changed
            .iter()
            .map(|at| located.get(at).copied())
            .collect::<Option<Vec<ItemId>>>()?;
        Some((items, Moves(moves)))
    }

    /// Seeds the checker with a snapshot's facts of the items not checked again.
    fn seed(&self, snapshot: &Snapshot, marked: &[bool]) {
        for seed in snapshot
            .seeds
            .iter()
            .filter(|seed| !marked[seed.item.0 as usize])
        {
            if let Some(digest) = &seed.digest {
                (self.digests.borrow_mut().done)
                    .entry(seed.item)
                    .or_insert_with(|| Rc::new(digest.clone()));
            }
            if let Some(body) = &seed.body {
                (self.body_dependencies.borrow_mut())
                    .entry(seed.item)
                    .or_insert_with(|| body.clone().map(Rc::new));
            }
            if let Some(traps) = seed.traps {
                self.may_traps
                    .borrow_mut()
                    .entry(seed.item)
                    .or_insert(traps);
            }
        }
    }

    /// Gives the checker an item's outcome in place of the item's own work.
    fn restore_outcome(&self, item: ItemId, outcome: &Outcome) {
        self.kept.restored.borrow_mut().insert(item);
        self.judged.borrow_mut().insert(item);
        self.kept.judged_alone(item, outcome.alone);
        if outcome.alone {
            self.rejections.borrow_mut().insert(item);
        }
        let owned = |diagnostics: &[Diagnostic]| -> Vec<Found> {
            diagnostics
                .iter()
                .map(|diagnostic| Found {
                    owner: Some(item),
                    diagnostic: diagnostic.clone(),
                })
                .collect()
        };
        self.found.borrow_mut().extend(owned(&outcome.found));
        self.unused.borrow_mut().extend(owned(&outcome.unused));
        let mut fields = self.fields_read.borrow_mut();
        fields.extend(outcome.fields.iter().cloned());
        let mut labels = self.field_labels.borrow_mut();
        for (owner, name, label) in &outcome.labels {
            labels
                .entry((*owner, name.clone()))
                .or_default()
                .push(*label);
        }
        let module = self.db.module_of(item);
        let mut performed = self.performed.borrow_mut();
        performed.extend(
            outcome
                .performs
                .iter()
                .map(|authority| (module, authority.clone())),
        );
        if !outcome.needs.is_empty() {
            (self.needs.borrow_mut())
                .entry(item)
                .or_default()
                .extend(outcome.needs.iter().copied());
        }
        if let Some(names) = &outcome.names {
            (self.named.borrow_mut())
                .entry(item)
                .or_insert_with(|| Rc::new(names.iter().copied().collect()));
        }
    }

    /// Seeds the checker with a snapshot's facts and gives it the outcomes and green entries of
    /// the items not checked again, at the positions of the state checked; none when one does
    /// not lie outside the changed bodies.
    fn restore(
        &self,
        snapshot: &Snapshot,
        marked: &[bool],
        moves: &Moves,
    ) -> Option<(HashMap<ItemId, Outcome>, HashMap<ItemId, Greened>)> {
        self.seed(snapshot, marked);
        let mut outcomes = HashMap::new();
        let mut greens = HashMap::new();
        for item in self.outcome_items() {
            if marked[item.0 as usize] {
                continue;
            }
            let outcome = snapshot.outcomes.get(&item)?.clone();
            let outcome = moves.outcome(&self.db.item(item).file.path, outcome)?;
            self.restore_outcome(item, &outcome);
            outcomes.insert(item, outcome);
            if let Some(green) = snapshot.greens.get(&item) {
                greens.insert(item, green.clone());
            }
        }
        Some((outcomes, greens))
    }

    /// Checks the state of a closure from the snapshot of an earlier state: restores the
    /// outcomes of the items whose bodies, and whose own work's reads, are unchanged, checks the
    /// others, and tells whether that gives what a full check of the state gives.
    fn check_restored(&self, state: State, key: Hash, bytes: &[u8]) -> bool {
        let Some(snapshot) = (Cursor { bytes, at: 0 }).snapshot() else {
            return false;
        };
        let Some((mut changed, moves)) = self.changed_bodies(&snapshot, &state) else {
            return false;
        };
        let items = self.outcome_items();
        changed.extend(
            items
                .iter()
                .filter(|item| !snapshot.outcomes.contains_key(item)),
        );
        let renamed: HashSet<ItemId> = changed
            .iter()
            .copied()
            .filter(|item| {
                let held = snapshot
                    .outcomes
                    .get(item)
                    .and_then(|outcome| outcome.names.as_ref());
                held.is_none_or(|names| !names.iter().eq(self.names(*item).iter()))
            })
            .collect();
        if !renamed.is_empty() {
            let Some(names_reads) = snapshot.names_reads.pairs() else {
                return false;
            };
            changed.extend(
                (names_reads.into_iter())
                    .filter(|(_, read)| renamed.contains(read))
                    .map(|(reader, _)| reader),
            );
        }
        let old = components(&snapshot_edges(&snapshot));
        let Some(marked) = rechecked(&snapshot, &changed, &old) else {
            return false;
        };
        let Some((outcomes, greens)) = self.restore(&snapshot, &marked, &moves) else {
            return false;
        };
        let facts: HashMap<ItemId, OutcomeFacts> = items
            .iter()
            .filter(|item| marked[item.0 as usize])
            .filter_map(|item| {
                let outcome = snapshot.outcomes.get(item)?;
                let facts = OutcomeFacts {
                    alone: outcome.alone,
                    rejected: outcome.rejected,
                    value: outcome.value,
                };
                Some((*item, facts))
            })
            .collect();
        *self.kept.plan.borrow_mut() = Some(Plan {
            state,
            restored: Some(Restoring {
                key,
                outcomes,
                reads: snapshot.reads.unmarked(&marked),
                names_reads: snapshot.names_reads.unmarked(&marked),
                greens,
                files: snapshot.files.clone(),
            }),
        });
        self.check_all();
        self.accepts(&snapshot, &facts, &marked, &old)
    }

    /// Checks the checker's state whole.
    fn wholly_checked(&self, state: State) {
        *self.kept.plan.borrow_mut() = Some(Plan {
            state,
            restored: None,
        });
        self.check_all();
    }

    /// Whether nothing the check reports depends on when restored work ran: no `name.case` fix,
    /// no `unused.field` fix joining labels, no error of a quiet package, and the diagnostics
    /// that order alike coming from one item's work or rules relating items each.
    fn reports_apart(&self) -> bool {
        let found = self.found.borrow();
        let unused = self.unused.borrow();
        !found
            .iter()
            .any(|found| found.diagnostic.code == Code::NameCase)
            && !unused
                .iter()
                .any(|found| found.diagnostic.code == Code::UnusedField)
            && !self
                .quiet
                .borrow()
                .iter()
                .any(|diagnostic| diagnostic.code.class() == Class::Error)
            && owned_apart(&found)
            && owned_apart(&unused)
    }

    /// Whether a check that restored from a snapshot gives what a full check gives: nothing it
    /// reports depends on when the restored work ran, the items it checked again give the facts
    /// the snapshot holds of them, and the cycles of the dependencies are the snapshot's.
    fn accepts(
        &self,
        snapshot: &Snapshot,
        facts: &HashMap<ItemId, OutcomeFacts>,
        marked: &[bool],
        old: &Components,
    ) -> bool {
        if !self.reports_apart() {
            return false;
        }
        for item in self.outcome_items() {
            let Some(held) = facts.get(&item) else {
                continue;
            };
            let Some(alone) = self.kept.alone.borrow().get(&item).copied() else {
                return false;
            };
            if alone != held.alone || self.rejections.borrow().contains(&item) != held.rejected {
                return false;
            }
            if held.value.is_some() {
                self.const_value(item);
                if self.const_hash(item) != held.value {
                    return false;
                }
            }
        }
        let mut reshaped = false;
        for seed in snapshot
            .seeds
            .iter()
            .filter(|seed| marked[seed.item.0 as usize])
        {
            if seed
                .digest
                .as_ref()
                .is_some_and(|digest| *self.uncompensated_digest(seed.item) != *digest)
            {
                return false;
            }
            if let Some(body) = &seed.body {
                let now = self.body_dependencies(seed.item);
                reshaped |= now.as_deref() != body.as_ref();
            }
            if seed
                .traps
                .is_some_and(|traps| self.may_trap(seed.item) != traps)
            {
                return false;
            }
        }
        !reshaped || self.cycles_agree(snapshot, old)
    }

    /// Whether the cycles of the dependencies the check computed are the snapshot's.
    fn cycles_agree(&self, snapshot: &Snapshot, old: &Components) -> bool {
        let mut edges = snapshot_edges(snapshot);
        for (item, found) in self.body_dependencies.borrow().iter() {
            if let Some(edge) = edges.get_mut(item.0 as usize) {
                *edge = found
                    .iter()
                    .flat_map(|listed| listed.iter().map(|item| item.0))
                    .collect();
            }
        }
        components(&edges) == *old
    }

    /// Writes the snapshot of the state the checker checked to the build cache, unless it
    /// restored from it, makes it the latest of its closure under its target in its workspace,
    /// and deletes the one it restored from.
    pub fn keep(&self) {
        let plan = self.kept.plan.borrow();
        let Some(plan) = plan.as_ref() else {
            return;
        };
        let quiet_error =
            (self.quiet.borrow().iter()).any(|diagnostic| diagnostic.code.class() == Class::Error);
        let Some(cache) = Cache::open()
            .ok()
            .and_then(Result::ok)
            .filter(|_| !quiet_error)
        else {
            return;
        };
        let restored = plan.restored.as_ref().map(|restoring| restoring.key);
        if restored != Some(plan.state.key)
            && cache
                .replace_entry(&plan.state.key, &self.snapshot(plan))
                .is_err()
        {
            return;
        }
        let mut named = Vec::new();
        put_hash(&mut named, &plan.state.key);
        if cache.replace(&plan.state.latest, &named).is_err() {
            return;
        }
        if let Some(key) = restored
            && key != plan.state.key
        {
            let _ = cache.remove(&key);
        }
    }
}

#[cfg(test)]
mod tests;
