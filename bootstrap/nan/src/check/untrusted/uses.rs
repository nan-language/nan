//! The uses of decoders, encoders and validators that a body names: the codec uses of a function,
//! which each instantiation naming it substitutes, and the declassifying uses, each needing a
//! `validate` grant on its line.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use crate::check::Checker;
use crate::check::conform::Conformance;
use crate::check::db::ItemId;
use crate::check::sig::{GenericDef, SigKind};
use crate::check::tir::{Builtin, Call, Callee, TBody, TExpr, TKind};
use crate::check::ty::{Arg, Param, Subst, Ty};
use crate::syntax::ast::Tok;
use crate::workspace::Authority;

/// What a use of a type's codec runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Codec {
    Decode,
    Encode,
}

/// A body: a function's, test's, const's or other item's own, or the default body and clauses of
/// an interface's method by index.
type Unit = (ItemId, Option<usize>);

/// A call of an interface method, by index, with its own type arguments.
type Called<'a> = Option<(usize, &'a [Arg])>;

/// What a body uses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Uses {
    /// The type parameters of the body whose decoders or encoders it uses.
    codecs: BTreeSet<(Param, Codec)>,
    /// The tokens of its declassifying uses of decoders and encoders, and of its uses of
    /// validators through conformances.
    declassifying: BTreeSet<Tok>,
}

/// The uses of the bodies computed, and those being computed.
#[derive(Default)]
pub struct Cache {
    done: HashMap<Unit, Rc<Uses>>,
    /// The bodies being computed, outermost first, each with the uses found in its last walk and
    /// the lowest position among them that its walk read, if any.
    open: Vec<(Unit, Rc<Uses>, Option<usize>)>,
}

impl Checker<'_> {
    /// Reports the `validate` grant each declassifying use in an item's bodies needs.
    pub fn check_uses(&self, item: ItemId) {
        let units: Vec<Unit> = match &self.sig(item).kind {
            SigKind::Interface { methods } => (0..methods.len())
                .map(|index| (item, Some(index)))
                .collect(),
            _ => vec![(item, None)],
        };
        for unit in units {
            for &token in &self.body_uses(unit).declassifying {
                self.require(item, token, Authority::Validate);
            }
        }
    }

    /// The codec uses of a function: of its body, or for a supplied `Codec` member of
    /// `mem::wire`, `text::json` or `text::construction`, the decoder of `T` for `decode`,
    /// `canonical` and `erased` and its encoder for `encode` and `try_encode`.
    pub fn codec_uses(&self, function: ItemId) -> Vec<(Param, Codec)> {
        let Some(head) = self.fn_sig(function) else {
            return Vec::new();
        };
        if head.generics.is_empty() {
            return Vec::new();
        }
        if self.db.ast(function).kind.is_fn_with_body() {
            return self
                .body_uses((function, None))
                .codecs
                .iter()
                .copied()
                .collect();
        }
        if !self.db.is_std(function) {
            return Vec::new();
        }
        let codec = match self.db.canonical(function).as_str() {
            "mem::wire::Codec.decode"
            | "mem::wire::Codec.canonical"
            | "mem::wire::Codec.erased"
            | "text::json::Codec.decode"
            | "text::construction::Codec.decode" => Codec::Decode,
            "mem::wire::Codec.encode"
            | "mem::wire::Codec.try_encode"
            | "text::json::Codec.encode"
            | "text::json::Codec.try_encode" => Codec::Encode,
            _ => return Vec::new(),
        };
        vec![(head.generics[0].param, codec)]
    }

    /// The uses of a body, computed once; within a cycle of bodies reaching each other, again
    /// until the outermost one's are the same as its last walk's.
    fn body_uses(&self, unit: Unit) -> Rc<Uses> {
        self.db.note(unit.0);
        {
            let mut cache = self.body_uses.borrow_mut();
            if let Some(done) = cache.done.get(&unit) {
                return done.clone();
            }
            if let Some(position) = cache.open.iter().position(|(open, ..)| *open == unit) {
                let last = cache.open.last_mut().expect("an open body");
                last.2 = Some(last.2.map_or(position, |low| low.min(position)));
                return cache.open[position].1.clone();
            }
            cache.open.push((unit, Rc::new(Uses::default()), None));
        }
        loop {
            let found = Rc::new(self.db.about(Some(unit.0), || self.walk_uses(unit)));
            let mut cache = self.body_uses.borrow_mut();
            let position = cache.open.len() - 1;
            let (_, last, low) = cache.open.last_mut().expect("the body computed");
            match *low {
                Some(low) if low < position => {
                    cache.open.pop();
                    let enclosing = cache.open.last_mut().expect("an enclosing body");
                    enclosing.2 = Some(enclosing.2.map_or(low, |known| known.min(low)));
                    return found;
                }
                Some(_) if *last != found => *last = found,
                _ => {
                    cache.open.pop();
                    cache.done.insert(unit, found.clone());
                    return found;
                }
            }
        }
    }

    fn walk_uses(&self, unit: Unit) -> Uses {
        let (body, own) = match unit {
            (item, None) => {
                let own = self
                    .fn_sig(item)
                    .map(|head| head.generics)
                    .unwrap_or_default();
                (self.typed(item), own)
            }
            (interface, Some(method)) => {
                let known = self
                    .method_bodies
                    .borrow()
                    .get(&(interface, method))
                    .cloned();
                let body = known.or_else(|| {
                    self.typed(interface);
                    self.method_bodies
                        .borrow()
                        .get(&(interface, method))
                        .cloned()
                });
                let sig = self.sig(interface);
                let mut own = sig.generics.clone();
                if let SigKind::Interface { methods } = &sig.kind {
                    own.extend(methods[method].sig.generics.iter().cloned());
                }
                (body, own)
            }
        };
        let mut walk = Walk {
            ck: self,
            item: unit.0,
            own: own.iter().map(|def| def.param).collect(),
            uses: Uses::default(),
        };
        if let Some(body) = body {
            walk.body(&body);
        }
        walk.uses
    }
}

struct Walk<'w, 'c> {
    ck: &'w Checker<'c>,
    item: ItemId,
    /// The type parameters of the body.
    own: Vec<Param>,
    uses: Uses,
}

impl Walk<'_, '_> {
    fn body(&mut self, body: &TBody) {
        for clause in &body.clauses {
            self.expr(&clause.expr);
        }
        for example in &body.examples {
            self.body(&example.body);
        }
        if let Some(root) = &body.root {
            self.expr(root);
        }
    }

    fn expr(&mut self, expr: &TExpr) {
        let ck = self.ck;
        let item = self.item;
        let line = || ck.db.item(item).line(expr.token);
        match &expr.kind {
            TKind::Call(call) | TKind::Index(call) => ck.db.on_line(line(), || self.call(call)),
            TKind::FnValue { item, type_args } => {
                ck.db
                    .on_line(line(), || self.named(expr.token, *item, type_args));
            }
            _ => {}
        }
        expr.each_child(&mut |child| self.expr(child));
    }

    /// The uses a call names: those of the instantiation it calls, of the decoder or encoder
    /// an `Encode` method runs, and of the conformances of its receiver and its type arguments to
    /// their bounds, or of a type viewed as `Dyn`.
    fn call(&mut self, call: &Call) {
        let ck = self.ck;
        let token = call.token;
        match &call.callee {
            Callee::Function { item } => self.named(token, *item, &call.type_args),
            Callee::Method {
                interface,
                method,
                self_ty,
                dynamic: false,
            } => {
                if Some(interface.interface) == ck.db.core_item("Encode") {
                    let named =
                        |name| ck.interface_method(interface.interface, name) == Some(*method);
                    if named("decode") {
                        self.codec(token, self_ty, Codec::Decode);
                    } else if named("encode") {
                        self.codec(token, self_ty, Codec::Encode);
                    }
                }
                let own = call
                    .type_args
                    .get(1 + interface.args.len()..)
                    .unwrap_or_default();
                if Some(interface.interface) != ck.db.core_item("Encode")
                    && !matches!(self_ty, Ty::Param(_))
                {
                    self.conforms(token, self_ty, interface.interface, Some((*method, own)));
                }
                if let Some(head) = ck.method_head(interface, *method, self_ty) {
                    self.bounded(token, &head.generics, own);
                }
            }
            Callee::Builtin(Builtin::DynView) => {
                let Some(arg) = call.args.first() else {
                    return;
                };
                let (viewed, view) = match &call.head.result {
                    Ty::Dyn(_) => (Some(&arg.expr.ty), &call.head.result),
                    boxed => (arg.expr.ty.type_arg(0), boxed.type_arg(0).unwrap_or(boxed)),
                };
                if let (Some(viewed), Ty::Dyn(bound)) = (viewed, view)
                    && !matches!(viewed, Ty::Param(_))
                {
                    self.conforms(token, viewed, bound.interface, None);
                }
            }
            _ => {}
        }
    }

    /// The uses of naming an instantiation of a function: those of the conformances of its type
    /// arguments to their bounds, and of the decoders and encoders its codec uses substitute.
    fn named(&mut self, token: Tok, function: ItemId, args: &[Arg]) {
        let Some(head) = self.ck.fn_sig(function) else {
            return;
        };
        if head.generics.is_empty() {
            return;
        }
        self.bounded(token, &head.generics, args);
        let subst = Subst::of(head.generics.iter().map(|def| def.param), args);
        for (param, codec) in self.ck.codec_uses(function) {
            self.codec(token, &subst.ty(&Ty::Param(param)), codec);
        }
    }

    /// The uses of the conformances of type arguments that are no type parameters to the bounds
    /// of the generic parameters they are given to.
    fn bounded(&mut self, token: Tok, generics: &[GenericDef], args: &[Arg]) {
        for (def, arg) in generics.iter().zip(args) {
            if let Arg::Ty(ty) = arg
                && !matches!(ty, Ty::Param(_))
            {
                for bound in &def.bounds {
                    self.conforms(token, ty, bound.interface, None);
                }
            }
        }
    }

    /// The uses of a type's conformance to an interface other than `Encode` and the property
    /// interfaces, a call of one of its methods with its own type arguments `called` included:
    /// through an impl line, those its implementing functions and default bodies name; through a
    /// derive, those of its constituent types' conformances; through the toolchain, those of its
    /// type arguments'.
    fn conforms(&mut self, token: Tok, ty: &Ty, interface: ItemId, called: Called) {
        let ck = self.ck;
        let skipped = ["Encode", "Any"]
            .iter()
            .any(|name| ck.db.core_item(name) == Some(interface));
        if !skipped && ck.property_interface(interface).is_none() {
            self.conforms_from(token, ty, interface, called, &mut Vec::new());
        }
    }

    fn conforms_from(
        &mut self,
        token: Tok,
        ty: &Ty,
        interface: ItemId,
        called: Called,
        seen: &mut Vec<Ty>,
    ) {
        if ty.has_error() || seen.contains(ty) {
            return;
        }
        seen.push(ty.clone());
        let ck = self.ck;
        match ck.conformance(ty, interface) {
            Some((Conformance::Impl { line, .. }, args)) => {
                self.implemented(token, ty, line, (interface, &args), called);
            }
            Some((Conformance::Derive, _)) => {
                if let Ty::Item(item, args) = ty {
                    let subst = Subst::of(ck.sig(*item).generics.iter().map(|def| def.param), args);
                    for constituent in ck.constituents(*item) {
                        let constituent = subst.ty(&constituent);
                        self.conforms_from(token, &constituent, interface, called, seen);
                    }
                }
            }
            Some((Conformance::Builtin, _)) => {
                for arg in ty.args().iter().filter_map(Arg::ty) {
                    self.conforms_from(token, arg, interface, called, seen);
                }
            }
            _ => {}
        }
    }

    /// The uses of a type's conformance through an impl line to an interface with its arguments:
    /// each implementing function that is a validator, and what each implementing function or
    /// default body names with the type's arguments substituted, and the own type arguments of
    /// the method `called`.
    fn implemented(
        &mut self,
        token: Tok,
        ty: &Ty,
        line: ItemId,
        (interface, args): (ItemId, &[Arg]),
        called: Called,
    ) {
        let ck = self.ck;
        let sig = ck.sig(interface);
        let SigKind::Interface { methods } = &sig.kind else {
            return;
        };
        for (index, method) in methods.iter().enumerate() {
            let own = called
                .filter(|(method, _)| *method == index)
                .map_or(&[][..], |(_, own)| own);
            match ck.implementing_function(line, &method.name) {
                Some(function) => {
                    if ck.is_validator(function) {
                        self.uses.declassifying.insert(token);
                    }
                    let Some(head) = ck.fn_sig(function) else {
                        continue;
                    };
                    let (inherited, declared) = head.generics.split_at(head.inherited);
                    let mut subst = Subst::of(inherited.iter().map(|def| def.param), ty.args());
                    for (def, arg) in declared.iter().zip(own) {
                        subst.insert(def.param, arg.clone());
                    }
                    for (param, codec) in ck.codec_uses(function) {
                        self.codec(token, &subst.ty(&Ty::Param(param)), codec);
                    }
                }
                None if method.default => {
                    let mut subst = Subst::of(
                        sig.generics
                            .iter()
                            .filter(|def| !def.param.is_self())
                            .map(|def| def.param),
                        args,
                    );
                    subst.insert(
                        Param {
                            owner: interface,
                            index: Param::SELF,
                        },
                        Arg::Ty(ty.clone()),
                    );
                    for (def, arg) in method.sig.generics.iter().zip(own) {
                        subst.insert(def.param, arg.clone());
                    }
                    for &(param, codec) in &ck.body_uses((interface, Some(index))).codecs {
                        self.codec(token, &subst.ty(&Ty::Param(param)), codec);
                    }
                }
                None => {}
            }
        }
    }

    /// A use of the decoder or encoder of `ty`: of a type parameter of the body, one of its codec
    /// uses; of a type parameter of another item, which no argument substituted, declassifying;
    /// of another type, declassifying when it decodes no parsing type or encodes one holding an
    /// Untrusted instantiation, and a use of the same codec of each type parameter of the body the
    /// type holds.
    fn codec(&mut self, token: Tok, ty: &Ty, codec: Codec) {
        if ty.has_error() {
            return;
        }
        if let Ty::Param(param) = ty {
            if self.own.contains(param) {
                self.uses.codecs.insert((*param, codec));
            } else {
                self.uses.declassifying.insert(token);
            }
            return;
        }
        let declassifying = match codec {
            Codec::Decode => !self.ck.is_parsing(ty),
            Codec::Encode => self.ck.holds_untrusted(ty),
        };
        if declassifying {
            self.uses.declassifying.insert(token);
        }
        let held = RefCell::new(Vec::new());
        self.ck.any_part(ty, &|part| {
            if let Ty::Param(param) = part {
                held.borrow_mut().push(*param);
            }
            false
        });
        for param in held.into_inner() {
            if self.own.contains(&param) {
                self.uses.codecs.insert((param, codec));
            }
        }
    }
}
