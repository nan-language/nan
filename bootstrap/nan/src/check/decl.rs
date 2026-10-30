//! The rules of declarations: names, their conventions, duplicates and clashes, and the shapes of
//! heads that their names promise.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::Checker;
use super::db::{ItemId, ModuleId, PackageId, TypeKey};
use super::interface::openness;
use super::props::Property;
use super::resolve::Named;
use super::sig::SigKind;
use super::ty::{Scalar, Ty};
use crate::diagnostic::{Code, Offer};
use crate::syntax::ast::{self, FnModifier, ItemKind, Mode};
use crate::syntax::parser::is_pascal;
use crate::workspace::Authority;

/// The naming convention a declared name follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Case {
    Snake,
    Pascal,
    Screaming,
}

pub fn is_snake(name: &str) -> bool {
    follows(name, Case::Snake)
}

/// Whether an item's name belongs to its module's values namespace.
pub fn in_values_namespace(item: &ast::Item) -> bool {
    matches!(
        &item.kind,
        ItemKind::Const(_) | ItemKind::Registry(_) | ItemKind::Contribute(_) | ItemKind::Test(_)
    ) || matches!(&item.kind, ItemKind::Fn(function) if function.label.owner.is_none())
}

impl Checker<'_> {
    /// The folded names of a module's items in the values namespace, which no local may fold to.
    pub fn module_values(&self, module: ModuleId) -> Rc<HashSet<String>> {
        if let Some(known) = self.module_values.borrow().get(&module) {
            return known.clone();
        }
        let names: HashSet<String> = self
            .db
            .module(module)
            .items
            .iter()
            .map(|item| self.db.declaration(*item))
            .filter(|ast| in_values_namespace(ast))
            .filter_map(declared_name)
            .map(|declared| fold(&declared.text))
            .collect();
        let names = Rc::new(names);
        self.module_values
            .borrow_mut()
            .insert(module, names.clone());
        names
    }
}

/// A name written in a convention: split into words at underscores and before each uppercase
/// letter following a lowercase letter or a digit, empty words dropped, then joined lowercase with
/// `_`, capitalised, or uppercase with `_`.
pub fn converted(name: &str, case: Case) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut previous: Option<char> = None;
    for c in name.chars() {
        let starts = c == '_'
            || c.is_ascii_uppercase()
                && previous.is_some_and(|previous| {
                    previous.is_ascii_lowercase() || previous.is_ascii_digit()
                });
        if starts || words.is_empty() {
            words.push(String::new());
        }
        if c != '_' {
            words.last_mut().expect("a word").push(c);
        }
        previous = Some(c);
    }
    words.retain(|word| !word.is_empty());
    match case {
        Case::Snake => words.join("_").to_ascii_lowercase(),
        Case::Screaming => words.join("_").to_ascii_uppercase(),
        Case::Pascal => words
            .iter()
            .map(|word| {
                let lower = word.to_ascii_lowercase();
                let mut chars = lower.chars();
                chars
                    .next()
                    .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
                    .unwrap_or_default()
            })
            .collect(),
    }
}

pub fn follows(name: &str, case: Case) -> bool {
    let words_of = |lower: bool| {
        name.split('_').all(|word| {
            !word.is_empty()
                && word.chars().all(|c| {
                    c.is_ascii_digit()
                        || if lower {
                            c.is_ascii_lowercase()
                        } else {
                            c.is_ascii_uppercase()
                        }
                })
        })
    };
    match case {
        Case::Snake => name.starts_with(|c: char| c.is_ascii_lowercase()) && words_of(true),
        Case::Pascal => is_pascal(name),
        Case::Screaming => {
            name.len() >= 2 && name.starts_with(|c: char| c.is_ascii_uppercase()) && words_of(false)
        }
    }
}

/// A name with each uppercase letter made lowercase and each underscore removed.
pub fn fold(name: &str) -> String {
    name.chars()
        .filter(|c| *c != '_')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Whether a name folds to a core vocabulary name, or with `scalars_only` to a scalar's.
fn folds_to_core(name: &str, scalars_only: bool) -> bool {
    let folded = fold(name);
    let core = [
        "u8", "u16", "u32", "u64", "usize", "i8", "i16", "i32", "i64", "isize", "f32", "f64",
        "bool", "char",
    ];
    if core.contains(&folded.as_str()) {
        return true;
    }
    !scalars_only
        && [
            "unit",
            "never",
            "option",
            "some",
            "none",
            "result",
            "ok",
            "err",
            "array",
            "slice",
            "slicemut",
            "str",
            "untrusted",
            "dyn",
            "eq",
            "ord",
            "hash",
            "copy",
            "default",
            "render",
            "encode",
            "generate",
            "send",
            "inert",
            "anybits",
            "any",
        ]
        .contains(&folded.as_str())
}

impl Checker<'_> {
    pub fn check_package(&self, package: PackageId) {
        self.check_manifest(package);
        self.check_program(package);
        self.check_contributors(package);
        for &module in &self.db.package(package).modules {
            self.check_module_names(module);
            for &item in &self.db.module(module).items {
                self.judge_alone(item);
            }
        }
        self.check_package_whole(package);
    }

    fn check_case(&self, item: ItemId, name: &ast::Name, case: Case) {
        if !follows(&name.text, case) {
            self.report(item, name.token, Code::NameCase);
            self.misnamed.borrow_mut().push((item, name.token, case));
        }
    }

    /// The duplicate and folded clashes among the names a module declares.
    fn check_module_names(&self, module: ModuleId) {
        let data = self.db.module(module);
        let mut values: Vec<(ItemId, &ast::Name)> = Vec::new();
        let mut types: Vec<(ItemId, &ast::Name)> = Vec::new();
        let mut seen: HashMap<String, ItemId> = HashMap::new();
        for &item in &data.items {
            let ast = self.db.ast(item);
            let Some(name) = declared_name(ast) else {
                continue;
            };
            if super::db::is_member(ast) {
                continue;
            }
            if seen.contains_key(&name.text) {
                self.report(item, name.token, Code::ItemDuplicate);
                continue;
            }
            seen.insert(name.text.clone(), item);
            match ast.kind {
                ItemKind::Struct(_)
                | ItemKind::Enum(_)
                | ItemKind::Interface(_)
                | ItemKind::Layout(_)
                | ItemKind::Registers(_) => types.push((item, name)),
                _ => values.push((item, name)),
            }
        }
        for namespace in [values, types] {
            let mut folded: HashMap<String, ItemId> = HashMap::new();
            for (item, name) in namespace {
                if folded.insert(fold(&name.text), item).is_some() {
                    self.report(item, name.token, Code::NameClash);
                }
            }
        }
        self.check_member_names(module);
    }

    /// The members a module declares that are named like an earlier member of their type in
    /// the module, like one in another module when outside the type's module, or like a field or
    /// variant of the type.
    fn check_member_names(&self, module: ModuleId) {
        let mut seen: HashSet<(TypeKey, &str)> = HashSet::new();
        for &item in &self.db.module(module).items {
            let ItemKind::Fn(function) = &self.db.ast(item).kind else {
                continue;
            };
            let Some(&key) = self.db.owners.get(&item) else {
                continue;
            };
            let name = &function.label.name;
            let repeated = !seen.insert((key, name.text.as_str()));
            let elsewhere = !self.declares_type(module, key)
                && self
                    .db
                    .members(key, &name.text)
                    .iter()
                    .any(|other| self.db.module_of(*other) != module);
            if repeated || elsewhere || self.names_part(key, &name.text) {
                self.report(item, name.token, Code::ItemDuplicate);
            }
        }
    }

    /// Whether a module declares a type: a struct, enum or layout of it, or for the scalars and
    /// `Unit`, the root module of core.
    fn declares_type(&self, module: ModuleId, key: TypeKey) -> bool {
        match key {
            TypeKey::Item(target) => self.db.module_of(target) == module,
            TypeKey::Scalar(_) | TypeKey::Unit => {
                self.db.package(self.db.core()).root == Some(module)
            }
        }
    }

    /// Whether a member name is a field or variant of its type.
    fn names_part(&self, key: TypeKey, name: &str) -> bool {
        let TypeKey::Item(target) = key else {
            return false;
        };
        match &self.sig(target).kind {
            SigKind::Struct { fields } | SigKind::Layout { fields, .. } => {
                fields.iter().any(|field| field.name == name)
            }
            SigKind::Enum { variants } => variants.iter().any(|variant| variant.name == name),
            _ => false,
        }
    }

    pub(super) fn check_item(&self, item: ItemId) {
        let sig = self.sig(item);
        self.report_problems(item, &sig.problems);
        self.check_docs(item);
        self.check_doc_binding(item);
        self.check_test_only(item);
        self.check_interface_names(item);
        self.check_value_types(item);
        self.check_history_forms(item);
        let ast = &self.db.ast(item).clone();
        match &ast.kind {
            ItemKind::Fn(function) => {
                if !matches!(function.modifier, Some((FnModifier::Foreign, _))) {
                    self.check_case(item, &function.label.name, Case::Snake);
                }
                if function.label.owner.is_none()
                    && !self.in_core(item)
                    && folds_to_core(&function.label.name.text, true)
                {
                    self.report(item, function.label.name.token, Code::NameCoreRedefined);
                }
                self.check_generic_names(item, &function.head.generics);
                self.check_params(item, &function.head.params);
                if let SigKind::Fn(fn_sig) = &sig.kind {
                    self.check_prefix(item, &function.label.name, fn_sig);
                    self.check_unused_generics(item, fn_sig);
                    if let Some(code) = self.fn_head_problem(fn_sig) {
                        let fixes = match (code, &function.head.uses) {
                            (Code::EffectForeignWait, Some(uses)) => {
                                vec![self.fix_deleting(item, uses.token, uses.word.token)]
                            }
                            _ => Vec::new(),
                        };
                        self.report_fixed(item, (ast.token, ast.token), code, fixes, Offer::Each);
                    } else {
                        self.check_fails(
                            item,
                            fn_sig,
                            &function.head,
                            matches!(function.body, ast::FnBody::None),
                        );
                    }
                    if let Some(owner) = &function.label.owner {
                        self.check_method_placement(item, owner);
                        self.check_implementing(item, fn_sig);
                        if function.label.name.text == "abandon" && !fn_sig.abandon_head() {
                            self.report(item, ast.token, Code::DeferAbandonHead);
                        }
                    }
                    let entry = self.is_entry(item)
                        || matches!(
                            fn_sig.modifier,
                            Some(FnModifier::Interrupt | FnModifier::Landing)
                        );
                    if entry {
                        self.check_stack_bound(item, &fn_sig.limits);
                    }
                    let block = match &function.body {
                        ast::FnBody::Block(block) => Some(block),
                        _ => None,
                    };
                    self.check_thresholds(item, &function.clauses, block);
                    match function.modifier.map(|(modifier, _)| modifier) {
                        Some(FnModifier::Validator) => self.check_validator(item, fn_sig),
                        Some(FnModifier::Landing) => {
                            self.check_landing(item, fn_sig);
                            self.require(item, ast.token, Authority::Priv);
                        }
                        Some(FnModifier::Foreign) => {
                            self.check_foreign(item, function, fn_sig);
                            self.require(item, ast.token, Authority::Trusted);
                        }
                        _ => {}
                    }
                    self.check_dma_completion(item, fn_sig);
                }
            }
            ItemKind::Struct(structure) => {
                self.check_type_item(item);
                self.check_derived_collisions(item);
                self.check_decoder_derive(item);
                let generated = structure
                    .derives
                    .iter()
                    .any(|entry| entry.text == "Generate");
                if generated && self.derive_problem(item, "Generate").is_none() {
                    self.check_generated(item);
                }
                self.check_type_name(item, &structure.name);
                self.check_generic_names(item, &structure.generics);
                for field in structure.fields() {
                    self.check_case(item, &field.name, Case::Snake);
                }
            }
            ItemKind::Enum(enumeration) => {
                self.check_type_item(item);
                self.check_from_variants(item);
                self.check_derived_collisions(item);
                self.check_decoder_derive(item);
                self.check_type_name(item, &enumeration.name);
                self.check_generic_names(item, &enumeration.generics);
                for variant in &enumeration.variants {
                    self.check_case(item, &variant.name, Case::Pascal);
                    for field in variant.payload.iter().flatten() {
                        if let Some(label) = field.label.as_ref().and_then(label_name) {
                            self.check_case(item, &label, Case::Snake);
                        }
                    }
                }
            }
            ItemKind::Interface(interface) => {
                self.check_type_name(item, &interface.name);
                self.check_generic_names(item, &interface.generics);
                for method in &interface.methods {
                    self.check_case(item, &method.name, Case::Snake);
                    self.check_generic_names(item, &method.head.generics);
                    self.check_params(item, &method.head.params);
                }
                if let SigKind::Interface { methods } = &sig.kind {
                    for (method, method_sig) in interface.methods.iter().zip(methods) {
                        self.check_prefix(item, &method.name, &method_sig.sig);
                        self.check_unused_generics(item, &method_sig.sig);
                        if let Some(code) = self.fn_head_problem(&method_sig.sig) {
                            self.report(item, method.token, code);
                        } else {
                            self.check_fails(
                                item,
                                &method_sig.sig,
                                &method.head,
                                method.body.is_none(),
                            );
                        }
                        self.check_thresholds(item, &method.clauses, method.body.as_ref());
                    }
                }
                self.check_method_clauses(item, interface);
            }
            ItemKind::Layout(layout) => {
                self.check_layout(item);
                self.check_type_name(item, &layout.name);
                for field in &layout.fields {
                    self.check_case(item, &field.name, Case::Snake);
                }
                for tag in layout.tags.iter().flatten() {
                    self.check_case(item, &tag.name, Case::Pascal);
                }
            }
            ItemKind::Registers(registers) => {
                self.check_registers(item);
                self.check_type_name(item, &registers.name);
                for register in &registers.registers {
                    self.check_case(item, &register.name, Case::Snake);
                }
            }
            ItemKind::Const(constant) => {
                if let SigKind::Const { ty } = &sig.kind
                    && !ty.has_error()
                    && (!self.has(ty, Property::Inert) || self.has(ty, Property::Linear))
                {
                    self.report(item, constant.name.token, Code::ItemConstType);
                }
                self.check_case(item, &constant.name, Case::Screaming);
                if !self.in_core(item) && folds_to_core(&constant.name.text, true) {
                    self.report(item, constant.name.token, Code::NameCoreRedefined);
                }
            }
            ItemKind::Registry(registry) => {
                self.check_registry(item, registry);
                self.check_case(item, &registry.name, Case::Snake);
            }
            ItemKind::Contribute(contribution) => {
                self.check_contribution(item, contribution);
                self.check_case(item, &contribution.name, Case::Snake);
            }
            ItemKind::Test(test) => {
                if test.disputed {
                    self.report(item, ast.token, Code::TodoDisputed);
                }
                self.check_case(item, &test.name, Case::Snake);
                self.check_test_head(item, test);
                self.check_test_reach(item, test);
            }
            ItemKind::Retired(_) => self.check_tombstone(item),
            ItemKind::Impl(_) if !self.check_generate_impl(item) && self.check_impl(item) => {
                self.check_capability_impl(item);
                self.check_decoder_impl(item);
                self.check_component_impl(item);
            }
            _ => {}
        }
        if let Some(generics) = generic_params(ast) {
            self.where_conditions(item, generics);
        }
        if let Some(body) = self.check_bodies(item) {
            let body = std::rc::Rc::new(body);
            let proj = matches!(&ast.kind, ItemKind::Fn(function)
                if function.modifier.is_some_and(|(modifier, _)| modifier == FnModifier::Proj));
            self.check_flow(item, &body, proj);
            if matches!(ast.kind, ItemKind::Fn(_) | ItemKind::Const(_)) {
                self.check_float_use(item, &body);
            }
            self.bodies.borrow_mut().insert(item, body.clone());
            if ast.kind.is_fn_with_body() {
                self.check_limits(item, &body);
            }
            self.check_expect_purity(item, &body);
            if let ItemKind::Fn(function) = &ast.kind {
                self.check_decreases(item, function);
            }
            if let ItemKind::Test(test) = &ast.kind {
                self.check_property_test(item, test, &body);
            }
        }
        if self.is_entry(item) {
            self.check_entry_head(item);
        }
    }

    /// The decreases clauses of a function: one is needed when it belongs to a cycle or reaches
    /// a failed construct, and every other is unneeded, its fix deleting it.
    fn check_decreases(&self, item: ItemId, function: &ast::FnItem) {
        let clauses = function
            .clauses
            .iter()
            .filter_map(|clause| match &clause.kind {
                ast::ClauseKind::Decreases(measure) => Some((clause.token, measure.last)),
                _ => None,
            });
        let mut clauses = clauses.peekable();
        let needed = self.in_cycle(item) || clauses.peek().is_some() && self.reaches_failure(item);
        for (first, last) in clauses.skip(usize::from(needed)) {
            let fix = self.fix_deleting_lines(item, first, last);
            self.report_fixed(
                item,
                (first, last),
                Code::ContractDecreasesUnneeded,
                vec![fix],
                Offer::Each,
            );
        }
    }

    /// Whether an item belongs to `core`, which declares the core vocabulary.
    fn in_core(&self, item: ItemId) -> bool {
        self.db.package_of(item) == self.db.core()
    }

    /// A type's name: PascalCase, and outside core no core vocabulary name.
    fn check_type_name(&self, item: ItemId, name: &ast::Name) {
        self.check_case(item, name, Case::Pascal);
        if !self.in_core(item) && folds_to_core(&name.text, false) {
            self.report(item, name.token, Code::NameCoreRedefined);
        }
    }

    /// The generic parameters an item or method declares: their names, the type of a const
    /// parameter and where a `where` clause stands.
    fn check_generic_names(&self, item: ItemId, generics: &[ast::GenericParam]) {
        let mut seen: Vec<String> = Vec::new();
        for param in generics {
            let constant = param
                .bounds
                .first()
                .and_then(|bound| match &bound.path.segments[..] {
                    [name] if bound.args.is_none() => Scalar::from_name(&name.text),
                    _ => None,
                });
            match constant {
                Some(scalar)
                    if !(scalar.is_integer() || scalar == Scalar::Bool)
                        || param.bounds.len() > 1 =>
                {
                    self.report(item, param.name.token, Code::GenericConstType);
                }
                None if param.where_clause.is_some() => {
                    self.report(item, param.name.token, Code::GenericWhereMisplaced);
                }
                _ => {}
            }
            self.check_case(item, &param.name, Case::Pascal);
            if !self.in_core(item) && folds_to_core(&param.name.text, false) {
                self.report(item, param.name.token, Code::NameCoreRedefined);
            }
            let folded = fold(&param.name.text);
            if seen.contains(&folded) {
                self.report(item, param.name.token, Code::NameClash);
            }
            seen.push(folded);
        }
    }

    /// The items the interface parts of a `pub` or `pkg` item name are at least as visible: its
    /// generic bounds, head and owner type, field, payload, register and entry types.
    fn check_interface_names(&self, item: ItemId) {
        let ast = self.db.ast(item);
        let own = openness(ast.visibility);
        if own == 0 {
            return;
        }
        let mut types: Vec<(&ast::TypeExpr, u8)> = Vec::new();
        let mut bounds: Vec<&ast::Bound> = Vec::new();
        let mut paths: Vec<&ast::Path> = Vec::new();
        match &ast.kind {
            ItemKind::Fn(function) => {
                head_parts(&function.head, own, &mut types, &mut bounds);
                paths.extend(function.label.owner.iter());
            }
            ItemKind::Struct(structure) => {
                bounds.extend(structure.generics.iter().flat_map(|param| &param.bounds));
                types.extend(
                    structure
                        .fields()
                        .map(|field| (&field.ty, own.min(openness(field.visibility)))),
                );
            }
            ItemKind::Enum(enumeration) => {
                bounds.extend(enumeration.generics.iter().flat_map(|param| &param.bounds));
                for variant in &enumeration.variants {
                    types.extend(
                        variant
                            .payload
                            .iter()
                            .flatten()
                            .map(|field| (&field.ty, own)),
                    );
                }
            }
            ItemKind::Interface(interface) => {
                bounds.extend(interface.generics.iter().flat_map(|param| &param.bounds));
                for method in &interface.methods {
                    head_parts(&method.head, own, &mut types, &mut bounds);
                }
            }
            ItemKind::Layout(layout) => {
                for field in &layout.fields {
                    if let ast::LayoutFieldKind::Typed { ty, .. } = &field.kind {
                        types.push((ty, own));
                    }
                }
            }
            ItemKind::Registers(registers) => {
                types.extend(
                    registers
                        .registers
                        .iter()
                        .map(|register| (&register.ty, own)),
                );
            }
            ItemKind::Const(constant) => types.extend(constant.ty.iter().map(|ty| (ty, own))),
            ItemKind::Registry(registry) => types.push((&registry.entry, own)),
            _ => {}
        }
        let module = self.db.module_of(item);
        let less_visible = |path: &ast::Path, required: u8| {
            matches!(self.db.resolve(module, path), Ok(Named::Item(named))
                if openness(self.db.ast(named).visibility) < required)
        };
        for path in paths
            .into_iter()
            .chain(bounds.iter().map(|bound| &bound.path))
        {
            if less_visible(path, own) {
                self.report(item, path.token(), Code::NamePrivateInInterface);
            }
        }
        let mut pending: Vec<(&ast::TypeExpr, u8)> = types;
        pending.extend(
            bounds
                .iter()
                .flat_map(|bound| bound.args.iter().flatten())
                .filter_map(|arg| match arg {
                    ast::GenericArg::Type(ty) => Some((ty, own)),
                    _ => None,
                }),
        );
        while let Some((ty, required)) = pending.pop() {
            if required == 0 {
                continue;
            }
            match &ty.kind {
                ast::TypeKind::Path { path, args } => {
                    if less_visible(path, required) {
                        self.report(item, path.token(), Code::NamePrivateInInterface);
                    }
                    pending.extend(args.iter().flatten().filter_map(|arg| match arg {
                        ast::GenericArg::Type(ty) => Some((ty, required)),
                        _ => None,
                    }));
                }
                ast::TypeKind::Fn(fn_type) => {
                    pending.extend(fn_type.params.iter().map(|param| (&param.ty, required)));
                    pending.extend(
                        fn_type
                            .result
                            .iter()
                            .chain(fn_type.fails.iter())
                            .map(|ty| (ty, required)),
                    );
                }
            }
        }
    }

    /// A function named `Type.name`: `Type` is declared in its module, the scalars and `Unit` in
    /// the root module of core, or an impl line of its module targets it with an interface
    /// declaring `name`.
    fn check_method_placement(&self, item: ItemId, owner: &ast::Path) {
        let module = self.db.module_of(item);
        let key = match self.db.resolve(module, owner) {
            Err(error) => {
                if let Some(code) = error.code {
                    self.report_problem(item, error.token, code);
                }
                return;
            }
            Ok(_) => self.db.owner_key(module, owner),
        };
        let declared = key.is_some_and(|key| self.declares_type(module, key));
        let implemented = !self.implemented_lines(item).is_empty();
        if !declared && !implemented {
            self.report(item, self.db.ast(item).token, Code::ItemMethodMisplaced);
        }
    }

    /// A head's `fails` clause names a failure type, and beside waiting a cancellable one; a body
    /// judges the latter by its effects, so only a head without one is judged here.
    fn check_fails(&self, item: ItemId, sig: &super::sig::FnSig, head: &ast::Head, bodiless: bool) {
        if let (Some(fails), Some(written)) = (&sig.fails, &head.fails)
            && !self.is_failure_type(fails)
        {
            self.report(item, written.token, Code::ErrorFailureType);
            return;
        }
        if bodiless && self.cancel_unmapped(&sig.fn_ty(), false) {
            let fixes = match &sig.fails {
                None => {
                    let after = head
                        .result
                        .as_ref()
                        .map_or(head.close, |result| result.last);
                    vec![self.fix_appending(item, after, " fails task::Cancelled")]
                }
                Some(fails) => self.cancelled_variant_fix(fails).into_iter().collect(),
            };
            let construct = (head.token, head.token);
            self.report_fixed(
                item,
                construct,
                Code::ErrorCancelUnmapped,
                fixes,
                Offer::Each,
            );
        }
    }

    /// The `limits stack N` an entry of a boot, task, interrupt or landing context states when
    /// checked for a machine target, reported on the entry's head.
    pub fn check_stack_bound(&self, item: ItemId, limits: &super::ty::LimitSet) {
        if self.db.target.is_machine() && limits.stack.is_none() {
            self.report(item, self.db.ast(item).token, Code::SuspendStackMissing);
        }
    }

    /// Each generic parameter of a function's or method's own list occurs in a parameter's type,
    /// a failed type holding each.
    fn check_unused_generics(&self, item: ItemId, sig: &super::sig::FnSig) {
        let unused = sig.own_generics().iter().any(|def| {
            !sig.params
                .iter()
                .any(|param| param.ty.has_error() || param.ty.mentions(&|param| param == def.param))
        });
        if unused {
            self.report(item, self.db.ast(item).token, Code::GenericParameterUnused);
        }
    }

    /// The parameters of a head: their names' convention and distinctness.
    fn check_params(&self, item: ItemId, params: &[ast::Param]) {
        let mut seen: Vec<&str> = Vec::new();
        let mut folded: Vec<String> = Vec::new();
        for param in params {
            let ast::ParamKind::Named { name, .. } = &param.kind else {
                continue;
            };
            self.check_case(item, name, Case::Snake);
            if seen.contains(&name.text.as_str()) {
                self.report(item, name.token, Code::ItemDuplicate);
            } else if folded.contains(&fold(&name.text)) {
                self.report(item, name.token, Code::NameClash);
            }
            seen.push(&name.text);
            folded.push(fold(&name.text));
        }
    }

    /// The head shape a function's or interface method's name prefix promises.
    fn check_prefix(&self, item: ItemId, name: &ast::Name, sig: &super::sig::FnSig) {
        let first_mode = sig
            .params
            .first()
            .filter(|param| param.is_self)
            .map(|param| param.mode);
        let option = self.db.core_item("Option");
        let holds = |prefix: &str| -> Option<bool> {
            let rest = name.text.strip_prefix(prefix)?;
            if rest.is_empty() {
                return None;
            }
            Some(match prefix {
                "try_" => sig.fails.is_some(),
                "checked_" => matches!(&sig.result, Ty::Item(item, _) if Some(*item) == option),
                "is_" => sig.result == Ty::Scalar(Scalar::Bool),
                "to_" => {
                    first_mode == Some(Mode::Read)
                        && !sig.is_proj()
                        && !self.is_view(&sig.result, false)
                }
                "as_" => sig.is_proj(),
                "into_" => first_mode == Some(Mode::Take),
                _ => first_mode == Some(Mode::Mut),
            })
        };
        let broken = ["try_", "checked_", "is_", "to_", "as_", "into_", "with_"]
            .into_iter()
            .filter_map(holds)
            .any(|holds| !holds);
        if broken && !sig.result.has_error() {
            self.report(item, name.token, Code::NamePrefixMismatch);
        }
    }

    /// Whether a type is a view; a type parameter counts as none.
    pub fn is_view(&self, ty: &Ty, params: bool) -> bool {
        let _ = params;
        match ty {
            Ty::Item(item, _) => {
                let name = self.db.canonical(*item);
                matches!(
                    name.as_str(),
                    "core::Slice" | "core::SliceMut" | "core::Str"
                ) || self.db.ast(*item).has_modifier(ast::TypeModifier::View)
            }
            Ty::Dyn(_) => true,
            _ => false,
        }
    }
}

/// The types and bounds a head writes, each type required at visibility `own`.
fn head_parts<'h>(
    head: &'h ast::Head,
    own: u8,
    types: &mut Vec<(&'h ast::TypeExpr, u8)>,
    bounds: &mut Vec<&'h ast::Bound>,
) {
    bounds.extend(head.generics.iter().flat_map(|param| &param.bounds));
    for param in &head.params {
        if let ast::ParamKind::Named { ty, .. } = &param.kind {
            types.push((ty, own));
        }
    }
    types.extend(
        head.result
            .iter()
            .chain(head.fails.iter())
            .map(|ty| (ty, own)),
    );
}

/// The name an item declares, a function named after a type giving its member name.
pub fn declared_name(item: &ast::Item) -> Option<&ast::Name> {
    Some(match &item.kind {
        ItemKind::Fn(function) => &function.label.name,
        ItemKind::Struct(item) => &item.name,
        ItemKind::Enum(item) => &item.name,
        ItemKind::Interface(item) => &item.name,
        ItemKind::Layout(item) => &item.name,
        ItemKind::Registers(item) => &item.name,
        ItemKind::Const(item) => &item.name,
        ItemKind::Registry(item) => &item.name,
        ItemKind::Contribute(item) => &item.name,
        ItemKind::Test(item) => &item.name,
        _ => return None,
    })
}

/// The label of a payload field, when it is one identifier.
fn label_name(label: &ast::TypeExpr) -> Option<ast::Name> {
    match &label.kind {
        ast::TypeKind::Path { path, args: None } if path.segments.len() == 1 => {
            Some(path.segments[0].clone())
        }
        _ => None,
    }
}

/// The generic parameters an item declares.
pub(super) fn generic_params(item: &ast::Item) -> Option<&[ast::GenericParam]> {
    Some(match &item.kind {
        ItemKind::Fn(function) => &function.head.generics,
        ItemKind::Struct(structure) => &structure.generics,
        ItemKind::Enum(enumeration) => &enumeration.generics,
        ItemKind::Interface(interface) => &interface.generics,
        _ => return None,
    })
}
