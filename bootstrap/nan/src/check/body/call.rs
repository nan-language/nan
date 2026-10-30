//! Calls: what a call calls, the inference of its generic arguments, its arguments with their
//! labels, modes and markers, its failure and waiting markers, and the call forms the toolchain
//! supplies; constructions of structs, layouts and variants; indexing.

use std::rc::Rc;

use super::expr::TypeTarget;
use super::retired::Use;
use super::{Body, Written};
use crate::check::Member;
use crate::check::conform::{Candidate, type_key};
use crate::check::db::ItemId;
use crate::check::props::Property;
use crate::check::resolve::Named;
use crate::check::sig::{FnSig, ParamSig, Payload, SigKind};
use crate::check::tir::{Builtin, Call, Callee, Form, Lit, LocalKind, TArg, TExpr, TKind};
use crate::check::ty::{Arg, Args, Const, FnTy, Param, Scalar, Subst, Ty, no_args};
use crate::check::unused::FieldLabel;
use crate::diagnostic::{Code, Fix};
use crate::nir::{self, BitConversion, BitsOwner, Conversion};
use crate::syntax::ast::{
    self, ExprKind, FailureMarker, FnKind, ItemKind, Mode, Tok, TypeModifier,
};
use crate::workspace::{Arch, Authority};

/// The std functions that belong to one architecture's machine targets: its privileged
/// intrinsics other than `invalidate_page`, and the x86_64 interrupt descriptor table.
const INTRINSIC_ARCHS: [(&str, Arch); 8] = [
    ("hw::Ring0.load_idt", Arch::Amd64),
    ("hw::Ring0.write_cr3", Arch::Amd64),
    ("hw::Ring0.read_msr", Arch::Amd64),
    ("hw::Ring0.write_msr", Arch::Amd64),
    ("hw::Idt.of_vectors", Arch::Amd64),
    ("hw::Ring0.write_ttbr0", Arch::Aarch64),
    ("hw::Ring0.read_sysreg", Arch::Aarch64),
    ("hw::Ring0.write_sysreg", Arch::Aarch64),
];

/// The named conversions.
const CONVERSIONS: [&str; 6] = [
    "widen",
    "fit",
    "checked_fit",
    "truncate",
    "saturate",
    "nearest",
];

pub fn is_conversion(name: &str) -> bool {
    CONVERSIONS.contains(&name) || matches!(name, "to_bits" | "from_bits" | "checked_from_bits")
}

/// The inference of a call's generic arguments: each parameter fixed by the first part matching
/// it.
#[derive(Clone)]
pub struct Infer {
    pub params: Vec<Param>,
    pub subst: Subst,
}

impl Infer {
    pub fn new(params: Vec<Param>) -> Infer {
        Infer {
            params,
            subst: Subst::default(),
        }
    }

    fn unfixed(&self, param: Param) -> bool {
        self.params.contains(&param) && self.subst.get(param).is_none()
    }

    /// Whether a type holds a parameter not fixed yet.
    pub fn open(&self, ty: &Ty) -> bool {
        ty.mentions(&|param| self.unfixed(param))
    }

    /// Matches a type holding the call's parameters against a found type, fixing each unfixed
    /// parameter to the part at its position, a failed part included; false when a part differs.
    pub fn unify(&mut self, pattern: &Ty, found: &Ty) -> bool {
        match (pattern, found) {
            (Ty::Param(param), _) if self.params.contains(param) => {
                match self.subst.get(*param).cloned() {
                    Some(Arg::Ty(fixed)) => {
                        fixed == *found
                            || found.is_never()
                            || found.has_error()
                            || fixed.has_error()
                    }
                    Some(Arg::Const(_)) => false,
                    None => {
                        self.subst.insert(*param, Arg::Ty(found.clone()));
                        true
                    }
                }
            }
            (_, Ty::Error) | (Ty::Error, _) => true,
            (Ty::Item(left, left_args), Ty::Item(right, right_args)) => {
                left == right
                    && left_args.len() == right_args.len()
                    && left_args
                        .iter()
                        .zip(right_args.iter())
                        .all(|(left, right)| self.unify_arg(left, right))
            }
            (Ty::Fn(left), Ty::Fn(right)) => {
                if left.params.len() != right.params.len() {
                    return false;
                }
                let params = left
                    .params
                    .iter()
                    .zip(&right.params)
                    .all(|(left, right)| self.unify(&left.ty, &right.ty));
                let result = self.unify(&left.result, &right.result);
                if let (Some(left), Some(right)) = (&left.fails, &right.fails) {
                    self.unify(left, right);
                }
                params && result
            }
            (Ty::Dyn(left), Ty::Dyn(right)) => {
                left.interface == right.interface
                    && left
                        .args
                        .iter()
                        .zip(right.args.iter())
                        .all(|(left, right)| self.unify_arg(left, right))
            }
            _ => pattern == found || found.is_never() || found.has_error() || pattern.has_error(),
        }
    }

    fn unify_arg(&mut self, pattern: &Arg, found: &Arg) -> bool {
        match (pattern, found) {
            (Arg::Ty(pattern), Arg::Ty(found)) => self.unify(pattern, found),
            (Arg::Const(Const::Param(param)), Arg::Const(found)) if self.params.contains(param) => {
                match self.subst.get(*param).cloned() {
                    Some(Arg::Const(fixed)) => fixed == *found || *found == Const::Error,
                    _ => {
                        self.subst.insert(*param, Arg::Const(found.clone()));
                        true
                    }
                }
            }
            (Arg::Const(left), Arg::Const(right)) => {
                left == right || *left == Const::Error || *right == Const::Error
            }
            _ => false,
        }
    }
}

/// An operation the toolchain supplies under a std path, which no std item declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intrinsic {
    /// `text::fmt`.
    Template,
    /// `hw::mint_root`.
    MintRoot,
}

/// The markers written before a call: its failure marker, `await` and the mode marking its
/// receiver.
#[derive(Clone, Copy, Debug, Default)]
pub struct Markers {
    pub failure: Option<(FailureMarker, Tok)>,
    pub awaited: Option<Tok>,
    pub mode: Option<(Mode, Tok)>,
    /// Whether a call further out of a projection chain takes `mut self` of this call's result.
    pub chain_mut: bool,
    /// Whether the mode marks the root of the projection chain that is the receiver, which it
    /// covers, rather than the receiver.
    pub chained: bool,
    /// The last token of the receiver a method call passes for `self`.
    pub receiver: Option<Tok>,
    /// The first and last tokens of the call they mark, its markers aside.
    pub start: Option<Tok>,
    pub last: Option<Tok>,
}

/// A head being called with what is known of its generic arguments.
#[derive(Clone)]
pub struct Callable {
    callee: Callee,
    head: FnSig,
    infer: Infer,
    /// The parameters whose omitted arguments the expected type fixes first: a member path's
    /// type parameters.
    from_expected: bool,
    /// Whether the receiver is the first parenthesised argument rather than a value before `.`.
    receiver_first: bool,
}

impl Callable {
    /// A function called by its path.
    pub fn function(item: ItemId, head: FnSig) -> Callable {
        let params: Vec<Param> = head.generics.iter().map(|def| def.param).collect();
        Callable {
            callee: Callee::Function { item },
            head,
            infer: Infer::new(params),
            from_expected: false,
            receiver_first: false,
        }
    }
}

impl Body<'_, '_> {
    /// An expression with markers before it: they apply to the call that ends the postfix
    /// expression, or precede `when`.
    pub fn marked(&mut self, expr: &ast::Expr, expected: Option<&Ty>) -> TExpr {
        let ExprKind::Marked {
            failure,
            awaited,
            mode,
            inner,
        } = &expr.kind
        else {
            return self.expr(expr, expected);
        };
        if self.cx.example {
            let marker = match (failure, awaited) {
                (Some((FailureMarker::Try, token)), _) => Some(*token),
                (_, Some(token)) => Some(*token),
                _ => None,
            };
            if let Some(token) = marker {
                self.report(token, Code::ExampleForm);
            }
        }
        if let ExprKind::When { .. } = inner.kind {
            if let Some(marker @ (FailureMarker::Try, _)) = failure {
                self.unneeded_failure_marker(*marker);
            }
            let attempt = match failure {
                Some((FailureMarker::Attempt, token)) => Some(*token),
                _ => None,
            };
            return self.when(inner, attempt, expected);
        }
        if !self.is_call(inner) {
            self.report(expr.token, Code::ExprMarkerTarget);
            let typed = self.expr(inner, expected);
            return TExpr {
                token: expr.token,
                last: expr.last,
                ..typed
            };
        }
        let markers = Markers {
            failure: *failure,
            awaited: *awaited,
            mode: *mode,
            ..Markers::default()
        };
        let typed = self.call(inner, expected, Some(markers));
        TExpr {
            token: expr.token,
            last: expr.last,
            ..typed
        }
    }

    /// `old(e)`: the value of a Copy expression at entry, holding no `result` and no other `old`.
    fn old(&mut self, token: Tok, value: &ast::Expr, expected: Option<&Ty>) -> TExpr {
        if self.cx.old {
            self.report(token, Code::ContractOldForm);
        }
        let saved = self.cx;
        self.cx.old = true;
        let typed = self.expr(value, expected);
        self.cx = saved;
        if !typed.ty.has_error() && !self.ck.has(&typed.ty, Property::Copy) {
            self.report(token, Code::ContractOldForm);
        }
        TExpr {
            token,
            last: token,
            ty: typed.ty.clone(),
            kind: TKind::Old(Box::new(typed)),
        }
    }

    /// Whether an expression is a call and not a construction.
    fn is_call(&mut self, expr: &ast::Expr) -> bool {
        let ExprKind::Call { callee, .. } = &expr.kind else {
            return false;
        };
        !self.is_construction(callee)
    }

    /// Whether a callee names a struct, layout or variant, which makes its call a construction.
    fn is_construction(&mut self, callee: &ast::Expr) -> bool {
        match &callee.kind {
            ExprKind::Path { path, .. } => {
                if path.segments.len() == 1 && self.lookup_local(&path.segments[0].text).is_some() {
                    return false;
                }
                let named = self.ck.db.resolve(self.module, path).ok().or_else(|| {
                    let tombstone = self.ck.renamed_path(self.module, path)?;
                    self.ck.forwarding(tombstone)
                });
                match named {
                    Some(Named::Variant(..)) => true,
                    Some(Named::Item(item)) => matches!(
                        self.ck.db.ast(item).kind,
                        ItemKind::Struct(_) | ItemKind::Layout(_)
                    ),
                    _ => false,
                }
            }
            ExprKind::Field { base, name } => match self.type_target(base) {
                Some(TypeTarget::Type(Ty::Item(item, _))) => match &self.ck.sig(item).kind {
                    SigKind::Enum { variants } => {
                        variants.iter().any(|variant| variant.name == name.text)
                    }
                    _ => false,
                },
                _ => false,
            },
            _ => false,
        }
    }

    /// A call expression, a construction included.
    pub fn call(
        &mut self,
        expr: &ast::Expr,
        expected: Option<&Ty>,
        markers: Option<Markers>,
    ) -> TExpr {
        let ExprKind::Call { callee, args, .. } = &expr.kind else {
            return self.expr(expr, expected);
        };
        let token = expr.token;
        let markers = Markers {
            start: Some(expr.token),
            last: Some(expr.last),
            ..markers.unwrap_or_default()
        };
        let call = Use {
            token,
            args,
            expected,
            markers,
        };
        match &callee.kind {
            ExprKind::Path {
                path,
                args: generic_args,
            } => {
                match self.intrinsic(path) {
                    Some(Intrinsic::Template) => {
                        self.report(token, Code::TextTemplatePosition);
                        self.template(token, args);
                        return TExpr::failed(token);
                    }
                    Some(Intrinsic::MintRoot) => return self.mint_root(token, args, markers),
                    None => {}
                }
                if self.cx.ensures
                    && generic_args.is_none()
                    && path.segments.len() == 1
                    && path.segments[0].text == "old"
                    && let [arg] = &args[..]
                    && arg.label.is_none()
                    && !matches!(arg.value.kind, ExprKind::Marked { .. })
                {
                    return self.old(token, &arg.value, expected);
                }
                if path.segments.len() == 1
                    && generic_args.is_none()
                    && let Some(local) = self.lookup_local(&path.segments[0].text)
                {
                    self.note_use(local);
                    let value = TExpr {
                        token: callee.token,
                        last: callee.last,
                        ty: self.local(local).ty.clone(),
                        kind: TKind::Local(local),
                    };
                    return self.value_call(token, value, args, expected, markers);
                }
                let named = match self.ck.db.resolve(self.module, path) {
                    Ok(named) => named,
                    Err(error) if error.code == Some(Code::NameUnknown) => 'unknown: {
                        if generic_args.is_none()
                            && let Some(form) = self.ck.db.spelled(self.module, path)
                        {
                            let matched = self.matched(&form, None, None, call, Body::retired_head);
                            if let [tombstone] = matched[..]
                                && self.ck.renaming(&form, self.module) == Some(tombstone)
                            {
                                match self.ck.forward_path(self.item, self.module, path) {
                                    Some(named) => break 'unknown named,
                                    None => {
                                        self.skim_args(args);
                                        return TExpr::failed(token);
                                    }
                                }
                            }
                            if !matched.is_empty() {
                                return self.retired_call(callee, &matched, call);
                            }
                        }
                        let stub = match &path.segments[..] {
                            [name] if generic_args.is_none() => {
                                self.stub(&name.text, None, args, expected, markers)
                            }
                            _ => {
                                self.skim_args(args);
                                None
                            }
                        };
                        self.unknown_path(error.token, stub);
                        return TExpr::failed(token);
                    }
                    Err(error) => {
                        self.report_path(error);
                        self.skim_args(args);
                        return TExpr::failed(token);
                    }
                };
                match named {
                    Named::Item(item) => {
                        if !self.ck.db.visible(self.module, item) {
                            self.report(callee.token, Code::NamePrivate);
                        }
                        match &self.ck.db.ast(item).kind {
                            ItemKind::Fn(_) => {
                                let Some(head) = self.ck.fn_sig(item) else {
                                    return TExpr::failed(token);
                                };
                                let forbidden = match head.modifier {
                                    Some(ast::FnModifier::Interrupt) => Some(Code::KernelIrqCall),
                                    Some(ast::FnModifier::Landing) => Some(Code::HwLandingCalled),
                                    _ => None,
                                };
                                if let Some(code) = forbidden {
                                    self.report(token, code);
                                    self.skim_args(args);
                                    return TExpr::failed(token);
                                }
                                let callable = Callable::function(item, head);
                                if generic_args.is_none()
                                    && let Some(form) = self.ck.canonical_path(item)
                                {
                                    let matched = self.matched(
                                        &form,
                                        Some(&callable),
                                        None,
                                        call,
                                        Body::retired_head,
                                    );
                                    if !matched.is_empty() {
                                        return self.retired_call(callee, &matched, call);
                                    }
                                }
                                self.apply(token, callable, None, args, expected, markers)
                            }
                            ItemKind::Struct(_) | ItemKind::Layout(_) => {
                                self.no_markers(token, markers);
                                self.construct(
                                    token,
                                    item,
                                    None,
                                    generic_args.as_deref(),
                                    args,
                                    expected,
                                )
                            }
                            _ => {
                                let value = self.expr(callee, None);
                                self.value_call(token, value, args, expected, markers)
                            }
                        }
                    }
                    Named::Variant(enumeration, variant) => {
                        self.no_markers(token, markers);
                        self.construct(
                            token,
                            enumeration,
                            Some(variant),
                            generic_args.as_deref(),
                            args,
                            expected,
                        )
                    }
                    _ => {
                        self.report(callee.token, Code::NameUnknown);
                        self.skim_args(args);
                        TExpr::failed(token)
                    }
                }
            }
            ExprKind::Field { base, name } => {
                if let Some(target) = self.type_target(base) {
                    return self.member_call(token, target, base, name, args, expected, markers);
                }
                self.method_call(token, base, name, args, expected, markers)
            }
            _ => {
                let value = self.expr(callee, None);
                self.value_call(token, value, args, expected, markers)
            }
        }
    }

    fn no_markers(&self, token: Tok, markers: Markers) {
        if markers.failure.is_some() || markers.awaited.is_some() || markers.mode.is_some() {
            self.report(token, Code::ExprMarkerTarget);
        }
    }

    /// The stub a call of an unknown member `name` of a type declared in the body's module makes,
    /// `self` taking the receiver of a call on a value; none for another type.
    fn member_stub(
        &mut self,
        ty: &Ty,
        name: &ast::Name,
        receiver: Option<&TExpr>,
        args: &[ast::Arg],
        expected: Option<&Ty>,
        markers: Markers,
    ) -> Option<String> {
        let owner = match ty {
            Ty::Item(item, _) if self.ck.db.module_of(*item) == self.module => *item,
            _ => {
                self.skim_args(args);
                return None;
            }
        };
        let label = format!(
            "{}.{}",
            self.ck.db.ast(owner).name().unwrap_or_default(),
            name.text
        );
        let receiver = receiver.map(|_| markers.mode.map(|(mode, _)| mode));
        self.stub(&label, receiver, args, expected, markers)
    }

    /// The stub a call of an unknown function named `label` makes: a private function whose
    /// parameters follow the arguments, `self` first in the mode given when there is a receiver,
    /// whose result is the type expected at the call unless it is an expression statement, failing
    /// and waiting as its markers state, with a `todo` body; none when an argument's type or the
    /// expected type is not fixed, or when two parameters share a name.
    fn stub(
        &mut self,
        label: &str,
        receiver: Option<Option<Mode>>,
        args: &[ast::Arg],
        expected: Option<&Ty>,
        markers: Markers,
    ) -> Option<String> {
        let mut params: Vec<(String, String)> = Vec::new();
        if let Some(mode) = receiver {
            params.push(("self".to_owned(), format!("{}self", mode_word(mode))));
        }
        let mut fixed = true;
        for (index, arg) in args.iter().enumerate() {
            let (marker, value) = self.marked_value(&arg.value);
            let typed = self.skim(value);
            fixed &= !typed.ty.has_error() && !matches!(value.kind, ExprKind::Closure(_));
            let name = match (arg.label_name(), &value.kind) {
                (Some(label), _) => label.text.clone(),
                (None, ExprKind::Path { path, args: None })
                    if path.segments.len() == 1
                        && self.lookup_local(&path.segments[0].text).is_some() =>
                {
                    path.segments[0].text.clone()
                }
                _ if index == 0 => "value".to_owned(),
                _ => return None,
            };
            let mode = mode_word(marker.map(|(mode, _)| mode));
            let ty = self.ck.show(&typed.ty, self.module);
            params.push((name.clone(), format!("{mode}{name}: {ty}")));
        }
        let names: std::collections::BTreeSet<&str> =
            params.iter().map(|(name, _)| name.as_str()).collect();
        if !fixed || names.len() < params.len() {
            return None;
        }
        let first = [
            markers.failure.map(|(_, token)| token),
            markers.awaited,
            markers.mode.map(|(_, token)| token),
            markers.start,
        ]
        .into_iter()
        .flatten()
        .min();
        let statement = self.statement.is_some() && self.statement == first.zip(markers.last);
        let (result, fails) = match (
            statement,
            expected,
            markers.failure.map(|(marker, _)| marker),
        ) {
            (true, _, Some(FailureMarker::Try)) => (None, self.frame().failure.clone()),
            (true, _, _) => (None, None),
            (false, Some(expected), Some(FailureMarker::Attempt)) if !expected.has_error() => {
                match expected.args() {
                    [Arg::Ty(value), Arg::Ty(failure)] => {
                        (Some(value.clone()), Some(failure.clone()))
                    }
                    _ => return None,
                }
            }
            (false, Some(expected), marker) if !expected.has_error() => (
                Some(expected.clone()),
                if marker == Some(FailureMarker::Try) {
                    self.frame().failure.clone()
                } else {
                    None
                },
            ),
            _ => return None,
        };
        let mut head = format!(
            "fn {label}({})",
            params
                .iter()
                .map(|(_, param)| param.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        if let Some(result) = &result {
            head.push_str(&format!(" -> {}", self.ck.show(result, self.module)));
        }
        if let Some(fails) = &fails {
            head.push_str(&format!(" fails {}", self.ck.show(fails, self.module)));
        }
        if markers.awaited.is_some() {
            head.push_str(" uses wait");
        }
        let body = if result.is_some() {
            format!("return todo \"{label}\"")
        } else {
            format!("todo \"{label}\"")
        };
        Some(format!("{head} {{\n    {body}\n}} {label}\n"))
    }

    /// Types the arguments of a call that failed, so their own rules still apply.
    pub fn skim_args(&mut self, args: &[ast::Arg]) {
        for arg in args {
            if let ExprKind::Closure(_) = arg.value.kind {
                continue;
            }
            let (_, value) = self.marked_value(&arg.value);
            self.skim(value);
        }
    }

    /// A whole argument or construction value split into the mode written before it and the
    /// expression it marks; a mode before a method call marks that call's receiver instead.
    fn marked_value<'e>(&self, value: &'e ast::Expr) -> (Option<(Mode, Tok)>, &'e ast::Expr) {
        match &value.kind {
            ExprKind::Marked {
                failure: None,
                awaited: None,
                mode: Some(mode),
                inner,
            } if !self.is_method_call(inner) => (Some(*mode), &**inner),
            _ => (None, value),
        }
    }

    /// The intrinsic a path names through its std alias.
    pub fn intrinsic(&self, path: &ast::Path) -> Option<Intrinsic> {
        let [alias, name] = &path.segments[..] else {
            return None;
        };
        let db = &self.ck.db;
        let package = db.module(self.module).package;
        let own = db.std.get(alias.text.as_str()) == Some(&package);
        if !own && db.module_named(package, &alias.text).is_some() {
            return None;
        }
        let aliased = db.alias(package, &alias.text)?;
        let intrinsic = match (alias.text.as_str(), name.text.as_str()) {
            ("text", "fmt") => Intrinsic::Template,
            ("hw", "mint_root") => Intrinsic::MintRoot,
            _ => return None,
        };
        (db.std.get(alias.text.as_str()) == Some(&aliased)).then_some(intrinsic)
    }

    /// A whole argument or receiver of a call, the one position where a template call stands.
    pub fn operand(&mut self, value: &ast::Expr, expected: Option<&Ty>) -> TExpr {
        if let ExprKind::Call { callee, args, .. } = &value.kind
            && let ExprKind::Path { path, args: None } = &callee.kind
            && self.intrinsic(path) == Some(Intrinsic::Template)
        {
            return self.template(value.token, args);
        }
        self.expr(value, expected)
    }

    /// A value written with `mut`: a place whose indexing calls `at_mut` when it writes through
    /// an index, as the place of a `mut` access, or else an operand.
    pub fn mut_operand(&mut self, value: &ast::Expr, expected: Option<&Ty>) -> TExpr {
        if !through_index(value) {
            return self.operand(value, expected);
        }
        self.place(value, false)
            .unwrap_or_else(|| TExpr::failed(value.token))
    }

    /// `hw::mint_root()`: the target's root grant, minted once by its platform package's entry
    /// landing fn, outside every loop and closure.
    fn mint_root(&mut self, token: Tok, args: &[ast::Arg], markers: Markers) -> TExpr {
        self.builtin_markers(markers);
        if !args.is_empty() {
            self.report(token, Code::ExprArgumentCount);
            self.skim_args(args);
            return TExpr::failed(token);
        }
        let landing = matches!(&self.ck.db.ast(self.item).kind,
            ItemKind::Fn(function) if function.modifier.is_some_and(|(modifier, _)| modifier == ast::FnModifier::Landing));
        let legal = landing
            && self.ck.db.is_std(self.item)
            && self.loops.is_empty()
            && self.frames.len() == 1
            && !std::mem::replace(&mut self.minted, true);
        if !legal {
            self.report(token, Code::HwMintRootMisplaced);
        }
        let grant = self
            .ck
            .db
            .target
            .root_grant()
            .and_then(|(module, name)| self.ck.db.std_item(module, name));
        let ty = grant.map_or(Ty::Error, |grant| Ty::item(grant, Vec::new()));
        TExpr {
            token,
            last: token,
            ty: ty.clone(),
            kind: TKind::Call(Box::new(Call {
                token,
                callee: Callee::Builtin(Builtin::MintRoot),
                type_args: no_args(),
                args: Vec::new(),
                head: plain_head(ty),
                proj: false,
                failure: None,
                awaited: false,
            })),
        }
    }

    /// Reports a failure marker a call does not need, with the fix deleting a `try`.
    fn unneeded_failure_marker(&self, (marker, token): (FailureMarker, Tok)) {
        let fixes = match marker {
            FailureMarker::Try => vec![self.ck.fix_deleting_forward(self.item, token, token)],
            FailureMarker::Attempt => Vec::new(),
        };
        self.report_fixed((token, token), Code::ErrorMarkerUnneeded, fixes);
    }

    /// Reports an `await` a call does not need, with the fix deleting it.
    fn unneeded_await(&self, token: Tok) {
        let fix = self.ck.fix_deleting_forward(self.item, token, token);
        self.report_fixed((token, token), Code::EffectAwaitUnneeded, vec![fix]);
    }

    /// The markers before a call of a toolchain operation, which never fails, waits or takes a
    /// receiver.
    fn builtin_markers(&self, markers: Markers) {
        if let Some(marker) = markers.failure {
            self.unneeded_failure_marker(marker);
        }
        if let Some(marker) = markers.awaited {
            self.unneeded_await(marker);
        }
        if let Some((_, marker)) = markers.mode {
            self.report(marker, Code::ExprMarkerTarget);
        }
    }

    /// A call through a function value.
    fn value_call(
        &mut self,
        token: Tok,
        value: TExpr,
        args: &[ast::Arg],
        expected: Option<&Ty>,
        markers: Markers,
    ) -> TExpr {
        let fn_ty = match &value.ty {
            Ty::Fn(fn_ty) => (**fn_ty).clone(),
            Ty::Error => {
                self.skim_args(args);
                return TExpr::failed(token);
            }
            _ => {
                self.report(token, Code::NameUnknown);
                self.skim_args(args);
                return TExpr::failed(token);
            }
        };
        match fn_ty.kind {
            FnKind::Task => {
                self.report(token, Code::TaskTaskFnCall);
                self.skim_args(args);
                return TExpr::failed(token);
            }
            FnKind::Interrupt => {
                self.report(token, Code::KernelIrqCall);
                self.skim_args(args);
                return TExpr::failed(token);
            }
            FnKind::Plain => {}
        }
        let own_param = matches!(&value.kind, TKind::Local(local)
            if matches!(self.local(*local).kind, LocalKind::Param(_)) && self.local(*local).closure == self.frame().closure);
        let head = FnSig {
            generics: Vec::new(),
            inherited: 0,
            params: fn_ty
                .params
                .iter()
                .map(|param| ParamSig {
                    name: param.name.clone(),
                    mode: param.mode,
                    ty: param.ty.clone(),
                    token,
                    is_self: false,
                })
                .collect(),
            result: fn_ty.result.clone(),
            fails: fn_ty.fails.clone(),
            wait: fn_ty.wait,
            limits: fn_ty.limits,
            modifier: None,
            convention: fn_ty.convention.clone(),
        };
        let callable = Callable {
            callee: Callee::Value(Box::new(value)),
            head,
            infer: Infer::new(Vec::new()),
            from_expected: false,
            receiver_first: false,
        };
        self.apply_with(token, callable, None, args, expected, markers, own_param)
    }

    /// `T.name(...)`, `I.name(...)` or `module.name(...)`: a construction, an associated function
    /// or method called through its type, a qualified interface call, or a built-in member.
    #[allow(clippy::too_many_arguments)]
    pub fn member_call(
        &mut self,
        token: Tok,
        target: TypeTarget,
        base: &ast::Expr,
        name: &ast::Name,
        args: &[ast::Arg],
        expected: Option<&Ty>,
        markers: Markers,
    ) -> TExpr {
        let ty = match target {
            TypeTarget::Module => {
                self.dot_for_path(name);
                self.skim_args(args);
                return TExpr::failed(token);
            }
            TypeTarget::Failed => {
                self.skim_args(args);
                return TExpr::failed(token);
            }
            TypeTarget::Interface(interface) => {
                return self.qualified_call(token, interface, base, name, args, expected, markers);
            }
            TypeTarget::Type(ty) => ty,
        };
        let written_args = matches!(&base.kind, ExprKind::Path { args: Some(_), .. });
        if ty.has_error() {
            self.skim_args(args);
            return TExpr::failed(token);
        }
        if let Ty::Item(item, _) = &ty {
            let item = *item;
            if self.core_variant_qualified(item, &name.text) {
                self.qualified_core_variant(base.token, name);
                self.skim_args(args);
                return TExpr::failed(token);
            }
            if let SigKind::Enum { variants } = &self.ck.sig(item).kind
                && let Some(variant) = variants
                    .iter()
                    .position(|variant| variant.name == name.text)
            {
                self.name_member(name.token, Member::Variant(item, name.text.clone()));
                self.no_markers(token, markers);
                let generic_args = match &base.kind {
                    ExprKind::Path { args, .. } => args.clone(),
                    _ => None,
                };
                return self.construct(
                    token,
                    item,
                    Some(variant),
                    generic_args.as_deref(),
                    args,
                    expected,
                );
            }
        }
        if let Some(scalar) = ty.scalar()
            && is_conversion(&name.text)
        {
            return self.conversion(token, scalar, name, args, markers);
        }
        if let Ty::Dyn(bound) = &ty
            && matches!(name.text.as_str(), "view" | "view_mut")
        {
            return self.dyn_view(token, (**bound).clone(), name, args, markers);
        }
        if let Some(Ty::Dyn(bound)) = self.ck.boxed_dyn(&ty)
            && name.text == "from_box"
        {
            return self.boxed_dyn_call(token, &ty, Ty::Dyn(bound), args, markers);
        }
        if let Ty::Item(item, _) = &ty
            && let ItemKind::Layout(_) = self.ck.db.ast(*item).kind
            && matches!(
                name.text.as_str(),
                "to_bits" | "from_bits" | "checked_from_bits"
            )
        {
            return self.layout_bits(token, &ty, name, args, markers);
        }
        let candidates = self.ck.method_candidates(&ty, &name.text);
        let call = Use {
            token,
            args,
            expected,
            markers,
        };
        if candidates.is_empty()
            && let Some(form) = self.member_form(&ty, &name.text)
        {
            let matched = self.matched(&form, None, None, call, |body, tombstone| {
                let candidate = body.retired_candidate(tombstone, &ty);
                body.member_callable(candidate, &ty, written_args)
            });
            if !matched.is_empty() {
                return self.retired_member(ty, base, name, written_args, &matched, call);
            }
        }
        let requalified = |interfaces: Vec<ItemId>| -> Vec<Fix> {
            interfaces
                .into_iter()
                .map(|interface| {
                    let written = self.ck.item_written(interface, self.module);
                    self.ck
                        .fix_replacing(self.item, base.token, base.last, &written)
                })
                .collect()
        };
        let candidate = match candidates.len() {
            0 => {
                let interfaces = self.unqualified(&ty, &name.text);
                let construct = (base.token, name.token);
                let written = self.ck.written(self.item, base.token, name.token);
                if interfaces.is_empty()
                    && self.ck.report_newer_release(self.item, construct, &written)
                {
                    self.skim_args(args);
                } else if interfaces.is_empty() {
                    let stub = self.member_stub(&ty, name, None, args, expected, markers);
                    self.unknown_member(name, &ty, stub);
                } else {
                    let fixes = requalified(interfaces);
                    self.report_fixed(
                        (name.token, name.token),
                        Code::InterfaceUnqualifiedCall,
                        fixes,
                    );
                    self.skim_args(args);
                }
                return TExpr::failed(token);
            }
            1 => candidates.into_iter().next().expect("one candidate"),
            _ => {
                let interfaces = candidates
                    .iter()
                    .filter_map(|candidate| self.candidate_interface(candidate))
                    .collect();
                let fixes = requalified(interfaces);
                self.report_fixed(
                    (name.token, name.token),
                    Code::InterfaceAmbiguousMethod,
                    fixes,
                );
                self.skim_args(args);
                return TExpr::failed(token);
            }
        };
        self.name_candidate(name.token, &candidate, &name.text);
        let Some(callable) = self.member_callable(candidate, &ty, written_args) else {
            self.skim_args(args);
            return TExpr::failed(token);
        };
        if let Some(form) = self.member_form(&ty, &name.text) {
            let matched = self.matched(&form, Some(&callable), None, call, |body, tombstone| {
                let candidate = body.retired_candidate(tombstone, &ty);
                body.member_callable(candidate, &ty, written_args)
            });
            if !matched.is_empty() {
                return self.retired_member(ty, base, name, written_args, &matched, call);
            }
        }
        self.apply(token, callable, None, args, expected, markers)
    }

    /// The callable a candidate stands for in a call naming it through its type, the receiver
    /// passed first; `written_args` when the type path writes the type's arguments, which are
    /// otherwise inferred.
    pub fn member_callable(
        &mut self,
        candidate: Candidate,
        ty: &Ty,
        written_args: bool,
    ) -> Option<Callable> {
        let mut callable = self.callable(candidate, ty)?;
        callable.receiver_first = true;
        if !written_args && let Some(key) = type_key(ty) {
            let type_params: Vec<Param> = self
                .ck
                .type_generics(key)
                .iter()
                .map(|def| def.param)
                .collect();
            callable.infer.params.extend(type_params.iter().copied());
            for param in type_params {
                callable.infer.subst.0.remove(&param);
            }
            callable.from_expected = true;
        }
        Some(callable)
    }

    /// The callable a method resolution candidate stands for, its type's arguments substituted.
    pub fn callable(&mut self, candidate: Candidate, self_ty: &Ty) -> Option<Callable> {
        match candidate {
            Candidate::Function { item, subst } => {
                let head = self.ck.fn_sig(item)?;
                let own: Vec<Param> = head.own_generics().iter().map(|def| def.param).collect();
                let mut infer = Infer::new(own);
                infer.subst = subst.clone();
                Some(Callable {
                    callee: Callee::Function { item },
                    head,
                    infer,
                    from_expected: false,
                    receiver_first: false,
                })
            }
            Candidate::Method {
                bound,
                method,
                self_ty: method_self,
                function,
                dynamic,
            } => {
                if let Some((item, subst)) = function
                    && !dynamic
                {
                    let head = self.ck.fn_sig(item)?;
                    let own: Vec<Param> = head.own_generics().iter().map(|def| def.param).collect();
                    let mut infer = Infer::new(own);
                    let type_subst = type_key(self_ty)
                        .map(|key| self.ck.type_subst(key, self_ty))
                        .unwrap_or_default();
                    infer.subst = Subst(type_subst.0.into_iter().chain(subst.0).collect());
                    return Some(Callable {
                        callee: Callee::Function { item },
                        head,
                        infer,
                        from_expected: false,
                        receiver_first: false,
                    });
                }
                let head = self.ck.method_head(&bound, method, &method_self)?;
                let own: Vec<Param> = head.generics.iter().map(|def| def.param).collect();
                Some(Callable {
                    callee: Callee::Method {
                        interface: bound,
                        method,
                        self_ty: method_self,
                        dynamic,
                    },
                    head,
                    infer: Infer::new(own),
                    from_expected: false,
                    receiver_first: false,
                })
            }
        }
    }

    /// Whether a method of that name belongs to an interface the type implements but method
    /// resolution does not consider.
    fn unqualified(&self, ty: &Ty, name: &str) -> Vec<ItemId> {
        let Some(key) = type_key(ty) else {
            return Vec::new();
        };
        let mut interfaces: Vec<ItemId> = self
            .ck
            .implemented_interfaces(ty, key)
            .into_iter()
            .filter(|interface| self.ck.interface_method(*interface, name).is_some())
            .collect();
        self.ck.db.about(None, || {
            for (index, data) in self.ck.db.items.iter().enumerate() {
                let interface = ItemId(index as u32);
                if matches!(data.ast.kind, ItemKind::Interface(_))
                    && self.ck.interface_method(interface, name).is_some()
                    && !interfaces.contains(&interface)
                    && self.ck.implements(ty, interface)
                {
                    interfaces.push(interface);
                }
            }
        });
        interfaces
    }

    /// The interface a method candidate's function belongs to.
    fn candidate_interface(&self, candidate: &Candidate) -> Option<ItemId> {
        match candidate {
            Candidate::Method { bound, .. } => Some(bound.interface),
            Candidate::Function { item, .. } => self
                .ck
                .implemented_lines(*item)
                .into_iter()
                .find_map(|line| match &self.ck.sig(line).kind {
                    SigKind::Impl {
                        interface: Some(bound),
                        ..
                    } => Some(bound.interface),
                    _ => None,
                }),
        }
    }

    /// The fixes writing a method call `x.f(...)`, its receiver starting at `receiver`, as the
    /// qualified call `I.f(x, ...)` of each interface, the receiver's mode marker kept on its
    /// argument.
    fn qualified_calls(
        &self,
        interfaces: Vec<ItemId>,
        receiver: Tok,
        name: &ast::Name,
        markers: Markers,
    ) -> Vec<Fix> {
        let (Some(receiver_last), Some(last)) = (markers.receiver, markers.last) else {
            return Vec::new();
        };
        let syntax = &self.ck.db.item(self.item).file.syntax;
        let start = markers.mode.map_or(receiver, |(_, marker)| marker);
        let written = self.ck.written(self.item, start, receiver_last);
        let open = syntax.next(name.token as usize + 1) as Tok;
        let arguments = if open + 1 < last {
            format!(", {}", self.ck.written(self.item, open + 1, last - 1))
        } else {
            String::new()
        };
        interfaces
            .into_iter()
            .map(|interface| {
                let qualifier = self.ck.item_written(interface, self.module);
                let call = format!("{qualifier}.{}({written}{arguments})", name.text);
                self.ck.fix_replacing(self.item, start, last, &call)
            })
            .collect()
    }

    /// `x.name(...)`: a method call, the receiver passed for `self`.
    fn method_call(
        &mut self,
        token: Tok,
        base: &ast::Expr,
        name: &ast::Name,
        args: &[ast::Arg],
        expected: Option<&Ty>,
        markers: Markers,
    ) -> TExpr {
        let receiver_markers = Markers {
            mode: markers.mode,
            chain_mut: false,
            ..Markers::default()
        };
        let receiver = match &base.kind {
            ExprKind::Call { callee, .. }
                if markers.mode.is_some()
                    && matches!(callee.kind, ExprKind::Field { .. })
                    && !self.is_construction(callee) =>
            {
                let chain = Markers {
                    chain_mut: true,
                    ..receiver_markers
                };
                self.call(base, None, Some(chain))
            }
            _ if matches!(markers.mode, Some((Mode::Mut, _))) => self.mut_operand(base, None),
            _ => self.operand(base, None),
        };
        if receiver.ty.has_error() {
            self.skim_args(args);
            return TExpr::failed(token);
        }
        self.method_on(token, receiver, name, args, expected, markers)
    }

    /// A method call on a typed receiver.
    pub fn method_on(
        &mut self,
        token: Tok,
        receiver: TExpr,
        name: &ast::Name,
        args: &[ast::Arg],
        expected: Option<&Ty>,
        markers: Markers,
    ) -> TExpr {
        let syntax = &self.ck.db.item(self.item).file.syntax;
        let dot = syntax
            .previous(name.token as usize)
            .unwrap_or(name.token as usize);
        let markers = Markers {
            receiver: syntax.previous(dot).map(|last| last as Tok),
            ..markers
        };
        if (self.ck.is_std(&receiver.ty, "core::Str")
            || self.ck.is_std(&receiver.ty, "text::String")
            || self.ck.is_std(&receiver.ty, "text::Fixed"))
            && name.text == "len"
        {
            let fix = self
                .ck
                .fix_replacing(self.item, name.token, name.token, "byte_len");
            self.report_fixed((name.token, name.token), Code::TextBareLen, vec![fix]);
            self.skim_args(args);
            return TExpr::failed(token);
        }
        if name.text == "to_bits"
            && let Some(bits) = self.bits_of(&receiver.ty)
        {
            return self.bits_call(token, receiver, bits, args, markers);
        }
        if let Some(callable) = self.dma_access(&receiver.ty, &name.text) {
            return self.apply_dma(token, receiver, callable, args, markers);
        }
        let candidates = self.ck.method_candidates(&receiver.ty, &name.text);
        let form = self.member_form(&receiver.ty, &name.text);
        let call = Use {
            token,
            args,
            expected,
            markers,
        };
        let candidate = match candidates.len() {
            0 => {
                if let Some(form) = &form {
                    let ty = receiver.ty.clone();
                    let matched =
                        self.matched(form, None, Some(&receiver), call, |body, tombstone| {
                            let candidate = body.retired_candidate(tombstone, &ty);
                            body.callable(candidate, &ty)
                        });
                    if !matched.is_empty() {
                        return self.retired_method(receiver, name, &matched, call);
                    }
                }
                let field = match &receiver.ty {
                    Ty::Item(item, _) => match &self.ck.sig(*item).kind {
                        SigKind::Struct { fields } => {
                            fields.iter().any(|field| field.name == name.text)
                        }
                        _ => false,
                    },
                    _ => false,
                };
                let interfaces = self.unqualified(&receiver.ty, &name.text);
                let start = markers.mode.map_or(receiver.token, |(_, marker)| marker);
                let construct = (start, markers.last.unwrap_or(name.token));
                if field {
                    let fix = self
                        .ck
                        .fix_inserting(self.item, start, "(")
                        .and(self.ck.fix_appending(self.item, name.token, ")"));
                    self.report_fixed(construct, Code::ExprFieldCall, vec![fix]);
                } else if !interfaces.is_empty() {
                    let fixes = self.qualified_calls(interfaces, receiver.token, name, markers);
                    self.report_fixed(construct, Code::InterfaceUnqualifiedCall, fixes);
                } else {
                    let ty = receiver.ty.clone();
                    let stub =
                        self.member_stub(&ty, name, Some(&receiver), args, expected, markers);
                    self.unknown_member(name, &ty, stub);
                    return TExpr::failed(token);
                }
                self.skim_args(args);
                return TExpr::failed(token);
            }
            1 => candidates.into_iter().next().expect("one candidate"),
            _ => {
                let interfaces = candidates
                    .iter()
                    .filter_map(|candidate| self.candidate_interface(candidate))
                    .collect();
                let fixes = self.qualified_calls(interfaces, receiver.token, name, markers);
                let start = markers.mode.map_or(receiver.token, |(_, marker)| marker);
                let construct = (start, markers.last.unwrap_or(name.token));
                self.report_fixed(construct, Code::InterfaceAmbiguousMethod, fixes);
                self.skim_args(args);
                return TExpr::failed(token);
            }
        };
        self.name_candidate(name.token, &candidate, &name.text);
        let Some(callable) = self.callable(candidate, &receiver.ty.clone()) else {
            self.skim_args(args);
            return TExpr::failed(token);
        };
        if !callable.head.has_self() {
            self.report(name.token, Code::NameUnknown);
            self.skim_args(args);
            return TExpr::failed(token);
        }
        let projected = matches!(&receiver.kind, TKind::Call(call) if call.proj);
        let markers = Markers {
            mode: markers.mode.filter(|_| !projected),
            chained: projected && markers.mode.is_some(),
            ..markers
        };
        if let Some(form) = &form {
            let call = Use { markers, ..call };
            let ty = receiver.ty.clone();
            let live = Some(&callable);
            let matched = self.matched(form, live, Some(&receiver), call, |body, tombstone| {
                let candidate = body.retired_candidate(tombstone, &ty);
                body.callable(candidate, &ty)
            });
            if !matched.is_empty() {
                return self.retired_method(receiver, name, &matched, call);
            }
        }
        self.apply(token, callable, Some(receiver), args, expected, markers)
    }

    /// `I.name(x, ...)`: a qualified call of an interface's method.
    #[allow(clippy::too_many_arguments)]
    fn qualified_call(
        &mut self,
        token: Tok,
        interface: ItemId,
        base: &ast::Expr,
        name: &ast::Name,
        args: &[ast::Arg],
        expected: Option<&Ty>,
        markers: Markers,
    ) -> TExpr {
        let _ = base;
        let Some(method) = self.ck.interface_method(interface, &name.text) else {
            self.report(name.token, Code::NameUnknown);
            self.skim_args(args);
            return TExpr::failed(token);
        };
        let has_self = match &self.ck.sig(interface).kind {
            SigKind::Interface { methods } => methods[method].sig.has_self(),
            _ => false,
        };
        let self_ty = if has_self {
            match args.first() {
                Some(first) => {
                    let typed = self.expr(&first.value, None);
                    typed.ty
                }
                None => Ty::Error,
            }
        } else {
            expected.cloned().unwrap_or(Ty::Error)
        };
        let args_bound = match self.ck.conformance(&self_ty, interface) {
            Some((_, args)) => args,
            None if matches!(self_ty, Ty::Dyn(_)) || self.ck.boxed_dyn(&self_ty).is_some() => {
                crate::check::ty::no_args()
            }
            None => {
                if !self_ty.has_error() {
                    self.report(token, Code::InterfaceNotImplemented);
                }
                self.skim_args(args);
                return TExpr::failed(token);
            }
        };
        let bound = crate::check::ty::Bound {
            interface,
            args: args_bound,
        };
        let Some(head) = self.ck.method_head(&bound, method, &self_ty) else {
            return TExpr::failed(token);
        };
        let own: Vec<Param> = head.generics.iter().map(|def| def.param).collect();
        let callable = Callable {
            callee: Callee::Method {
                interface: bound,
                method,
                self_ty: self_ty.clone(),
                dynamic: matches!(self_ty, Ty::Dyn(_)),
            },
            head,
            infer: Infer::new(own),
            from_expected: false,
            receiver_first: true,
        };
        self.apply(token, callable, None, args, expected, markers)
    }

    pub fn apply(
        &mut self,
        token: Tok,
        callable: Callable,
        receiver: Option<TExpr>,
        args: &[ast::Arg],
        expected: Option<&Ty>,
        markers: Markers,
    ) -> TExpr {
        self.apply_with(token, callable, receiver, args, expected, markers, false)
    }

    /// Checks a call's arguments against its head: their count, labels, inference, modes and
    /// markers; then its failure and waiting markers. `own_param` when the callee is the
    /// enclosing function's own parameter, through which it waits.
    #[allow(clippy::too_many_arguments)]
    fn apply_with(
        &mut self,
        token: Tok,
        mut callable: Callable,
        receiver: Option<TExpr>,
        args: &[ast::Arg],
        expected: Option<&Ty>,
        markers: Markers,
        own_param: bool,
    ) -> TExpr {
        let head = callable.head.clone();
        let params = &head.params;
        let offset = usize::from(receiver.is_some());
        if args.len() + offset != params.len() {
            self.report(token, Code::ExprArgumentCount);
            self.skim_args(args);
            return TExpr::failed(token);
        }
        let self_first = callable.receiver_first && head.has_self();
        self.check_labels(&params[offset..], args, self_first);
        if callable.from_expected
            && let Some(expected) = expected
        {
            let result = head.result.clone();
            callable.infer.unify(&result, expected);
        }
        let mut typed_args: Vec<TArg> = Vec::new();
        let mut waits_through_args = false;
        let mut all_own = true;
        if let Some(receiver) = receiver {
            let param = &params[0];
            let pattern = callable.infer.subst.ty(&param.ty);
            if !callable.infer.unify(&pattern, &receiver.ty) && !receiver.ty.has_error() {
                self.report_mismatch(
                    receiver.token,
                    &callable.infer.subst.ty(&param.ty),
                    &receiver.ty,
                );
            }
            if !markers.chained {
                self.check_mode(
                    (receiver.token, markers.receiver.unwrap_or(receiver.token)),
                    &receiver,
                    param.mode,
                    markers.mode,
                    markers.chain_mut,
                    true,
                );
            }
            typed_args.push(TArg {
                expr: receiver,
                mode: param.mode,
            });
        }
        for (arg, param) in args.iter().zip(&params[offset..]) {
            let pattern = callable.infer.subst.ty(&param.ty);
            let (marker, value) = self.marked_value(&arg.value);
            let typed = match (&value.kind, &pattern) {
                (ExprKind::Closure(closure), Ty::Fn(wanted)) => {
                    let wanted = (**wanted).clone();
                    let written = self.written_as(&callable, &typed_args, &param.name);
                    let typed = self.closure(value.token, closure, Some(&wanted), written);
                    if let Ty::Fn(found) = &typed.ty {
                        callable.infer.unify(&pattern, &Ty::Fn(found.clone()));
                        let fixed = callable.infer.subst.fn_ty(&wanted);
                        self.fit(found, &fixed, value.token);
                        if found.wait && wanted.kind != FnKind::Task {
                            waits_through_args = true;
                            all_own = false;
                        }
                    }
                    typed
                }
                (ExprKind::Closure(_), _) => {
                    self.report(value.token, Code::ClosureSecondClass);
                    TExpr::failed(value.token)
                }
                _ => {
                    let hint = (!callable.infer.open(&pattern)).then_some(&pattern);
                    let typed = match marker {
                        Some((Mode::Mut, _)) => self.mut_operand(value, hint),
                        _ => self.operand(value, hint),
                    };
                    if let (Ty::Fn(found), Ty::Fn(wanted)) = (&typed.ty, &pattern) {
                        if let TKind::FnValue { item: function, .. } = typed.kind
                            && wanted.kind == FnKind::Task
                        {
                            self.ck.check_stack_bound(function, &found.limits);
                        }
                        callable.infer.unify(&pattern, &typed.ty);
                        let fixed = callable.infer.subst.fn_ty(wanted);
                        self.fit(found, &fixed, value.token);
                        if found.wait && wanted.kind != FnKind::Task {
                            waits_through_args = true;
                            let is_own = matches!(&typed.kind, TKind::Local(local)
                                if matches!(self.local(*local).kind, LocalKind::Param(_)));
                            all_own &= is_own;
                        }
                    } else if !callable.infer.unify(&pattern, &typed.ty) {
                        let shown = callable.infer.subst.ty(&param.ty);
                        self.expect(&typed, &shown, value.token);
                    }
                    typed
                }
            };
            self.check_mode(
                (value.token, value.last),
                &typed,
                param.mode,
                marker,
                false,
                false,
            );
            typed_args.push(TArg {
                expr: typed,
                mode: param.mode,
            });
        }
        self.fix_failures(&mut callable);
        let unfixed: Vec<Param> = callable
            .infer
            .params
            .iter()
            .copied()
            .filter(|param| callable.infer.subst.get(*param).is_none())
            .collect();
        if !unfixed.is_empty() {
            self.report(token, Code::GenericUninferred);
            return TExpr::failed(token);
        }
        self.check_call_bounds(token, &callable, &head);
        self.check_callee(token, &callable);
        self.check_volatile(token, &callable, &typed_args);
        self.check_spawn(token, &callable, &typed_args);
        self.check_lexical_spawns(&callable, &typed_args);
        let subst = callable.infer.subst.clone();
        let instantiated = subst.fn_ty(&head.fn_ty());
        let failure = instantiated.fails.clone().filter(|ty| *ty != Ty::Never);
        let may_wait = instantiated.wait || waits_through_args;
        let only_own = own_param || (!instantiated.wait && all_own);
        let mut result = instantiated.result.clone();
        let first = markers
            .failure
            .map(|(_, marker)| marker)
            .or(markers.awaited)
            .or(markers.mode.map(|(_, marker)| marker))
            .unwrap_or(token);
        let construct = (first, markers.last.unwrap_or(token));
        match markers.awaited {
            Some(await_token) if !may_wait => self.unneeded_await(await_token),
            None if may_wait => {
                let before = markers.mode.map_or(token, |(_, marker)| marker);
                let fix = self.ck.fix_inserting(self.item, before, "await ");
                self.report_fixed(construct, Code::EffectAwaitMissing, vec![fix]);
            }
            _ => {}
        }
        if may_wait && !only_own {
            self.frames.last_mut().expect("a frame").may_wait = true;
        }
        if may_wait && self.cx.when {
            self.report(token, Code::TaskWhenWait);
        }
        match (markers.failure, &failure) {
            (Some(marker), None) => self.unneeded_failure_marker(marker),
            (None, Some(failure)) => {
                let converts = self
                    .frame()
                    .failure
                    .clone()
                    .is_some_and(|frame_failure| self.ck.converts(failure, &frame_failure));
                let fixes = if converts {
                    vec![self.ck.fix_inserting(self.item, first, "try ")]
                } else {
                    Vec::new()
                };
                self.report_fixed(construct, Code::ErrorUnhandled, fixes);
            }
            (Some((FailureMarker::Try, marker)), Some(failure)) => {
                let frame_failure = self.frame().failure.clone();
                let test = self.frame().test;
                if let (Some(frame_failure), false) = (&frame_failure, test)
                    && !self.ck.converts(failure, frame_failure)
                {
                    let fixes = self
                        .ck
                        .variant_added_fix(failure, frame_failure)
                        .into_iter()
                        .collect();
                    self.report_fixed((marker, marker), Code::ErrorUnconvertible, fixes);
                }
            }
            (Some((FailureMarker::Attempt, _)), Some(failure)) => {
                result = self.ck.result_ty(result, failure.clone());
            }
            (None, None) => {}
        }
        if let Some(failure) = &failure
            && !matches!(markers.failure, Some((FailureMarker::Attempt, _)))
        {
            self.frames.last_mut().expect("a frame").raise(failure);
        }
        let type_args = type_args(&callable.callee, &head, &subst);
        let callee = callable.callee;
        TExpr {
            token,
            last: token,
            ty: result,
            kind: TKind::Call(Box::new(Call {
                token,
                callee,
                type_args,
                args: typed_args,
                head: instantiated,
                proj: head.is_proj(),
                failure: markers.failure,
                awaited: markers.awaited.is_some(),
            })),
        }
    }

    /// Fixes the failure parameters no argument fixed: `Never`, or `task::Cancelled` for one the
    /// head requires cancellable.
    fn fix_failures(&self, callable: &mut Callable) {
        let failure_params: Vec<Param> = callable
            .infer
            .params
            .iter()
            .copied()
            .filter(|param| callable.infer.subst.get(*param).is_none())
            .filter(|param| self.ck.implied(*param).failure)
            .collect();
        for param in failure_params {
            let ty = if self.ck.implied(param).cancellable {
                match self.ck.db.std_item("task", "Cancelled") {
                    Some(cancelled) => Ty::item(cancelled, Vec::new()),
                    None => Ty::Never,
                }
            } else {
                Ty::Never
            };
            callable.infer.subst.insert(param, Arg::Ty(ty));
        }
    }

    /// The rules a call obeys for what it calls: the grant its operation needs, the calls of
    /// validators, and where `inspect` and `Untrusted.of` stand.
    fn check_callee(&self, token: Tok, callable: &Callable) {
        let subst = &callable.infer.subst;
        let Callee::Function { item, .. } = &callable.callee else {
            return;
        };
        let item = *item;
        if self.ck.is_trusted_primitive(item) && !self.cx.trusted {
            self.report(token, Code::TrustedPrimitiveOutside);
        }
        if let Some(authority) = self.ck.call_authority(item) {
            self.ck.require(self.item, token, authority);
        }
        self.ck.check_validator_use(self.item, token, item);
        if !self.ck.db.is_std(item) {
            return;
        }
        let path = self.ck.db.canonical(item);
        let wrapped = callable
            .head
            .generics
            .first()
            .and_then(|def| subst.get(def.param));
        if path
            .strip_prefix("hw::Ptr.")
            .is_some_and(|member| member.starts_with("atomic_"))
        {
            let integer = matches!(
                wrapped,
                Some(Arg::Ty(Ty::Scalar(
                    Scalar::U8 | Scalar::U16 | Scalar::U32 | Scalar::U64 | Scalar::Usize
                )))
            );
            if !integer && !matches!(wrapped, Some(Arg::Ty(Ty::Error))) {
                self.report(token, Code::HwAtomicType);
            }
        }
        match path.as_str() {
            "hw::Anchor.with" if !self.ck.is_entry_context(self.item) => {
                self.report(token, Code::KernelAnchorOutsideEntry);
            }
            "sys::Domain.run" | "sys::Domain.run_sized" if self.ck.db.target.is_machine() => {
                self.report(token, Code::DomainUnavailable);
            }
            "hw::Ports.map" if self.ck.db.target.arch() == Some(Arch::Aarch64) => {
                self.report(token, Code::RegisterPortTarget);
            }
            _ if INTRINSIC_ARCHS.iter().any(|(intrinsic, arch)| {
                *intrinsic == path && self.ck.db.target.arch().is_some_and(|of| of != *arch)
            }) =>
            {
                self.report(token, Code::HwIntrinsicTarget);
            }
            "host::Runner.run" => {
                if let Some(code) = self.ck.runner_problem(&callable.head, subst) {
                    self.report(token, code);
                }
            }
            "core::Untrusted.inspect" if !self.inspecting() => {
                self.report(token, Code::UntrustedInspectOutsideValidator);
            }
            "core::Untrusted.of" => {
                if let Some(Arg::Ty(wrapped)) = wrapped
                    && !self.ck.wraps_untrusted(wrapped)
                {
                    self.report(token, Code::UntrustedWrappedAlias);
                }
            }
            _ => {}
        }
    }

    /// The role of a closure written as the argument for a parameter of a call: the body of a
    /// `task::scope` call, or the task closure of a lexical spawn.
    fn written_as(&self, callable: &Callable, receiver: &[TArg], param: &str) -> Written {
        let Callee::Function { item, .. } = &callable.callee else {
            return Written::Other;
        };
        if !self.ck.db.is_std(*item) {
            return Written::Other;
        }
        match self.ck.db.canonical(*item).as_str() {
            "task::scope" if param == "body" => Written::ScopeBody,
            "task::Scope.spawn" | "task::Scope.spawn_sized" => receiver
                .first()
                .and_then(|receiver| self.lexical_scope(&receiver.expr))
                .map_or(Written::Other, Written::Lent),
            _ => Written::Other,
        }
    }

    /// The body closure whose parameter a spawn's receiver is, when the spawn is lexical: written
    /// in that closure or in closures nested in it that are not task closures.
    fn lexical_scope(&self, receiver: &TExpr) -> Option<u32> {
        let TKind::Local(local) = receiver.kind else {
            return None;
        };
        let declared = self.local(self.binding(local));
        let scope = declared.closure;
        if !matches!(declared.kind, LocalKind::Param(_)) || !self.scope_bodies.contains(&scope) {
            return None;
        }
        let at = self
            .frames
            .iter()
            .position(|frame| frame.closure == scope)?;
        self.frames[at + 1..]
            .iter()
            .all(|frame| !frame.task)
            .then_some(scope)
    }

    /// The failure type of a spawned child: at a lexical spawn it converts to the failure type of
    /// its scope's `task::scope` call, checked once that call is typed; elsewhere it is
    /// `task::Cancelled`.
    fn check_spawn(&mut self, token: Tok, callable: &Callable, args: &[TArg]) {
        let Callee::Function { item, .. } = &callable.callee else {
            return;
        };
        if !self.ck.db.is_std(*item)
            || !matches!(
                self.ck.db.canonical(*item).as_str(),
                "task::Scope.spawn" | "task::Scope.spawn_sized"
            )
        {
            return;
        }
        let Some(Arg::Ty(failure)) = callable
            .head
            .own_generics()
            .get(1)
            .and_then(|def| callable.infer.subst.get(def.param))
        else {
            return;
        };
        let scope = args
            .first()
            .and_then(|receiver| self.lexical_scope(&receiver.expr));
        match scope {
            Some(scope) => self.lexical_spawns.push((scope, failure.clone(), token)),
            None => {
                if !failure.has_error()
                    && !failure.is_never()
                    && !self.ck.is_std(failure, "task::Cancelled")
                {
                    self.report(token, Code::TaskUnscopedFailure);
                }
            }
        }
    }

    /// At a `task::scope` call whose body is a closure, the lexical spawns on that closure: each
    /// child failure type converts to the call's failure type.
    fn check_lexical_spawns(&mut self, callable: &Callable, args: &[TArg]) {
        let Callee::Function { item, .. } = &callable.callee else {
            return;
        };
        if !self.ck.db.is_std(*item) || self.ck.db.canonical(*item) != "task::scope" {
            return;
        }
        let Some(TKind::Closure(body)) = args.get(1).map(|arg| &arg.expr.kind) else {
            return;
        };
        let Some(Arg::Ty(scope_failure)) = callable
            .head
            .own_generics()
            .first()
            .and_then(|def| callable.infer.subst.get(def.param))
        else {
            return;
        };
        let (spawns, others) = std::mem::take(&mut self.lexical_spawns)
            .into_iter()
            .partition(|(scope, ..)| *scope == body.index);
        self.lexical_spawns = others;
        for (_, failure, token) in spawns {
            if !self.ck.converts(&failure, scope_failure) {
                let fixes = self
                    .ck
                    .variant_added_fix(&failure, scope_failure)
                    .into_iter()
                    .collect();
                self.report_fixed((token, token), Code::ErrorUnconvertible, fixes);
            }
        }
    }

    /// A volatile call of a register window: its register operand written as the member path
    /// `R.f`, readable for a read and writable for a write, an array register exactly for the
    /// `_at` forms.
    fn check_volatile(&self, token: Tok, callable: &Callable, args: &[TArg]) {
        let Callee::Function { item, .. } = &callable.callee else {
            return;
        };
        let path = self.ck.db.canonical(*item);
        let Some(member) = path
            .strip_prefix("hw::Mmio.")
            .or_else(|| path.strip_prefix("hw::Ports."))
        else {
            return;
        };
        if !self.ck.db.is_std(*item) || !matches!(member, "read" | "write" | "read_at" | "write_at")
        {
            return;
        }
        let Some(operand) = args.get(1).map(|arg| &arg.expr) else {
            return;
        };
        if operand.ty.has_error() {
            return;
        }
        let TKind::Register { block, index } = operand.kind else {
            self.report(operand.token, Code::RegisterOperand);
            return;
        };
        let SigKind::Registers { registers } = &self.ck.sig(block).kind else {
            return;
        };
        let register = &registers[index];
        let reads = member.starts_with("read");
        let allowed = match register.access {
            ast::Access::Read => reads,
            ast::Access::Write => !reads,
            ast::Access::ReadWrite | ast::Access::WriteOneClear => true,
        };
        if !allowed {
            let code = if reads {
                Code::RegisterNotReadable
            } else {
                Code::RegisterNotWritable
            };
            self.report(token, code);
            return;
        }
        let array = self.ck.is_std(&register.ty, "core::Array");
        if array != member.ends_with("_at") {
            self.report(token, Code::RegisterArrayAccess);
        }
    }

    /// Whether `inspect` may be called here: in the body or an ensures, example or decreases
    /// clause of a validator.
    fn inspecting(&self) -> bool {
        self.ck.is_validator(self.item) && !self.cx.requires
    }

    /// The bounds of the generic parameters a call fixes: each argument satisfies them; the
    /// `where` conditions of its const parameters are judged once elaborated.
    fn check_call_bounds(&self, token: Tok, callable: &Callable, head: &FnSig) {
        for def in &head.generics {
            if let Some(Arg::Ty(arg)) = callable.infer.subst.get(def.param)
                && let Some(code) = self.ck.argument_problem(def, arg, &callable.infer.subst)
            {
                self.ck
                    .report_argument(self.item, token, def, arg, &callable.infer.subst, code);
                return;
            }
        }
        if let Callee::Function { item } = &callable.callee
            && head.generics.iter().any(|def| def.constant.is_some())
        {
            let args: Args = head
                .generics
                .iter()
                .map(|def| {
                    callable
                        .infer
                        .subst
                        .get(def.param)
                        .cloned()
                        .unwrap_or_else(|| crate::check::sig::param_arg(def))
                })
                .collect();
            self.ck.instantiated(self.item, token, *item, args);
        }
    }

    fn is_method_call(&self, expr: &ast::Expr) -> bool {
        matches!(&expr.kind, ExprKind::Call { callee, .. } if matches!(callee.kind, ExprKind::Field { .. }))
    }

    /// The labels of a call's arguments after the receiver: the leading one optional, the others
    /// labelled or punned, each its parameter's name; reported once, at the first fault.
    fn check_labels(&self, params: &[ParamSig], args: &[ast::Arg], self_first: bool) {
        let leading = usize::from(self_first);
        let fault = args
            .iter()
            .zip(params)
            .enumerate()
            .find(|(index, (arg, param))| match &arg.label {
                Some(_) if self_first && *index == 0 => true,
                Some(_) => arg.label_name().is_none_or(|name| name.text != param.name),
                None => *index > leading && !self.punned(&arg.value, &param.name),
            });
        let Some((index, (arg, param))) = fault else {
            return;
        };
        let fix = self.reordered(
            params.get(leading + 1..).unwrap_or_default(),
            args.get(leading + 1..).unwrap_or_default(),
        );
        let fix = fix.unwrap_or_else(|| match &arg.label {
            Some(label) if self_first && index == 0 => {
                let syntax = &self.ck.db.item(self.item).file.syntax;
                let start = syntax.tokens[label.token as usize].start;
                let end = syntax.tokens[arg.value.token as usize].start;
                self.ck.fix_bytes(self.item, start, end, "")
            }
            Some(label) => self
                .ck
                .fix_replacing(self.item, label.token, label.last, &param.name),
            None => self
                .ck
                .fix_inserting(self.item, arg.value.token, &format!("{}: ", param.name)),
        });
        self.report_fixed(
            (arg.token, arg.value.last),
            Code::ExprArgumentLabel,
            vec![fix],
        );
    }

    /// The fix reordering the arguments after the leading one into parameter order, when each is
    /// labelled or punned and their names name each of `params` exactly once.
    fn reordered(&self, params: &[ParamSig], args: &[ast::Arg]) -> Option<Fix> {
        let (first, last) = (args.first()?, args.last()?);
        if params.len() != args.len() {
            return None;
        }
        let named: Vec<(&str, &ast::Arg)> = args
            .iter()
            .map(|arg| {
                let name = match arg.label_name() {
                    Some(label) => label.text.as_str(),
                    None => self.punned_name(&arg.value)?,
                };
                Some((name, arg))
            })
            .collect::<Option<_>>()?;
        let ordered: Vec<String> = params
            .iter()
            .map(|param| {
                let mut matching = named.iter().filter(|(name, _)| *name == param.name);
                let (_, arg) = matching.next()?;
                matching
                    .next()
                    .is_none()
                    .then(|| self.ck.written(self.item, arg.token, arg.value.last))
            })
            .collect::<Option<_>>()?;
        Some(
            self.ck
                .fix_replacing(self.item, first.token, last.value.last, &ordered.join(", ")),
        )
    }

    /// The name of an argument that is a binding or parameter, alone or after a mode.
    fn punned_name<'e>(&self, value: &'e ast::Expr) -> Option<&'e str> {
        let inner = match &value.kind {
            ExprKind::Marked {
                failure: None,
                awaited: None,
                mode: Some(_),
                inner,
            } => inner,
            _ => value,
        };
        match &inner.kind {
            ExprKind::Path { path, args: None } if path.segments.len() == 1 => {
                Some(&path.segments[0].text)
            }
            ExprKind::SelfValue => Some("self"),
            _ => None,
        }
    }

    /// Whether an argument is a binding or parameter, alone or after a mode, spelt like its
    /// parameter.
    fn punned(&self, value: &ast::Expr, name: &str) -> bool {
        self.punned_name(value) == Some(name)
    }

    /// The marking rules: a place passed to a `mut` parameter is marked `mut`, a non-Copy place
    /// passed to a `take` parameter `take`, and nothing else is marked. `construct` is the
    /// argument's first and last tokens.
    fn check_mode(
        &self,
        (token, last): (Tok, Tok),
        arg: &TExpr,
        mode: Mode,
        marker: Option<(Mode, Tok)>,
        chain_mut: bool,
        receiver: bool,
    ) {
        if arg.ty.has_error() {
            return;
        }
        let place = is_place(arg) || (receiver && self.is_part(arg));
        let required = if !place {
            None
        } else {
            match mode {
                Mode::Mut => Some(Mode::Mut),
                Mode::Take if !self.ck.has(&arg.ty, Property::Copy) => Some(Mode::Take),
                Mode::Take => None,
                Mode::Read if chain_mut => Some(Mode::Mut),
                Mode::Read => None,
            }
        };
        let written = marker.map(|(mode, _)| mode);
        if written == required {
            return;
        }
        let construct = (marker.map_or(token, |(_, marker)| marker), last);
        let fix = match (marker, required) {
            (None, Some(required)) => {
                self.ck
                    .fix_inserting(self.item, token, &format!("{} ", required.word()))
            }
            (Some((_, marker)), Some(required)) => {
                self.ck
                    .fix_replacing(self.item, marker, marker, required.word())
            }
            (Some((_, marker)), None) => self.ck.fix_deleting_forward(self.item, marker, marker),
            (None, None) => return,
        };
        let code = match (written, required) {
            (None, Some(_)) => Code::ModeUnmarked,
            (Some(Mode::Take), None) if place && mode == Mode::Take => Code::ModeCopyTaken,
            _ => Code::ModeMismatch,
        };
        self.report_fixed(construct, code, vec![fix]);
    }

    /// Whether a typed expression is a part: the result of a projection call of that form, an
    /// indexing, a view binding of a part, or a field of one.
    pub fn is_part(&self, expr: &TExpr) -> bool {
        match &expr.kind {
            TKind::Call(call) => call.proj && self.ck.projection_form(call) == Form::Part,
            TKind::Index(_) => true,
            TKind::Local(local) => self.part_bindings.contains(local),
            TKind::Field { base, .. } => self.is_part(base),
            _ => false,
        }
    }

    /// Whether a typed expression is a wrapped part: the result of a projection call of that
    /// form, or a view binding of one.
    pub fn is_wrapped_part(&self, expr: &TExpr) -> bool {
        match &expr.kind {
            TKind::Call(call) => call.proj && self.ck.projection_form(call) == Form::Wrapped,
            TKind::Local(local) => self.wrapped_bindings.contains(local),
            _ => false,
        }
    }

    /// Whether a typed expression names, in place, what a read parameter holds: the parameter, a
    /// view binding of what it holds, or a field of one.
    pub fn is_read_view(&self, expr: &TExpr) -> bool {
        match &expr.kind {
            TKind::Local(local) => {
                self.read_views.contains(local)
                    || matches!(self.local(*local).kind, LocalKind::Param(Mode::Read))
            }
            TKind::Field { base, .. } => self.is_read_view(base),
            _ => false,
        }
    }

    /// Whether an item is `Option` or `Result`, whose payloads wrap parts and read views
    /// unmarked.
    fn wraps(&self, item: ItemId) -> bool {
        let db = &self.ck.db;
        [db.core_item("Option"), db.core_item("Result")].contains(&Some(item))
    }

    /// A construction of a struct or layout, or of a variant with a payload.
    fn construct(
        &mut self,
        token: Tok,
        item: ItemId,
        variant: Option<usize>,
        generic_args: Option<&[ast::GenericArg]>,
        args: &[ast::Arg],
        expected: Option<&Ty>,
    ) -> TExpr {
        let sig = self.ck.sig(item);
        let params: Vec<Param> = sig.generics.iter().map(|def| def.param).collect();
        let mut infer = Infer::new(params.clone());
        if let Some(generic_args) = generic_args {
            match self.enum_args(token, item, Some(generic_args), None) {
                Some(Ty::Item(_, written)) => {
                    infer.subst = Subst::of(params.iter().copied(), &written);
                }
                _ => {
                    self.skim_args(args);
                    return TExpr::failed(token);
                }
            }
        } else if let Some(Ty::Item(found, found_args)) = expected
            && *found == item
        {
            infer.subst = Subst::of(params.iter().copied(), found_args);
        }
        let consumer = self.ck.db.package_of(item) != self.ck.db.module(self.module).package;
        let (fields, labelled): (Vec<(String, Ty)>, bool) = match (&sig.kind, variant) {
            (SigKind::Struct { fields }, None) => {
                if consumer && self.ck.db.ast(item).has_modifier(TypeModifier::Open) {
                    self.report(token, Code::TypeOpenConstruction);
                    self.skim_args(args);
                    return TExpr::failed(token);
                }
                for field in fields {
                    if !self
                        .ck
                        .db
                        .field_visible(self.module, item, field.visibility)
                    {
                        self.report(token, Code::TypeHiddenField);
                        self.skim_args(args);
                        return TExpr::failed(token);
                    }
                }
                (
                    fields
                        .iter()
                        .map(|field| (field.name.clone(), field.ty.clone()))
                        .collect(),
                    true,
                )
            }
            (SigKind::Layout { fields, .. }, None) => (
                fields
                    .iter()
                    .map(|field| (field.name.clone(), field.ty.clone()))
                    .collect(),
                true,
            ),
            (SigKind::Enum { variants }, Some(index)) => match &variants[index].payload {
                Payload::None => {
                    self.report(token, Code::TypePayloadShape);
                    self.skim_args(args);
                    return TExpr::failed(token);
                }
                Payload::Single(ty) => {
                    let [arg] = args else {
                        self.report(token, Code::TypePayloadShape);
                        self.skim_args(args);
                        return TExpr::failed(token);
                    };
                    if arg.label.is_some() {
                        self.report(token, Code::TypePayloadShape);
                        self.skim_args(args);
                        return TExpr::failed(token);
                    }
                    (vec![(String::new(), ty.clone())], false)
                }
                Payload::Labelled(fields) => {
                    if args.is_empty() {
                        self.report(token, Code::TypePayloadShape);
                        return TExpr::failed(token);
                    }
                    (fields.clone(), true)
                }
            },
            _ => {
                self.report(token, Code::NameUnknown);
                self.skim_args(args);
                return TExpr::failed(token);
            }
        };
        let mut values: Vec<(String, TExpr)> = Vec::new();
        let mut named: Vec<String> = Vec::new();
        let mut failed = false;
        let mut wrapped = false;
        for arg in args {
            let (name, ty) = if labelled {
                let Some(label) = arg.label_name() else {
                    let named_elsewhere = |name: &str| {
                        args.iter()
                            .any(|other| other.label_name().is_some_and(|label| label.text == name))
                    };
                    let fixes = match &arg.value.kind {
                        ExprKind::Path { path, args: None }
                            if path.segments.len() == 1
                                && fields
                                    .iter()
                                    .any(|(name, _)| *name == path.segments[0].text)
                                && !named_elsewhere(&path.segments[0].text) =>
                        {
                            let name = &path.segments[0].text;
                            vec![self.ck.fix_inserting(
                                self.item,
                                arg.value.token,
                                &format!("{name}: "),
                            )]
                        }
                        _ => Vec::new(),
                    };
                    self.report_fixed(
                        (arg.token, arg.value.last),
                        Code::TypeUnlabelledField,
                        fixes,
                    );
                    self.skim(&arg.value);
                    failed = true;
                    continue;
                };
                let owner = variant.is_none().then_some(item);
                let Some((name, ty)) = self.labelled_field(owner, label, &fields) else {
                    self.skim(&arg.value);
                    failed = true;
                    continue;
                };
                if variant.is_none() {
                    self.name_member(label.token, Member::Field(item, name.clone()));
                    let label = FieldLabel {
                        item: self.item,
                        first: arg.token,
                        last: arg.value.last,
                        simple: place_or_literal(&arg.value),
                    };
                    self.ck.record_label(item, name, label);
                }
                if named.contains(name) {
                    self.report(arg.token, Code::TypeDuplicateLabel);
                    failed = true;
                }
                named.push(name.clone());
                (name.clone(), ty.clone())
            } else {
                fields[0].clone()
            };
            let (marker, value) = self.marked_value(&arg.value);
            let pattern = infer.subst.ty(&ty);
            let hint = (!infer.open(&pattern)).then_some(&pattern);
            let typed = self.expr(value, hint);
            if !infer.unify(&pattern, &typed.ty) {
                let shown = infer.subst.ty(&ty);
                self.expect(&typed, &shown, value.token);
            } else if let (Ty::Fn(found), Ty::Fn(wanted)) = (&typed.ty, &pattern) {
                self.fit(found, wanted, value.token);
            }
            if let TKind::Closure(_) = typed.kind {
                self.report(value.token, Code::ClosureSecondClass);
            }
            let wraps_part = self.wraps(item)
                && (self.is_part(&typed) || self.is_read_view(&typed))
                && !self.ck.has(&typed.ty, Property::Copy);
            let mode = if wraps_part { Mode::Read } else { Mode::Take };
            self.check_mode(
                (value.token, value.last),
                &typed,
                mode,
                marker,
                false,
                false,
            );
            wrapped |= wraps_part;
            values.push((name, typed));
        }
        if labelled && !failed {
            let missing = fields.iter().any(|(name, _)| !named.contains(name));
            if missing {
                self.report(token, Code::TypeMissingField);
            }
        }
        let unfixed = params.iter().any(|param| infer.subst.get(*param).is_none());
        if unfixed {
            self.report(token, Code::GenericUninferred);
            return TExpr::failed(token);
        }
        let args: Vec<Arg> = params
            .iter()
            .map(|param| {
                infer
                    .subst
                    .get(*param)
                    .cloned()
                    .expect("every parameter is fixed")
            })
            .collect();
        let ty = Ty::item(item, args);
        if !params.is_empty() {
            self.ck.check_instantiation(self.item, &ty, token);
        }
        TExpr {
            token,
            last: token,
            ty,
            kind: TKind::Construct {
                variant,
                values,
                wrapped,
            },
        }
    }

    /// `x[i]`: a call of the `at`, or `at_mut` where the element is written, that method
    /// resolution finds on the type of `x`.
    pub fn index(&mut self, token: Tok, base: &ast::Expr, index: &ast::Expr, write: bool) -> TExpr {
        let base_typed = if write {
            match self.place(base, false) {
                Some(place) => place,
                None => self.expr(base, None),
            }
        } else {
            self.expr(base, None)
        };
        if base_typed.ty.has_error() {
            self.skim(index);
            return TExpr::failed(token);
        }
        let text_types = ["core::Str", "text::String", "text::Fixed"];
        if text_types
            .iter()
            .any(|path| self.ck.is_std(&base_typed.ty, path))
        {
            let fix = self.ck.fix_appending(self.item, base.last, ".as_bytes()");
            self.report_fixed((token, token), Code::TextIntegerIndex, vec![fix]);
            self.skim(index);
            return TExpr::failed(token);
        }
        let name = if write { "at_mut" } else { "at" };
        let candidates = self.ck.method_candidates(&base_typed.ty, name);
        let callable = match candidates.into_iter().next() {
            Some(candidate) => self.callable(candidate, &base_typed.ty.clone()),
            None => None,
        };
        let Some(callable) = callable.filter(|callable| {
            callable.head.is_proj() && callable.head.params.len() == 2 && callable.head.has_self()
        }) else {
            self.report(token, Code::OwnNotIndexable);
            self.skim(index);
            return TExpr::failed(token);
        };
        let subst = callable.infer.subst.clone();
        let index_ty = subst.ty(&callable.head.params[1].ty);
        let index_typed = self.check(index, &index_ty);
        let head = subst.fn_ty(&callable.head.fn_ty());
        let result = head.result.clone();
        let type_args = type_args(&callable.callee, &callable.head, &subst);
        TExpr {
            token,
            last: token,
            ty: result,
            kind: TKind::Index(Box::new(Call {
                token,
                callee: callable.callee,
                type_args,
                args: vec![
                    TArg {
                        expr: base_typed,
                        mode: callable.head.params[0].mode,
                    },
                    TArg {
                        expr: index_typed,
                        mode: Mode::Read,
                    },
                ],
                head,
                proj: true,
                failure: None,
                awaited: false,
            })),
        }
    }

    /// A named conversion or bit conversion `T.name(x)`.
    fn conversion(
        &mut self,
        token: Tok,
        target: Scalar,
        name: &ast::Name,
        args: &[ast::Arg],
        markers: Markers,
    ) -> TExpr {
        self.builtin_markers(markers);
        let written = name;
        let name = name.text.as_str();
        let (ck, item) = (self.ck, self.item);
        let renamed = |to: &str| vec![ck.fix_replacing(item, written.token, written.token, to)];
        let unconverted = |arg: &ast::Arg| {
            let operand = ck.written(item, arg.value.token, arg.value.last);
            let last = markers.last.unwrap_or(token);
            vec![ck.fix_replacing(item, token, last, &operand)]
        };
        let [arg] = args else {
            self.report(token, Code::ExprArgumentCount);
            self.skim_args(args);
            return TExpr::failed(token);
        };
        let (result, source) = match name {
            "to_bits" => {
                let typed = self.check(&arg.value, &Ty::Scalar(target));
                if target.is_unsigned_bits() {
                    let construct = (token, markers.last.unwrap_or(token));
                    self.report_fixed(construct, Code::NumSameTypeConversion, unconverted(arg));
                    return TExpr::failed(token);
                }
                (Ty::Scalar(target.bits_type()), typed)
            }
            "from_bits" | "checked_from_bits" => {
                let bits = Ty::Scalar(target.bits_type());
                let typed = self.check(&arg.value, &bits);
                if target.is_unsigned_bits() {
                    let construct = (token, markers.last.unwrap_or(token));
                    self.report_fixed(construct, Code::NumSameTypeConversion, unconverted(arg));
                    return TExpr::failed(token);
                }
                if name == "from_bits" && !self.ck.has(&Ty::Scalar(target), Property::AnyBits) {
                    let construct = (written.token, written.token);
                    let fixes = renamed("checked_from_bits");
                    self.report_fixed(construct, Code::NumFromBitsNotAnybits, fixes);
                    return TExpr::failed(token);
                }
                let result = if name == "from_bits" {
                    Ty::Scalar(target)
                } else {
                    self.ck.option_ty(Ty::Scalar(target))
                };
                (result, typed)
            }
            _ => {
                let typed = self.expr(&arg.value, None);
                let Some(source) = typed.ty.scalar() else {
                    if !typed.ty.has_error() {
                        self.report(token, Code::NumConversionUndefined);
                    }
                    return TExpr::failed(token);
                };
                if !source.is_number() || !target.is_number() {
                    self.report(token, Code::NumConversionUndefined);
                    return TExpr::failed(token);
                }
                if source == target {
                    let construct = (token, markers.last.unwrap_or(token));
                    self.report_fixed(construct, Code::NumSameTypeConversion, unconverted(arg));
                    return TExpr::failed(token);
                }
                let widening = widens(source, target);
                let construct = (written.token, written.token);
                if name == "widen" && !widening {
                    self.report_fixed(construct, Code::NumWidenLossy, renamed("fit"));
                    return TExpr::failed(token);
                }
                if name != "widen" && widening {
                    self.report_fixed(construct, Code::NumWidenApplies, renamed("widen"));
                    return TExpr::failed(token);
                }
                let defined = match name {
                    "truncate" => source.is_integer() && target.is_integer(),
                    "saturate" => target.is_integer(),
                    "nearest" => source.is_float() || target.is_float(),
                    _ => true,
                };
                if !defined {
                    self.report(token, Code::NumConversionUndefined);
                    return TExpr::failed(token);
                }
                let result = if name == "checked_fit" {
                    self.ck.option_ty(Ty::Scalar(target))
                } else {
                    Ty::Scalar(target)
                };
                (result, typed)
            }
        };
        let builtin = match (Conversion::named(name), BitConversion::named(name)) {
            (Some(conversion), _) => nir::Builtin::Conversion(conversion, target),
            (None, Some(bits)) => nir::Builtin::Bits(bits, BitsOwner::Scalar(target)),
            (None, None) => unreachable!("a conversion's name"),
        };
        builtin_call(token, Builtin::Conversion(builtin), source, result)
    }

    /// `L.from_bits(bits)` and `L.checked_from_bits(bits)` of a layout.
    fn layout_bits(
        &mut self,
        token: Tok,
        layout: &Ty,
        written: &ast::Name,
        args: &[ast::Arg],
        markers: Markers,
    ) -> TExpr {
        self.builtin_markers(markers);
        let name = written.text.as_str();
        let bits = self.bits_of(layout).unwrap_or(Ty::Error);
        let [arg] = args else {
            self.report(token, Code::ExprArgumentCount);
            self.skim_args(args);
            return TExpr::failed(token);
        };
        let owner = match layout {
            Ty::Item(item, _) => BitsOwner::Layout(*item),
            _ => return TExpr::failed(token),
        };
        let builtin = |name: &str| {
            let conversion = BitConversion::named(name).expect("a bit conversion's name");
            Builtin::Conversion(nir::Builtin::Bits(conversion, owner))
        };
        if name == "to_bits" {
            let source = self.check(&arg.value, layout);
            return builtin_call(token, builtin(name), source, bits);
        }
        let source = self.check(&arg.value, &bits);
        if name == "from_bits" && !self.ck.has(layout, Property::AnyBits) {
            let fix =
                self.ck
                    .fix_replacing(self.item, written.token, written.token, "checked_from_bits");
            let construct = (written.token, written.token);
            self.report_fixed(construct, Code::NumFromBitsNotAnybits, vec![fix]);
            return TExpr::failed(token);
        }
        let result = if name == "from_bits" {
            layout.clone()
        } else {
            self.ck.option_ty(layout.clone())
        };
        builtin_call(token, builtin(name), source, result)
    }

    /// The bits type of a scalar or layout.
    fn bits_of(&self, ty: &Ty) -> Option<Ty> {
        match ty {
            Ty::Scalar(scalar) => Some(Ty::Scalar(scalar.bits_type())),
            Ty::Item(item, _) => match &self.ck.sig(*item).kind {
                SigKind::Layout { bits, .. } => Some(bits.clone().unwrap_or(Ty::Error)),
                _ => None,
            },
            _ => None,
        }
    }

    /// `x.to_bits()`: the bit pattern of a scalar or layout as its bits type.
    fn bits_call(
        &mut self,
        token: Tok,
        receiver: TExpr,
        bits: Ty,
        args: &[ast::Arg],
        markers: Markers,
    ) -> TExpr {
        if let Some(marker) = markers.failure {
            self.unneeded_failure_marker(marker);
        }
        if let Some(marker) = markers.awaited {
            self.unneeded_await(marker);
        }
        self.check_mode(
            (receiver.token, markers.receiver.unwrap_or(receiver.token)),
            &receiver,
            Mode::Read,
            markers.mode,
            markers.chain_mut,
            true,
        );
        if !args.is_empty() {
            self.report(token, Code::ExprArgumentCount);
            self.skim_args(args);
            return TExpr::failed(token);
        }
        if receiver.ty.scalar().is_some_and(Scalar::is_unsigned_bits) {
            let last = markers.last.unwrap_or(token);
            let receiver_last = markers.receiver.unwrap_or(receiver.token);
            let operand = self.ck.written(self.item, receiver.token, receiver_last);
            let fix = self
                .ck
                .fix_replacing(self.item, receiver.token, last, &operand);
            self.report_fixed(
                (receiver.token, last),
                Code::NumSameTypeConversion,
                vec![fix],
            );
            return TExpr::failed(token);
        }
        let owner = match &receiver.ty {
            Ty::Scalar(scalar) => BitsOwner::Scalar(*scalar),
            Ty::Item(item, _) => BitsOwner::Layout(*item),
            _ => return TExpr::failed(token),
        };
        let builtin = nir::Builtin::Bits(BitConversion::To, owner);
        builtin_call(token, Builtin::Conversion(builtin), receiver, bits)
    }

    /// `Dyn[I].view(x)` and `Dyn[I].view_mut(mut x)`.
    fn dyn_view(
        &mut self,
        token: Tok,
        bound: crate::check::ty::Bound,
        written: &ast::Name,
        args: &[ast::Arg],
        markers: Markers,
    ) -> TExpr {
        let name = written.text.as_str();
        let [arg] = args else {
            self.report(token, Code::ExprArgumentCount);
            self.skim_args(args);
            return TExpr::failed(token);
        };
        let (marker, value) = self.marked_value(&arg.value);
        self.builtin_markers(markers);
        let typed = self.expr(value, None);
        if !typed.ty.has_error() && !self.ck.implements(&typed.ty, bound.interface) {
            self.report(token, Code::InterfaceNotImplemented);
            return TExpr::failed(token);
        }
        let takes_mut = match &self.ck.sig(bound.interface).kind {
            SigKind::Interface { methods } => methods.iter().any(|method| {
                method
                    .sig
                    .params
                    .first()
                    .is_some_and(|param| param.is_self && param.mode == Mode::Mut)
            }),
            _ => false,
        };
        let allowed = if takes_mut { "view_mut" } else { "view" };
        if name != allowed {
            let mut fix = self
                .ck
                .fix_replacing(self.item, written.token, written.token, allowed);
            match marker {
                Some((_, marker)) if !takes_mut => {
                    fix = fix.and(self.ck.fix_deleting_forward(self.item, marker, marker));
                }
                None if takes_mut => {
                    fix = fix.and(self.ck.fix_inserting(self.item, value.token, "mut "));
                }
                _ => {}
            }
            let construct = (written.token, written.token);
            self.report_fixed(construct, Code::InterfaceDynViewMode, vec![fix]);
            return TExpr::failed(token);
        }
        let mode = if name == "view_mut" {
            Mode::Mut
        } else {
            Mode::Read
        };
        self.check_mode(
            (value.token, value.last),
            &typed,
            mode,
            marker,
            false,
            false,
        );
        let ty = Ty::Dyn(Rc::new(bound));
        TExpr {
            token,
            last: token,
            ty: ty.clone(),
            kind: TKind::Call(Box::new(Call {
                token,
                callee: Callee::Builtin(Builtin::DynView),
                type_args: no_args(),
                args: vec![TArg { expr: typed, mode }],
                head: plain_head(ty),
                proj: true,
                failure: None,
                awaited: false,
            })),
        }
    }

    /// `mem::Box[Dyn[I]].from_box(take b)`.
    fn boxed_dyn_call(
        &mut self,
        token: Tok,
        boxed: &Ty,
        bound: Ty,
        args: &[ast::Arg],
        markers: Markers,
    ) -> TExpr {
        self.builtin_markers(markers);
        let [arg] = args else {
            self.report(token, Code::ExprArgumentCount);
            return TExpr::failed(token);
        };
        let (marker, value) = self.marked_value(&arg.value);
        let typed = self.expr(value, None);
        if let (Ty::Dyn(bound), Some(inner)) = (&bound, typed.ty.type_arg(0))
            && !self.ck.implements(inner, bound.interface)
        {
            self.report(token, Code::InterfaceNotImplemented);
        }
        self.check_mode(
            (value.token, value.last),
            &typed,
            Mode::Take,
            marker,
            false,
            false,
        );
        TExpr {
            token,
            last: token,
            ty: boxed.clone(),
            kind: TKind::Call(Box::new(Call {
                token,
                callee: Callee::Builtin(Builtin::DynView),
                type_args: no_args(),
                args: vec![TArg {
                    expr: typed,
                    mode: Mode::Take,
                }],
                head: plain_head(boxed.clone()),
                proj: false,
                failure: None,
                awaited: false,
            })),
        }
    }

    /// `text::fmt("template", ...)`: holes filled by labelled or punned arguments.
    fn template(&mut self, token: Tok, args: &[ast::Arg]) -> TExpr {
        let Some((template, rest)) = args.split_first() else {
            self.report(token, Code::ExprArgumentCount);
            return TExpr::failed(token);
        };
        let (text, holes) = match &template.value.kind {
            ExprKind::Literal(literal)
                if literal.kind == ast::LiteralKind::Text && template.label.is_none() =>
            {
                let value = crate::syntax::literal::text_value(self.text(literal.tokens[0]));
                match template_holes(&value) {
                    Some(holes) => (value, holes),
                    None => {
                        self.report(template.token, Code::TextTemplateMalformed);
                        return TExpr::failed(token);
                    }
                }
            }
            _ => {
                self.report(template.token, Code::TextTemplateNotLiteral);
                return TExpr::failed(token);
            }
        };
        let mut names: Vec<String> = Vec::new();
        let text_arg = |token: Tok, text: String, str_ty: Ty| TArg {
            expr: TExpr {
                token,
                last: token,
                ty: str_ty,
                kind: TKind::Literal(Lit::Text(text)),
            },
            mode: Mode::Read,
        };
        let mut typed_args = vec![text_arg(template.token, text, self.ck.str_ty())];
        let render = self.ck.db.core_item("Render");
        for arg in rest {
            let name = match arg.label_name() {
                Some(name) => name.text.clone(),
                None if arg.label.is_none() => match &arg.value.kind {
                    ExprKind::Path { path, args: None } if path.segments.len() == 1 => {
                        path.segments[0].text.clone()
                    }
                    _ => {
                        self.report(arg.token, Code::ExprArgumentLabel);
                        return TExpr::failed(token);
                    }
                },
                None => {
                    self.report(arg.token, Code::ExprArgumentLabel);
                    return TExpr::failed(token);
                }
            };
            if names.contains(&name) {
                self.report(arg.token, Code::TextArgumentDuplicate);
            }
            if !holes.contains(&name) {
                self.report(arg.token, Code::TextArgumentUnused);
            }
            names.push(name.clone());
            typed_args.push(text_arg(arg.token, name, self.ck.str_ty()));
            let (marker, value) = self.marked_value(&arg.value);
            if let Some((_, marker)) = marker {
                self.report(marker, Code::ModeMismatch);
            }
            let typed = self.operand(value, None);
            let renders = matches!(typed.ty, Ty::Dyn(ref bound) if Some(bound.interface) == render)
                || render.is_some_and(|render| self.ck.implements(&typed.ty, render));
            if !renders && !typed.ty.has_error() {
                self.report(arg.value.token, Code::GenericUnsatisfiedBound);
            }
            typed_args.push(TArg {
                expr: typed,
                mode: Mode::Read,
            });
        }
        if holes.iter().any(|hole| !names.contains(hole)) {
            self.report(template.token, Code::TextHoleUnbound);
        }
        let ty = match render {
            Some(render) => Ty::Dyn(Rc::new(crate::check::ty::Bound {
                interface: render,
                args: crate::check::ty::no_args(),
            })),
            None => Ty::Error,
        };
        TExpr {
            token,
            last: token,
            ty: ty.clone(),
            kind: TKind::Call(Box::new(Call {
                token,
                callee: Callee::Builtin(Builtin::Template),
                type_args: no_args(),
                args: typed_args,
                head: plain_head(ty),
                proj: false,
                failure: None,
                awaited: false,
            })),
        }
    }

    /// The head of a DMA access call on an `hw::DmaShared`.
    fn dma_access(&self, ty: &Ty, name: &str) -> Option<String> {
        (self.ck.is_std(ty, "hw::DmaShared")
            && matches!(
                name,
                "read_acquire" | "write_release" | "read_acquire_at" | "write_release_at"
            ))
        .then(|| name.to_owned())
    }

    /// A DMA access call: its field operand `T.f`, a field of the buffer's bytes layout.
    fn apply_dma(
        &mut self,
        token: Tok,
        receiver: TExpr,
        name: String,
        args: &[ast::Arg],
        markers: Markers,
    ) -> TExpr {
        self.ck.require(self.item, token, Authority::Device);
        let layout = receiver.ty.type_arg(0).cloned().unwrap_or(Ty::Error);
        let Some(first) = args.first() else {
            self.report(token, Code::HwDmaOperand);
            return TExpr::failed(token);
        };
        let field = match (&first.value.kind, &layout) {
            (ExprKind::Field { base, name: field }, Ty::Item(item, _)) => {
                match self.type_target(base) {
                    Some(TypeTarget::Type(Ty::Item(written, _))) if written == *item => {
                        match &self.ck.sig(*item).kind {
                            SigKind::Layout { fields, .. } => fields
                                .iter()
                                .position(|candidate| candidate.name == field.text)
                                .map(|index| (index, fields[index].ty.clone())),
                            _ => None,
                        }
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        let Some((field_index, field_ty)) = field else {
            self.report(first.token, Code::HwDmaOperand);
            return TExpr::failed(token);
        };
        let array_element = self
            .ck
            .is_std(&field_ty, "core::Array")
            .then(|| field_ty.type_arg(0).cloned())
            .flatten();
        let indexed = name.ends_with("_at");
        let element = match (indexed, &array_element) {
            (true, Some(element)) => element.clone(),
            (false, None) => field_ty.clone(),
            _ => {
                self.report(token, Code::HwDmaOperand);
                return TExpr::failed(token);
            }
        };
        let write = name.starts_with("write");
        let mut index_arg = None;
        let mut value_arg = None;
        for arg in &args[1..] {
            let label = arg.label_name().map(|name| name.text.as_str());
            match label {
                Some("index") if indexed && index_arg.is_none() => {
                    index_arg = Some(self.check(&arg.value, &Ty::Scalar(Scalar::Usize)));
                }
                Some("value") if write && value_arg.is_none() => {
                    value_arg = Some(self.check(&arg.value, &element));
                }
                Some("index" | "value") => {
                    self.report(arg.token, Code::HwDmaOperand);
                    return TExpr::failed(token);
                }
                _ => {
                    self.report(arg.token, Code::ExprArgumentLabel);
                    return TExpr::failed(token);
                }
            }
        }
        if index_arg.is_some() != indexed || value_arg.is_some() != write {
            self.report(token, Code::HwDmaOperand);
            return TExpr::failed(token);
        }
        self.check_mode(
            (receiver.token, markers.receiver.unwrap_or(receiver.token)),
            &receiver,
            Mode::Mut,
            markers.mode,
            false,
            true,
        );
        let result = if write { Ty::Unit } else { element };
        // The access carries, as type arguments, its field's bytes layout, the field's index in
        // it and whether the access writes and is indexed; its operands are the buffer, then the
        // index of an indexed access, then the value of a write.
        let type_args = Args::from(vec![
            Arg::Ty(layout),
            Arg::Const(Const::Int(field_index as i128)),
            Arg::Const(Const::Bool(write)),
            Arg::Const(Const::Bool(indexed)),
        ]);
        let mut call_args = vec![TArg {
            expr: receiver,
            mode: Mode::Mut,
        }];
        call_args.extend(index_arg.map(|expr| TArg {
            expr,
            mode: Mode::Read,
        }));
        call_args.extend(value_arg.map(|expr| TArg {
            expr,
            mode: Mode::Take,
        }));
        TExpr {
            token,
            last: token,
            ty: result.clone(),
            kind: TKind::Call(Box::new(Call {
                token,
                callee: Callee::Builtin(Builtin::DmaAccess),
                type_args,
                args: call_args,
                head: plain_head(result),
                proj: false,
                failure: markers.failure,
                awaited: markers.awaited.is_some(),
            })),
        }
    }
}

/// Whether an expression is a place or a literal, whose value names no effect.
fn place_or_literal(expr: &ast::Expr) -> bool {
    match &expr.kind {
        ExprKind::Literal(_) | ExprKind::SelfValue | ExprKind::Path { args: None, .. } => true,
        ExprKind::Field { base: inner, .. } | ExprKind::Paren(inner) => place_or_literal(inner),
        ExprKind::Unary {
            op: ast::UnaryOp::Neg,
            operand,
        } => matches!(operand.kind, ExprKind::Literal(_)),
        _ => false,
    }
}

/// Whether a typed expression is a place: a local, and a path of fields and indexes from one.
/// The head of a toolchain operation: a plain function that neither fails nor waits.
/// The generic arguments of a call: for an interface method, `Self` and the interface's
/// arguments before the method's own; otherwise those of the head's generic parameters.
fn type_args(callee: &Callee, head: &FnSig, subst: &Subst) -> Args {
    let own = head.generics.iter().map(|def| match def.constant {
        Some(_) => subst.arg(&Arg::Const(Const::Param(def.param))),
        None => Arg::Ty(subst.ty(&Ty::Param(def.param))),
    });
    match callee {
        Callee::Method {
            interface, self_ty, ..
        } => std::iter::once(Arg::Ty(self_ty.clone()))
            .chain(interface.args.iter().cloned())
            .chain(own)
            .collect(),
        _ => own.collect(),
    }
}

fn plain_head(result: Ty) -> FnTy {
    FnTy {
        kind: FnKind::Plain,
        params: Vec::new(),
        result,
        fails: None,
        wait: false,
        limits: Default::default(),
        convention: None,
    }
}

/// A call of a toolchain operation on one read argument.
fn builtin_call(token: Tok, builtin: Builtin, source: TExpr, result: Ty) -> TExpr {
    let head = plain_head(result.clone());
    TExpr {
        token,
        last: token,
        ty: result,
        kind: TKind::Call(Box::new(Call {
            token,
            callee: Callee::Builtin(builtin),
            type_args: no_args(),
            args: vec![TArg {
                expr: source,
                mode: Mode::Read,
            }],
            head,
            proj: false,
            failure: None,
            awaited: false,
        })),
    }
}

/// Whether an expression is a field path through an index, whose `mut` access writes the
/// element.
fn through_index(expr: &ast::Expr) -> bool {
    match &expr.kind {
        ExprKind::Index { .. } => true,
        ExprKind::Field { base, .. } => through_index(base),
        _ => false,
    }
}

pub fn is_place(expr: &TExpr) -> bool {
    match &expr.kind {
        TKind::Local(_) => true,
        TKind::Field { base, .. } => is_place(base),
        TKind::Index(call) => call.args.first().is_some_and(|base| is_place(&base.expr)),
        _ => false,
    }
}

/// Whether every value of `source` is exactly a value of `target`.
fn widens(source: Scalar, target: Scalar) -> bool {
    if source == target {
        return false;
    }
    let range =
        |scalar: Scalar| -> Option<(i128, i128)> { scalar.is_integer().then(|| scalar.range()) };
    match (source.is_float(), target.is_float()) {
        (false, false) => {
            let (Some((low, high)), Some((target_low, target_high))) =
                (range(source), range(target))
            else {
                return false;
            };
            low >= target_low && high <= target_high
        }
        (false, true) => {
            let mantissa = if target == Scalar::F32 { 24 } else { 53 };
            let Some((low, high)) = range(source) else {
                return false;
            };
            high < (1i128 << mantissa) && low > -(1i128 << mantissa)
        }
        (true, true) => source == Scalar::F32 && target == Scalar::F64,
        (true, false) => false,
    }
}

/// The hole names of a template's value, or none when it holds a stray brace.
fn template_holes(value: &str) -> Option<Vec<String>> {
    let mut holes = Vec::new();
    let chars: Vec<char> = value.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        match chars[index] {
            '{' if chars.get(index + 1) == Some(&'{') => index += 2,
            '}' if chars.get(index + 1) == Some(&'}') => index += 2,
            '{' => {
                let end = chars[index..].iter().position(|c| *c == '}')? + index;
                let name: String = chars[index + 1..end].iter().collect();
                let ident = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                if !ident || crate::syntax::grammar::Keyword::from_spelling(&name).is_some() {
                    return None;
                }
                holes.push(name);
                index = end + 1;
            }
            '}' => return None,
            _ => index += 1,
        }
    }
    Some(holes)
}

/// A mode as a parameter writes it before its name.
fn mode_word(mode: Option<Mode>) -> &'static str {
    match mode {
        Some(Mode::Mut) => "mut ",
        Some(Mode::Take) => "take ",
        _ => "",
    }
}
