//! Reversibility classes: their order, joins and compensation, their text and encoding, the
//! classes of the std operations without a body, and the undo clauses that compensate a digest.

use super::Checker;
use super::db::ItemId;
use super::resolve::Named;
use crate::diagnostic::Code;
use crate::encoding::{list, uvar};
use crate::syntax::ast::{ClauseKind, FnItem, ItemKind, Name, Path};
use crate::workspace::Target;

/// What code does to state outside the program, at most.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Class {
    #[default]
    Traceless,
    Recorded,
    Restorable,
    /// Irreversible effects of the kinds whose bits are set, the lowest bit the first kind.
    Irreversible(u8),
}

/// The words of the classes, in the class table's order.
pub const WORDS: [&str; 4] = ["traceless", "recorded", "restorable", "irreversible"];

/// The kinds of irreversible effects, in their order.
pub const KINDS: [&str; 4] = ["emit", "actuate", "erase", "spend"];

const EMIT: u8 = 1;
const ERASE: u8 = 4;

/// The volatile calls, which with DMA access calls are the device accesses.
const VOLATILE: [&str; 8] = [
    "hw::Mmio.read",
    "hw::Mmio.write",
    "hw::Mmio.read_at",
    "hw::Mmio.write_at",
    "hw::Ports.read",
    "hw::Ports.write",
    "hw::Ports.read_at",
    "hw::Ports.write_at",
];

/// The operations of the sys capabilities and their resources whose class is not `traceless`
/// on the host targets and under `any`, with that class.
const SYS: [(&str, Class); 14] = [
    ("sys::Log.info", Class::Recorded),
    ("sys::Log.warn", Class::Recorded),
    ("sys::Log.error", Class::Recorded),
    ("sys::Stdio.write_out", Class::Recorded),
    ("sys::Stdio.write_err", Class::Recorded),
    ("sys::fs::File.write", Class::Restorable),
    ("sys::Net.listen", Class::Restorable),
    ("sys::Fs.create", Class::Irreversible(ERASE)),
    ("sys::Fs.write_all", Class::Irreversible(ERASE)),
    ("sys::Fs.remove", Class::Irreversible(ERASE)),
    ("sys::Net.connect", Class::Irreversible(EMIT)),
    ("sys::net::Conn.write", Class::Irreversible(EMIT)),
    ("sys::net::Conn.close", Class::Irreversible(EMIT)),
    ("sys::net::Conn.abandon", Class::Irreversible(EMIT)),
];

impl Class {
    /// The greatest class.
    pub const WORST: Class = Class::Irreversible(0b1111);

    /// The class a word and its kinds name, if they name one.
    pub fn named<'a>(word: &str, kinds: impl IntoIterator<Item = &'a str>) -> Option<Class> {
        Some(match word {
            "traceless" => Class::Traceless,
            "recorded" => Class::Recorded,
            "restorable" => Class::Restorable,
            "irreversible" => {
                Class::Irreversible(kinds.into_iter().try_fold(0, |bits, kind| {
                    Some(bits | 1 << KINDS.iter().position(|known| *known == kind)?)
                })?)
            }
            _ => return None,
        })
    }

    /// The class a digest line writes.
    pub fn parse(text: &str) -> Option<Class> {
        match text.strip_prefix("irreversible(") {
            Some(kinds) => Class::named("irreversible", kinds.strip_suffix(')')?.split(", ")),
            None => Class::named(text, []),
        }
    }

    /// Its row's position in the class table.
    fn position(self) -> u8 {
        match self {
            Class::Traceless => 0,
            Class::Recorded => 1,
            Class::Restorable => 2,
            Class::Irreversible(_) => 3,
        }
    }

    pub fn join(self, other: Class) -> Class {
        match (self, other) {
            (Class::Irreversible(first), Class::Irreversible(second)) => {
                Class::Irreversible(first | second)
            }
            _ if other.position() > self.position() => other,
            _ => self,
        }
    }

    /// The greatest class at most both.
    pub fn meet(self, other: Class) -> Class {
        match (self, other) {
            (Class::Irreversible(first), Class::Irreversible(second)) => match first & second {
                0 => Class::Restorable,
                kinds => Class::Irreversible(kinds),
            },
            _ if other.position() < self.position() => other,
            _ => self,
        }
    }

    pub fn at_most(self, bound: Class) -> bool {
        match (self, bound) {
            (Class::Irreversible(kinds), Class::Irreversible(bound)) => kinds & !bound == 0,
            _ => self.position() <= bound.position(),
        }
    }

    /// The class of code with an undo clause: an emission stays irreversible, and every other
    /// effect becomes restorable.
    pub fn compensated(self) -> Class {
        match self {
            Class::Irreversible(kinds) if kinds & EMIT != 0 => Class::Irreversible(EMIT),
            Class::Irreversible(_) => Class::Restorable,
            other => other,
        }
    }

    /// The class as a limit and a digest line write it.
    pub fn text(self) -> String {
        match self {
            Class::Irreversible(kinds) => {
                let named: Vec<&str> = (KINDS.iter().enumerate())
                    .filter(|(bit, _)| kinds & 1 << bit != 0)
                    .map(|(_, kind)| *kind)
                    .collect();
                format!("irreversible({})", named.join(", "))
            }
            other => WORDS[usize::from(other.position())].to_owned(),
        }
    }

    /// The class in the canonical NIR encoding: its position, followed for an irreversible
    /// class by its kinds' positions.
    pub fn encode(self, out: &mut Vec<u8>) {
        uvar(out, u128::from(self.position()));
        if let Class::Irreversible(kinds) = self {
            let positions: Vec<usize> = (0..KINDS.len())
                .filter(|bit| kinds & 1 << bit != 0)
                .collect();
            list(out, positions.into_iter(), |out, position| {
                uvar(out, position as u128)
            });
        }
    }
}

/// A digest line's effects part split into its facts and its class, which it writes last.
pub fn split_effects(effects: &str) -> (&str, Class) {
    let written = (WORDS[1..].iter())
        .find_map(|word| effects.find(word))
        .and_then(|start| Some((start, Class::parse(&effects[start..])?)));
    match written {
        Some((start, class)) => (effects[..start].trim_end(), class),
        None => (effects, Class::Traceless),
    }
}

/// Whether a std operation is a volatile call.
pub fn is_volatile(path: &str) -> bool {
    VOLATILE.contains(&path)
}

/// The class of a std operation without a body under a target: the greatest for a volatile
/// call and a firmware call, and on the machine targets, where the sys capabilities are sims,
/// `traceless` for theirs.
pub fn supplied(path: &str, target: Target) -> Class {
    if is_volatile(path) || path == "hw::Ring0.firmware_call" {
        return Class::WORST;
    }
    if path == "boot::Services.write_console" {
        return Class::Recorded;
    }
    if target.is_machine() {
        return Class::Traceless;
    }
    SYS.iter()
        .find(|(operation, _)| *operation == path)
        .map_or(Class::Traceless, |(_, class)| *class)
}

impl Checker<'_> {
    /// Whether a function has an undo clause, which compensates its digest.
    pub fn compensates(&self, item: ItemId) -> bool {
        matches!(&self.db.ast(item).kind, ItemKind::Fn(function)
            if function.clauses.iter().any(|clause| matches!(clause.kind, ClauseKind::Undo { .. })))
    }

    /// The undoer an undo clause of a function names: a function item of the function's
    /// package, other than the function, visible wherever it is.
    pub fn undoer(&self, item: ItemId, target: &Path, member: Option<&Name>) -> Option<ItemId> {
        let module = self.db.module_of(item);
        let function = match member {
            Some(member) => *self
                .db
                .members(self.db.owner_key(module, target)?, &member.text)
                .first()?,
            None => match self.db.resolve(module, target) {
                Ok(Named::Item(function)) => function,
                _ => return None,
            },
        };
        (matches!(self.db.ast(function).kind, ItemKind::Fn(_))
            && function != item
            && self.db.package_of(function) == self.db.package_of(item)
            && self.seen_with(function, item))
        .then_some(function)
    }

    /// The undo clauses of a function: each names an undoer, whose class is at most
    /// `restorable`.
    pub fn check_undo(&self, item: ItemId, function: &FnItem) {
        for clause in &function.clauses {
            let ClauseKind::Undo { target, member, .. } = &clause.kind else {
                continue;
            };
            match self.undoer(item, target, member.as_ref()) {
                None => self.report(item, clause.token, Code::EffectUndoTarget),
                Some(undoer) if !self.digest(undoer).class.at_most(Class::Restorable) => {
                    self.report(item, clause.token, Code::EffectUndoIrreversible);
                }
                Some(_) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests;
