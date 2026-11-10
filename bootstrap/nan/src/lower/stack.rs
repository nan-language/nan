//! Stack limits: the frame size and call sites of each lowered function, the `stack N` demands
//! on lowered functions and function values, the final build's judging of each demand, the
//! stack each grown call needs, and what the routine reserve of a task's stack holds.

use std::collections::{HashMap, HashSet};

use cranelift_module::FuncId;

use crate::check::Checker;
use crate::check::db::ItemId;
use crate::diagnostic::{Code, Diagnostic, Reason, Role};
use crate::nir::Span;

/// The stub call bound of every concrete target: the most of its caller's stack a call of a
/// support routine or compiler stub uses.
pub const STUB_CALL_BOUND: u64 = 256;

/// The stack bound of a function-typed parameter: its bytes, or none with the path of calls
/// down to the unbounded call site that leaves it so, empty when nothing does.
type Bound = Result<u64, Vec<(FuncId, usize)>>;

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
    /// A call through the function's own function-typed parameter at a position, or, for a
    /// `task fn` parameter, the walk moving its captures, or of a method of its own `Dyn`
    /// parameter there whose head demands no `stack N`.
    Param(usize),
    /// A call the `stack N` of its type or head bounds; none for an unbounded one.
    Bounded(Option<u64>),
    /// A call of a compiler stub, the C library or a support routine another image defines.
    Stub,
    /// A call of a support routine the image defines, which a stack use counts as a stub's: the
    /// rest of its use lies in the routine reserve of a task's stack.
    Routine(FuncId),
    /// A support routine's call of a glue of a family, through an address a record holds.
    Glues(Family),
    /// A call of code that the usable part of the stack it runs on holds but for the support
    /// routines it calls: an entry glue's call of its task's entry, or a grown or nested call's
    /// call of its thunk where the stack has room for its need.
    Usable,
    /// A grown call: a call of a wrapper, which calls its thunk through the support routine
    /// growing the stack, the thunk making the call `inner` describes.
    Grown {
        wrapper: FuncId,
        thunk: FuncId,
        inner: Box<Kind>,
    },
    /// A nested decoding: a call of the support routine running a decoding function on a stack
    /// with room for its need, until lowering finds that need bounded and makes it a call of a
    /// support routine; unbounded.
    Nested(FuncId),
}

/// The families of glues support routines call through addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    /// The glue a task starts with: an entry glue, or another glue, which the usable part of
    /// the task's stack holds but for the routines it calls.
    Task,
    /// The glue a task runs once its entry has ended.
    End,
    /// The glue an executor thread runs once a task has ended.
    Done,
    /// The glue withdrawing a stopped task's wait.
    Withdraw,
    /// The glue writing a value in a trap report.
    Written,
    /// The walk moving a value into another domain's reach.
    Transfer,
    /// The walk moving a task closure's captures into another domain's reach.
    Captures,
    /// The glue calling a hook of a kernel policy, which a CPU never calls while a hook runs on
    /// it.
    Policy,
    /// The release of a value of a type.
    Release,
}

/// What a direct call passes for a function-typed or `Dyn` parameter of its callee.
#[derive(Clone, Debug)]
pub enum Binding {
    /// A closure or a named function, by the lowered function its value calls.
    Function(FuncId),
    /// For a `task fn` parameter, the walk moving a closure's captures, by the glue it calls.
    Walk(FuncId),
    /// For a `Dyn` parameter, a `Dyn` value made there, by the code of each method of its
    /// table.
    Methods(Vec<FuncId>),
    /// The calling function's own parameter at a position.
    Param(usize),
    /// Another function value, bounded by the `stack N` its type demands.
    Bounded(Option<u64>),
}

/// A lowered function: its frame size, the `stack N` the type of each of its function-typed
/// and `Dyn` parameters demands, by position, none for a `task fn` or `Dyn` one, and its call
/// sites.
pub struct Frame {
    pub function: FuncId,
    /// The canonical path naming it in reasons, or the type a move walks or a release releases;
    /// empty for other code lowering writes itself.
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

/// A grown call's thunk and the lowered function it calls, none for a function value's code.
pub struct Grown {
    pub thunk: FuncId,
    pub callee: Option<FuncId>,
}

/// A task entry a lowering writes: the function its task starts in, where it is written, and
/// the stack its task gets.
pub struct Task {
    pub function: FuncId,
    pub at: At,
    pub stack: u64,
}

/// Each task's stack use in bytes, its entry judged on its own, or for an unbounded one the
/// because-chain down to the call site that leaves it unbounded.
pub fn uses(ck: &Checker, frames: &[Frame], tasks: &[Task]) -> Vec<Result<u64, Vec<Reason>>> {
    let mut judge = Judge {
        frames: frames.iter().map(|frame| (frame.function, frame)).collect(),
        known: HashMap::new(),
        active: Vec::new(),
        hooked: false,
        routines: None,
    };
    tasks
        .iter()
        .map(|task| {
            let own = judge
                .frames
                .get(&task.function)
                .map_or_else(Vec::new, |frame| own_bounds(frame));
            match judge.function(task.function, &own) {
                Use::Bounded(bytes, _) => Ok(bytes),
                Use::Unbounded(path) => Err(judge.because(ck, &path, true)),
            }
        })
        .collect()
}

/// Where code lies, as `<path>:<line>`.
pub fn site(ck: &Checker, at: At) -> String {
    let (path, line) = location(ck, at);
    format!("{path}:{line}")
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
        hooked: false,
        routines: None,
    };
    let mut found = Vec::new();
    for demand in demands {
        judge.demand(ck, demand, &mut found);
    }
    found
}

struct Judge<'f> {
    frames: HashMap<FuncId, &'f Frame>,
    known: HashMap<(FuncId, Vec<Bound>, bool), Use>,
    /// The functions of the path judged, each with whether a policy hook runs there.
    active: Vec<(FuncId, bool)>,
    /// Whether a policy hook runs where the function judged is called.
    hooked: bool,
    /// When judging what the routine reserve holds, the glues of each family and what a call of
    /// code the usable part holds uses beyond it.
    routines: Option<(&'f HashMap<Family, Vec<FuncId>>, u64)>,
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
        self.active.push((function, self.hooked));
        let mut deepest: (u64, Vec<(FuncId, usize)>) = (0, Vec::new());
        let mut unbounded = Vec::new();
        for (index, site) in frame.sites.iter().enumerate() {
            match self.site(&site.kind, &bounds) {
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
    fn function(&mut self, function: FuncId, bounds: &[Bound]) -> Use {
        if self.active.contains(&(function, self.hooked)) {
            return Use::Unbounded(Vec::new());
        }
        let key = (function, bounds.to_vec(), self.hooked);
        if let Some(known) = self.known.get(&key) {
            return known.clone();
        }
        let Some(frame) = self.frames.get(&function).copied() else {
            return Use::Bounded(STUB_CALL_BOUND, Vec::new());
        };
        self.active.push((function, self.hooked));
        let mut deepest = (0, Vec::new());
        let mut unbounded = None;
        for (index, site) in frame.sites.iter().enumerate() {
            match self.site(&site.kind, bounds) {
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
    fn site(&mut self, kind: &Kind, bounds: &[Bound]) -> Use {
        let bound = |bytes: Bound| match bytes {
            Ok(bytes) => Use::Bounded(bytes, Vec::new()),
            Err(path) => Use::Unbounded(path),
        };
        match kind {
            Kind::Stub => Use::Bounded(STUB_CALL_BOUND, Vec::new()),
            Kind::Routine(callee) => match self.routines {
                Some(_) => self.function(*callee, &[]),
                None => Use::Bounded(STUB_CALL_BOUND, Vec::new()),
            },
            Kind::Glues(family) => match self.routines {
                Some(_) if *family == Family::Policy && self.hooked => Use::Bounded(0, Vec::new()),
                Some((families, beneath)) => {
                    let floor = match family {
                        Family::Task => beneath,
                        _ => 0,
                    };
                    let hooked = self.hooked;
                    self.hooked = hooked || *family == Family::Policy;
                    let glues = families.get(family).map_or(&[][..], Vec::as_slice);
                    let most = self.most(glues, floor);
                    self.hooked = hooked;
                    most
                }
                None => Use::Unbounded(Vec::new()),
            },
            Kind::Usable => match self.routines {
                Some((_, beneath)) => Use::Bounded(beneath, Vec::new()),
                None => Use::Unbounded(Vec::new()),
            },
            Kind::Grown {
                wrapper,
                thunk,
                inner,
            } => match self.site(inner, bounds) {
                Use::Bounded(bytes, path) => {
                    let carried = self.size(*wrapper) + STUB_CALL_BOUND + self.size(*thunk);
                    Use::Bounded(bytes + carried, path)
                }
                unbounded => unbounded,
            },
            Kind::Bounded(bytes) => bound(bytes.ok_or_else(Vec::new)),
            Kind::Nested(_) => Use::Unbounded(Vec::new()),
            Kind::Param(position) => bound(bound_at(bounds, *position)),
            Kind::Direct { callee, bindings } => {
                let mut callee_bounds = match self.frames.get(callee) {
                    Some(frame) => own_bounds(frame),
                    None => Vec::new(),
                };
                for (position, binding) in bindings {
                    let passed = match binding {
                        Binding::Function(function) | Binding::Walk(function) => {
                            match self.own_use(*function) {
                                Use::Bounded(bytes, _) => Ok(bytes),
                                Use::Unbounded(path) => Err(path),
                            }
                        }
                        Binding::Methods(methods) => {
                            let mut most = Ok(0);
                            for method in methods {
                                match self.own_use(*method) {
                                    Use::Bounded(bytes, _) => {
                                        most = most.map(|most: u64| most.max(bytes));
                                    }
                                    Use::Unbounded(path) => {
                                        most = Err(path);
                                        break;
                                    }
                                }
                            }
                            most
                        }
                        Binding::Param(own) => bound_at(bounds, *own),
                        Binding::Bounded(bytes) => bytes.ok_or_else(Vec::new),
                    };
                    if callee_bounds.len() <= *position {
                        callee_bounds.resize(*position + 1, Err(Vec::new()));
                    }
                    callee_bounds[*position] = passed;
                }
                self.function(*callee, &callee_bounds)
            }
        }
    }

    /// The most stack any of some functions uses under the bounds of their own parameters, at
    /// least `floor`.
    fn most(&mut self, functions: &[FuncId], floor: u64) -> Use {
        let mut most = Use::Bounded(floor, Vec::new());
        for function in functions {
            match (self.own_use(*function), &most) {
                (Use::Bounded(bytes, path), Use::Bounded(deepest, _)) if bytes > *deepest => {
                    most = Use::Bounded(bytes, path);
                }
                (Use::Bounded(..), _) => {}
                (unbounded, _) => return unbounded,
            }
        }
        most
    }

    /// The stack use of a function under the bounds of its own parameters.
    fn own_use(&mut self, function: FuncId) -> Use {
        let own = self
            .frames
            .get(&function)
            .map_or_else(Vec::new, |frame| own_bounds(frame));
        self.function(function, &own)
    }

    /// The because-chain of a call path: a `called` reason per call, and for an unbounded path
    /// a `raised` reason for its last call site, the unbounded one. Calls of code lowering
    /// writes itself, support routines and compiler stubs among them, have no reason.
    fn because(&self, ck: &Checker, path: &[(FuncId, usize)], unbounded: bool) -> Vec<Reason> {
        let mut because = Vec::new();
        for (step, (function, index)) in path.iter().enumerate() {
            let site = &self.frames[function].sites[*index];
            let role = match unbounded && step + 1 == path.len() {
                true => Role::Raised,
                false => Role::Called,
            };
            let subject = self.subject(&site.kind);
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

    /// What a call site's reason names: its callee, empty for code lowering writes itself.
    fn subject(&self, kind: &Kind) -> String {
        match kind {
            Kind::Direct { callee, .. } => self.name(*callee),
            Kind::Param(_) => "parameter".to_owned(),
            Kind::Bounded(_) => "function value".to_owned(),
            Kind::Stub | Kind::Routine(_) | Kind::Glues(_) | Kind::Usable | Kind::Nested(_) => {
                String::new()
            }
            Kind::Grown { inner, .. } => self.subject(inner),
        }
    }

    fn size(&self, function: FuncId) -> u64 {
        self.frames.get(&function).map_or(0, |frame| frame.size)
    }

    fn name(&self, function: FuncId) -> String {
        self.frames
            .get(&function)
            .map_or_else(String::new, |frame| frame.name.clone())
    }
}

/// The most stack any of `functions` uses beyond the usable part of a task's stack: calls of
/// support routines the image defines and of glues through their addresses followed, and a
/// call of code the usable part holds using `beneath`; none when one reaches an unbounded call
/// site.
pub fn routine_use(
    frames: &[Frame],
    families: &HashMap<Family, Vec<FuncId>>,
    beneath: u64,
    functions: &[FuncId],
) -> Option<u64> {
    let mut judge = Judge {
        frames: frames.iter().map(|frame| (frame.function, frame)).collect(),
        known: HashMap::new(),
        active: Vec::new(),
        hooked: false,
        routines: Some((families, beneath)),
    };
    match judge.most(functions, 0) {
        Use::Bounded(bytes, _) => Some(bytes),
        Use::Unbounded(_) => None,
    }
}

/// The stack each grown call's thunk needs on the stack it runs on: its frame and the stack use
/// of what it calls up to the grown calls that code makes, each using its wrapper's frame and
/// the stub call bound there. A function value's code needs the most any lowered function but
/// those `excluded` needs, that function called directly.
pub fn needs(frames: &[Frame], grown: &[Grown], excluded: &HashSet<FuncId>) -> Vec<u64> {
    let mut segments = Segments {
        frames: frames.iter().map(|frame| (frame.function, frame)).collect(),
        known: HashMap::new(),
        active: HashSet::new(),
    };
    let value = frames
        .iter()
        .filter(|frame| !excluded.contains(&frame.function))
        .filter_map(|frame| segments.segment(frame.function))
        .max()
        .unwrap_or(0);
    grown
        .iter()
        .map(|grown| {
            let called = match grown.callee {
                Some(callee) => segments
                    .segment(callee)
                    .expect("a grown call's callee reaching only grown calls unbounded"),
                None => value,
            };
            segments.size(grown.thunk) + called
        })
        .collect()
}

/// The most nestings of a decoding, each of which every cycle of decoding functions' calls of
/// each other passes, and the one outside them.
const NESTINGS: u64 = 129;

/// The stack a nested decoding by `decoder` needs: along each path of calls, the frames of the
/// functions outside a cycle of calls once, and those of each cycle of `decoders` once per
/// nesting; none when a path reaches a cycle holding another function or an unbounded call
/// site.
pub fn nested_need(frames: &[Frame], decoder: FuncId, decoders: &HashSet<FuncId>) -> Option<u64> {
    let frames: HashMap<FuncId, &Frame> =
        frames.iter().map(|frame| (frame.function, frame)).collect();
    let mut components = Components {
        frames: &frames,
        index: HashMap::new(),
        low: HashMap::new(),
        stack: Vec::new(),
        of: HashMap::new(),
        members: Vec::new(),
    };
    components.visit(decoder);
    let mut known = HashMap::new();
    let component = components.of[&decoder];
    deepest(&frames, &components, decoders, component, &mut known)
}

/// The strongly connected components of the calls from a function, numbered as they complete,
/// each callee's before its caller's.
struct Components<'f> {
    frames: &'f HashMap<FuncId, &'f Frame>,
    index: HashMap<FuncId, usize>,
    low: HashMap<FuncId, usize>,
    stack: Vec<FuncId>,
    of: HashMap<FuncId, usize>,
    members: Vec<Vec<FuncId>>,
}

impl Components<'_> {
    fn visit(&mut self, function: FuncId) {
        let number = self.index.len();
        self.index.insert(function, number);
        self.low.insert(function, number);
        self.stack.push(function);
        for callee in callees(self.frames, function) {
            if !self.index.contains_key(&callee) {
                self.visit(callee);
                let low = self.low[&function].min(self.low[&callee]);
                self.low.insert(function, low);
            } else if self.stack.contains(&callee) {
                let low = self.low[&function].min(self.index[&callee]);
                self.low.insert(function, low);
            }
        }
        if self.low[&function] == self.index[&function] {
            let mut members = Vec::new();
            loop {
                let member = self.stack.pop().expect("a member of the component");
                self.of.insert(member, self.members.len());
                members.push(member);
                if member == function {
                    break;
                }
            }
            self.members.push(members);
        }
    }
}

/// The functions a function calls directly, through grown calls among them.
fn callees(frames: &HashMap<FuncId, &Frame>, function: FuncId) -> Vec<FuncId> {
    let Some(frame) = frames.get(&function) else {
        return Vec::new();
    };
    (frame.sites.iter())
        .filter_map(|site| direct(&site.kind))
        .collect()
}

/// The function a call site calls directly, a grown call's included.
fn direct(kind: &Kind) -> Option<FuncId> {
    match kind {
        Kind::Direct { callee, .. } => Some(*callee),
        Kind::Grown { inner, .. } => direct(inner),
        _ => None,
    }
}

/// The deepest stack a call of a component's functions uses: its frames, once per nesting for a
/// cycle, then its deepest call leaving it.
fn deepest(
    frames: &HashMap<FuncId, &Frame>,
    components: &Components,
    decoders: &HashSet<FuncId>,
    component: usize,
    known: &mut HashMap<usize, Option<u64>>,
) -> Option<u64> {
    if let Some(used) = known.get(&component) {
        return *used;
    }
    let members = &components.members[component];
    let cyclic = members.len() > 1
        || members
            .iter()
            .any(|member| callees(frames, *member).contains(member));
    if cyclic && !members.iter().all(|member| decoders.contains(member)) {
        known.insert(component, None);
        return None;
    }
    let mut own = 0;
    let mut leaving = Some(0);
    for member in members {
        let Some(frame) = frames.get(member) else {
            own += STUB_CALL_BOUND;
            continue;
        };
        let mut size = frame.size;
        for site in &frame.sites {
            let used = match &site.kind {
                Kind::Stub | Kind::Routine(_) | Kind::Nested(_) => Some(STUB_CALL_BOUND),
                Kind::Bounded(bytes) => *bytes,
                Kind::Param(_) | Kind::Glues(_) | Kind::Usable => None,
                Kind::Direct { .. } | Kind::Grown { .. } => {
                    if let Kind::Grown { wrapper, thunk, .. } = &site.kind {
                        let carried = frames.get(wrapper).map_or(0, |frame| frame.size)
                            + STUB_CALL_BOUND
                            + frames.get(thunk).map_or(0, |frame| frame.size);
                        size = size.max(frame.size + carried);
                    }
                    let callee = direct(&site.kind).expect("a direct callee");
                    let target = components.of[&callee];
                    match target == component {
                        true => Some(0),
                        false => deepest(frames, components, decoders, target, known),
                    }
                }
            };
            leaving = leaving.zip(used).map(|(leaving, used)| leaving.max(used));
        }
        own += size;
    }
    let rounds = if cyclic { NESTINGS } else { 1 };
    let used = leaving.map(|leaving| own * rounds + leaving);
    known.insert(component, used);
    used
}

/// The stack a task whose entry is `function` starts with when its stack grows: 64 KiB, or the
/// stack use of the entry up to the grown calls it makes if more.
pub fn grown_start(frames: &[Frame], function: FuncId) -> u64 {
    let mut segments = Segments {
        frames: frames.iter().map(|frame| (frame.function, frame)).collect(),
        known: HashMap::new(),
        active: HashSet::new(),
    };
    let used = segments
        .segment(function)
        .expect("a grown entry reaching only grown calls unbounded");
    used.max(super::runtime::tasks::STACK)
}

/// The stack use of lowered functions up to the grown calls they make, none for a function
/// reaching an unbounded call site another way.
struct Segments<'f> {
    frames: HashMap<FuncId, &'f Frame>,
    known: HashMap<FuncId, Option<u64>>,
    active: HashSet<FuncId>,
}

impl Segments<'_> {
    fn segment(&mut self, function: FuncId) -> Option<u64> {
        if let Some(known) = self.known.get(&function) {
            return *known;
        }
        let Some(frame) = self.frames.get(&function).copied() else {
            return Some(STUB_CALL_BOUND);
        };
        if !self.active.insert(function) {
            return None;
        }
        let mut deepest = Some(0);
        for site in &frame.sites {
            let used = match &site.kind {
                Kind::Stub | Kind::Routine(_) | Kind::Nested(_) => Some(STUB_CALL_BOUND),
                Kind::Grown { wrapper, .. } => Some(self.size(*wrapper) + STUB_CALL_BOUND),
                Kind::Bounded(bytes) => *bytes,
                Kind::Glues(_) | Kind::Usable => None,
                Kind::Param(position) => frame
                    .params
                    .iter()
                    .find(|(at, _)| at == position)
                    .and_then(|(_, bytes)| *bytes),
                Kind::Direct { callee, .. } => self.segment(*callee),
            };
            deepest = deepest.zip(used).map(|(deepest, used)| deepest.max(used));
        }
        self.active.remove(&function);
        let segment = deepest.map(|deepest| frame.size + deepest);
        self.known.insert(function, segment);
        segment
    }

    fn size(&self, function: FuncId) -> u64 {
        self.frames.get(&function).map_or(0, |frame| frame.size)
    }
}

/// The bounds a function's parameters have when it is judged on its own.
fn own_bounds(frame: &Frame) -> Vec<Bound> {
    let len = frame
        .params
        .iter()
        .map(|(position, _)| position + 1)
        .max()
        .unwrap_or(0);
    let mut bounds = vec![Err(Vec::new()); len];
    for (position, bytes) in &frame.params {
        bounds[*position] = bytes.ok_or_else(Vec::new);
    }
    bounds
}

/// The bound of the parameter at a position, none past the last.
fn bound_at(bounds: &[Bound], position: usize) -> Bound {
    bounds
        .get(position)
        .cloned()
        .unwrap_or_else(|| Err(Vec::new()))
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

#[cfg(test)]
mod tests;
