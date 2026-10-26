//! The interpreter runs of check time: the const evaluation of consts, const asserts and
//! contributions, and example runs, each reporting how it ended.

use std::collections::BTreeSet;
use std::rc::Rc;

use super::Checker;
use super::db::ItemId;
use crate::diagnostic::{Code, Role};
use crate::interp::{ConstValue, End, Interpreter, Report, Val};
use crate::syntax::ast::{ItemKind, Tok};

impl Checker<'_> {
    /// The value of a const or contribution: its const evaluation's, none when that ends
    /// without one or needs the value it computes, as the evaluations then under way do. The
    /// item is judged before its evaluation starts.
    pub fn const_value(&self, item: ItemId) -> Option<Rc<ConstValue>> {
        self.db.note(item);
        if let Some(&needing) = self.evaluating.borrow().last() {
            self.needs
                .borrow_mut()
                .entry(needing)
                .or_default()
                .insert(item);
        }
        if let Some(value) = self.const_values.borrow().get(&item) {
            return value.clone();
        }
        let under_way = self
            .evaluating
            .borrow()
            .iter()
            .position(|other| *other == item);
        if let Some(position) = under_way {
            let cycle = self.evaluating.borrow()[position..].to_vec();
            self.cyclic.borrow_mut().extend(cycle);
            return None;
        }
        self.rejected(item);
        if let Some(value) = self.const_values.borrow().get(&item) {
            return value.clone();
        }
        self.evaluating.borrow_mut().push(item);
        let muted = self.muted.replace(0);
        let value = self.db.about(Some(item), || self.evaluate(item));
        self.muted.set(muted);
        self.evaluating.borrow_mut().pop();
        self.const_values.borrow_mut().insert(item, value.clone());
        value
    }

    /// Runs the const evaluation of a const, const assert or contribution and reports its end,
    /// unless it needs its own value.
    fn evaluate(&self, item: ItemId) -> Option<Rc<ConstValue>> {
        let mut interpreter = Interpreter::new(self);
        let end = interpreter.run(item, crate::nir::Role::Body, 0, Vec::new(), &[]);
        if self.cyclic.borrow().contains(&item) {
            return None;
        }
        let token = self.db.ast(item).token;
        let assert = matches!(self.db.ast(item).kind, ItemKind::ConstAssert(_));
        match end {
            End::Value(value) => {
                if assert && value == Val::Bool(false) {
                    self.report(item, token, Code::ConstAssertFalse);
                }
                match interpreter.export(&value) {
                    Some(value) => return Some(Rc::new(value)),
                    None => self.report(item, token, Code::ConstViewNotStatic),
                }
            }
            End::Trap(report) => self.report_run(item, token, Code::ConstTrapped, &report),
            end => self.report_end(item, token, &end),
        }
        None
    }

    /// Each set of consts and contributions whose evaluations need one another's values,
    /// reported once at its member whose canonical path sorts first.
    pub(super) fn check_const_cycles(&self) {
        let needs = self.needs.borrow();
        let reaches = |from: ItemId| {
            let mut seen = BTreeSet::new();
            let mut pending = vec![from];
            while let Some(current) = pending.pop() {
                for next in needs.get(&current).into_iter().flatten() {
                    if seen.insert(*next) {
                        pending.push(*next);
                    }
                }
            }
            seen
        };
        let mut reported: BTreeSet<ItemId> = BTreeSet::new();
        for &item in needs.keys() {
            if reported.contains(&item) || !reaches(item).contains(&item) {
                continue;
            }
            let cycle: Vec<ItemId> = reaches(item)
                .into_iter()
                .filter(|other| reaches(*other).contains(&item))
                .collect();
            reported.extend(cycle.iter().copied());
            if let Some(first) = cycle
                .iter()
                .min_by_key(|member| self.db.canonical(**member))
            {
                self.report(*first, self.db.ast(*first).token, Code::ItemConstCycle);
            }
        }
    }

    /// Runs every example of a function that is not disputed, reporting each that yields
    /// `false` or ends otherwise than with a value.
    pub fn run_examples(&self, item: ItemId) {
        let Some(body) = self.typed(item) else {
            return;
        };
        for (index, example) in body.examples.iter().enumerate() {
            if example.disputed {
                continue;
            }
            let mut interpreter = Interpreter::new(self);
            let end = self.db.about(Some(item), || {
                interpreter.run(
                    item,
                    crate::nir::Role::Example,
                    index as u32,
                    Vec::new(),
                    &[],
                )
            });
            match end {
                End::Value(Val::Bool(false)) => {
                    self.report(item, example.token, Code::ExampleFailed);
                }
                End::Value(_) => {}
                End::Trap(report) => {
                    self.report_run(item, example.token, Code::ExampleTrapped, &report)
                }
                end => self.report_end(item, example.token, &end),
            }
        }
    }

    /// Reports a run ending at its step limit, a violation or an uninterpretable operation; a
    /// run reaching what has no value reports nothing.
    fn report_end(&self, item: ItemId, token: Tok, end: &End) {
        let (code, report) = match end {
            End::Exhausted(report) => (Code::ConstStepsExhausted, report),
            End::Violation(report) => (Code::ConstTrustedViolation, report),
            End::Uninterpretable(report) => (Code::ConstUninterpretable, report),
            _ => return,
        };
        self.report_run(item, token, code, report);
    }

    /// Reports a run's end with its run report as the because-chain: the operation that
    /// ended it, then the calls in progress, innermost first.
    fn report_run(&self, item: ItemId, token: Tok, code: Code, report: &Report) {
        let (at_item, at_span) = report.at;
        let mut because =
            vec![self.reason(at_item, at_span.first, Role::Raised, report.word.clone())];
        because.extend(report.callers.iter().map(|(caller, span)| {
            self.reason(
                *caller,
                span.first,
                Role::Called,
                self.db.canonical(*caller),
            )
        }));
        self.report_because(item, token, code, because);
    }
}

#[cfg(test)]
mod tests;
