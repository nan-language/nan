//! Protocols: the rules of their operations and summary types, the components serving them and
//! their handlers, the operations an outbox calls, and protocol hashes.

use std::collections::HashSet;
use std::ops::Range;

use super::Checker;
use super::db::{ItemId, ModuleId};
use super::interface::narrower;
use super::props::Property;
use super::resolve::Named;
use super::reversibility::Class;
use super::sig::{FnSig, ParamSig, SigKind};
use super::ty::{Arg, Const, LimitSet, Ty};
use crate::diagnostic::Code;
use crate::hash::Hash;
use crate::syntax::ast::{self, ClauseKind, ItemKind, Mode, ProtocolItem, Visibility};

/// The names no operation takes: the outbox's own method, the component's `start` and the
/// summary read.
const RESERVED: [&str; 3] = ["post", "start", "summary"];

/// What a function of a component serving a protocol stands for: the handler of an operation,
/// by its index, or the summary function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Served {
    Handler(usize),
    Summary,
}

impl Checker<'_> {
    /// The rules of a protocol's operations and summary type.
    pub fn check_protocol(&self, item: ItemId, protocol: &ProtocolItem) {
        let sig = self.sig(item);
        let SigKind::Protocol {
            operations,
            summary,
            ..
        } = &sig.kind
        else {
            return;
        };
        let mut seen = HashSet::new();
        for (index, (operation, head)) in protocol.operations.iter().zip(operations).enumerate() {
            let method = &operation.method;
            if !seen.insert(method.name.text.as_str()) {
                self.report(item, method.name.token, Code::ItemDuplicate);
            } else if RESERVED.contains(&method.name.text.as_str()) {
                self.report(item, method.name.token, Code::ComponentOperationName);
            }
            if !head.sig.limits.pure && head.sig.limits.class.is_none() {
                self.report(item, method.token, Code::ComponentOperationClass);
            }
            let sent = operation.visibility == Visibility::Pub;
            let types = head.sig.params.iter().map(|param| &param.ty);
            let wrong = types
                .chain([&head.sig.result])
                .chain(&head.sig.fails)
                .filter(|ty| !ty.has_error() && **ty != Ty::Unit)
                .any(|ty| !self.operation_type(ty, sent));
            if wrong {
                self.report(item, method.token, Code::ComponentOperationType);
            }
            for clause in &method.clauses {
                match &clause.kind {
                    ClauseKind::Requires(_) | ClauseKind::Ensures(_) | ClauseKind::Missing => {}
                    ClauseKind::Undo { target, member, .. } => {
                        match self.operation_undoer(item, index, target, member.as_ref()) {
                            None => self.report(item, clause.token, Code::EffectUndoTarget),
                            Some(undoer)
                                if !self.written_class(item, undoer).at_most(Class::Restorable) =>
                            {
                                self.report(item, clause.token, Code::EffectUndoIrreversible);
                            }
                            Some(_) => {}
                        }
                    }
                    _ => self.report(item, clause.token, Code::ComponentOperationClause),
                }
            }
        }
        if let (Some(ty), Some(written)) = (summary, &protocol.summary)
            && !ty.has_error()
        {
            let encode = self.db.core_item("Encode");
            let encoded = encode.is_none_or(|encode| self.implements(ty, encode));
            if !encoded || !self.has(ty, Property::Copy) {
                self.report(item, written.token, Code::ComponentSummaryType);
            }
        }
    }

    /// Whether a type may be one of an operation's: Send and not linear, and, for an operation
    /// sent between closures, implementing Encode or a handle type.
    fn operation_type(&self, ty: &Ty, sent: bool) -> bool {
        if !self.has(ty, Property::Send) || self.has(ty, Property::Linear) {
            return false;
        }
        let encode = self.db.core_item("Encode");
        !sent || encode.is_none_or(|encode| self.implements(ty, encode)) || self.is_handle(ty)
    }

    /// Whether a type is a handle type: a primitive capability or an outbox.
    fn is_handle(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Item(item, _) => {
                self.is_primitive_capability(*item) || self.db.canonical(*item) == "sys::Outbox"
            }
            _ => false,
        }
    }

    /// The operation an undo clause of a protocol's operation names: another operation of it,
    /// named by a path of one segment.
    fn operation_undoer(
        &self,
        item: ItemId,
        index: usize,
        target: &ast::Path,
        member: Option<&ast::Name>,
    ) -> Option<usize> {
        let [name] = &target.segments[..] else {
            return None;
        };
        if member.is_some() {
            return None;
        }
        let SigKind::Protocol { operations, .. } = &self.sig(item).kind else {
            return None;
        };
        operations
            .iter()
            .position(|operation| operation.name == name.text)
            .filter(|found| *found != index)
    }

    /// The class an operation's limits state: `traceless` for `pure`, and the greatest class
    /// without either.
    fn written_class(&self, protocol: ItemId, index: usize) -> Class {
        let SigKind::Protocol { operations, .. } = &self.sig(protocol).kind else {
            return Class::WORST;
        };
        let limits = &operations[index].sig.limits;
        match (limits.pure, limits.class) {
            (true, _) => Class::Traceless,
            (false, Some(class)) => class,
            (false, None) => Class::WORST,
        }
    }

    /// The class of an operation: the class its limits state, compensated when it has an undo
    /// clause.
    pub fn operation_class(&self, protocol: ItemId, index: usize) -> Class {
        let class = self.written_class(protocol, index);
        match self.operation_undoes(protocol, index) {
            true => class.compensated(),
            false => class,
        }
    }

    /// Whether an operation has an undo clause.
    pub fn operation_undoes(&self, protocol: ItemId, index: usize) -> bool {
        matches!(&self.db.ast(protocol).kind, ItemKind::Protocol(declared)
            if declared.operations[index].method.clauses.iter()
                .any(|clause| matches!(clause.kind, ClauseKind::Undo { .. })))
    }

    /// The positions among `count` arguments of a call of a method, or parameters of its
    /// implementing function, that the method's contract functions read: all of them for an
    /// interface's method, and for a protocol's operation those after the receiver standing for
    /// the operation's parameters.
    pub fn contract_params(&self, interface: ItemId, method: usize, count: usize) -> Range<usize> {
        match &self.sig(interface).kind {
            SigKind::Protocol { operations, .. } => {
                let params = operations
                    .get(method)
                    .map_or(0, |operation| operation.sig.params.len());
                1..(1 + params).min(count)
            }
            _ => 0..count,
        }
    }

    /// Whether an item is a protocol.
    pub fn is_protocol(&self, item: ItemId) -> bool {
        matches!(self.db.ast(item).kind, ItemKind::Protocol(_))
    }

    /// The protocol of an outbox type posting to a component serving one.
    pub fn outbox_protocol(&self, ty: &Ty) -> Option<ItemId> {
        let Ty::Item(outbox, args) = ty else {
            return None;
        };
        if self.db.canonical(*outbox) != "sys::Outbox" || !self.db.is_std(*outbox) {
            return None;
        }
        match args.first().and_then(Arg::ty) {
            Some(Ty::Item(protocol, _)) if self.is_protocol(*protocol) => Some(*protocol),
            _ => None,
        }
    }

    /// The protocol an impl line naming `sys::Component` serves, when its messages are one.
    pub fn line_protocol(&self, line: ItemId) -> Option<ItemId> {
        let SigKind::Impl {
            interface: Some(bound),
            ..
        } = &self.sig(line).kind
        else {
            return None;
        };
        if self.db.canonical(bound.interface) != "sys::Component" {
            return None;
        }
        match bound.args.get(1).and_then(Arg::ty) {
            Some(Ty::Item(protocol, _)) if self.is_protocol(*protocol) => Some(*protocol),
            _ => None,
        }
    }

    /// The protocol an impl line of `module` serves, read from its syntax: the protocol its
    /// `sys::Component` bound names as messages.
    pub fn written_protocol(&self, module: ModuleId, line: &ast::ImplItem) -> Option<ItemId> {
        let Ok(Named::Item(interface)) = self.db.resolve(module, &line.interface.path) else {
            return None;
        };
        if self.db.canonical(interface) != "sys::Component" {
            return None;
        }
        let Some(ast::GenericArg::Type(message)) = line.interface.args.as_ref()?.get(1) else {
            return None;
        };
        let ast::TypeKind::Path { path, args: None } = &message.kind else {
            return None;
        };
        match self.db.resolve(module, path) {
            Ok(Named::Item(protocol)) if self.is_protocol(protocol) => Some(protocol),
            _ => None,
        }
    }

    /// What a function named `name` stands for in a component serving a protocol, by its
    /// syntax.
    pub fn serves(&self, protocol: ItemId, name: &str) -> Option<Served> {
        let ItemKind::Protocol(declared) = &self.db.ast(protocol).kind else {
            return None;
        };
        if name == "summary" && declared.summary.is_some() {
            return Some(Served::Summary);
        }
        declared
            .operations
            .iter()
            .position(|operation| operation.method.name.text == name)
            .map(Served::Handler)
    }

    /// The names of the functions an impl line serving a protocol requires, `start` first.
    pub fn served_names(&self, protocol: ItemId) -> Vec<String> {
        let ItemKind::Protocol(declared) = &self.db.ast(protocol).kind else {
            return Vec::new();
        };
        let mut names = vec!["start".to_owned()];
        names.extend(
            (declared.operations.iter()).map(|operation| operation.method.name.text.clone()),
        );
        if declared.summary.is_some() {
            names.push("summary".to_owned());
        }
        names
    }

    /// The impl line serving a protocol through which a function is a handler or the summary
    /// function, with the protocol and what the function stands for.
    pub fn served(&self, function: ItemId) -> Option<(ItemId, ItemId, Served)> {
        let ItemKind::Fn(written) = &self.db.ast(function).kind else {
            return None;
        };
        self.implemented_lines(function)
            .into_iter()
            .find_map(|line| {
                let protocol = self.line_protocol(line)?;
                let served = self.serves(protocol, &written.label.name.text)?;
                Some((line, protocol, served))
            })
    }

    /// The handler or summary function of that name of a component state serving a protocol.
    pub fn served_function(&self, state: &Ty, name: &str) -> Option<ItemId> {
        let component = self.db.std_item("sys", "Component")?;
        match self.conformance(state, component)? {
            (super::conform::Conformance::Impl { line, .. }, _) => {
                self.implementing_function(line, name)
            }
            _ => None,
        }
    }

    /// The head a handler or the summary function of a component serving a protocol has, its
    /// state type `state`.
    pub fn served_head(&self, line: ItemId, protocol: ItemId, served: Served, state: &Ty) -> FnSig {
        let receiver = |mode| ParamSig {
            name: "self".to_owned(),
            mode,
            ty: state.clone(),
            token: 0,
            is_self: true,
        };
        let SigKind::Protocol {
            operations,
            summary,
            ..
        } = &self.sig(protocol).kind
        else {
            return bare(Vec::new(), Ty::Error, None, false);
        };
        let Served::Handler(index) = served else {
            let summary = summary.clone().unwrap_or(Ty::Error);
            return bare(vec![receiver(Mode::Read)], summary, None, false);
        };
        let env = match &self.sig(line).kind {
            SigKind::Impl {
                interface: Some(bound),
                ..
            } => bound.args.first().and_then(Arg::ty).cloned(),
            _ => None,
        };
        let context = self.db.std_item("sys", "Cx").map_or(Ty::Error, |cx| {
            Ty::item(cx, vec![Arg::Ty(Ty::item(protocol, Vec::new()))])
        });
        let operation = &operations[index].sig;
        let mut params = vec![receiver(Mode::Mut)];
        params.extend(operation.params.iter().cloned());
        for (name, ty) in [("env", env.unwrap_or(Ty::Error)), ("cx", context)] {
            params.push(ParamSig {
                name: name.to_owned(),
                mode: Mode::Mut,
                ty,
                token: 0,
                is_self: false,
            });
        }
        bare(
            params,
            operation.result.clone(),
            operation.fails.clone(),
            false,
        )
    }

    /// The visibility a handler or the summary function has: the less visible of its state and
    /// its operation, or of its state and its protocol.
    pub fn served_visibility(
        &self,
        protocol: ItemId,
        served: Served,
        state: Visibility,
    ) -> Visibility {
        let visibility = self.db.ast(protocol).visibility;
        let visibility = match (served, &self.db.ast(protocol).kind) {
            (Served::Handler(index), ItemKind::Protocol(declared)) => {
                narrower(visibility, declared.operations[index].visibility)
            }
            _ => visibility,
        };
        narrower(visibility, state)
    }

    /// The head of an operation or of `summary` called as a method of an outbox of its protocol.
    pub fn outbox_head(&self, protocol: ItemId, method: usize, outbox: &Ty) -> Option<FnSig> {
        let SigKind::Protocol {
            operations,
            summary,
            ..
        } = &self.sig(protocol).kind
        else {
            return None;
        };
        let receiver = ParamSig {
            name: "self".to_owned(),
            mode: Mode::Read,
            ty: outbox.clone(),
            token: 0,
            is_self: true,
        };
        let send_error = self
            .db
            .std_item("task", "SendError")
            .map(|item| Ty::item(item, Vec::new()));
        let Some(operation) = operations.get(method) else {
            return Some(bare(vec![receiver], summary.clone()?, send_error, false));
        };
        let operation = &operation.sig;
        let mut params = vec![receiver];
        params.extend(operation.params.iter().cloned());
        let fails = match &operation.fails {
            Some(failure) => {
                let call_error = self.db.std_item("sys", "CallError");
                call_error.map(|item| Ty::item(item, vec![Arg::Ty(failure.clone())]))
            }
            None => send_error,
        };
        Some(bare(params, operation.result.clone(), fails, true))
    }

    /// Whether a method an outbox of a protocol calls is one of its operations, rather than
    /// `summary`.
    pub fn is_operation(&self, protocol: ItemId, method: usize) -> bool {
        matches!(&self.sig(protocol).kind, SigKind::Protocol { operations, .. } if method < operations.len())
    }

    /// Whether an operation is a request: it has a result or a `fails` clause.
    pub fn is_request(&self, protocol: ItemId, index: usize) -> bool {
        match &self.sig(protocol).kind {
            SigKind::Protocol { operations, .. } => {
                operations.get(index).is_some_and(|operation| {
                    operation.sig.result != Ty::Unit || operation.sig.fails.is_some()
                })
            }
            _ => false,
        }
    }

    /// The method an outbox of a protocol calls by a name: an operation, or `summary`, numbered
    /// after the operations.
    pub fn outbox_method(&self, protocol: ItemId, name: &str) -> Option<usize> {
        let SigKind::Protocol {
            operations,
            summary,
            ..
        } = &self.sig(protocol).kind
        else {
            return None;
        };
        if name == "summary" && summary.is_some() {
            return Some(operations.len());
        }
        operations
            .iter()
            .position(|operation| operation.name == name)
    }

    /// Whether an operation of a protocol may be named from a module.
    pub fn operation_visible(&self, from: ModuleId, protocol: ItemId, index: usize) -> bool {
        match &self.db.ast(protocol).kind {
            ItemKind::Protocol(declared) => match declared.operations.get(index) {
                Some(operation) => self.db.field_visible(from, protocol, operation.visibility),
                None => self.db.visible(from, protocol),
            },
            _ => false,
        }
    }

    /// The operations of a protocol that enter its hash: its `pub` ones.
    fn hashed_operations(&self, protocol: ItemId) -> Vec<usize> {
        let ItemKind::Protocol(declared) = &self.db.ast(protocol).kind else {
            return Vec::new();
        };
        (0..declared.operations.len())
            .filter(|index| declared.operations[*index].visibility == Visibility::Pub)
            .collect()
    }

    /// The hash of each operation that enters a protocol's hash, by name, and the protocol hash.
    pub fn protocol_hashes(&self, protocol: ItemId) -> (Hash, Vec<(String, Hash)>) {
        let sig = self.sig(protocol);
        let SigKind::Protocol {
            operations,
            summary,
            ..
        } = &sig.kind
        else {
            return (Hash::of(b"protocol\n"), Vec::new());
        };
        let normalised = self.forms(protocol).normalised;
        let module = self.db.module_of(protocol);
        let lines: Vec<&str> = normalised.lines().collect();
        let mut hashed: Vec<(String, Hash)> = self
            .hashed_operations(protocol)
            .into_iter()
            .map(|index| {
                let operation = &operations[index].sig;
                let mut bytes = b"operation\n".to_vec();
                for line in operation_lines(&lines, &operations[index].name) {
                    bytes.extend(line.as_bytes());
                    bytes.push(b'\n');
                }
                let types = operation.params.iter().map(|param| &param.ty);
                let result = Some(&operation.result).filter(|ty| **ty != Ty::Unit);
                for ty in types.chain(result).chain(&operation.fails) {
                    bytes.extend(self.type_line(ty, module).as_bytes());
                    bytes.push(b'\n');
                }
                (operations[index].name.clone(), Hash::of(&bytes))
            })
            .collect();
        hashed.sort_by(|left, right| left.0.cmp(&right.0));
        let mut bytes = b"protocol\n".to_vec();
        if let Some(summary) = summary {
            bytes.extend(format!("summary {}\n", self.schema(summary).hash().full()).as_bytes());
        }
        for (name, hash) in &hashed {
            bytes.extend(format!("{name} {}\n", hash.full()).as_bytes());
        }
        (Hash::of(&bytes), hashed)
    }

    /// The line of an operation hash naming one of its types: its schema hash, or `handle` and
    /// its normalised text for a handle type.
    fn type_line(&self, ty: &Ty, module: ModuleId) -> String {
        match self.is_handle(ty) {
            true => format!("handle {}", self.canonical_text(ty, module)),
            false => self.schema(ty).hash().full(),
        }
    }

    /// A type's text, each item named by its canonical path, other types as `module` writes
    /// them.
    fn canonical_text(&self, ty: &Ty, module: ModuleId) -> String {
        let Ty::Item(item, args) = ty else {
            return self.show(ty, module);
        };
        let path = self.db.canonical(*item);
        if args.is_empty() {
            return path;
        }
        let args: Vec<String> = (args.iter())
            .map(|arg| match arg {
                Arg::Ty(ty) => self.canonical_text(ty, module),
                Arg::Const(Const::Int(value)) => value.to_string(),
                Arg::Const(Const::Bool(value)) => value.to_string(),
                Arg::Const(_) => "_".to_owned(),
            })
            .collect();
        format!("{path}[{}]", args.join(", "))
    }
}

/// A head without generic parameters, modifier, convention or limits.
fn bare(params: Vec<ParamSig>, result: Ty, fails: Option<Ty>, wait: bool) -> FnSig {
    FnSig {
        generics: Vec::new(),
        inherited: 0,
        params,
        result,
        fails,
        wait,
        limits: LimitSet::default(),
        modifier: None,
        convention: None,
    }
}

/// The lines of an operation in its protocol's normalised form: its head line, then the clause
/// lines below it.
fn operation_lines<'l>(lines: &[&'l str], name: &str) -> Vec<&'l str> {
    let head = |line: &str| {
        let line = line.trim_start();
        let line = line
            .strip_prefix("pub ")
            .or_else(|| line.strip_prefix("pkg "))
            .unwrap_or(line);
        line.strip_prefix("fn ")
            .and_then(|rest| rest.strip_prefix(name))
            .is_some_and(|rest| rest.starts_with('('))
    };
    let Some(start) = lines.iter().position(|line| head(line)) else {
        return Vec::new();
    };
    let mut found = vec![lines[start]];
    found.extend(
        lines[start + 1..]
            .iter()
            .take_while(|line| line.starts_with("        ")),
    );
    found
}
