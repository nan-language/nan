//! Tests: what a test's target names and whether it lies in the test's reach, and the paths to
//! the items only tests and examples may name.

use std::collections::BTreeSet;

use super::Checker;
use super::db::{ItemId, TypeKey};
use super::resolve::Named;
use super::sig::SigKind;
use crate::diagnostic::{Code, Offer};
use crate::syntax::ast::{ClauseKind, ItemKind, TestItem};

/// The std modules whose items only tests and examples name.
const TEST_ONLY: [&str; 3] = ["testing", "sys::sim", "hw::sim"];

/// The interfaces whose methods a derive writes.
const DERIVED: [&str; 7] = [
    "Eq", "Ord", "Hash", "Default", "Render", "Encode", "Generate",
];

/// What a test's target names.
#[derive(Clone, Copy)]
pub enum Target {
    Function(ItemId),
    /// A method a derive of this type writes.
    Derived(ItemId),
}

impl Checker<'_> {
    /// What a test's target names: a function item, or a method a derive writes.
    pub fn test_target(&self, item: ItemId, test: &TestItem) -> Option<Target> {
        let module = self.db.module_of(item);
        let Some(member) = &test.member else {
            return match self.db.resolve(module, &test.target) {
                Ok(Named::Item(function))
                    if matches!(self.db.ast(function).kind, ItemKind::Fn(_)) =>
                {
                    Some(Target::Function(function))
                }
                _ => None,
            };
        };
        let key = self.db.owner_key(module, &test.target)?;
        if let Some(function) = self.db.members(key, &member.text).first() {
            return Some(Target::Function(*function));
        }
        let TypeKey::Item(ty) = key else {
            return None;
        };
        let derived = DERIVED.iter().any(|name| {
            self.db.core_item(name).is_some_and(|interface| {
                self.derives(ty, interface)
                    && matches!(&self.sig(interface).kind, SigKind::Interface { methods }
                        if methods.iter().any(|method| method.name == member.text))
            })
        });
        derived.then_some(Target::Derived(ty))
    }

    /// A test's target lies in its reach: the test and, for each item of the reach, the items it
    /// names, the impl lines of each type and the contributions to each registry it names, its
    /// `for` clause naming nothing.
    pub fn check_test_reach(&self, item: ItemId, test: &TestItem) {
        let Some(target) = self.test_target(item, test) else {
            return;
        };
        let reached = self.db.about(None, || {
            let functions = self.member_functions();
            let mut reach: BTreeSet<ItemId> = BTreeSet::from([item]);
            let mut pending: Vec<ItemId> = self.names(item, &functions).into_iter().collect();
            while let Some(next) = pending.pop() {
                if !reach.insert(next) || self.db.is_std(next) {
                    continue;
                }
                pending.extend(self.names(next, &functions));
                match self.db.declaration(next).kind {
                    ItemKind::Struct(_) | ItemKind::Enum(_) | ItemKind::Layout(_) => {
                        pending.extend(self.impl_lines(TypeKey::Item(next)));
                    }
                    ItemKind::Registry(_) => pending.extend(
                        (0..self.db.items.len())
                            .map(|index| ItemId(index as u32))
                            .filter(|other| self.contribution_registry(*other) == Some(next)),
                    ),
                    _ => {}
                }
            }
            match target {
                Target::Function(function) => reach.contains(&function),
                Target::Derived(ty) => reach.contains(&ty),
            }
        });
        if !reached {
            self.report(item, self.db.ast(item).token, Code::TestTargetUnreachable);
        }
    }

    /// Each path of an item other than a test naming an item of `testing`, `sys::sim` or
    /// `hw::sim`, outside its example clauses.
    pub fn check_test_only(&self, item: ItemId) {
        let ast = self.db.ast(item);
        let clauses = match &ast.kind {
            ItemKind::Test(_) => return,
            ItemKind::Fn(function) => function.clauses.iter().collect(),
            ItemKind::Interface(interface) => interface
                .methods
                .iter()
                .flat_map(|method| &method.clauses)
                .collect(),
            _ => Vec::new(),
        };
        let examples: Vec<(u32, u32)> = clauses
            .into_iter()
            .filter_map(|clause| match &clause.kind {
                ClauseKind::Example(example) => Some((clause.token, example.last)),
                _ => None,
            })
            .collect();
        let module = self.db.module_of(item);
        for path in self.written_paths(item) {
            let (first, last) = (path.token(), path.last().token);
            if examples
                .iter()
                .any(|(start, end)| (*start..=*end).contains(&first))
            {
                continue;
            }
            let Ok(Named::Item(named)) = self.db.resolve(module, &path) else {
                continue;
            };
            let named_module = self.db.module_path(self.db.module_of(named));
            if self.db.is_std(named) && TEST_ONLY.contains(&named_module.as_str()) {
                self.report_fixed(
                    item,
                    (first, last),
                    Code::TestTestOnlyItem,
                    Vec::new(),
                    Offer::Each,
                );
            }
        }
    }
}
