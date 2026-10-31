//! Unknown names: the candidates a `name.unknown` diagnostic ranks, lists and offers as exact
//! fixes, and the stub it offers for an unknown callee with no candidate near.

use super::Checker;
use super::db::{ItemId, ModuleId, TypeKey};
use super::decl::fold;
use super::sig::SigKind;
use super::ty::Ty;
use crate::diagnostic::{Code, Fix, Offer, Reason, Role};
use crate::syntax::ast::{ItemKind, Tok};
use crate::syntax::grammar::Punct;
use crate::syntax::token::TokenKind;

/// The verbs of the controlled vocabulary with their synonyms.
const SYNONYMS: [(&str, &[&str]); 41] = [
    ("empty", &["new", "init"]),
    ("of", &["wrap", "from_value"]),
    ("filled", &["repeat"]),
    ("zeroed", &["zero_bytes"]),
    ("push", &["push_back", "enqueue"]),
    ("append", &["extend", "concat"]),
    ("insert", &["put", "add_entry"]),
    ("pop", &["pop_back", "dequeue"]),
    ("remove", &["delete", "erase"]),
    ("retain", &["filter", "keep_if"]),
    ("truncate", &["shorten"]),
    ("clear", &["reset", "wipe"]),
    ("fill", &["set_all"]),
    ("copy_from", &["assign", "clone_from"]),
    ("contains", &["has", "includes"]),
    ("position", &["find_index"]),
    ("find", &["index_of", "search"]),
    ("at", &["item", "element"]),
    ("get", &["retrieve", "get_or_none"]),
    ("view", &["as_slice"]),
    ("cursor", &["iter"]),
    ("extract", &["replace", "swap_out"]),
    ("contents", &["deref", "unbox", "inner"]),
    ("boxed", &["allocate", "box_value"]),
    ("share", &["clone", "dup"]),
    ("split", &["fork", "divide"]),
    ("merge", &["combine"]),
    ("close", &["shutdown", "dispose"]),
    ("abandon", &["abort", "drop", "kill"]),
    ("revoke", &["disable", "withdraw"]),
    ("spawn", &["go", "launch"]),
    ("cancel", &["stop"]),
    ("send", &["emit", "dispatch"]),
    ("post", &["notify", "deliver"]),
    ("receive", &["recv"]),
    ("drain", &["consume"]),
    ("wait", &["block", "park"]),
    ("encode", &["serialize"]),
    ("decode", &["deserialize"]),
    ("render", &["to_string", "format", "display"]),
    ("intern", &["symbol_of"]),
];

/// How many candidates a diagnostic lists.
const LISTED: usize = 5;

impl Checker<'_> {
    /// Reports an unknown name written over the tokens `(first, last)` of an item: a path that a
    /// newer release of its alias's source names as such, otherwise with the candidates near it
    /// among `candidates` listed as notes and offered as exact fixes, or, when none is near,
    /// `stub` inserted after the item.
    pub fn report_unknown(
        &self,
        item: ItemId,
        (first, last): (Tok, Tok),
        candidates: Vec<String>,
        stub: Option<String>,
    ) {
        let written = self.written(item, first, last);
        if self.report_newer_release(item, (first, last), &written) {
            return;
        }
        let near = near(&written, candidates);
        let because = near
            .iter()
            .map(|candidate| Reason {
                role: Role::Note,
                subject: candidate.clone(),
                at: None,
            })
            .collect();
        let fixes: Vec<Fix> = if near.is_empty() {
            stub.into_iter()
                .map(|stub| {
                    let data = self.db.item(item);
                    let syntax = data.syntax();
                    let end = *syntax.item_lines(&syntax.items[data.index]).end();
                    let at = syntax
                        .lines
                        .get(end as usize)
                        .map_or(syntax.source.len() as u32, |next| next.start);
                    self.fix_bytes(item, at, at, &format!("\n{stub}"))
                })
                .collect()
        } else {
            near.iter()
                .map(|candidate| self.fix_replacing(item, first, last, candidate))
                .collect()
        };
        self.report_diagnostic(
            item,
            crate::diagnostic::Diagnostic {
                because,
                fixes,
                offer: Offer::Each,
                ..self.on(item, (first, last), Code::NameUnknown)
            },
        );
    }

    /// The tokens of the path holding `token`: its run of identifiers joined by `::`.
    pub fn path_span(&self, item: ItemId, token: Tok) -> (Tok, Tok) {
        let syntax = self.db.item(item).syntax();
        let joins = |at: usize| {
            syntax.tokens.get(at).map(|token| token.kind)
                == Some(TokenKind::Punct(Punct::ColonColon))
        };
        let ident =
            |at: usize| syntax.tokens.get(at).map(|token| token.kind) == Some(TokenKind::Ident);
        let mut first = token as usize;
        while first >= 2 && joins(first - 1) && ident(first - 2) {
            first -= 2;
        }
        let mut last = token as usize;
        while joins(last + 1) && ident(last + 2) {
            last += 2;
        }
        (first as Tok, last as Tok)
    }

    /// The paths code in a module may write for an item: the items it may name as it writes
    /// them, functions named after a type excepted.
    pub fn item_candidates(&self, module: ModuleId) -> Vec<String> {
        (0..self.db.items.len())
            .map(|index| ItemId(index as u32))
            .filter(|item| {
                let ast = self.db.declaration(*item);
                !super::db::is_member(ast)
                    && ast.name().is_some()
                    && !matches!(ast.kind, ItemKind::Test(_))
                    && self.db.visible(module, *item)
            })
            .map(|item| self.item_written(item, module))
            .collect()
    }

    /// The types an item's type expressions may name: its generic parameters, the types its
    /// module may name and the core vocabulary's.
    pub fn type_candidates(&self, item: ItemId) -> Vec<String> {
        let module = self.db.module_of(item);
        let mut found: Vec<String> = self
            .shapes(item)
            .iter()
            .map(|def| def.name.clone())
            .collect();
        found.extend(
            (0..self.db.items.len())
                .map(|index| ItemId(index as u32))
                .filter(|other| {
                    matches!(
                        self.db.declaration(*other).kind,
                        ItemKind::Struct(_)
                            | ItemKind::Enum(_)
                            | ItemKind::Interface(_)
                            | ItemKind::Layout(_)
                            | ItemKind::Registers(_)
                    ) && self.db.visible(module, *other)
                })
                .map(|other| self.item_written(other, module)),
        );
        found.extend(
            super::resolve::core_vocabulary()
                .filter(|name| !matches!(*name, "Some" | "None" | "Ok" | "Err"))
                .map(str::to_owned),
        );
        found
    }

    /// The members of a type code in a module may name: its visible fields, its variants and the
    /// visible functions named after it.
    pub fn member_candidates(&self, module: ModuleId, ty: &Ty) -> Vec<String> {
        let mut found = Vec::new();
        let key = match ty {
            Ty::Item(item, _) => {
                match &self.sig(*item).kind {
                    SigKind::Struct { fields } | SigKind::Layout { fields, .. } => found.extend(
                        fields
                            .iter()
                            .filter(|field| {
                                !field.is_kept()
                                    && self.db.field_visible(module, *item, field.visibility)
                            })
                            .map(|field| field.name.clone()),
                    ),
                    SigKind::Enum { variants } => {
                        found.extend(variants.iter().map(|variant| variant.name.clone()));
                    }
                    _ => {}
                }
                TypeKey::Item(*item)
            }
            Ty::Scalar(scalar) => TypeKey::Scalar(*scalar),
            Ty::Unit => TypeKey::Unit,
            _ => return found,
        };
        if let Some(members) = self.db.members.get(&key) {
            for (name, functions) in members {
                if functions
                    .iter()
                    .any(|function| self.db.visible(module, *function))
                {
                    found.push(name.clone());
                }
            }
        }
        found
    }
}

/// The candidates near a written name, closest first: those within a third of the length of the
/// longer folded form, at most as many as a diagnostic lists.
pub fn near(written: &str, candidates: Vec<String>) -> Vec<String> {
    let ranker = Ranker::new(written);
    let mut near: Vec<(usize, String)> = candidates
        .into_iter()
        .filter(|candidate| candidate != written)
        .filter_map(|candidate| {
            let distance = ranker.distance_within(&candidate)?;
            Some((distance, candidate))
        })
        .collect();
    near.sort();
    near.dedup();
    near.truncate(LISTED);
    near.into_iter().map(|(_, candidate)| candidate).collect()
}

/// The folded form of a path: its segments' folded forms joined by `::`.
fn fold_path(path: &str) -> String {
    path.split("::").map(fold).collect::<Vec<_>>().join("::")
}

/// A written name compared with candidates: its folded form and the names that replacing a run of
/// its words that the controlled vocabulary lists as a synonym by its verb gives.
struct Ranker {
    folded: Vec<char>,
    synonyms: Vec<String>,
}

impl Ranker {
    fn new(written: &str) -> Ranker {
        let words: Vec<&str> = written.split('_').collect();
        let mut synonyms = Vec::new();
        for start in 0..words.len() {
            for end in start + 1..=words.len() {
                let run = words[start..end].join("_");
                for (verb, listed) in SYNONYMS {
                    if listed.contains(&run.as_str()) {
                        synonyms.push([&words[..start], &[verb], &words[end..]].concat().join("_"));
                    }
                }
            }
        }
        Ranker {
            folded: fold_path(written).chars().collect(),
            synonyms,
        }
    }

    /// The distance to a candidate when it is within a third of the length of the longer folded
    /// form: 0 when a synonym's replacement gives the candidate, and otherwise the fewest edits
    /// turning one folded form into the other.
    fn distance_within(&self, candidate: &str) -> Option<usize> {
        let other: Vec<char> = fold_path(candidate).chars().collect();
        let bound = self.folded.len().max(other.len()).div_ceil(3);
        if self.synonyms.iter().any(|synonym| synonym == candidate) {
            return Some(0);
        }
        if self.folded.len().abs_diff(other.len()) > bound {
            return None;
        }
        Some(edits(&self.folded, &other)).filter(|distance| *distance <= bound)
    }
}

/// The fewest insertions, deletions, substitutions and transpositions of adjacent characters
/// turning one text into the other, no character edited twice.
fn edits(left: &[char], right: &[char]) -> usize {
    let mut table = vec![vec![0usize; right.len() + 1]; left.len() + 1];
    for (at, row) in table.iter_mut().enumerate() {
        row[0] = at;
    }
    for (other, cell) in table[0].iter_mut().enumerate() {
        *cell = other;
    }
    for at in 1..=left.len() {
        for other in 1..=right.len() {
            let cost = usize::from(left[at - 1] != right[other - 1]);
            let mut best = (table[at - 1][other] + 1)
                .min(table[at][other - 1] + 1)
                .min(table[at - 1][other - 1] + cost);
            if at > 1
                && other > 1
                && left[at - 1] == right[other - 2]
                && left[at - 2] == right[other - 1]
            {
                best = best.min(table[at - 2][other - 2] + 1);
            }
            table[at][other] = best;
        }
    }
    table[left.len()][right.len()]
}

#[cfg(test)]
mod tests;
