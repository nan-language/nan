//! The ledger: per item, its entries for trusted code, `assume`, foreign and landing fns,
//! validators and declassifying decoders, `device`, `priv` and `capability` grants, waivers,
//! `todo`, `dbg`, device premises and undo clauses, each with its line and parts.

use super::Checker;
use super::db::{ItemId, PackageId};
use super::ty::Ty;
use crate::format::normal::Numbering;
use crate::syntax::ast::{ClauseKind, FnModifier, ItemKind, Mode};
use crate::syntax::grammar::Keyword;
use crate::syntax::token::TokenKind;
use crate::workspace::Authority;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Trusted,
    Assume,
    Foreign,
    Landing,
    Validator,
    Decoder,
    Grant,
    Waiver,
    Todo,
    Dbg,
    Class,
    Undo,
}

impl Kind {
    pub fn word(self) -> &'static str {
        match self {
            Kind::Trusted => "trusted",
            Kind::Assume => "assume",
            Kind::Foreign => "foreign",
            Kind::Landing => "landing",
            Kind::Validator => "validator",
            Kind::Decoder => "decoder",
            Kind::Grant => "grant",
            Kind::Waiver => "waiver",
            Kind::Todo => "todo",
            Kind::Dbg => "dbg",
            Kind::Class => "class",
            Kind::Undo => "undo",
        }
    }

    /// Its row's position in the ledger table.
    pub fn position(self) -> u64 {
        self as u64
    }
}

/// A ledger entry: its kind, the item holding it, its file and line, the texts of its parts and
/// whether it is a trust escape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    pub item: String,
    pub path: String,
    pub line: u32,
    pub texts: Vec<String>,
    pub escape: bool,
}

impl Checker<'_> {
    /// An item's ledger, in line order.
    pub fn ledger(&self, item: ItemId) -> Vec<Entry> {
        let Some(holder) = self.canonical_path(item) else {
            return Vec::new();
        };
        let data = self.db.item(item);
        let path = data.file.path.clone();
        let mut entries = Vec::new();
        let mut add = |kind: Kind, line: u32, texts: Vec<String>, escape: bool| {
            entries.push(Entry {
                kind,
                item: holder.clone(),
                path: path.clone(),
                line,
                texts,
                escape,
            });
        };
        let syntax = data.syntax();
        let span = &syntax.items[data.index];
        for token in span.first..span.end {
            let next_text = || {
                let next = &syntax.tokens[token + 1];
                (next.kind == TokenKind::Text)
                    .then(|| crate::syntax::literal::text_value(syntax.text(token + 1)))
            };
            let line = syntax.tokens[token].line;
            match syntax.tokens[token].kind {
                TokenKind::Keyword(Keyword::Trusted) => {
                    if let Some(premise) = next_text() {
                        add(Kind::Trusted, line, vec![premise], true);
                    }
                }
                TokenKind::Keyword(Keyword::Todo) => {
                    if let Some(text) = next_text() {
                        add(Kind::Todo, line, vec![text], false);
                    }
                }
                TokenKind::Keyword(Keyword::Dbg) => add(Kind::Dbg, line, Vec::new(), false),
                TokenKind::Keyword(Keyword::Assume) => {
                    let because = (token + 1..span.end)
                        .find(|at| syntax.text(*at) == "because")
                        .expect("an assume states its reason");
                    let condition = (token as u32 + 1, because as u32 - 1);
                    let reason = crate::syntax::literal::text_value(syntax.text(because + 1));
                    let text = self.fragment(item, condition, Numbering::Introduced);
                    add(Kind::Assume, line, vec![text, reason], true);
                }
                _ => {}
            }
        }
        let ast = self.db.ast(item);
        match &ast.kind {
            ItemKind::Fn(function) => {
                let head_line = data.head_line();
                match function.modifier.map(|(modifier, _)| modifier) {
                    Some(FnModifier::Foreign) => add(Kind::Foreign, head_line, Vec::new(), true),
                    Some(FnModifier::Landing) => add(Kind::Landing, head_line, Vec::new(), true),
                    _ => {}
                }
                if self.is_validator(item) {
                    let declassifying = self.declassifying_validator(item);
                    let class = if declassifying {
                        "declassifying"
                    } else {
                        "parsing"
                    };
                    add(
                        Kind::Validator,
                        head_line,
                        vec![class.to_owned()],
                        declassifying,
                    );
                }
                for clause in &function.clauses {
                    match &clause.kind {
                        ClauseKind::Waive { length, reason } => {
                            let threshold = if *length { "length" } else { "depth" };
                            add(
                                Kind::Waiver,
                                data.line(clause.token),
                                vec![threshold.to_owned(), reason.value.clone()],
                                false,
                            );
                        }
                        ClauseKind::Undo {
                            target,
                            member,
                            premise,
                        } => {
                            let undoer = match self.undoer(item, target, member.as_ref()) {
                                Some(undoer) => self.db.canonical(undoer),
                                None => match member {
                                    Some(member) => format!("{}.{}", target.written(), member.text),
                                    None => target.written(),
                                },
                            };
                            add(
                                Kind::Undo,
                                data.line(clause.token),
                                vec![undoer, premise.value.clone()],
                                true,
                            );
                        }
                        _ => {}
                    }
                }
                for (token, class) in self.device_premises(item) {
                    let line = token.map_or(head_line, |token| data.line(token));
                    add(Kind::Class, line, vec![class.text()], true);
                }
            }
            ItemKind::Struct(_) | ItemKind::Enum(_) => {
                let (derives, token) = match &ast.kind {
                    ItemKind::Struct(structure) => (&structure.derives, structure.derives_token),
                    ItemKind::Enum(enumeration) => {
                        (&enumeration.derives, enumeration.derives_token)
                    }
                    _ => unreachable!("a struct or enum"),
                };
                let encode = derives.iter().find(|name| name.text == "Encode");
                if let (Some(encode), Some(_)) = (encode, token)
                    && self.declassifying_decoder(item)
                {
                    add(Kind::Decoder, data.line(encode.token), Vec::new(), true);
                }
            }
            ItemKind::Protocol(protocol) => {
                let path = self.db.canonical(item);
                for operation in &protocol.operations {
                    for clause in &operation.method.clauses {
                        let ClauseKind::Undo {
                            target, premise, ..
                        } = &clause.kind
                        else {
                            continue;
                        };
                        let undoer = format!("{path}.{}", target.written());
                        add(
                            Kind::Undo,
                            data.line(clause.token),
                            vec![undoer, premise.value.clone()],
                            true,
                        );
                    }
                }
            }
            _ => {}
        }
        entries.sort_by_key(|entry| (entry.line, entry.kind));
        entries
    }

    /// The ledger entries of a package's `package` item: one per `device`, `priv` or `capability`
    /// grant.
    pub fn grant_ledger(&self, package: PackageId) -> Vec<Entry> {
        let data = self.db.package(package);
        let Some(manifest) = data.source.manifest.as_ref() else {
            return Vec::new();
        };
        let path = format!("{}/package.nan", data.source.dir);
        manifest
            .allows
            .iter()
            .filter(|allow| {
                matches!(
                    allow.authority,
                    Authority::Device | Authority::Priv | Authority::Capability
                )
            })
            .map(|allow| {
                let mut texts = vec![allow.authority.written()];
                texts.extend(allow.names.iter().cloned());
                Entry {
                    kind: Kind::Grant,
                    item: data.name.clone(),
                    path: path.clone(),
                    line: allow.line,
                    texts,
                    escape: true,
                }
            })
            .collect()
    }

    /// The ledger of a package: its `package` item's and its items', by path and line.
    pub fn package_ledger(&self, package: PackageId) -> Vec<Entry> {
        let mut entries = self.grant_ledger(package);
        for &module in &self.db.package(package).modules {
            for &item in &self.db.module(module).items {
                entries.extend(self.ledger(item));
            }
        }
        entries.sort_by(|left, right| {
            (&left.path, left.line, left.kind).cmp(&(&right.path, right.line, right.kind))
        });
        entries
    }

    /// Whether a validator is declassifying: it implements a method of `core::Decoder`, or one
    /// of its export types is no parsing type.
    pub fn declassifying_validator(&self, function: ItemId) -> bool {
        let declared = matches!(&self.db.ast(function).kind,
            ItemKind::Fn(head) if head.modifier.is_some_and(|(modifier, _)| modifier == FnModifier::Validator));
        let Some(head) = self.fn_sig(function).filter(|_| declared) else {
            return true;
        };
        let mut exports: Vec<Ty> = vec![head.result.clone()];
        exports.extend(head.fails.clone());
        for param in &head.params {
            if self.is_std(&param.ty, "core::Untrusted") {
                continue;
            }
            if param.mode == Mode::Mut || self.exposes(&param.ty) {
                exports.push(param.ty.clone());
            }
        }
        !exports.iter().all(|export| self.is_parsing(export))
    }

    /// Whether a parameter's type makes it an export type of a validator: it is or contains a
    /// type parameter, a function type, `Dyn`, a type with interior mutability, `task::Sender` or
    /// `sys::Outbox`.
    fn exposes(&self, ty: &Ty) -> bool {
        let exposing = |part: &Ty| {
            matches!(part, Ty::Param(_) | Ty::Fn(_) | Ty::Dyn(_))
                || self.is_std(part, "task::Sender")
                || self.is_std(part, "sys::Outbox")
        };
        self.any_part(ty, &exposing) || self.has_interior(ty)
    }
}
