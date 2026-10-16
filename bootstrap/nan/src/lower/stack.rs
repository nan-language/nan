//! Stack limits: the frame size and call sites of each lowered function, the `stack N` demands
//! on lowered functions and function values, and the final build's judging of each demand.

use std::collections::HashMap;

use cranelift_module::FuncId;

use crate::check::Checker;
use crate::check::db::ItemId;
use crate::diagnostic::{Code, Diagnostic, Reason, Role};
use crate::nir::Span;

/// The stub call bound of `x86_64-host`: the most of its caller's stack a call of a support
/// routine or compiler stub uses.
pub const STUB_CALL_BOUND: u64 = 256;

/// Where in source a call or a use lies.
#[derive(Clone, Copy, Debug)]
pub struct At {
    pub item: ItemId,
    pub span: Span,
}

/// A call site of a lowered function.
#[derive(Clone, Debug)]
pub struct Site {
    pub kind: Kind,
    pub at: Option<At>,
}

#[derive(Clone, Debug)]
pub enum Kind {
    /// A direct call, with what it passes for the callee's function-typed parameters, by
    /// position.
    Direct {
        callee: FuncId,
        bindings: Vec<(usize, Binding)>,
    },
    /// A call through the function's own function-typed parameter at a position.
    Param(usize),
    /// A call the `stack N` of its type or head bounds; none for an unbounded one.
    Bounded(Option<u64>),
    /// A call of a support routine, a compiler stub or the C library.
    Stub,
}

/// What a direct call passes for a function-typed parameter of its callee.
#[derive(Clone, Debug)]
pub enum Binding {
    /// A closure or a named function, by the lowered function its value calls.
    Function(FuncId),
    /// The calling function's own parameter at a position.
    Param(usize),
    /// Another function value, bounded by the `stack N` its type demands.
    Bounded(Option<u64>),
}

/// A lowered function: its frame size, the `stack N` the type of each of its function-typed
/// parameters demands, by position, and its call sites.
pub struct Frame {
    pub function: FuncId,
    /// The canonical path naming it in reasons; empty for code lowering writes itself.
    pub name: String,
    pub size: u64,
    pub params: Vec<(usize, Option<u64>)>,
    pub sites: Vec<Site>,
}

/// A `stack N` demanded of a lowered function or of a function value, and where a breach is
/// reported.
pub struct Demand {
    pub judged: Judged,
    pub stack: u64,
    pub report: Report,
}

pub enum Judged {
    Function(FuncId),
    /// A function value, whose stack use is the `stack N` its own type demands, if any.
    Value(Option<u64>),
}

pub enum Report {
    /// A demand of its own limits, of the interface method it implements or of the function
    /// type a closure is written for: at each unbounded call site in its text, or on the line
    /// of its head, at `head`, for an exceeded bound.
    Own { head: At },
    /// A demand of a function type that a named function or function value fits: at the use.
    Fit(At),
}

/// A stack use in bytes with its deepest call path, or none with the path of calls down to
/// the unbounded call site; each call is a function and the index of its site.
#[derive(Clone)]
enum Use {
    Bounded(u64, Vec<(FuncId, usize)>),
    Unbounded(Vec<(FuncId, usize)>),
}

/// Judges every demand against the frames of a final build's lowered functions.
pub fn judge(ck: &Checker, frames: &[Frame], demands: &[Demand]) -> Vec<Diagnostic> {
    let mut judge = Judge {
        frames: frames.iter().map(|frame| (frame.function, frame)).collect(),
        known: HashMap::new(),
        active: Vec::new(),
    };
    let mut found = Vec::new();
    for demand in demands {
        judge.demand(ck, demand, &mut found);
    }
    found
}

struct Judge<'f> {
    frames: HashMap<FuncId, &'f Frame>,
    known: HashMap<(FuncId, Vec<Option<u64>>), Use>,
    active: Vec<FuncId>,
}

impl Judge<'_> {
    fn demand(&mut self, ck: &Checker, demand: &Demand, found: &mut Vec<Diagnostic>) {
        let function = match demand.judged {
            Judged::Function(function) => function,
            Judged::Value(bound) => {
                let at = match &demand.report {
                    Report::Fit(at) | Report::Own { head: at } => *at,
                };
                let code = match bound {
                    None => Code::LimitStackUnbounded,
                    Some(bytes) if bytes > demand.stack => Code::LimitStackExceeded,
                    Some(_) => return,
                };
                found.extend(diagnostic(ck, at, code, Vec::new()));
                return;
            }
        };
        let frame = self.frames[&function];
        let bounds = own_bounds(frame);
        self.active.push(function);
        let mut deepest: (u64, Vec<(FuncId, usize)>) = (0, Vec::new());
        let mut unbounded = Vec::new();
        for (index, site) in frame.sites.iter().enumerate() {
            match self.site(site, &bounds) {
                Use::Unbounded(path) => unbounded.push((index, path)),
                Use::Bounded(bytes, path) if bytes > deepest.0 => {
                    deepest = (bytes, prepend((function, index), path));
                }
                Use::Bounded(..) => {}
            }
        }
        self.active.pop();
        if unbounded.is_empty() {
            if frame.size + deepest.0 > demand.stack {
                let because = self.because(ck, &deepest.1, false);
                let at = match &demand.report {
                    Report::Own { head } => *head,
                    Report::Fit(at) => *at,
                };
                found.extend(diagnostic(ck, at, Code::LimitStackExceeded, because));
            }
            return;
        }
        for (index, path) in unbounded {
            let path = prepend((function, index), path);
            let because = self.because(ck, &path, true);
            let at = match &demand.report {
                Report::Own { head } => frame.sites[index].at.unwrap_or(*head),
                Report::Fit(at) => *at,
            };
            found.extend(diagnostic(ck, at, Code::LimitStackUnbounded, because));
        }
    }

    /// The stack use of a function whose function-typed parameters have bounds, by position.
    fn function(&mut self, function: FuncId, bounds: &[Option<u64>]) -> Use {
        if self.active.contains(&function) {
            return Use::Unbounded(Vec::new());
        }
        let key = (function, bounds.to_vec());
        if let Some(known) = self.known.get(&key) {
            return known.clone();
        }
        let Some(frame) = self.frames.get(&function).copied() else {
            return Use::Bounded(STUB_CALL_BOUND, Vec::new());
        };
        self.active.push(function);
        let mut deepest = (0, Vec::new());
        let mut unbounded = None;
        for (index, site) in frame.sites.iter().enumerate() {
            match self.site(site, bounds) {
                Use::Unbounded(path) => {
                    unbounded = Some(prepend((function, index), path));
                    break;
                }
                Use::Bounded(bytes, path) if bytes > deepest.0 => {
                    deepest = (bytes, prepend((function, index), path));
                }
                Use::Bounded(..) => {}
            }
        }
        self.active.pop();
        let used = match unbounded {
            Some(path) => Use::Unbounded(path),
            None => Use::Bounded(frame.size + deepest.0, deepest.1),
        };
        self.known.insert(key, used.clone());
        used
    }

    /// The stack use of a call site under the bounds of its function's parameters.
    fn site(&mut self, site: &Site, bounds: &[Option<u64>]) -> Use {
        let bound = |bytes: Option<u64>| match bytes {
            Some(bytes) => Use::Bounded(bytes, Vec::new()),
            None => Use::Unbounded(Vec::new()),
        };
        match &site.kind {
            Kind::Stub => Use::Bounded(STUB_CALL_BOUND, Vec::new()),
            Kind::Bounded(bytes) => bound(*bytes),
            Kind::Param(position) => bound(bounds.get(*position).copied().flatten()),
            Kind::Direct { callee, bindings } => {
                let mut callee_bounds = match self.frames.get(callee) {
                    Some(frame) => own_bounds(frame),
                    None => Vec::new(),
                };
                for (position, binding) in bindings {
                    let passed = match binding {
                        Binding::Function(function) => {
                            let own = self
                                .frames
                                .get(function)
                                .map_or_else(Vec::new, |frame| own_bounds(frame));
                            match self.function(*function, &own) {
                                Use::Bounded(bytes, _) => Some(bytes),
                                Use::Unbounded(_) => None,
                            }
                        }
                        Binding::Param(own) => bounds.get(*own).copied().flatten(),
                        Binding::Bounded(bytes) => *bytes,
                    };
                    if callee_bounds.len() <= *position {
                        callee_bounds.resize(*position + 1, None);
                    }
                    callee_bounds[*position] = passed;
                }
                self.function(*callee, &callee_bounds)
            }
        }
    }

    /// The because-chain of a call path: a `called` reason per call, and for an unbounded path
    /// a `raised` reason for its last call site, the unbounded one.
    fn because(&self, ck: &Checker, path: &[(FuncId, usize)], unbounded: bool) -> Vec<Reason> {
        let mut because = Vec::new();
        for (step, (function, index)) in path.iter().enumerate() {
            let site = &self.frames[function].sites[*index];
            let last = unbounded && step + 1 == path.len();
            let (role, subject) = match (&site.kind, last) {
                (Kind::Direct { callee, .. }, false) => (Role::Called, self.name(*callee)),
                (Kind::Direct { callee, .. }, true) => (Role::Raised, self.name(*callee)),
                (Kind::Param(_), _) => (Role::Raised, "function parameter".to_owned()),
                (_, _) => (Role::Raised, "function value".to_owned()),
            };
            if subject.is_empty() {
                continue;
            }
            because.push(Reason {
                role,
                subject,
                at: site.at.map(|at| location(ck, at)),
            });
        }
        because
    }

    fn name(&self, function: FuncId) -> String {
        self.frames
            .get(&function)
            .map_or_else(String::new, |frame| frame.name.clone())
    }
}

/// The bounds a function's parameters have when it is judged on its own.
fn own_bounds(frame: &Frame) -> Vec<Option<u64>> {
    let len = frame
        .params
        .iter()
        .map(|(position, _)| position + 1)
        .max()
        .unwrap_or(0);
    let mut bounds = vec![None; len];
    for (position, bytes) in &frame.params {
        bounds[*position] = *bytes;
    }
    bounds
}

fn prepend(first: (FuncId, usize), mut rest: Vec<(FuncId, usize)>) -> Vec<(FuncId, usize)> {
    rest.insert(0, first);
    rest
}

/// The workspace file and line of a place in source.
fn location(ck: &Checker, at: At) -> (String, u32) {
    let data = ck.db.item(at.item);
    (data.file.path.clone(), data.line(at.span.first))
}

/// A diagnostic of a final build reported on a place in source; none on the carried std.
fn diagnostic(ck: &Checker, at: At, code: Code, because: Vec<Reason>) -> Option<Diagnostic> {
    let mut diagnostic = ck.diagnostic_on(at.item, (at.span.first, at.span.last), code)?;
    diagnostic.because = because;
    Some(diagnostic)
}
