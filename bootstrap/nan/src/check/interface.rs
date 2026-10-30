//! Impl lines and conformance: what an impl line targets and lists, where it lies, coherence, the
//! methods its target implements and their heads, collisions with derived methods, the clauses
//! of interface methods, and the interfaces `Dyn` accepts.

use super::Checker;
use super::db::{ItemId, TypeKey};
use super::sig::{FnSig, SigKind, param_arg};
use super::ty::{Bound, Subst, Ty};
use crate::diagnostic::{Code, Offer};
use crate::syntax::ast::{self, ClauseKind, ItemKind, Visibility};

/// The property interfaces, which no impl line names.
const PROPERTY_INTERFACES: [&str; 5] = [
    "core::Any",
    "core::Copy",
    "core::Send",
    "core::Inert",
    "core::AnyBits",
];

/// How visible an item is, private least.
pub fn openness(visibility: Visibility) -> u8 {
    match visibility {
        Visibility::Private => 0,
        Visibility::Pkg => 1,
        Visibility::Pub => 2,
    }
}

/// The narrower of two visibilities.
pub fn narrower(left: Visibility, right: Visibility) -> Visibility {
    if openness(left) < openness(right) {
        left
    } else {
        right
    }
}

impl Checker<'_> {
    /// The rules of an impl line in order; a line failing where it lies or coherence is otherwise
    /// not checked.
    pub fn check_impl(&self, item: ItemId) -> bool {
        let ast = self.db.ast(item);
        let ItemKind::Impl(implementation) = &ast.kind else {
            return false;
        };
        let token = ast.token;
        let module = self.db.module_of(item);
        let SigKind::Impl {
            target,
            interface: Some(bound),
            ..
        } = &self.sig(item).kind
        else {
            return false;
        };
        let Some(key) = *target else {
            match self.db.resolve(module, &implementation.target) {
                Ok(_) => self.report(item, token, Code::InterfaceImplTarget),
                Err(error) => {
                    if let Some(code) = error.code {
                        self.report_problem(item, error.token, code);
                    }
                }
            }
            return false;
        };
        if let Some(params) = &implementation.params {
            let generics = self.type_generics(key);
            let listed = params.len() == generics.len()
                && params
                    .iter()
                    .zip(&generics)
                    .all(|(param, def)| param.name.text == def.name);
            if !listed {
                self.report(item, token, Code::InterfaceImplParameters);
                return false;
            }
        }
        let target_module = match key {
            TypeKey::Item(target) => Some(self.db.module_of(target)),
            _ => None,
        };
        let interface_path = self.db.canonical(bound.interface);
        if target_module != Some(module) && self.db.module_of(bound.interface) != module {
            self.report(item, token, Code::InterfaceImplMisplaced);
            return false;
        }
        if PROPERTY_INTERFACES.contains(&interface_path.as_str()) {
            self.report(item, token, Code::PropPropertyImpl);
            return false;
        }
        if interface_path == "core::Encode" && !self.db.is_std(item) {
            self.report(item, token, Code::EncodeManualImpl);
            return false;
        }
        if self.duplicate_impl(item, key, bound.interface) {
            self.report(item, token, Code::InterfaceDuplicateImpl);
            return false;
        }
        let SigKind::Interface { methods } = &self.sig(bound.interface).kind else {
            return false;
        };
        for method in methods {
            let implemented =
                self.implementing_function(item, &method.name).is_some() || method.default;
            if !implemented {
                self.report(item, token, Code::InterfaceMissingMethod);
                return true;
            }
        }
        let target_package = target_module.map(|target| self.db.module(target).package);
        let collides = target_module.is_some_and(|target| target != module)
            && target_package == Some(self.db.package_of(bound.interface))
            && methods.iter().any(|method| {
                self.db
                    .members(key, &method.name)
                    .iter()
                    .any(|function| Some(self.db.module_of(*function)) == target_module)
            });
        if collides {
            self.report(item, token, Code::InterfaceMethodCollision);
        }
        true
    }

    /// Whether an impl line repeats the conformance of an earlier impl line or derive of its
    /// target to its interface, by file path then line.
    fn duplicate_impl(&self, line: ItemId, key: TypeKey, interface: ItemId) -> bool {
        let position = |item: ItemId| {
            let data = self.db.item(item);
            (data.file.path.clone(), data.line(data.ast.token))
        };
        let own = position(line);
        let earlier_line = self.db.about(None, || {
            self.db.items.iter().enumerate().any(|(index, data)| {
                let other = ItemId(index as u32);
                other != line
                    && matches!(data.ast.kind, ItemKind::Impl(_))
                    && matches!(&self.sig(other).kind, SigKind::Impl { target: Some(found), interface: Some(bound), .. }
                        if *found == key && bound.interface == interface)
                    && position(other) < own
            })
        });
        let derived = match key {
            TypeKey::Item(target) => {
                let name = self.db.ast(interface).name().unwrap_or_default();
                self.db.is_std(interface)
                    && derives(self.db.ast(target))
                        .iter()
                        .any(|entry| entry.text == name)
                    && position(target) < own
            }
            _ => false,
        };
        earlier_line || derived
    }

    /// The head and visibility of a function implementing a method of an impl line of its module.
    pub fn check_implementing(&self, function: ItemId, sig: &FnSig) {
        let token = self.db.ast(function).token;
        let ItemKind::Fn(written) = &self.db.ast(function).kind else {
            return;
        };
        for line in self.implemented_lines(function) {
            let SigKind::Impl {
                target_ty,
                interface: Some(bound),
                ..
            } = &self.sig(line).kind
            else {
                continue;
            };
            let Some(method) = self.interface_method(bound.interface, &written.label.name.text)
            else {
                continue;
            };
            let Some(wanted) = self.method_head(bound, method, target_ty) else {
                continue;
            };
            if !self.head_matches(sig, &wanted) {
                self.report(function, token, Code::InterfaceHeadMismatch);
                return;
            }
            let target_visibility = match self.db.owners.get(&function) {
                Some(TypeKey::Item(target)) => self.db.ast(*target).visibility,
                _ => Visibility::Pub,
            };
            let interface_visibility = self.db.ast(bound.interface).visibility;
            let expected = openness(target_visibility).min(openness(interface_visibility));
            let written_visibility = self.db.ast(function).visibility;
            if openness(written_visibility) != expected {
                let word = ["", "pkg", "pub"][usize::from(expected)];
                let fix = match (written_visibility, word) {
                    (Visibility::Private, word) => {
                        self.fix_inserting(function, token, &format!("{word} "))
                    }
                    (_, "") => self.fix_deleting_forward(function, token, token),
                    (_, word) => self.fix_replacing(function, token, token, word),
                };
                self.report_fixed(
                    function,
                    (token, token),
                    Code::InterfaceImplVisibility,
                    vec![fix],
                    Offer::Each,
                );
                return;
            }
        }
    }

    /// Whether an implementing function's head matches its method's, `Self` and the interface's
    /// parameters substituted: its modifier, own generic parameters, parameters, result, failure
    /// type and waiting. The result's droppability is judged under the function's own bounds.
    fn head_matches(&self, written: &FnSig, wanted: &FnSig) -> bool {
        let saved = self.scope.replace(written.generics.clone());
        let droppable_result = !self.has(&written.result, super::props::Property::Linear);
        *self.scope.borrow_mut() = saved;
        let modifier = written.modifier == wanted.modifier
            || (wanted.is_proj() && written.modifier.is_none() && droppable_result);
        let own = written.own_generics();
        if !modifier || own.len() != wanted.generics.len() {
            return false;
        }
        let renamed = Subst::of(
            wanted.generics.iter().map(|def| def.param),
            &own.iter().map(param_arg).collect::<Vec<_>>(),
        );
        let bounds = own.iter().zip(&wanted.generics).all(|(own, def)| {
            let renamed: Vec<Bound> = def
                .bounds
                .iter()
                .map(|bound| renamed.bound(bound))
                .collect();
            own.bounds == renamed
        });
        let params = written.params.len() == wanted.params.len()
            && written
                .params
                .iter()
                .zip(&wanted.params)
                .all(|(own, param)| {
                    own.name == param.name
                        && own.mode == param.mode
                        && own.ty == renamed.ty(&param.ty)
                });
        let fails = match (&written.fails, &wanted.fails) {
            (None, _) => true,
            (Some(own), Some(fails)) => *own == renamed.ty(fails),
            (Some(_), None) => false,
        };
        bounds
            && params
            && written.result == renamed.ty(&wanted.result)
            && fails
            && (!written.wait || wanted.wait)
    }

    /// Functions, fields and variants named like a method of an interface a struct or enum
    /// derives.
    pub fn check_derived_collisions(&self, item: ItemId) {
        let ast = self.db.ast(item);
        let mut methods: Vec<String> = Vec::new();
        for entry in derives(ast) {
            if let Some(interface) = self.db.core_item(&entry.text)
                && let SigKind::Interface { methods: declared } = &self.sig(interface).kind
            {
                methods.extend(declared.iter().map(|method| method.name.clone()));
            }
        }
        if methods.is_empty() {
            return;
        }
        let module = self.db.module_of(item);
        for name in &methods {
            for &function in self.db.members(TypeKey::Item(item), name) {
                if self.db.module_of(function) == module {
                    self.report(
                        function,
                        self.db.ast(function).token,
                        Code::InterfaceMethodCollision,
                    );
                }
            }
        }
        let members: Vec<&ast::Name> = match &ast.kind {
            ItemKind::Struct(structure) => structure.fields().map(|field| &field.name).collect(),
            ItemKind::Enum(enumeration) => enumeration
                .variants
                .iter()
                .map(|variant| &variant.name)
                .collect(),
            _ => Vec::new(),
        };
        for member in members {
            if methods.contains(&member.text) {
                self.report(item, member.token, Code::InterfaceMethodCollision);
            }
        }
    }

    /// The clauses of an interface's methods: requires, ensures and `todo`, with `decreases` and
    /// `waive` beside a default body.
    pub fn check_method_clauses(&self, item: ItemId, interface: &ast::InterfaceItem) {
        for method in &interface.methods {
            for clause in &method.clauses {
                let allowed = match clause.kind {
                    ClauseKind::Requires(_) | ClauseKind::Ensures(_) | ClauseKind::Missing => true,
                    ClauseKind::Decreases(_) | ClauseKind::Waive { .. } => method.body.is_some(),
                    ClauseKind::Example(_) | ClauseKind::Undo { .. } => false,
                };
                if !allowed {
                    self.report(item, clause.token, Code::InterfaceMethodClause);
                }
            }
        }
    }

    /// Whether `Dyn` accepts an interface: no property interface, each method taking `self` or
    /// `mut self` first, without generic parameters of its own and naming `Self` nowhere else.
    pub fn dyn_usable(&self, interface: ItemId) -> bool {
        if PROPERTY_INTERFACES.contains(&self.db.canonical(interface).as_str()) {
            return false;
        }
        let sig = self.sig(interface);
        let SigKind::Interface { methods } = &sig.kind else {
            return false;
        };
        let names_self = |ty: &Ty| ty.mentions(&|param| param.is_self());
        methods.iter().all(|method| {
            let head = &method.sig;
            let receiver = head
                .params
                .first()
                .is_some_and(|param| param.is_self && param.mode != ast::Mode::Take);
            receiver
                && head.own_generics().is_empty()
                && !head.params[1..].iter().any(|param| names_self(&param.ty))
                && !names_self(&head.result)
                && !head.fails.as_ref().is_some_and(names_self)
        })
    }
}

/// The entries of a struct's or enum's `derives` clause.
fn derives(item: &ast::Item) -> &[ast::Name] {
    match &item.kind {
        ItemKind::Struct(structure) => &structure.derives,
        ItemKind::Enum(enumeration) => &enumeration.derives,
        _ => &[],
    }
}
