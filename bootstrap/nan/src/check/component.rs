//! Components: the state an impl line naming `sys::Component` declares a component of, and the
//! uses of the runner `host::Runner`.

use std::rc::Rc;

use super::Checker;
use super::db::{ItemId, TypeKey};
use super::props::Property;
use super::sig::{FnSig, SigKind};
use super::ty::{Arg, Bound, Subst};
use crate::diagnostic::{Code, Fix, Offer};
use crate::syntax::ast::ItemKind;
use crate::syntax::grammar::Punct;
use crate::syntax::token::TokenKind;

/// The order of the entries of a `derives` clause in canonical format.
const DERIVES_ORDER: [&str; 11] = [
    "Copy", "Eq", "Ord", "Hash", "Default", "Render", "Encode", "Generate", "Send", "Inert",
    "AnyBits",
];

impl Checker<'_> {
    /// The component state of an impl line naming `sys::Component`: without generic parameters,
    /// Inert and implementing `Encode`, the first failing check reported on the line.
    pub fn check_component_impl(&self, item: ItemId) {
        let SigKind::Impl {
            target: Some(key),
            target_ty,
            interface: Some(bound),
        } = &self.sig(item).kind
        else {
            return;
        };
        if self.db.canonical(bound.interface) != "sys::Component" {
            return;
        }
        let state = match key {
            TypeKey::Item(state) => Some(*state),
            _ => None,
        };
        let encode = self.db.core_item("Encode");
        let code = if state.is_some_and(|state| !self.sig(state).generics.is_empty()) {
            Code::ComponentStateGeneric
        } else if !self.has(target_ty, Property::Inert) {
            Code::ComponentStateNotInert
        } else if encode.is_some_and(|encode| !self.implements(target_ty, encode)) {
            Code::ComponentStateNotEncode
        } else {
            return;
        };
        let fixes = match (code, state) {
            (Code::ComponentStateNotEncode, Some(state)) => {
                self.derive_added(state, "Encode").into_iter().collect()
            }
            _ => Vec::new(),
        };
        let token = self.db.ast(item).token;
        self.report_fixed(item, (token, token), code, fixes, Offer::Each);
    }

    /// The fix adding an entry to a struct's or enum's `derives` clause in canonical order,
    /// writing the clause when it has none.
    fn derive_added(&self, item: ItemId, entry: &str) -> Option<Fix> {
        let (name, derives) = match &self.db.ast(item).kind {
            ItemKind::Struct(structure) => (&structure.name, &structure.derives),
            ItemKind::Enum(enumeration) => (&enumeration.name, &enumeration.derives),
            _ => return None,
        };
        let rank = |written: &str| {
            DERIVES_ORDER
                .iter()
                .position(|known| *known == written)
                .unwrap_or(DERIVES_ORDER.len())
        };
        if let Some(later) = derives
            .iter()
            .find(|derived| rank(&derived.text) > rank(entry))
        {
            return Some(self.fix_inserting(item, later.token, &format!("{entry}, ")));
        }
        if let Some(last) = derives.last() {
            return Some(self.fix_appending(item, last.token, &format!(", {entry}")));
        }
        let syntax = &self.db.item(item).file.syntax;
        let opener = (name.token as usize..syntax.tokens.len())
            .find(|&at| syntax.tokens[at].kind == TokenKind::Punct(Punct::LBrace))?;
        let before = syntax.previous(opener)?;
        Some(self.fix_appending(item, before as u32, &format!(" derives {entry}")))
    }

    /// The rule a use of `host::Runner.run` breaks: a state that is no component for the Env and
    /// messages the use fixes, then code checked for a machine target.
    pub fn runner_problem(&self, head: &FnSig, subst: &Subst) -> Option<Code> {
        let fixed = |index: usize| match head
            .generics
            .get(index)
            .and_then(|def| subst.get(def.param))
        {
            Some(Arg::Ty(ty)) if !ty.has_error() => Some(ty.clone()),
            _ => None,
        };
        let parts = (
            fixed(0),
            fixed(head.inherited),
            fixed(head.inherited + 1),
            self.db.std_item("sys", "Component"),
        );
        if let (Some(state), Some(env), Some(message), Some(component)) = parts {
            let bound = Bound {
                interface: component,
                args: Rc::from(vec![Arg::Ty(env), Arg::Ty(message)]),
            };
            if !self.satisfies(&state, &bound) {
                return Some(Code::ComponentNotComponent);
            }
        }
        self.db
            .target
            .is_machine()
            .then_some(Code::ComponentUnavailable)
    }
}
