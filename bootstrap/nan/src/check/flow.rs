//! Ownership over a typed body: moves and the uses after them, the modes bindings allow, linear
//! consumption, holds and exclusivity, view bindings and the captures of closures. One forward
//! walk over the body's control flow carries what is moved, the holds projection results place
//! on their sources and the accesses that conflict with a hold; such an access is reported once
//! the hold's result is used on a path from it, which is when the result is live there.

mod place;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use place::Path;

use super::Checker;
use super::conform::Candidate;
use super::db::ItemId;
use super::props::Property;
use super::sig::SigKind;
use super::tir::{
    Call, Callee, LocalId, LocalKind, TBlock, TBody, TCapture, TClosure, TExpr, TKind, TPattern,
    TPatternKind, TStmt, TStmtKind,
};
use super::ty::Ty;
use crate::diagnostic::{Code, Fix, Offer, Reason, Role};
use crate::syntax::ast::{FailureMarker, Mode, Tok};
use crate::syntax::grammar::{Keyword, Punct};
use crate::syntax::token::TokenKind;

/// The first of the holder ids standing for loop cursors, one per loop depth.
const CURSORS: u32 = u32::MAX / 2;

/// The holder of a projection result no local binds yet.
const UNBOUND: LocalId = LocalId(u32::MAX);

/// Whether a place was moved on every path reaching a point, or on some.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Moved {
    Some,
    Every,
}

/// The restriction a live projection result places on its source.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Hold {
    /// The local, or loop cursor, whose uses keep the result live.
    holder: LocalId,
    source: Path,
    /// Whether a mut projection holds the source, rather than a read one freezing it.
    held: bool,
    /// Where the hold began.
    token: Tok,
}

/// An access conflicting with a hold: an error once the hold's result is used on a path from it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Pending {
    hold: Hold,
    access: Tok,
    /// The place accessed, written with the access's mode.
    subject: String,
}

/// A deferred call pending at a point: the scope it was registered in and the places it names
/// and takes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Deferred {
    depth: usize,
    places: Vec<Path>,
    taken: Vec<Path>,
}

/// What the walk knows at one reachable point of the body.
#[derive(Clone, Debug, Default, PartialEq)]
struct State {
    /// Each moved place path, how surely, and where.
    moved: BTreeMap<Path, (Moved, Tok)>,
    holds: BTreeSet<Hold>,
    pending: BTreeSet<Pending>,
    /// Each local holding a state token, and a suspension point after which it is live once
    /// used again.
    suspended: BTreeSet<(LocalId, Tok)>,
    /// The deferred calls pending, by their `defer` statement, and whether on every path.
    deferred: BTreeMap<Tok, (Deferred, bool)>,
}

impl State {
    /// The state where paths from two points meet.
    fn join(&mut self, other: &State) {
        let mut moved = BTreeMap::new();
        for (path, (how, token)) in &self.moved {
            let both = matches!(other.moved.get(path), Some((Moved::Every, _)));
            let how = if *how == Moved::Every && both {
                Moved::Every
            } else {
                Moved::Some
            };
            moved.insert(path.clone(), (how, *token));
        }
        for (path, (_, token)) in &other.moved {
            moved.entry(path.clone()).or_insert((Moved::Some, *token));
        }
        self.moved = moved;
        self.holds.extend(other.holds.iter().cloned());
        self.pending.extend(other.pending.iter().cloned());
        self.suspended.extend(other.suspended.iter().cloned());
        for (at, (call, every)) in &other.deferred {
            let joined = self.deferred.get(at).is_some_and(|(_, own)| *own) && *every;
            self.deferred.insert(*at, (call.clone(), joined));
        }
        for (at, (_, every)) in self.deferred.iter_mut() {
            *every &= other.deferred.contains_key(at);
        }
    }
}

/// Whether a place expression reaches its place through an index step.
fn through_index(expr: &TExpr) -> bool {
    match &expr.kind {
        TKind::Index(_) => true,
        TKind::Field { base, .. } => through_index(base),
        _ => false,
    }
}

/// Joins a state into an optional one, an absent state being an unreachable point.
fn merge(into: &mut Option<State>, from: Option<State>) {
    match (into.as_mut(), from) {
        (Some(state), Some(from)) => state.join(&from),
        (None, from) => *into = from,
        (Some(_), None) => {}
    }
}

/// The form of a part of a value of form `form`: a part of a part or wrapped part, and a value
/// of a view value.
fn part_of(form: Option<Form>) -> Option<Form> {
    match form {
        Some(Form::Part | Form::Wrapped) => Some(Form::Part),
        other => other,
    }
}

/// The form of a projection result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    /// A part of the receiver, named in place.
    Part,
    /// An `Option` or `Result` whose non-Copy payloads are parts.
    Wrapped,
    /// A view value its holder owns.
    View,
}

/// How an access reaches a place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Access {
    Read,
    Mut,
    Move,
    Assign,
}

impl Access {
    fn word(self) -> &'static str {
        match self {
            Access::Read => "",
            Access::Mut => "mut ",
            Access::Move => "take ",
            Access::Assign => "assign ",
        }
    }
}

/// A value as ownership reads it once its expression is evaluated.
#[derive(Clone, Debug, Default)]
struct Value {
    /// The place it names, when it is one.
    place: Option<Path>,
    /// The holds of the projection results it derives from.
    holds: Vec<Hold>,
    /// The form of the projection result it is, or derives from as a field.
    form: Option<Form>,
    /// Whether that part may be written.
    writable: bool,
    /// Whether it derives from a projection result whose receiver was a temporary.
    temporary: bool,
    /// Whether it is a temporary: the result of a call other than a projection, or a
    /// construction.
    fresh: bool,
    /// The locals it derives from, as a projection's result derives from its receiver.
    roots: Vec<LocalId>,
    /// The accesses a closure's captures make while it is live.
    captures: Vec<(Path, Access, Tok)>,
}

impl Value {
    fn place(path: Path) -> Value {
        Value {
            roots: vec![path.root],
            place: Some(path),
            ..Value::default()
        }
    }
}

/// The states at the jumps out of one loop.
struct LoopFlow {
    breaks: Option<State>,
    continues: Option<State>,
    /// How many scopes enclose the loop.
    scopes: usize,
    /// The token of a `for` loop whose cursor is linear.
    linear_cursor: Option<Tok>,
}

/// A block's scope: the locals it declares.
#[derive(Default)]
struct Scope {
    locals: Vec<LocalId>,
}

/// What a view binding names.
#[derive(Clone, Copy, Debug)]
struct View {
    writable: bool,
    wrapped: bool,
}

/// The walk of one function, test, const or closure body.
struct Flow<'c, 'a> {
    ck: &'c Checker<'a>,
    item: ItemId,
    body: &'c TBody,
    state: Option<State>,
    loops: Vec<LoopFlow>,
    /// The loop indexes of the typed body that loops enclosing this closure's body take.
    loop_base: usize,
    scopes: Vec<Scope>,
    views: HashMap<LocalId, View>,
    /// Where each local was bound.
    bound: HashMap<LocalId, Tok>,
    reported: HashSet<(Tok, Code)>,
    /// For a projection's body, the receiver and whether it is received by `mut`.
    projection: Option<(LocalId, bool)>,
    /// For a closure's body, whether it is a task closure's.
    task: Option<bool>,
    /// The locals assigned or accessed by `mut`, and those moved, in this body.
    mutated: HashSet<LocalId>,
    moved_locals: HashSet<LocalId>,
    /// The closures whose bodies were walked.
    walked: HashSet<u32>,
    /// Where each local is first used.
    first_use: HashMap<LocalId, Tok>,
    /// Whether the body holds a construct that failed a rule, whose effects are unknown: what
    /// rests on an effect being absent is then not reported.
    failed: bool,
    /// For a closure's body, the `fn` token opening the closure.
    opener: Option<Tok>,
    /// The first token of the statement being walked.
    statement: Option<Tok>,
}

impl Checker<'_> {
    /// The ownership rules of a typed body of an item, `proj` when it is a projection's.
    pub fn check_flow(&self, item: ItemId, body: &TBody, proj: bool) {
        let projection = body.params.first().filter(|_| proj).map(|&receiver| {
            (
                receiver,
                body.local(receiver).kind == LocalKind::Param(Mode::Mut),
            )
        });
        let mut flow = Flow::new(self, item, body);
        flow.projection = projection;
        flow.function();
    }
}

impl<'c, 'a> Flow<'c, 'a> {
    fn new(ck: &'c Checker<'a>, item: ItemId, body: &'c TBody) -> Flow<'c, 'a> {
        Flow {
            ck,
            item,
            body,
            state: Some(State::default()),
            loops: Vec::new(),
            loop_base: 0,
            scopes: vec![Scope::default()],
            views: HashMap::new(),
            bound: HashMap::new(),
            reported: HashSet::new(),
            projection: None,
            task: None,
            mutated: HashSet::new(),
            moved_locals: HashSet::new(),
            walked: HashSet::new(),
            first_use: HashMap::new(),
            failed: false,
            opener: None,
            statement: None,
        }
    }

    /// Reports a diagnostic with the fixes its rule names, once per token and code.
    fn report_fixed(&mut self, construct: (Tok, Tok), code: Code, fixes: Vec<Fix>) {
        if self.reported.insert((construct.0, code)) {
            self.ck
                .report_fixed(self.item, construct, code, fixes, Offer::Each);
        }
    }

    /// The fix listing a capture of the closure whose body is walked.
    fn listed(&self, local: LocalId, mode: Mode) -> Vec<Fix> {
        let name = &self.body.local(local).name;
        self.opener
            .map(|opener| {
                let capture = format!("{} {name}", mode.word());
                self.ck.fix_capture_listed(self.item, opener, &capture)
            })
            .into_iter()
            .collect()
    }

    fn function(&mut self) {
        for &param in &self.body.params {
            self.declare(param, self.body.local(param).token);
        }
        let Some(root) = &self.body.root else {
            return;
        };
        let end = match &root.kind {
            TKind::Block(block) => {
                self.block(block);
                block.end
            }
            _ => {
                let value = self.expr(root);
                self.returned(&value, root);
                root.token
            }
        };
        if self.state.is_some() {
            self.exit_scopes(0, end);
        }
    }

    // Reports.

    fn report(&mut self, token: Tok, code: Code) {
        if self.reported.insert((token, code)) {
            self.ck.report(self.item, token, code);
        }
    }

    fn report_because(&mut self, token: Tok, code: Code, because: Vec<Reason>) {
        if self.reported.insert((token, code)) {
            self.ck.report_because(self.item, token, code, because);
        }
    }

    fn reason(&self, token: Tok, role: Role, subject: String) -> Reason {
        self.ck.reason(self.item, token, role, subject)
    }

    // Facts of locals and types.

    fn name(&self, local: LocalId) -> String {
        if local.0 >= CURSORS {
            return "cursor".to_owned();
        }
        self.body.local(local).name.clone()
    }

    fn written(&self, path: &Path) -> String {
        path.written(&self.name(path.root))
    }

    fn is_copy(&self, ty: &Ty) -> bool {
        ty.has_error() || ty.is_never() || self.ck.has(ty, Property::Copy)
    }

    fn is_linear(&self, ty: &Ty) -> bool {
        !ty.has_error() && self.ck.has(ty, Property::Linear)
    }

    /// Whether a local owns its value, so that it may be moved.
    fn owns(&self, local: LocalId) -> bool {
        if self.views.contains_key(&local) {
            return false;
        }
        match self.body.local(local).kind {
            LocalKind::Let | LocalKind::Var | LocalKind::Pattern => true,
            LocalKind::Param(mode) | LocalKind::Element(mode) => mode == Mode::Take,
            LocalKind::Capture(mode) => mode == Mode::Take && self.task == Some(true),
            LocalKind::Quantified => false,
        }
    }

    fn declare(&mut self, local: LocalId, token: Tok) {
        if let Some(state) = self.state.as_mut() {
            state.moved.retain(|path, _| path.root != local);
            state.holds.retain(|hold| hold.holder != local);
            state.pending.retain(|pending| pending.hold.holder != local);
        }
        self.views.remove(&local);
        self.bound.insert(local, token);
        self.scopes.last_mut().expect("a scope").locals.push(local);
    }

    // Accesses.

    /// A suspension point: each local holding a state token becomes an error once used again.
    fn suspend(&mut self, token: Tok) {
        let held: Vec<LocalId> = self
            .scopes
            .iter()
            .flat_map(|scope| scope.locals.iter().copied())
            .filter(|local| self.ck.holds_token(&self.body.local(*local).ty))
            .collect();
        if let Some(state) = self.state.as_mut() {
            state
                .suspended
                .extend(held.into_iter().map(|local| (local, token)));
        }
    }

    /// A use of a local: each pending conflict of a hold it keeps becomes an error, and so does
    /// a state token it holds across a suspension point.
    fn use_holder(&mut self, holder: LocalId, token: Tok) {
        self.first_use.entry(holder).or_insert(token);
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let suspended = state
            .suspended
            .iter()
            .filter(|(local, _)| *local == holder)
            .map(|(_, point)| *point)
            .min();
        if let Some(point) = suspended {
            state.suspended.retain(|(local, _)| *local != holder);
            let bound = self.bound.get(&holder).copied().unwrap_or(token);
            let because = vec![
                self.reason(bound, Role::Bound, self.name(holder)),
                self.reason(token, Role::Used, self.name(holder)),
            ];
            self.report_because(point, Code::SuspendTokenAcrossAwait, because);
        }
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let fired: Vec<Pending> = state
            .pending
            .iter()
            .filter(|pending| pending.hold.holder == holder)
            .cloned()
            .collect();
        for pending in fired {
            let role = if pending.hold.held {
                Role::Held
            } else {
                Role::Frozen
            };
            let because = vec![
                self.reason(pending.hold.token, role, self.written(&pending.hold.source)),
                self.reason(token, Role::Used, self.name(holder)),
                self.reason(pending.access, Role::Accessed, pending.subject.clone()),
            ];
            self.report_because(pending.access, Code::OwnConflict, because);
        }
    }

    /// The checks every access to a place makes: the holds its root keeps, a move on a path to
    /// it, and the holds it conflicts with.
    fn access(&mut self, path: &Path, access: Access, token: Tok) {
        self.use_holder(path.root, token);
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let moved = state.moved.iter().find(|(moved, _)| {
            moved.overlaps(path) && !(access == Access::Assign && moved.starts_with(path))
        });
        if let Some((moved, (_, at))) = moved {
            let because = vec![self.reason(*at, Role::Moved, self.written(moved))];
            self.report_because(token, Code::OwnUseAfterMove, because);
        }
        let subject = format!("{}{}", access.word(), self.written(path));
        let state = self.state.as_mut().expect("a reachable point");
        let conflicting: Vec<Hold> = state
            .holds
            .iter()
            .filter(|hold| hold.source.overlaps(path) && (hold.held || access != Access::Read))
            .cloned()
            .collect();
        for hold in conflicting {
            state.pending.insert(Pending {
                hold,
                access: token,
                subject: subject.clone(),
            });
        }
    }

    /// A read of a value: its place, or the holders it derives from.
    fn read(&mut self, value: &Value, token: Tok) {
        if let Some(path) = &value.place {
            self.access(path, Access::Read, token);
        }
        for hold in value.holds.clone() {
            self.use_holder(hold.holder, token);
        }
    }

    /// A `mut` access or assignment of a place: modes must allow it and a view must be writable.
    fn write(&mut self, value: &Value, access: Access, token: Tok) {
        let Some(path) = &value.place else {
            if value.form.is_some() && !value.writable {
                self.report(token, Code::OwnReadOnlyView);
            }
            return;
        };
        let root = path.root;
        let local = self.body.local(root);
        if local.kind == LocalKind::Capture(Mode::Read) {
            let fixes = self.listed(root, Mode::Mut);
            self.report_fixed((token, token), Code::ClosureCaptureMode, fixes);
        } else if !local.assignable() {
            let syntax = &self.ck.db.item(self.item).file.syntax;
            let keyword = syntax
                .previous(local.token as usize)
                .filter(|&keyword| syntax.tokens[keyword].kind == TokenKind::Keyword(Keyword::Let));
            match keyword {
                Some(keyword) if self.reported.insert((token, Code::ModeImmutableBinding)) => {
                    let keyword = keyword as Tok;
                    let fix = self.ck.fix_replacing(self.item, keyword, keyword, "var");
                    self.ck.report_fixed(
                        self.item,
                        (token, token),
                        Code::ModeImmutableBinding,
                        vec![fix],
                        Offer::Each,
                    );
                }
                Some(_) => {}
                None => self.report(token, Code::ModeImmutableBinding),
            }
        } else if let Some(view) = self.views.get(&root)
            && (!view.writable || view.wrapped)
        {
            self.report(token, Code::OwnReadOnlyView);
        } else if value.form.is_some() && !value.writable {
            self.report(token, Code::OwnReadOnlyView);
        }
        self.mutated.insert(root);
        self.check_outside_write(path, token);
        self.access(path, access, token);
    }

    /// A write of a field a struct's predicates mention, or of a place within one, outside the
    /// struct's defining module.
    fn check_outside_write(&mut self, path: &Path, token: Tok) {
        if path.root.0 >= CURSORS {
            return;
        }
        let module = self.ck.db.module_of(self.item);
        let mut ty = self.body.local(path.root).ty.clone();
        for field in &path.fields {
            if let Ty::Item(item, _) = &ty
                && self.ck.db.module_of(*item) != module
                && self.ck.mentioned_fields(*item).contains(field)
            {
                self.report(token, Code::PredicateOutsideWrite);
                return;
            }
            let Some(next) = self.ck.field_type(&ty, field) else {
                return;
            };
            ty = next;
        }
    }

    /// A move of a value: an owned place becomes moved; a part, a wrapped part and a place that
    /// does not own its value are never moved.
    fn take(&mut self, value: &Value, ty: &Ty, token: Tok) {
        if self.is_copy(ty) {
            self.read(value, token);
            return;
        }
        match value.form {
            Some(Form::Wrapped) => {
                self.report(token, Code::OwnWrappedPartUse);
                return;
            }
            Some(Form::Part) => {
                self.report(token, Code::OwnMoveNotOwned);
                return;
            }
            _ => {}
        }
        let Some(path) = &value.place else {
            for hold in value.holds.clone() {
                self.use_holder(hold.holder, token);
            }
            return;
        };
        let root = path.root;
        if !self.owns(root) {
            if self.task == Some(true)
                && self.body.local(root).kind == LocalKind::Capture(Mode::Read)
            {
                let fixes = self.listed(root, Mode::Take);
                self.report_fixed((token, token), Code::ClosureCaptureMode, fixes);
            } else {
                self.report(token, Code::OwnMoveNotOwned);
            }
            return;
        }
        self.check_outside_write(path, token);
        let deferred = self.state.as_ref().is_some_and(|state| {
            state
                .deferred
                .values()
                .any(|(call, _)| call.places.iter().any(|named| named.overlaps(path)))
        });
        if deferred {
            let deferred = self.state.as_ref().and_then(|state| {
                state.deferred.values().find_map(|(call, _)| {
                    call.places
                        .iter()
                        .find(|named| named.overlaps(path))
                        .cloned()
                })
            });
            let fixes = deferred
                .zip(self.statement)
                .map(|(place, statement)| {
                    let indentation = self.ck.indentation(self.item, statement);
                    let undefer = format!("undefer {}\n{indentation}", self.written(&place));
                    self.ck.fix_inserting(self.item, statement, &undefer)
                })
                .into_iter()
                .collect();
            self.report_fixed((token, token), Code::DeferDeferredPlaceMoved, fixes);
        }
        self.access(path, Access::Move, token);
        self.moved_locals.insert(root);
        if let Some(state) = self.state.as_mut() {
            state.moved.insert(path.clone(), (Moved::Every, token));
        }
    }

    // Linear consumption.

    /// Whether a place's value is consumed on every path here: moved, or taken by a deferred call
    /// pending on every path and registered in a scope from `depth` inward, which runs at an exit
    /// of those scopes.
    fn consumed(&self, path: &Path, depth: usize) -> bool {
        let Some(state) = self.state.as_ref() else {
            return true;
        };
        state
            .moved
            .iter()
            .any(|(moved, (how, _))| *how == Moved::Every && path.starts_with(moved))
            || state.deferred.values().any(|(call, every)| {
                *every
                    && call.depth >= depth
                    && call.taken.iter().any(|taken| path.starts_with(taken))
            })
    }

    /// The exit of the scopes from `depth` inward: each linear value they own and have not
    /// consumed is dropped there.
    fn exit_scopes(&mut self, depth: usize, token: Tok) {
        if self.state.is_none() || self.failed {
            return;
        }
        let locals: Vec<LocalId> = self.scopes[depth..]
            .iter()
            .flat_map(|scope| scope.locals.iter().copied())
            .collect();
        for local in locals {
            let data = self.body.local(local);
            if !self.owns(local) || !self.is_linear(&data.ty) {
                continue;
            }
            let path = Path::of(local);
            if !self.consumed(&path, depth) {
                let at = self.bound.get(&local).copied().unwrap_or(data.token);
                let because = vec![self.reason(at, Role::Bound, data.name.clone())];
                self.report_because(token, Code::OwnLinearDropped, because);
            }
        }
    }

    /// An exit of every scope and loop of the body, by `return`, `fail` or a propagated failure.
    fn exit_body(&mut self, token: Tok) {
        if self.loops.iter().any(|flow| flow.linear_cursor.is_some()) {
            self.report(token, Code::OwnLinearDropped);
        }
        self.exit_scopes(0, token);
    }

    // Blocks and statements.

    fn block(&mut self, block: &TBlock) -> Value {
        self.scopes.push(Scope::default());
        let mut value = Value::default();
        for (index, stmt) in block.stmts.iter().enumerate() {
            if self.state.is_none() {
                break;
            }
            let last = index + 1 == block.stmts.len();
            value = self.stmt(stmt, last);
        }
        self.pop_scope(block.end);
        value
    }

    /// The end of the innermost scope at `token`: its linear values are checked, and the holds
    /// its locals keep end.
    fn pop_scope(&mut self, token: Tok) {
        let depth = self.scopes.len() - 1;
        self.exit_scopes(depth, token);
        let scope = self.scopes.pop().expect("a scope");
        self.forget(|holder| scope.locals.contains(&holder));
        if let Some(state) = self.state.as_mut() {
            state
                .suspended
                .retain(|(local, _)| !scope.locals.contains(local));
            let ran: Vec<Tok> = state
                .deferred
                .iter()
                .filter(|(_, (call, _))| call.depth >= depth)
                .map(|(at, _)| *at)
                .collect();
            for at in ran {
                let (call, _) = state.deferred.remove(&at).expect("a pending call");
                for taken in call.taken {
                    state.moved.insert(taken, (Moved::Every, token));
                }
            }
        }
    }

    /// Ends the holds of the holders `ended` gives, and the conflicts they would report.
    fn forget(&mut self, ended: impl Fn(LocalId) -> bool) {
        if let Some(state) = self.state.as_mut() {
            state.holds.retain(|hold| !ended(hold.holder));
            state.pending.retain(|pending| !ended(pending.hold.holder));
        }
    }

    /// One statement; the value of a block's last expression statement is the block's.
    fn stmt(&mut self, stmt: &TStmt, last: bool) -> Value {
        self.statement = Some(stmt.token);
        match &stmt.kind {
            TStmtKind::Let {
                pattern,
                value,
                otherwise,
            } => {
                let found = self.expr(value);
                if let Some(otherwise) = otherwise {
                    let matched = self.state.clone();
                    self.block(otherwise);
                    self.state = matched;
                }
                self.bind(pattern, &found, value, stmt.token);
            }
            TStmtKind::Var { local, value } => {
                let found = self.expr(value);
                self.bind_var(*local, &found, value, stmt.token);
            }
            TStmtKind::Discard(expr) => {
                let value = self.expr(expr);
                self.drop_value(&value, &expr.ty, expr.token);
            }
            TStmtKind::While {
                condition,
                invariants,
                body,
            } => self.looped(stmt.token, None, |flow| {
                flow.contract(invariants);
                let value = flow.expr(condition);
                flow.read(&value, condition.token);
                let exhausted = flow.state.clone();
                flow.block(body);
                exhausted
            }),
            TStmtKind::For {
                pattern,
                mode,
                iterated,
                cursor,
                invariants,
                body,
            } => self.for_loop(
                stmt.token, pattern, *mode, iterated, cursor, invariants, body,
            ),
            TStmtKind::Loop { invariants, body } => self.looped(stmt.token, None, |flow| {
                flow.contract(invariants);
                flow.block(body);
                None
            }),
            TStmtKind::Defer(call) => self.defer(stmt.token, call),
            TStmtKind::Undefer(place) => self.undefer(stmt.token, place),
            TStmtKind::Expect(condition) | TStmtKind::Assume(condition) => {
                self.contract(std::slice::from_ref(condition));
            }
            TStmtKind::Expr(expr) => {
                let value = self.expr(expr);
                if last {
                    return value;
                }
                self.read(&value, expr.token);
            }
            TStmtKind::Assign { target, op, value } => {
                self.assign(stmt.token, target, op.is_some(), value)
            }
        }
        Value::default()
    }

    /// Contract terms, which only read.
    fn contract(&mut self, terms: &[TExpr]) {
        for term in terms {
            let value = self.expr(term);
            self.read(&value, term.token);
        }
    }

    /// A deferred call: its operands are judged where it runs; the places it takes are consumed
    /// at each exit of its block while it is pending.
    fn defer(&mut self, token: Tok, call: &TExpr) {
        let TKind::Call(call) = &call.kind else {
            return;
        };
        let places = call
            .args
            .iter()
            .filter_map(|arg| self.place_of(&arg.expr))
            .collect();
        let taken = call
            .args
            .iter()
            .filter(|arg| arg.mode == Mode::Take)
            .filter_map(|arg| self.place_of(&arg.expr))
            .collect();
        let deferred = Deferred {
            depth: self.scopes.len() - 1,
            places,
            taken,
        };
        if let Some(state) = self.state.as_mut() {
            state.deferred.insert(token, (deferred, true));
        }
    }

    /// `undefer p`: cancels the deferred calls naming exactly `p`, one of which is pending on
    /// every path here.
    fn undefer(&mut self, token: Tok, place: &TExpr) {
        let path = self.place_of(place).filter(|_| !through_index(place));
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let names = |call: &Deferred| path.as_ref().is_some_and(|path| call.places.contains(path));
        let matched = state
            .deferred
            .values()
            .any(|(call, every)| *every && names(call));
        state.deferred.retain(|_, (call, _)| !names(call));
        if !matched {
            let last = self
                .ck
                .db
                .item(self.item)
                .file
                .syntax
                .tokens
                .iter()
                .enumerate()
                .skip(token as usize)
                .take_while(|(_, at)| !at.kind.is_virtual())
                .last()
                .map_or(token, |(index, _)| index as Tok);
            let fix = self.ck.fix_deleting_lines(self.item, token, last);
            self.report_fixed((token, last), Code::DeferUndeferUnmatched, vec![fix]);
        }
    }

    /// A loop: its body runs until the state at its head no longer grows. `body` walks one
    /// iteration from the head and gives the state where the loop ends without a `break`.
    fn looped(
        &mut self,
        token: Tok,
        linear_cursor: Option<Tok>,
        mut body: impl FnMut(&mut Self) -> Option<State>,
    ) {
        let entry = self.state.clone();
        let mut head = entry.clone();
        let depth = self.loops.len();
        loop {
            self.state = head.clone();
            if self.state.is_some() {
                self.use_holder(LocalId(CURSORS + depth as u32), token);
            }
            self.loops.push(LoopFlow {
                breaks: None,
                continues: None,
                scopes: self.scopes.len(),
                linear_cursor,
            });
            let ended = body(self);
            let flow = self.loops.pop().expect("the loop");
            let mut next = entry.clone();
            merge(&mut next, self.state.take());
            merge(&mut next, flow.continues);
            if next == head {
                let mut out = ended;
                merge(&mut out, flow.breaks);
                self.state = out;
                return;
            }
            head = next;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn for_loop(
        &mut self,
        token: Tok,
        pattern: &TPattern,
        mode: Mode,
        iterated: &TExpr,
        cursor: &Ty,
        invariants: &[TExpr],
        body: &TBlock,
    ) {
        let found = self.expr(iterated);
        let holder = LocalId(CURSORS + self.loops.len() as u32);
        let projecting = self.next_projects(cursor);
        let mut element = Value {
            form: projecting.then_some(Form::Part),
            writable: mode == Mode::Mut,
            ..Value::default()
        };
        match mode {
            Mode::Take => self.take(&found, &iterated.ty, iterated.token),
            Mode::Mut => {
                self.write(&found, Access::Mut, iterated.token);
                self.hold(&found, holder, true, iterated.token, &mut element);
            }
            Mode::Read if iterated.ty == *cursor => self.take(&found, &iterated.ty, iterated.token),
            Mode::Read => {
                self.read(&found, iterated.token);
                self.hold(&found, holder, false, iterated.token, &mut element);
            }
        }
        let linear_cursor = self.is_linear(cursor).then_some(token);
        let owned = !projecting;
        self.looped(token, linear_cursor, |flow| {
            let exhausted = flow.state.clone();
            flow.scopes.push(Scope::default());
            flow.match_part(pattern, &element, owned);
            flow.contract(invariants);
            flow.block(body);
            flow.pop_scope(body.end);
            exhausted
        });
        self.forget(|kept| kept == holder);
    }

    /// Places on a projection's receiver the hold its result carries, kept by `holder`.
    fn hold(
        &mut self,
        receiver: &Value,
        holder: LocalId,
        held: bool,
        token: Tok,
        result: &mut Value,
    ) {
        result.holds.extend(receiver.holds.iter().map(|hold| Hold {
            holder,
            ..hold.clone()
        }));
        if let Some(source) = &receiver.place {
            let hold = Hold {
                holder,
                source: source.clone(),
                held,
                token,
            };
            if let Some(state) = self.state.as_mut() {
                state.holds.insert(hold.clone());
            }
            result.holds.push(hold);
        }
    }

    /// Whether a cursor type's `next` is a projection.
    fn next_projects(&self, cursor: &Ty) -> bool {
        self.ck
            .method_candidates(cursor, "next")
            .into_iter()
            .any(|candidate| match candidate {
                Candidate::Function { item, .. } => {
                    self.ck.fn_sig(item).is_some_and(|sig| sig.is_proj())
                }
                Candidate::Method { bound, method, .. } => match &self.ck.sig(bound.interface).kind
                {
                    SigKind::Interface { methods } => methods[method].sig.is_proj(),
                    _ => false,
                },
            })
    }

    /// A branch's value, as its `if` or `match` yields it; one derived from a temporary the
    /// branch creates, which ends with the branch, is an error at `token`.
    fn branch_value(&mut self, mut value: Value, token: Tok, ty: &Ty) -> Value {
        if value.temporary && !self.is_copy(ty) {
            self.report(token, Code::OwnTemporarySource);
            value.temporary = false;
        }
        value
    }

    // Bindings and patterns.

    /// A `let` or `var` binding of a value to a pattern.
    fn bind(&mut self, pattern: &TPattern, value: &Value, expr: &TExpr, token: Tok) {
        if value.temporary && !self.is_copy(&expr.ty) {
            self.report(token, Code::OwnTemporarySource);
        }
        let owned = value.form.is_none() || value.form == Some(Form::View);
        let owned = owned && value.place.as_ref().is_none_or(|path| self.owns(path.root));
        self.match_part(pattern, value, owned);
    }

    /// A `var` binding: of a part, or of a value derived from a view binding, it is a view binding
    /// of the same part; any other value moves in.
    fn bind_var(&mut self, local: LocalId, value: &Value, expr: &TExpr, token: Tok) {
        if value.temporary && !self.is_copy(&expr.ty) {
            self.report(token, Code::OwnTemporarySource);
        }
        let viewing = matches!(value.form, Some(Form::Part | Form::Wrapped));
        if viewing && !self.is_copy(&expr.ty) {
            self.read(value, token);
            self.declare(local, token);
            let view = View {
                writable: value.writable,
                wrapped: value.form == Some(Form::Wrapped),
            };
            self.views.insert(local, view);
            self.keep_holds(local, value);
            return;
        }
        self.take(value, &expr.ty, token);
        self.declare(local, token);
        self.keep_holds(local, value);
    }

    /// Matches a part of a scrutinee, `owned` when the scrutinee owns its value.
    fn match_part(&mut self, pattern: &TPattern, part: &Value, owned: bool) {
        match &pattern.kind {
            TPatternKind::Failed | TPatternKind::Value(..) => {}
            TPatternKind::Wildcard => {
                if owned
                    && self.is_linear(&pattern.ty)
                    && part
                        .place
                        .as_ref()
                        .is_none_or(|path| !self.consumed(path, self.scopes.len()))
                {
                    self.report(pattern.token, Code::OwnLinearDropped);
                }
            }
            TPatternKind::Binding(local) => {
                let local = *local;
                if self.is_copy(&pattern.ty) {
                    self.read(part, pattern.token);
                    self.declare(local, pattern.token);
                } else if owned {
                    self.take(part, &pattern.ty, pattern.token);
                    self.declare(local, pattern.token);
                    self.keep_holds(local, part);
                } else {
                    self.read(part, pattern.token);
                    self.declare(local, pattern.token);
                    let view = View {
                        writable: part.writable,
                        wrapped: part.form == Some(Form::Wrapped),
                    };
                    self.views.insert(local, view);
                    let mut held = Value::default();
                    match &part.place {
                        Some(_) if part.form.is_none() => {
                            self.hold(part, local, false, pattern.token, &mut held);
                        }
                        _ => self.keep_holds(local, part),
                    }
                }
            }
            TPatternKind::Fields(variant, fields) => {
                for (name, sub) in fields {
                    let key = match variant {
                        Some(variant) => format!("{variant}.{name}"),
                        None => name.clone(),
                    };
                    let field = Value {
                        place: part.place.as_ref().map(|path| path.field(&key)),
                        form: part_of(part.form),
                        ..part.clone()
                    };
                    self.match_part(sub, &field, owned);
                }
                if owned
                    && self.is_linear(&pattern.ty)
                    && let Some(path) = &part.place
                    && let Some(state) = self.state.as_mut()
                {
                    state
                        .moved
                        .insert(path.clone(), (Moved::Every, pattern.token));
                }
            }
        }
    }

    /// Keeps, while a local is live, the holds of the value it binds.
    fn keep_holds(&mut self, local: LocalId, value: &Value) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        for hold in &value.holds {
            state.holds.insert(Hold {
                holder: local,
                ..hold.clone()
            });
        }
    }

    /// A value received by `discard`: a linear one is dropped.
    fn drop_value(&mut self, value: &Value, ty: &Ty, token: Tok) {
        self.read(value, token);
        if !self.is_linear(ty) || value.form.is_some() {
            return;
        }
        let owned = value
            .place
            .as_ref()
            .is_none_or(|path| self.owns(path.root) && !self.consumed(path, self.scopes.len()));
        if owned {
            self.report(token, Code::OwnLinearDropped);
        }
    }

    /// An assignment: its value is moved in, then the old value is released.
    fn assign(&mut self, token: Tok, target: &TExpr, compound: bool, value: &TExpr) {
        let found = self.expr(value);
        if compound {
            self.read(&found, value.token);
        } else {
            self.take(&found, &value.ty, value.token);
        }
        let place = self.expr(target);
        let Some(path) = place.place.clone() else {
            return;
        };
        let local = self.body.local(path.root);
        let escaping = matches!(
            local.kind,
            LocalKind::Param(Mode::Mut) | LocalKind::Capture(Mode::Mut)
        );
        if escaping && !found.holds.is_empty() && !self.is_copy(&value.ty) {
            self.report(token, Code::OwnViewEscape);
        }
        if compound {
            self.read(&place, token);
        }
        let overwritten = !compound
            && place.form.is_none()
            && !self.views.contains_key(&path.root)
            && self.is_linear(&target.ty)
            && !self.consumed(&path, self.scopes.len());
        if overwritten {
            self.report(token, Code::OwnLinearDropped);
        }
        self.write(&place, Access::Assign, token);
        if let Some(state) = self.state.as_mut() {
            state.moved.retain(|moved, _| !moved.starts_with(&path));
        }
    }

    /// The value a function or closure body returns: moved out, or in a projection derived from
    /// its receiver.
    fn returned(&mut self, value: &Value, expr: &TExpr) {
        let Some((receiver, held)) = self.projection else {
            self.take(value, &expr.ty, expr.token);
            return;
        };
        self.read(value, expr.token);
        if held
            && let Some(path) = &value.place
            && path.root == receiver
            && self.exposes_mentioned(path)
        {
            self.report(expr.token, Code::PredicateExposedField);
        }
        let derived = value.roots.contains(&receiver);
        if !self.is_copy(&expr.ty) && !derived && value.roots.iter().any(|root| *root != receiver) {
            self.report(expr.token, Code::OwnProjSource);
        }
    }

    /// Whether a path from the receiver of a boundary function of a struct's defining module
    /// passes through a field its predicates mention.
    fn exposes_mentioned(&self, path: &Path) -> bool {
        let ast = self.ck.db.ast(self.item);
        let boundary = ast.visibility != crate::syntax::ast::Visibility::Private
            || !self.ck.implemented_lines(self.item).is_empty();
        let module = self.ck.db.module_of(self.item);
        let mut ty = self.body.local(path.root).ty.clone();
        for field in &path.fields {
            if let Ty::Item(item, _) = &ty
                && self.ck.db.module_of(*item) == module
                && self.ck.mentioned_fields(*item).contains(field)
            {
                return boundary;
            }
            let Some(next) = self.ck.field_type(&ty, field) else {
                return false;
            };
            ty = next;
        }
        false
    }

    // Expressions.

    /// The place path an expression names: a local and its fields, up to an index step.
    fn place_of(&self, expr: &TExpr) -> Option<Path> {
        match &expr.kind {
            TKind::Local(local) => Some(Path::of(*local)),
            TKind::Field { base, name } => self.place_of(base).map(|path| path.field(name)),
            TKind::Index(call) => call.args.first().and_then(|base| self.place_of(&base.expr)),
            _ => None,
        }
    }

    fn expr(&mut self, expr: &TExpr) -> Value {
        if self.state.is_none() {
            return Value::default();
        }
        match &expr.kind {
            TKind::Local(local) => {
                let mut value = Value::place(Path::of(*local));
                if let Some(view) = self.views.get(local) {
                    value.form = Some(if view.wrapped {
                        Form::Wrapped
                    } else {
                        Form::Part
                    });
                    value.writable = view.writable;
                }
                if let Some(state) = self.state.as_ref() {
                    value.holds = state
                        .holds
                        .iter()
                        .filter(|hold| hold.holder == *local)
                        .cloned()
                        .collect();
                }
                value
            }
            TKind::Field { base, name } => {
                let base = self.expr(base);
                Value {
                    place: base.place.as_ref().map(|path| path.field(name)),
                    form: part_of(base.form),
                    ..base
                }
            }
            TKind::Index(call) | TKind::Call(call) => self.call(call, expr.token),
            TKind::Construct { values, .. } => self.construct(expr, values),
            TKind::Unary { operand, .. } => {
                let value = self.expr(operand);
                self.read(&value, operand.token);
                Value::default()
            }
            TKind::Binary { left, right, .. } => {
                let left_value = self.expr(left);
                self.read(&left_value, left.token);
                let right_value = self.expr(right);
                self.read(&right_value, right.token);
                Value::default()
            }
            TKind::If {
                condition,
                then,
                otherwise,
            } => {
                let found = self.expr(condition);
                self.read(&found, condition.token);
                let entry = self.state.clone();
                let value = self.block(then);
                let mut value = self.branch_value(value, tail_token(then, expr.token), &expr.ty);
                let after_then = self.state.take();
                self.state = entry;
                if let Some(otherwise) = otherwise {
                    let other = self.expr(otherwise);
                    let other = self.branch_value(other, value_token(otherwise), &expr.ty);
                    value.holds.extend(other.holds);
                    value.roots.extend(other.roots);
                }
                merge(&mut self.state, after_then);
                value
            }
            TKind::Block(block) => self.block(block),
            TKind::Match { scrutinee, arms } => self.match_expr(scrutinee, arms, &expr.ty),
            TKind::Closure(closure) => self.closure(expr.token, closure),
            TKind::When { accesses, body, .. } => {
                self.suspend(expr.token);
                for (_, cown, local) in accesses {
                    let value = self.expr(cown);
                    self.read(&value, cown.token);
                    self.declare(*local, cown.token);
                }
                let value = self.expr(body);
                self.read(&value, body.token);
                let escapes = value
                    .roots
                    .iter()
                    .any(|root| accesses.iter().any(|(_, _, local)| local == root));
                if escapes && !self.is_copy(&body.ty) {
                    self.report(expr.token, Code::TaskWhenEscape);
                }
                Value::default()
            }
            TKind::Trusted(inner) | TKind::Dbg(inner) => self.expr(inner),
            TKind::Old(inner) => {
                let value = self.expr(inner);
                self.read(&value, inner.token);
                Value::default()
            }
            TKind::Quantifier { range, body, .. } => {
                let found = self.expr(range);
                self.read(&found, range.token);
                let value = self.expr(body);
                self.read(&value, body.token);
                Value::default()
            }
            TKind::Return(value) => {
                if let Some(value) = value {
                    let found = self.expr(value);
                    self.returned(&found, value);
                }
                self.exit_body(expr.token);
                self.state = None;
                Value::default()
            }
            TKind::Fail(value) => {
                let found = self.expr(value);
                self.take(&found, &value.ty, value.token);
                self.exit_body(expr.token);
                self.state = None;
                Value::default()
            }
            TKind::Break(index) | TKind::Continue(index) => {
                let breaks = matches!(expr.kind, TKind::Break(_));
                self.jump(*index, breaks, expr.token);
                Value::default()
            }
            TKind::Failed => {
                self.failed = true;
                Value::default()
            }
            TKind::Value(_)
            | TKind::FnValue { .. }
            | TKind::Literal(_)
            | TKind::Unit
            | TKind::Result
            | TKind::ConstParam(_)
            | TKind::Register { .. }
            | TKind::Todo(_) => Value::default(),
        }
    }

    /// `break` or `continue` of the loop the typed body numbers `index`.
    fn jump(&mut self, index: usize, breaks: bool, token: Tok) {
        let Some(index) = index.checked_sub(self.loop_base) else {
            return;
        };
        let Some(target) = self.loops.get(index) else {
            return;
        };
        let scopes = target.scopes;
        let first_left = if breaks { index } else { index + 1 };
        if self.loops[first_left..]
            .iter()
            .any(|flow| flow.linear_cursor.is_some())
        {
            self.report(token, Code::OwnLinearDropped);
        }
        self.exit_scopes(scopes, token);
        let state = self.state.take();
        let target = &mut self.loops[index];
        if breaks {
            merge(&mut target.breaks, state);
        } else {
            merge(&mut target.continues, state);
        }
    }

    fn match_expr(&mut self, scrutinee: &TExpr, arms: &[super::tir::TArm], ty: &Ty) -> Value {
        let found = self.expr(scrutinee);
        let owned = (found.form.is_none() || found.form == Some(Form::View))
            && found.place.as_ref().is_none_or(|path| self.owns(path.root));
        let entry = self.state.clone();
        let mut out: Option<State> = None;
        let mut value = Value::default();
        for arm in arms {
            self.state = entry.clone();
            self.scopes.push(Scope::default());
            if arm.patterns.is_empty() && owned && self.is_linear(&scrutinee.ty) {
                let unconsumed = found
                    .place
                    .as_ref()
                    .is_none_or(|path| !self.consumed(path, self.scopes.len()));
                if unconsumed {
                    self.report(arm.token, Code::OwnLinearDropped);
                }
            }
            for pattern in &arm.patterns {
                self.match_part(pattern, &found, owned);
            }
            if let Some(guard) = &arm.guard {
                let condition = self.expr(guard);
                self.read(&condition, guard.token);
            }
            let yielded = self.expr(&arm.body);
            let yielded = self.branch_value(yielded, value_token(&arm.body), ty);
            value.holds.extend(yielded.holds);
            value.roots.extend(yielded.roots);
            self.pop_scope(arm.body.token);
            merge(&mut out, self.state.take());
        }
        if arms.is_empty() {
            out = entry;
        }
        self.state = out;
        value
    }

    /// A construction: its values move in, except parts wrapped in `Some`, `Ok` or `Err`.
    fn construct(&mut self, expr: &TExpr, values: &[(String, TExpr)]) -> Value {
        let wraps = ["core::Option", "core::Result"]
            .iter()
            .any(|path| self.ck.is_std(&expr.ty, path));
        let mut result = Value {
            fresh: true,
            ..Value::default()
        };
        for (_, value) in values {
            let found = self.expr(value);
            result.roots.extend(found.roots.iter().copied());
            if wraps && found.form == Some(Form::Part) {
                self.read(&found, value.token);
                result.form = Some(Form::Wrapped);
                result.writable = found.writable;
                result.holds.extend(found.holds);
                continue;
            }
            if self.ck.has(&expr.ty, Property::View) {
                result.holds.extend(found.holds.iter().cloned());
            }
            self.take(&found, &value.ty, value.token);
        }
        result
    }

    /// A call: its receiver and arguments evaluate in order, then their accesses become active
    /// together, then the call's result derives from its receiver when it is a projection.
    fn call(&mut self, call: &Call, token: Tok) -> Value {
        let mut values = Vec::new();
        for arg in &call.args {
            let value = self.expr(&arg.expr);
            values.push(value);
        }
        let mut accesses: Vec<(Path, Access, Tok)> = Vec::new();
        for (arg, value) in call.args.iter().zip(&values) {
            let copy = self.is_copy(&arg.expr.ty);
            let access = match arg.mode {
                Mode::Read => Access::Read,
                Mode::Mut => Access::Mut,
                Mode::Take if copy => Access::Read,
                Mode::Take => Access::Move,
            };
            let through_part = matches!(value.form, Some(Form::Part | Form::Wrapped));
            if let Some(path) = &value.place
                && !(copy && access == Access::Read)
                && !through_part
            {
                accesses.push((path.clone(), access, arg.expr.token));
            }
            for hold in &value.holds {
                let access = if hold.held { Access::Mut } else { Access::Read };
                accesses.push((hold.source.clone(), access, arg.expr.token));
            }
            accesses.extend(value.captures.iter().cloned());
        }
        for (later, (path, access, at)) in accesses.iter().enumerate() {
            let earlier = accesses[..later].iter().find(|(other, other_access, _)| {
                other.overlaps(path) && (*access != Access::Read || *other_access != Access::Read)
            });
            if let Some((other, other_access, other_at)) = earlier {
                let role = if *other_access == Access::Read {
                    Role::Frozen
                } else {
                    Role::Held
                };
                let because = vec![
                    self.reason(*other_at, role, self.written(other)),
                    self.reason(token, Role::Used, self.written(other)),
                    self.reason(
                        *at,
                        Role::Accessed,
                        format!("{}{}", access.word(), self.written(path)),
                    ),
                ];
                self.report_because(*at, Code::OwnConflict, because);
            }
        }
        for (arg, value) in call.args.iter().zip(&values) {
            match arg.mode {
                Mode::Read => self.read(value, arg.expr.token),
                Mode::Mut => self.write(value, Access::Mut, arg.expr.token),
                Mode::Take => self.take(value, &arg.expr.ty, arg.expr.token),
            }
        }
        if call.head.wait {
            self.suspend(token);
        }
        if call
            .failure
            .is_some_and(|(marker, _)| marker == FailureMarker::Try)
            && call
                .head
                .fails
                .as_ref()
                .is_some_and(|fails| !fails.is_never())
        {
            let saved = self.state.clone();
            self.exit_body(token);
            self.state = saved;
        }
        if !call.proj {
            return Value {
                fresh: true,
                ..Value::default()
            };
        }
        let Some(receiver) = values.first() else {
            return Value::default();
        };
        let held = call.args.first().is_some_and(|arg| arg.mode == Mode::Mut);
        let mut result = Value {
            form: Some(self.form(call)),
            writable: held && (receiver.form.is_none() || receiver.writable),
            temporary: receiver.temporary || receiver.fresh,
            roots: receiver.roots.clone(),
            ..Value::default()
        };
        if result.form != Some(Form::View) {
            result.place = receiver.place.clone();
        }
        result.holds.extend(receiver.holds.iter().cloned());
        if let Some(source) = &receiver.place
            && receiver.form.is_none()
        {
            result.holds.push(Hold {
                holder: UNBOUND,
                source: source.clone(),
                held,
                token,
            });
        }
        result
    }

    /// The form of a projection call's result, from its head's result type with each type
    /// parameter counted as no view.
    fn form(&self, call: &Call) -> Form {
        let declared = match &call.callee {
            Callee::Function { item, .. } => self.ck.fn_sig(*item).map(|sig| sig.result),
            Callee::Method {
                interface, method, ..
            } => match &self.ck.sig(interface.interface).kind {
                SigKind::Interface { methods } => Some(methods[*method].sig.result.clone()),
                _ => None,
            },
            _ => None,
        }
        .unwrap_or_else(|| call.head.result.clone());
        if ["core::Option", "core::Result"]
            .iter()
            .any(|path| self.ck.is_std(&declared, path))
        {
            return if self.ck.counts_as_view(&declared) {
                Form::View
            } else {
                Form::Wrapped
            };
        }
        if self.ck.counts_as_view(&declared) {
            Form::View
        } else {
            Form::Part
        }
    }

    /// The captures of a task closure, this its body's walk: each of a Send type, a view only
    /// lent, and lending legal only in a lexical spawn, of a parameter or capture of its scope's
    /// body closure.
    /// The fix capturing by `take` each place a task closure opened at `token` lends, when modes
    /// allow moving each one's root.
    fn lent_by_take(&self, token: Tok, closure: &TClosure) -> Option<Fix> {
        let lends = |capture: &TCapture| {
            let mode = capture.listed.map(|(mode, _)| mode);
            mode == Some(Mode::Mut)
                || mode.is_none() && !self.is_copy(&self.body.local(capture.outer).ty)
        };
        let lending: Vec<&TCapture> = closure
            .captures
            .iter()
            .filter(|capture| lends(capture))
            .collect();
        if lending.is_empty()
            || !lending
                .iter()
                .all(|capture| self.body.local(capture.outer).movable())
        {
            return None;
        }
        let entries: Vec<String> = closure
            .captures
            .iter()
            .filter(|capture| capture.listed.is_some() || lends(capture))
            .map(|capture| {
                let mode = match capture.listed {
                    Some((mode, _)) if !lends(capture) => mode,
                    _ => Mode::Take,
                };
                format!("{} {}", mode.word(), self.body.local(capture.outer).name)
            })
            .collect();
        let list = format!("[{}]", entries.join(", "));
        let syntax = &self.ck.db.item(self.item).file.syntax;
        let open = syntax.next(token as usize + 1);
        if syntax.tokens[open].kind != TokenKind::Punct(Punct::LBracket) {
            return Some(self.ck.fix_appending(self.item, token, &list));
        }
        let close = (open..syntax.tokens.len())
            .find(|&at| syntax.tokens[at].kind == TokenKind::Punct(Punct::RBracket))?;
        Some(
            self.ck
                .fix_replacing(self.item, open as Tok, close as Tok, &list),
        )
    }

    fn check_task_captures(&mut self, token: Tok, closure: &TClosure) {
        let lent_by_take = self.lent_by_take(token, closure);
        for capture in &closure.captures {
            let outer = self.body.local(capture.outer);
            let at = capture
                .listed
                .map(|(_, listed)| listed)
                .or_else(|| self.first_use.get(&capture.local).copied())
                .unwrap_or(token);
            let mode = capture.listed.map(|(mode, _)| mode);
            let lending = mode == Some(Mode::Mut) || (mode.is_none() && !self.is_copy(&outer.ty));
            let sendable = self.ck.has(&outer.ty, Property::Send)
                && (lending || !self.ck.has(&outer.ty, Property::View));
            if !outer.ty.has_error() && !sendable {
                self.report(at, Code::TaskCaptureNotSend);
                continue;
            }
            if !lending {
                continue;
            }
            let Some(scope) = closure.lent else {
                let fixes = lent_by_take.clone().into_iter().collect();
                self.report_fixed((at, at), Code::TaskLendOutsideScope, fixes);
                continue;
            };
            let mut root = capture.outer;
            while self.body.local(root).closure != scope
                && let Some(captured) = self.body.local(root).captured
            {
                root = captured;
            }
            let root = self.body.local(root);
            let of_scope = root.closure == scope
                && matches!(root.kind, LocalKind::Param(_) | LocalKind::Capture(_));
            if !of_scope {
                self.report(at, Code::TaskLendLocal);
            }
        }
    }

    /// A closure: its body is walked once on its own; at its creation `take` captures move in,
    /// and its other captures are accesses while it is live.
    fn closure(&mut self, token: Tok, closure: &TClosure) -> Value {
        if self.walked.insert(closure.index) {
            let mut inner = Flow::new(self.ck, self.item, self.body);
            inner.task = Some(closure.task);
            inner.opener = Some(token);
            inner.loop_base = self.loop_base + self.loops.len();
            inner.reported = std::mem::take(&mut self.reported);
            inner.walked = std::mem::take(&mut self.walked);
            inner.views = self.views.clone();
            for &param in &closure.params {
                inner.declare(param, inner.body.local(param).token);
            }
            for capture in &closure.captures {
                inner.declare(capture.local, token);
            }
            let end = match &closure.body.kind {
                TKind::Block(block) => {
                    inner.block(block);
                    block.end
                }
                _ => {
                    let value = inner.expr(&closure.body);
                    inner.returned(&value, &closure.body);
                    closure.body.token
                }
            };
            inner.exit_scopes(0, end);
            if closure.task {
                inner.check_task_captures(token, closure);
            }
            let judged = if inner.failed {
                &[][..]
            } else {
                &closure.captures[..]
            };
            for capture in judged {
                let listed_mut = matches!(capture.listed, Some((Mode::Mut, _)));
                if let Some((_, listed)) = capture.listed
                    && listed_mut
                    && !inner.mutated.contains(&capture.local)
                {
                    let syntax = &inner.ck.db.item(inner.item).file.syntax;
                    let name = syntax.next(listed as usize + 1) as Tok;
                    let fix = inner.ck.fix_capture_deleted(inner.item, listed, name);
                    inner.report_fixed((listed, name), Code::ClosureCaptureMode, vec![fix]);
                }
            }
            self.failed |= inner.failed;
            self.reported = inner.reported;
            self.walked = inner.walked;
        }
        let mut value = Value::default();
        for capture in &closure.captures {
            let outer = Path::of(capture.outer);
            let ty = &self.body.local(capture.outer).ty;
            let found = Value::place(outer.clone());
            let at = capture.listed.map_or(token, |(_, listed)| listed);
            match capture.listed.map(|(mode, _)| mode) {
                Some(Mode::Take) => self.take(&found, &ty.clone(), at),
                Some(Mode::Mut) => {
                    self.write(&found, Access::Mut, at);
                    value.captures.push((outer, Access::Mut, at));
                }
                _ => {
                    self.read(&found, at);
                    if !self.is_copy(ty) {
                        value.captures.push((outer, Access::Read, at));
                    }
                }
            }
        }
        value
    }
}

/// The token of the expression yielding an expression's value: a block's last statement's.
fn value_token(expr: &TExpr) -> Tok {
    match &expr.kind {
        TKind::Block(block) => tail_token(block, expr.token),
        _ => expr.token,
    }
}

/// The token of the statement yielding a block's value, or `token` for an empty block.
fn tail_token(block: &TBlock, token: Tok) -> Tok {
    block.stmts.last().map_or(token, |stmt| match &stmt.kind {
        TStmtKind::Expr(value) => value_token(value),
        _ => stmt.token,
    })
}
