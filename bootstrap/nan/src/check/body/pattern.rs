//! Patterns: their forms and types, the names they bind, refutability, and the disjointness and
//! exhaustiveness of match arms, decided exactly over the values of each type.

use super::Body;
use crate::check::Member;
use crate::check::db::{ItemId, TypeKey};
use crate::check::resolve::Named;
use crate::check::sig::{Payload, SigKind, VariantSig};
use crate::check::tir::{
    Compared, Lit, LocalId, LocalKind, TArm, TKind, TPattern, TPatternKind, ValueSpace, ValueTest,
};
use crate::check::ty::{Scalar, Subst, Ty};
use crate::check::unused::FieldLabel;
use crate::diagnostic::{Code, Fix};
use crate::interp::{ConstValue, Val};
use crate::syntax::ast::{self, ItemKind, LiteralKind, PatternKind, RangeKind, Tok, TypeModifier};

/// A pattern as exhaustiveness reads it.
#[derive(Clone, Debug, PartialEq)]
enum Pat {
    Wild,
    Ints(i128, i128),
    Bool(bool),
    Bytes(Vec<u8>),
    /// A constructor: a variant's index, or 0 for a struct, layout or `Unit`, with its fields in
    /// declaration order.
    Ctor(usize, Vec<Pat>),
}

/// The constructors of a type, as exhaustiveness counts them.
enum Shape {
    /// Integers or chars, as the intervals of the type.
    Ints(Vec<(i128, i128)>),
    Bool,
    /// Variants with the types of their fields; `open` when values outside them exist.
    Ctors {
        fields: Vec<Vec<Ty>>,
        open: bool,
    },
    /// Values no finite set of patterns covers.
    Infinite,
}

impl Body<'_, '_> {
    /// Types a whole pattern of a `let`, `for` or arm, whose names are distinct.
    pub fn top_pattern(&mut self, pattern: &ast::Pattern, ty: &Ty, kind: LocalKind) -> TPattern {
        self.bound_names.clear();
        self.pattern(pattern, ty, kind)
    }

    /// Types the alternatives of an arm: each binds the names of the first with the same types.
    pub fn arm_patterns(
        &mut self,
        token: Tok,
        patterns: &[ast::Pattern],
        ty: &Ty,
    ) -> Vec<TPattern> {
        let mut typed = Vec::new();
        let mut first: Option<Vec<(String, Ty)>> = None;
        let mut first_locals: Vec<LocalId> = Vec::new();
        let mut mismatched = false;
        for pattern in patterns {
            let start = self.locals.len();
            let hidden = match first {
                Some(_) => std::mem::take(self.scopes.last_mut().expect("the arm's scope")),
                None => Vec::new(),
            };
            typed.push(self.top_pattern(pattern, ty, LocalKind::Pattern));
            let declared: Vec<LocalId> = (start..self.locals.len())
                .map(|index| LocalId(index as u32))
                .collect();
            let mut bound: Vec<(String, Ty)> = self.locals[start..]
                .iter()
                .map(|local| (local.name.clone(), local.ty.clone()))
                .collect();
            bound.sort_by(|left, right| left.0.cmp(&right.0));
            match &first {
                None => {
                    first = Some(bound);
                    first_locals = declared;
                }
                Some(expected) => {
                    mismatched |= *expected != bound;
                    *self.scopes.last_mut().expect("the arm's scope") = hidden;
                    for later in declared {
                        let name = &self.local(later).name;
                        if let Some(earlier) = first_locals
                            .iter()
                            .find(|earlier| self.local(**earlier).name == *name)
                        {
                            self.usage
                                .alternatives
                                .entry(*earlier)
                                .or_default()
                                .push(later);
                        }
                    }
                }
            }
        }
        if mismatched {
            self.report(token, Code::ExprOrBindings);
        }
        typed
    }

    /// Types a pattern against its scrutinee's type and declares the names it binds.
    pub fn pattern(&mut self, pattern: &ast::Pattern, ty: &Ty, kind: LocalKind) -> TPattern {
        let token = pattern.token;
        let typed = |kind: TPatternKind| TPattern {
            token,
            ty: ty.clone(),
            kind,
        };
        match &pattern.kind {
            PatternKind::Wildcard => typed(TPatternKind::Wildcard),
            PatternKind::Literal { literal, range } => {
                let low = self.pattern_literal(token, literal, ty);
                match range {
                    None => typed(low.map_or(TPatternKind::Failed, equal)),
                    Some((range_kind, bound)) => {
                        self.range_pattern(token, ty, low, *range_kind, bound)
                    }
                }
            }
            PatternKind::Named {
                path,
                member,
                fields,
                range,
            } => {
                let name = &path.segments[0];
                let binding = path.segments.len() == 1
                    && member.is_none()
                    && fields.is_none()
                    && range.is_none()
                    && name.text != "None"
                    && name.text != "Unit"
                    && !is_screaming(&name.text);
                if binding {
                    if self.bound_names.contains(&name.text) {
                        self.report(name.token, Code::PatternDuplicateBinding);
                        return typed(TPatternKind::Failed);
                    }
                    self.bound_names.push(name.text.clone());
                    let local = self.declare(name, ty.clone(), kind);
                    return typed(TPatternKind::Binding(local));
                }
                if let Some((range_kind, bound)) = range {
                    let low = self.constant_value(token, path, ty);
                    return self.range_pattern(token, ty, low, *range_kind, bound);
                }
                self.named_pattern(token, path, member.as_ref(), fields.as_deref(), ty, kind)
            }
        }
    }

    /// The value a literal pattern matches and the literal, typed by the scrutinee's type.
    fn pattern_literal(
        &mut self,
        token: Tok,
        literal: &ast::PatternLiteral,
        ty: &Ty,
    ) -> Option<(ValueSpace, Compared)> {
        let written = literal.literal();
        if written.kind == LiteralKind::Float {
            self.report(token, Code::ExprFloatPattern);
            return None;
        }
        let negative = matches!(literal, ast::PatternLiteral::Number { negative: true, .. });
        let typed = self.literal(token, written, Some(ty), negative);
        if typed.ty.has_error() {
            return None;
        }
        if typed.ty != *ty && !ty.has_error() {
            self.report_mismatch(token, ty, &typed.ty);
            return None;
        }
        let TKind::Literal(value) = typed.kind else {
            return None;
        };
        let space = match &value {
            Lit::Int(value) => ValueSpace::Ints(*value, *value),
            Lit::Char(c) => ValueSpace::Ints(i128::from(u32::from(*c)), i128::from(u32::from(*c))),
            Lit::Bool(value) => ValueSpace::Bool(*value),
            Lit::Text(text) => ValueSpace::Bytes(text.clone().into_bytes()),
            Lit::Bytes(bytes) => ValueSpace::Bytes(bytes.clone()),
            Lit::Float(_) => return None,
        };
        Some((space, Compared::Literal(value)))
    }

    /// A range pattern: both bounds of the scrutinee's integer or char type, matching some value.
    fn range_pattern(
        &mut self,
        token: Tok,
        ty: &Ty,
        low: Option<(ValueSpace, Compared)>,
        kind: RangeKind,
        bound: &ast::RangeBound,
    ) -> TPattern {
        let high = match bound {
            ast::RangeBound::Literal(literal) => self.pattern_literal(token, literal, ty),
            ast::RangeBound::Path(path) => self.constant_value(token, path, ty),
        };
        let failed = TPattern {
            token,
            ty: ty.clone(),
            kind: TPatternKind::Failed,
        };
        let valid = match ty {
            Ty::Scalar(scalar) => scalar.is_integer() || *scalar == Scalar::Char,
            Ty::Error => true,
            _ => false,
        };
        if !valid {
            self.report(token, Code::PatternRangeKind);
            return failed;
        }
        let (Some((low_space, low)), Some((high_space, high))) = (low, high) else {
            return failed;
        };
        let exclusive = kind == RangeKind::Exclusive;
        let space = match (low_space, high_space) {
            (ValueSpace::Ints(low, _), ValueSpace::Ints(high, _)) => {
                let high = if exclusive { high - 1 } else { high };
                if low > high {
                    self.report(token, Code::PatternEmptyRange);
                    return failed;
                }
                ValueSpace::Ints(low, high)
            }
            _ => ValueSpace::Unknown,
        };
        TPattern {
            token,
            ty: ty.clone(),
            kind: TPatternKind::Value(space, ValueTest::Range(low, high, exclusive)),
        }
    }

    /// The value of a constant written as a pattern or range bound, a path naming a const item,
    /// and the const.
    fn constant_value(
        &mut self,
        token: Tok,
        path: &ast::Path,
        ty: &Ty,
    ) -> Option<(ValueSpace, Compared)> {
        if let [name] = &path.segments[..]
            && self.lookup_local(&name.text).is_some()
        {
            self.report(token, Code::PatternPathKind);
            return None;
        }
        let named = match self.ck.resolve_use(self.item, self.module, path) {
            Ok(named) => named,
            Err(error) => {
                self.report_path(error);
                return None;
            }
        };
        let Named::Item(item) = named else {
            self.report(token, Code::PatternPathKind);
            return None;
        };
        if !matches!(self.ck.db.ast(item).kind, ItemKind::Const(_)) {
            self.report(token, Code::PatternPathKind);
            return None;
        }
        if !self.ck.db.visible(self.module, item) {
            self.report(token, Code::NamePrivate);
        }
        let SigKind::Const { ty: const_ty } = &self.ck.sig(item).kind else {
            return None;
        };
        let kind_ok = match const_ty {
            Ty::Scalar(scalar) => {
                scalar.is_integer() || matches!(scalar, Scalar::Bool | Scalar::Char)
            }
            Ty::Error => true,
            other => {
                self.ck.is_std(other, "core::Str")
                    || *other == self.ck.slice_ty(Ty::Scalar(Scalar::U8))
            }
        };
        if !kind_ok {
            self.report(token, Code::PatternConstKind);
            return None;
        }
        if const_ty != ty && !const_ty.has_error() && !ty.has_error() {
            self.report_mismatch(token, ty, const_ty);
            return None;
        }
        let space = self
            .ck
            .const_value(item)
            .map_or(ValueSpace::Unknown, |value| value_space(&value));
        Some((space, Compared::Const(item)))
    }

    /// A variant, struct, layout, constant or `Unit` pattern.
    fn named_pattern(
        &mut self,
        token: Tok,
        path: &ast::Path,
        member: Option<&ast::Name>,
        fields: Option<&[ast::FieldPattern]>,
        ty: &Ty,
        kind: LocalKind,
    ) -> TPattern {
        let failed = |body: &mut Self, fields: Option<&[ast::FieldPattern]>| {
            for field in fields.into_iter().flatten() {
                body.pattern(&field.pattern, &Ty::Error, kind);
            }
            TPattern {
                token,
                ty: ty.clone(),
                kind: TPatternKind::Failed,
            }
        };
        if member.is_none()
            && fields.is_none()
            && path.segments.len() == 1
            && path.segments[0].text == "Unit"
        {
            if *ty != Ty::Unit && !ty.has_error() {
                self.report_mismatch(token, ty, &Ty::Unit);
            }
            return TPattern {
                token,
                ty: ty.clone(),
                kind: TPatternKind::Fields(None, Vec::new()),
            };
        }
        let named = match self.ck.resolve_use(self.item, self.module, path) {
            Ok(named) => named,
            Err(error) => {
                self.report_path(error);
                return failed(self, fields);
            }
        };
        let (item, variant) = match (named, member) {
            (Named::Variant(enumeration, variant), None) => (enumeration, Some(variant)),
            (Named::Item(item), None) => match &self.ck.db.ast(item).kind {
                ItemKind::Const(_) if fields.is_none() => {
                    let value = self.constant_value(token, path, ty);
                    return TPattern {
                        token,
                        ty: ty.clone(),
                        kind: value.map_or(TPatternKind::Failed, equal),
                    };
                }
                ItemKind::Struct(_) | ItemKind::Layout(_) if fields.is_some() => (item, None),
                _ => {
                    self.report(token, Code::PatternPathKind);
                    return failed(self, fields);
                }
            },
            (Named::Item(item), Some(member)) => {
                if self.core_variant_qualified(item, &member.text) {
                    self.qualified_core_variant(token, member);
                    return failed(self, fields);
                }
                match &self.ck.sig(item).kind {
                    SigKind::Enum { variants } => {
                        let position =
                            |name: &str| variants.iter().position(|variant| variant.name == name);
                        let renaming = match position(&member.text) {
                            Some(_) => None,
                            None => self.ck.member_renaming(
                                TypeKey::Item(item),
                                &member.text,
                                self.module,
                            ),
                        };
                        let member = match renaming {
                            Some(tombstone) => {
                                match self.ck.forward_member(self.item, member, tombstone) {
                                    Some(renamed) => renamed,
                                    None => return failed(self, fields),
                                }
                            }
                            None => member.clone(),
                        };
                        let Some(variant) = position(&member.text) else {
                            self.report(member.token, Code::NameUnknown);
                            return failed(self, fields);
                        };
                        self.name_member(member.token, Member::Variant(item, member.text.clone()));
                        (item, Some(variant))
                    }
                    SigKind::Layout { tags, .. }
                        if tags.contains(&member.text) && fields.is_none() =>
                    {
                        let index = tags
                            .iter()
                            .position(|tag| *tag == member.text)
                            .expect("a tag");
                        self.expect_item(token, item, ty);
                        return TPattern {
                            token,
                            ty: ty.clone(),
                            kind: TPatternKind::Fields(Some(index), Vec::new()),
                        };
                    }
                    _ => {
                        self.report(token, Code::PatternPathKind);
                        return failed(self, fields);
                    }
                }
            }
            _ => {
                self.report(token, Code::PatternPathKind);
                return failed(self, fields);
            }
        };
        if !self.ck.db.visible(self.module, item) {
            self.report(token, Code::NamePrivate);
        }
        if !self.expect_item(token, item, ty) {
            return failed(self, fields);
        }
        let sig = self.ck.sig(item);
        let subst = Subst::of(sig.generics.iter().map(|def| def.param), ty.args());
        let consumer = self.ck.db.package_of(item) != self.ck.db.module(self.module).package;
        let (declared, labelled): (Vec<(String, Ty)>, bool) = match (&sig.kind, variant) {
            (SigKind::Enum { variants }, Some(index)) => match &variants[index].payload {
                Payload::None => {
                    if fields.is_some() {
                        self.report(token, Code::TypePayloadShape);
                        return failed(self, fields);
                    }
                    (Vec::new(), false)
                }
                Payload::Single(payload) => {
                    match fields {
                        Some([single]) if single.label.is_none() => {}
                        _ => {
                            self.report(token, Code::TypePayloadShape);
                            return failed(self, fields);
                        }
                    }
                    (vec![(String::new(), subst.ty(payload))], false)
                }
                Payload::Labelled(payload) => {
                    if fields.is_none_or(|fields| fields.is_empty()) {
                        self.report(token, Code::TypePayloadShape);
                        return failed(self, fields);
                    }
                    (
                        payload
                            .iter()
                            .map(|(name, ty)| (name.clone(), subst.ty(ty)))
                            .collect(),
                        true,
                    )
                }
            },
            (SigKind::Struct { fields: declared }, None) => {
                if consumer && self.ck.db.ast(item).has_modifier(TypeModifier::Open) {
                    self.report(token, Code::PatternOpenStruct);
                    return failed(self, fields);
                }
                if declared.iter().any(|field| {
                    !self
                        .ck
                        .db
                        .field_visible(self.module, item, field.visibility)
                }) {
                    self.report(token, Code::TypeHiddenField);
                    return failed(self, fields);
                }
                (
                    declared
                        .iter()
                        .map(|field| (field.name.clone(), subst.ty(&field.ty)))
                        .collect(),
                    true,
                )
            }
            (
                SigKind::Layout {
                    fields: declared, ..
                },
                None,
            ) => (
                declared
                    .iter()
                    .map(|field| (field.name.clone(), field.ty.clone()))
                    .collect(),
                true,
            ),
            _ => {
                self.report(token, Code::PatternPathKind);
                return failed(self, fields);
            }
        };
        let mut typed_fields: Vec<(String, TPattern)> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        let mut complete = true;
        for field in fields.into_iter().flatten() {
            if labelled {
                let Some(label) = field.label_name() else {
                    self.report(field.token, Code::TypeUnlabelledField);
                    self.pattern(&field.pattern, &Ty::Error, kind);
                    complete = false;
                    continue;
                };
                let owner = variant.is_none().then_some(item);
                let Some((name, field_ty)) = self.labelled_field(owner, label, &declared) else {
                    self.pattern(&field.pattern, &Ty::Error, kind);
                    complete = false;
                    continue;
                };
                if seen.contains(name) {
                    self.report(field.token, Code::TypeDuplicateLabel);
                    complete = false;
                }
                if variant.is_none() {
                    self.name_member(label.token, Member::Field(item, name.clone()));
                    let wildcard = matches!(field.pattern.kind, PatternKind::Wildcard);
                    if !wildcard {
                        self.ck.record_read(item, name);
                    }
                    let label = FieldLabel {
                        item: self.item,
                        first: field.token,
                        last: field.pattern.token,
                        simple: wildcard,
                    };
                    self.ck.record_label(item, name, label);
                }
                seen.push(name.clone());
                let typed = self.pattern(&field.pattern, field_ty, kind);
                typed_fields.push((name.clone(), typed));
            } else {
                let typed = self.pattern(&field.pattern, &declared[0].1, kind);
                typed_fields.push((String::new(), typed));
            }
        }
        if labelled && complete && declared.iter().any(|(name, _)| !seen.contains(name)) {
            self.report(token, Code::TypeMissingField);
            complete = false;
        }
        if !complete {
            return TPattern {
                token,
                ty: ty.clone(),
                kind: TPatternKind::Failed,
            };
        }
        if labelled {
            typed_fields.sort_by_key(|(name, _)| {
                declared.iter().position(|(declared, _)| declared == name)
            });
        }
        TPattern {
            token,
            ty: ty.clone(),
            kind: TPatternKind::Fields(variant, typed_fields),
        }
    }

    /// Reports a pattern naming an item of another type than its scrutinee's.
    fn expect_item(&self, token: Tok, item: ItemId, ty: &Ty) -> bool {
        match ty {
            Ty::Item(found, _) if *found == item => true,
            Ty::Error => true,
            _ => {
                let written = self.ck.own_type(item);
                self.report_mismatch(token, ty, &written);
                false
            }
        }
    }

    /// Whether a pattern is refutable: some value of its type does not match it; unknown when
    /// it names a const without a value.
    pub fn refutable(&self, pattern: &TPattern) -> Option<bool> {
        let pat = self.pat(pattern)?;
        Some(self.useful(
            &[vec![pat]],
            &[Pat::Wild],
            std::slice::from_ref(&pattern.ty),
        ))
    }

    /// The disjointness and exhaustiveness of a match's arms, and the reachability of `match
    /// first`'s arms.
    pub fn check_arms(
        &self,
        token: Tok,
        first: bool,
        ty: &Ty,
        arms: &[TArm],
        written_arms: &[ast::Arm],
    ) {
        let written = written_arms;
        let types = [ty.clone()];
        let mut rows: Vec<Vec<Pat>> = Vec::new();
        let mut alternatives: Vec<(usize, Pat)> = Vec::new();
        let mut else_arm = None;
        for (index, (arm, written)) in arms.iter().zip(written).enumerate() {
            if matches!(written.patterns, ast::ArmPatterns::Else) {
                else_arm = Some(index);
                continue;
            }
            if arm
                .patterns
                .iter()
                .any(|pattern| matches!(pattern.kind, TPatternKind::Failed))
            {
                return;
            }
            let Some(pats) = arm
                .patterns
                .iter()
                .map(|pattern| self.pat(pattern))
                .collect::<Option<Vec<Pat>>>()
            else {
                return;
            };
            if first {
                let reachable = pats
                    .iter()
                    .any(|pat| self.useful(&rows, std::slice::from_ref(pat), &types));
                if !reachable || else_arm.is_some() {
                    self.unreachable_arm(written);
                    return;
                }
            } else {
                for pat in &pats {
                    let overlaps = alternatives
                        .iter()
                        .any(|(_, earlier)| self.overlap(earlier, pat, ty));
                    if overlaps {
                        let has_else = written_arms
                            .iter()
                            .any(|arm| matches!(arm.patterns, ast::ArmPatterns::Else));
                        let fixes = match &written.patterns {
                            ast::ArmPatterns::Patterns(patterns)
                                if !has_else
                                    && matches!(&patterns[..], [pattern]
                                        if matches!(pattern.kind, ast::PatternKind::Wildcard)) =>
                            {
                                let wildcard = patterns[0].token;
                                vec![self.ck.fix_replacing(self.item, wildcard, wildcard, "else")]
                            }
                            _ => Vec::new(),
                        };
                        self.report_fixed((arm.token, arm.token), Code::ExprOverlappingArms, fixes);
                        return;
                    }
                    alternatives.push((index, pat.clone()));
                }
            }
            if arm.guard.is_none() {
                rows.extend(pats.into_iter().map(|pat| vec![pat]));
            }
        }
        let open_rest = self.useful(&rows, &[Pat::Wild], &types);
        let closed_rest = self.useful_closed(&rows, ty);
        match else_arm {
            Some(index) => {
                if !first && !open_rest {
                    self.unreachable_arm(&written_arms[index]);
                }
            }
            None if open_rest && closed_rest => {
                let fixes = self
                    .missing_arms(ty, &rows, written_arms)
                    .into_iter()
                    .collect();
                self.report_fixed((token, token), Code::ExprNonExhaustive, fixes);
            }
            None if open_rest => self.report(token, Code::PatternOpenEnumNoElse),
            None => {}
        }
    }

    /// Reports an arm no value reaches, with the fix deleting it.
    fn unreachable_arm(&self, arm: &ast::Arm) {
        let last = match &arm.body {
            ast::ArmBody::Block(block) => block.end,
            ast::ArmBody::Expr(expr) => expr.last,
        };
        let fix = self.ck.fix_deleting_lines(self.item, arm.token, last);
        self.report_fixed((arm.token, last), Code::ExprUnreachableArm, vec![fix]);
    }

    /// The fix of a non-exhaustive match: after its last arm, an arm `P => todo "unhandled"` per
    /// variant no arm names when the values no arm matches all belong to those, and otherwise
    /// one arm `else => todo "unhandled"`.
    fn missing_arms(&self, ty: &Ty, rows: &[Vec<Pat>], arms: &[ast::Arm]) -> Option<Fix> {
        let last_arm = arms.last()?;
        let last = match &last_arm.body {
            ast::ArmBody::Block(block) => block.end,
            ast::ArmBody::Expr(expr) => expr.last,
        };
        let indentation = self.ck.indentation(self.item, last_arm.token);
        let types = [ty.clone()];
        let named: Vec<usize> = rows
            .iter()
            .filter_map(|row| match row.first() {
                Some(Pat::Ctor(index, _)) => Some(*index),
                _ => None,
            })
            .collect();
        let patterns = match ty {
            Ty::Item(item, _) => match &self.ck.sig(*item).kind {
                SigKind::Enum { variants } => {
                    let covered = named.iter().all(|&index| {
                        let wild = vec![Pat::Wild; self.payload_width(&variants[index].payload)];
                        !self.useful(rows, &[Pat::Ctor(index, wild)], &types)
                    });
                    covered.then(|| {
                        variants
                            .iter()
                            .enumerate()
                            .filter(|(index, _)| !named.contains(index))
                            .map(|(_, variant)| self.variant_pattern(*item, variant))
                            .collect::<Vec<_>>()
                    })
                }
                _ => None,
            },
            _ => None,
        };
        let patterns = patterns.unwrap_or_else(|| vec!["else".to_owned()]);
        let added: String = patterns
            .iter()
            .map(|pattern| format!("{indentation}{pattern} => todo \"unhandled\"\n"))
            .collect();
        let syntax = &self.ck.db.item(self.item).file.syntax;
        let line = syntax.tokens[last as usize].line as usize;
        let at = syntax
            .lines
            .get(line)
            .map_or(syntax.source.len() as u32, |next| next.start);
        Some(self.ck.fix_bytes(self.item, at, at, &added))
    }

    /// The number of payload values a variant's pattern matches.
    fn payload_width(&self, payload: &Payload) -> usize {
        match payload {
            Payload::None => 0,
            Payload::Single(_) => 1,
            Payload::Labelled(fields) => fields.len(),
        }
    }

    /// A variant's pattern with `_` for each payload value, as the module writes it.
    fn variant_pattern(&self, enumeration: ItemId, variant: &VariantSig) -> String {
        let core = self.ck.db.is_std(enumeration)
            && matches!(
                self.ck.db.canonical(enumeration).as_str(),
                "core::Option" | "core::Result"
            );
        let name = if core {
            variant.name.clone()
        } else {
            format!(
                "{}.{}",
                self.ck.item_written(enumeration, self.module),
                variant.name
            )
        };
        match &variant.payload {
            Payload::None => name,
            Payload::Single(_) => format!("{name}(_)"),
            Payload::Labelled(fields) => {
                let fields: Vec<String> = fields
                    .iter()
                    .map(|(label, _)| format!("{label}: _"))
                    .collect();
                format!("{name}({})", fields.join(", "))
            }
        }
    }

    /// Whether some value is matched by no row when every enum has only its declared variants.
    fn useful_closed(&self, rows: &[Vec<Pat>], ty: &Ty) -> bool {
        let saved = self.closed.get();
        self.closed.set(true);
        let useful = self.useful(rows, &[Pat::Wild], std::slice::from_ref(ty));
        self.closed.set(saved);
        useful
    }

    /// A pattern as exhaustiveness reads it; none when it names a const without a value.
    fn pat(&self, pattern: &TPattern) -> Option<Pat> {
        Some(match &pattern.kind {
            TPatternKind::Wildcard | TPatternKind::Binding(_) | TPatternKind::Failed => Pat::Wild,
            TPatternKind::Value(space, _) => match space {
                ValueSpace::Ints(low, high) => Pat::Ints(*low, *high),
                ValueSpace::Bool(value) => Pat::Bool(*value),
                ValueSpace::Bytes(bytes) => Pat::Bytes(bytes.clone()),
                ValueSpace::Unknown => return None,
            },
            TPatternKind::Fields(variant, fields) => Pat::Ctor(
                variant.unwrap_or(0),
                fields
                    .iter()
                    .map(|(_, field)| self.pat(field))
                    .collect::<Option<_>>()?,
            ),
        })
    }

    /// The constructors of a type.
    fn shape(&self, ty: &Ty) -> Shape {
        match ty {
            Ty::Scalar(Scalar::Bool) => Shape::Bool,
            Ty::Scalar(Scalar::Char) => Shape::Ints(vec![(0, 0xD7FF), (0xE000, 0x10_FFFF)]),
            Ty::Scalar(scalar) if scalar.is_integer() => Shape::Ints(vec![scalar.range()]),
            Ty::Unit => Shape::Ctors {
                fields: vec![Vec::new()],
                open: false,
            },
            Ty::Item(item, args) => {
                let sig = self.ck.sig(*item);
                let subst = Subst::of(sig.generics.iter().map(|def| def.param), args);
                match &sig.kind {
                    SigKind::Struct { fields } => Shape::Ctors {
                        fields: vec![fields.iter().map(|field| subst.ty(&field.ty)).collect()],
                        open: false,
                    },
                    SigKind::Layout { fields, tags, .. } if tags.is_empty() => Shape::Ctors {
                        fields: vec![fields.iter().map(|field| field.ty.clone()).collect()],
                        open: false,
                    },
                    SigKind::Layout { tags, .. } => Shape::Ctors {
                        fields: tags.iter().map(|_| Vec::new()).collect(),
                        open: false,
                    },
                    SigKind::Enum { variants } => {
                        let open = !self.closed.get()
                            && self.ck.db.ast(*item).has_modifier(TypeModifier::Open)
                            && self.ck.db.package_of(*item)
                                != self.ck.db.module(self.module).package;
                        Shape::Ctors {
                            fields: variants
                                .iter()
                                .map(|variant| match &variant.payload {
                                    Payload::None => Vec::new(),
                                    Payload::Single(ty) => vec![subst.ty(ty)],
                                    Payload::Labelled(fields) => {
                                        fields.iter().map(|(_, ty)| subst.ty(ty)).collect()
                                    }
                                })
                                .collect(),
                            open,
                        }
                    }
                    _ => Shape::Infinite,
                }
            }
            _ => Shape::Infinite,
        }
    }

    /// Whether `vector` matches a value no row of `matrix` matches. Every type being inhabited,
    /// when the first column misses a constructor of its type, the rows it starts with a wildcard
    /// decide alone.
    fn useful(&self, matrix: &[Vec<Pat>], vector: &[Pat], types: &[Ty]) -> bool {
        let Some((head, rest)) = vector.split_first() else {
            return matrix.is_empty();
        };
        let ty = &types[0];
        let rest_types = &types[1..];
        match head {
            Pat::Ctor(index, fields) => {
                let field_types = self.ctor_fields(ty, *index, fields.len());
                let specialized = self.specialize(matrix, *index, &field_types);
                let mut next = fields.clone();
                next.extend(rest.iter().cloned());
                let mut next_types = field_types;
                next_types.extend(rest_types.iter().cloned());
                self.useful(&specialized, &next, &next_types)
            }
            Pat::Ints(low, high) => {
                self.split_ints(matrix, *low, *high)
                    .into_iter()
                    .any(|(low, high)| {
                        let rows = self.rows_covering_ints(matrix, low, high);
                        self.useful(&rows, rest, rest_types)
                    })
            }
            Pat::Bool(value) => {
                let rows = self.rows_with(matrix, |pat| {
                    matches!(pat, Pat::Wild) || *pat == Pat::Bool(*value)
                });
                self.useful(&rows, rest, rest_types)
            }
            Pat::Bytes(bytes) => {
                let rows = self.rows_with(matrix, |pat| match pat {
                    Pat::Wild => true,
                    Pat::Bytes(other) => other == bytes,
                    _ => false,
                });
                self.useful(&rows, rest, rest_types)
            }
            Pat::Wild => {
                let heads = || matrix.iter().filter_map(|row| row.first());
                let default = || self.rows_with(matrix, |pat| matches!(pat, Pat::Wild));
                match self.shape(ty) {
                    Shape::Bool => {
                        let complete = [true, false]
                            .iter()
                            .all(|value| heads().any(|pat| *pat == Pat::Bool(*value)));
                        if !complete {
                            return self.useful(&default(), rest, rest_types);
                        }
                        [true, false].into_iter().any(|value| {
                            let rows = self.rows_with(matrix, |pat| {
                                matches!(pat, Pat::Wild) || *pat == Pat::Bool(value)
                            });
                            self.useful(&rows, rest, rest_types)
                        })
                    }
                    Shape::Ints(ranges) => {
                        let pieces: Vec<(i128, i128)> = (ranges.into_iter())
                            .flat_map(|(low, high)| self.split_ints(matrix, low, high))
                            .collect();
                        let complete = pieces.iter().all(|(low, high)| {
                            heads().any(|pat| {
                                matches!(pat, Pat::Ints(start, end) if start <= low && high <= end)
                            })
                        });
                        if !complete {
                            return self.useful(&default(), rest, rest_types);
                        }
                        pieces.into_iter().any(|(low, high)| {
                            let rows = self.rows_covering_ints(matrix, low, high);
                            self.useful(&rows, rest, rest_types)
                        })
                    }
                    Shape::Ctors { fields, open } => {
                        let complete = !open
                            && (0..fields.len()).all(|index| {
                                heads().any(
                                    |pat| matches!(pat, Pat::Ctor(found, _) if *found == index),
                                )
                            });
                        if !complete {
                            return self.useful(&default(), rest, rest_types);
                        }
                        fields.into_iter().enumerate().any(|(index, field_types)| {
                            let specialized = self.specialize(matrix, index, &field_types);
                            let mut next: Vec<Pat> =
                                field_types.iter().map(|_| Pat::Wild).collect();
                            next.extend(rest.iter().cloned());
                            let mut next_types = field_types;
                            next_types.extend(rest_types.iter().cloned());
                            self.useful(&specialized, &next, &next_types)
                        })
                    }
                    Shape::Infinite => self.useful(&default(), rest, rest_types),
                }
            }
        }
    }

    /// The field types of a constructor of a type.
    fn ctor_fields(&self, ty: &Ty, index: usize, count: usize) -> Vec<Ty> {
        match self.shape(ty) {
            Shape::Ctors { fields, .. } => fields
                .get(index)
                .cloned()
                .unwrap_or_else(|| vec![Ty::Error; count]),
            _ => vec![Ty::Error; count],
        }
    }

    /// The rows of a matrix whose first pattern matches constructor `index`, its fields spread.
    fn specialize(&self, matrix: &[Vec<Pat>], index: usize, field_types: &[Ty]) -> Vec<Vec<Pat>> {
        matrix
            .iter()
            .filter_map(|row| {
                let (head, rest) = row.split_first()?;
                let mut fields = match head {
                    Pat::Wild => field_types.iter().map(|_| Pat::Wild).collect(),
                    Pat::Ctor(found, fields) if *found == index => fields.clone(),
                    _ => return None,
                };
                fields.extend(rest.iter().cloned());
                Some(fields)
            })
            .collect()
    }

    fn rows_with(&self, matrix: &[Vec<Pat>], keep: impl Fn(&Pat) -> bool) -> Vec<Vec<Pat>> {
        matrix
            .iter()
            .filter(|row| row.first().is_some_and(&keep))
            .map(|row| row[1..].to_vec())
            .collect()
    }

    /// The rows whose first pattern matches every value of an interval.
    fn rows_covering_ints(&self, matrix: &[Vec<Pat>], low: i128, high: i128) -> Vec<Vec<Pat>> {
        self.rows_with(matrix, |pat| match pat {
            Pat::Wild => true,
            Pat::Ints(start, end) => *start <= low && high <= *end,
            _ => false,
        })
    }

    /// An interval split at the bounds of the matrix's first-column intervals, so each part lies
    /// wholly inside or outside each of them.
    fn split_ints(&self, matrix: &[Vec<Pat>], low: i128, high: i128) -> Vec<(i128, i128)> {
        let mut cuts = vec![low, high + 1];
        for row in matrix {
            if let Some(Pat::Ints(start, end)) = row.first() {
                for cut in [*start, *end + 1] {
                    if cut > low && cut <= high {
                        cuts.push(cut);
                    }
                }
            }
        }
        cuts.sort();
        cuts.dedup();
        cuts.windows(2).map(|pair| (pair[0], pair[1] - 1)).collect()
    }

    /// Whether some value of `ty` matches both patterns.
    fn overlap(&self, left: &Pat, right: &Pat, ty: &Ty) -> bool {
        match (left, right) {
            (Pat::Wild, _) | (_, Pat::Wild) => true,
            (Pat::Ints(a, b), Pat::Ints(c, d)) => a <= d && c <= b,
            (Pat::Bool(a), Pat::Bool(b)) => a == b,
            (Pat::Bytes(a), Pat::Bytes(b)) => a == b,
            (Pat::Ctor(a, left), Pat::Ctor(b, right)) => {
                a == b && {
                    let types = self.ctor_fields(ty, *a, left.len());
                    left.iter()
                        .zip(right)
                        .zip(types.iter())
                        .all(|((left, right), ty)| self.overlap(left, right, ty))
                }
            }
            _ => false,
        }
    }
}

/// The values a constant pattern matches: those equal to its const's value.
fn value_space(value: &ConstValue) -> ValueSpace {
    match &value.value {
        Val::Int(value) => ValueSpace::Ints(*value, *value),
        Val::Char(c) => {
            let c = i128::from(u32::from(*c));
            ValueSpace::Ints(c, c)
        }
        Val::Bool(value) => ValueSpace::Bool(*value),
        Val::View(view) => ValueSpace::Bytes(
            value
                .elements(view)
                .iter()
                .map(|byte| byte.int() as u8)
                .collect(),
        ),
        _ => ValueSpace::Unknown,
    }
}

/// The pattern matching a value equal to a literal or const.
fn equal((space, compared): (ValueSpace, Compared)) -> TPatternKind {
    TPatternKind::Value(space, ValueTest::Eq(compared))
}

fn is_screaming(name: &str) -> bool {
    name.len() >= 2
        && name.starts_with(|c: char| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}
