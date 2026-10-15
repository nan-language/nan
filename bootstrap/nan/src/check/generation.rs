//! Generation: the constraints a generation obeys, read from the predicates of the structs it
//! builds and the `requires` of a property test's target with visible transparent functions
//! unfolded, and its generator gaps: conjuncts naming generated values that are not
//! constructive, and constructive conjuncts no values satisfy.

use std::collections::{BTreeSet, HashMap};

use super::Checker;
use super::conform::type_key;
use super::db::ItemId;
use super::resolve::Named;
use super::sig::{SigKind, int_literal};
use super::testing::Target;
use super::tir::TBody;
use super::ty::{Arg, Const, Param, Subst, Ty};
use crate::diagnostic::Code;
use crate::syntax::ast::{
    self, BinaryOp, ClauseKind, Expr, ExprKind, FnBody, ItemKind, LiteralKind, ParamKind,
    StructMember, TestItem, UnaryOp,
};

/// How deep transparent functions unfold inside one another.
const UNFOLDING: usize = 8;

/// A generated value a name stands for: its path from the generation's roots, and its type.
type Value = (String, Ty);

/// A drawn integer: its path, and the bounds of its values.
type Drawn = (String, (i128, i128));

/// A term of a conjunct.
enum Term {
    Drawn(Drawn),
    /// Integer literals, consts and const generic parameters joined by `+` and `-`, with its
    /// value when each part's is known.
    Constant(Option<i128>),
    Other,
}

/// A constructive conjunct: `left op right + offset`, without `right` for `left op offset`.
struct Constructive {
    left: Drawn,
    op: BinaryOp,
    right: Option<Drawn>,
    offset: Option<i128>,
}

/// Where the names of an expression are read: the item holding it, and for the body of an
/// unfolded transparent function, the argument each parameter stands for with the scope that
/// argument is read in.
struct Scope<'c> {
    item: ItemId,
    params: HashMap<&'c str, (&'c Expr, usize)>,
}

/// The reading of the conjuncts of one struct's predicates, or of one target's requires: the
/// generated values and const generic parameters their names stand for in the first scope.
struct Reader<'c, 'a> {
    ck: &'c Checker<'a>,
    scopes: Vec<Scope<'c>>,
    values: HashMap<String, Value>,
    consts: HashMap<String, Option<i128>>,
}

impl<'c, 'a> Reader<'c, 'a> {
    fn new(
        ck: &'c Checker<'a>,
        item: ItemId,
        values: HashMap<String, Value>,
        consts: HashMap<String, Option<i128>>,
    ) -> Reader<'c, 'a> {
        Reader {
            ck,
            scopes: vec![Scope {
                item,
                params: HashMap::new(),
            }],
            values,
            consts,
        }
    }

    /// The constructive conjuncts of conditions, or none when one of them is a gap: a conjunct
    /// naming a generated value that is not constructive, an `a != c` among them included when
    /// another names `a` with another drawn integer.
    fn read(&mut self, conditions: &[&'c Expr]) -> Option<Vec<Constructive>> {
        let mut conjuncts = Vec::new();
        for condition in conditions {
            self.conjuncts(condition, 0, 0, &mut conjuncts);
        }
        let mut constructive = Vec::new();
        for (conjunct, scope) in conjuncts {
            if !self.names_value(conjunct, scope) {
                continue;
            }
            constructive.push(self.constructive(conjunct, scope)?);
        }
        let linked = |drawn: &str| {
            constructive.iter().any(|other| {
                other
                    .right
                    .as_ref()
                    .is_some_and(|(right, _)| (other.left.0 == drawn) != (right == drawn))
            })
        };
        let unlinked = constructive
            .iter()
            .filter(|conjunct| conjunct.op == BinaryOp::Ne)
            .all(|conjunct| !linked(&conjunct.left.0));
        unlinked.then_some(constructive)
    }

    /// The conjuncts of a condition read in a scope: the operands of its top-level `and`, each
    /// call of a visible transparent function replaced by the conjuncts of its body.
    fn conjuncts(
        &mut self,
        expr: &'c Expr,
        scope: usize,
        depth: usize,
        found: &mut Vec<(&'c Expr, usize)>,
    ) {
        match &expr.kind {
            ExprKind::Paren(inner) => self.conjuncts(inner, scope, depth, found),
            ExprKind::Binary {
                op: BinaryOp::And,
                left,
                right,
                ..
            } => {
                self.conjuncts(left, scope, depth, found);
                self.conjuncts(right, scope, depth, found);
            }
            ExprKind::Call { callee, args } if depth < UNFOLDING => {
                match self.unfolded(callee, args, scope) {
                    Some((body, inner)) => self.conjuncts(body, inner, depth + 1, found),
                    None => found.push((expr, scope)),
                }
            }
            _ => found.push((expr, scope)),
        }
    }

    /// The body of the transparent function a call read in a scope names, when visible there,
    /// with the scope binding its parameters to the call's receiver and arguments.
    fn unfolded(
        &mut self,
        callee: &'c Expr,
        args: &'c [ast::Arg],
        scope: usize,
    ) -> Option<(&'c Expr, usize)> {
        let db = &self.ck.db;
        let module = db.module_of(self.scopes[scope].item);
        let (function, receiver) = match &callee.kind {
            ExprKind::Path { path, args: None } => match db.resolve(module, path) {
                Ok(Named::Item(function)) => (function, None),
                _ => return None,
            },
            ExprKind::Field { base, name } => {
                let (_, ty) = self.place(base, scope)?;
                let function = *db.members(type_key(&ty)?, &name.text).first()?;
                (function, Some(&**base))
            }
            _ => return None,
        };
        if !db.visible(module, function) {
            return None;
        }
        let ItemKind::Fn(item) = &db.ast(function).kind else {
            return None;
        };
        let FnBody::Expr(body) = &item.body else {
            return None;
        };
        let mut values = args.iter().map(|arg| &arg.value);
        let mut params = HashMap::new();
        for param in &item.head.params {
            let value = match param.kind {
                ParamKind::SelfValue => receiver?,
                ParamKind::Named { .. } => values.next()?,
            };
            params.insert(param.name(), (value, scope));
        }
        self.scopes.push(Scope {
            item: function,
            params,
        });
        Some((body, self.scopes.len() - 1))
    }

    /// The generated value a place read in a scope names: a name or `self` standing for one,
    /// or a field of one.
    fn place(&self, expr: &'c Expr, scope: usize) -> Option<Value> {
        let named = |name: &str| match self.scopes[scope].params.get(name) {
            Some((argument, outer)) => self.place(argument, *outer),
            None => self.values.get(name).filter(|_| scope == 0).cloned(),
        };
        match &expr.kind {
            ExprKind::Paren(inner) => self.place(inner, scope),
            ExprKind::Path { path, args: None } if path.segments.len() == 1 => {
                named(&path.segments[0].text)
            }
            ExprKind::SelfValue => named("self"),
            ExprKind::Field { base, name } => {
                let (path, ty) = self.place(base, scope)?;
                let field = self.ck.field_type(&ty, &name.text)?;
                Some((format!("{path}.{}", name.text), field))
            }
            _ => None,
        }
    }

    /// Whether an expression read in a scope names a generated value.
    fn names_value(&self, expr: &'c Expr, scope: usize) -> bool {
        let mut names: Vec<String> = Vec::new();
        expr.visit(&mut |inner| match &inner.kind {
            ExprKind::Path { path, args: None } if path.segments.len() == 1 => {
                names.push(path.segments[0].text.clone());
            }
            ExprKind::SelfValue => names.push("self".to_owned()),
            _ => {}
        });
        names
            .iter()
            .any(|name| match self.scopes[scope].params.get(name.as_str()) {
                Some((argument, outer)) => self.names_value(argument, *outer),
                None => scope == 0 && self.values.contains_key(name.as_str()),
            })
    }

    /// A term read in a scope.
    fn term(&self, expr: &'c Expr, scope: usize) -> Term {
        let item = self.scopes[scope].item;
        match &expr.kind {
            ExprKind::Paren(inner) => self.term(inner, scope),
            ExprKind::Literal(literal) if literal.kind == LiteralKind::Int => {
                Term::Constant(int_literal(self.ck.db.item(item).text(literal.tokens[0])))
            }
            ExprKind::Unary {
                op: UnaryOp::Neg,
                operand,
            } => match self.term(operand, scope) {
                Term::Constant(value) => Term::Constant(value.map(|value| -value)),
                _ => Term::Other,
            },
            ExprKind::Binary {
                op: op @ (BinaryOp::Add | BinaryOp::Sub),
                left,
                right,
                ..
            } => match (self.term(left, scope), self.term(right, scope)) {
                (Term::Constant(left), Term::Constant(right)) => {
                    Term::Constant(left.zip(right).map(|(left, right)| match op {
                        BinaryOp::Add => left + right,
                        _ => left - right,
                    }))
                }
                _ => Term::Other,
            },
            _ => {
                if let Some((path, ty)) = self.place(expr, scope) {
                    return match ty {
                        Ty::Scalar(scalar) if scalar.is_integer() => {
                            Term::Drawn((path, scalar.range()))
                        }
                        _ => Term::Other,
                    };
                }
                self.length(expr, scope)
                    .or_else(|| self.constant(expr, scope))
                    .unwrap_or(Term::Other)
            }
        }
    }

    /// A length term read in a scope: `len()` of a `mem::List`, `mem::Bytes` or `mem::Bounded`,
    /// or `as_str().char_count()` of a `text::String`, generated.
    fn length(&self, expr: &'c Expr, scope: usize) -> Option<Term> {
        let (base, name) = called(expr)?;
        let (path, ty) = match name {
            "char_count" => {
                let (base, _) = called(base).filter(|(_, name)| *name == "as_str")?;
                let (path, ty) = self.place(base, scope)?;
                self.ck
                    .is_std(&ty, "text::String")
                    .then(|| (format!("{path}.as_str().char_count()"), ty))?
            }
            "len" => {
                let (path, ty) = self.place(base, scope)?;
                (format!("{path}.len()"), ty)
            }
            _ => return None,
        };
        let high = match self.ck.std_path(&ty)?.as_str() {
            "mem::List" | "mem::Bytes" | "text::String" => 64,
            "mem::Bounded" => match ty.args().get(1) {
                Some(Arg::Const(Const::Int(bound))) => *bound,
                _ => i128::from(u64::MAX),
            },
            _ => return None,
        };
        Some(Term::Drawn((path, (0, high))))
    }

    /// A const or const generic parameter a path read in a scope names, with its value when it
    /// is an integer literal.
    fn constant(&self, expr: &'c Expr, scope: usize) -> Option<Term> {
        let ExprKind::Path { path, args: None } = &expr.kind else {
            return None;
        };
        if let [name] = &path.segments[..]
            && scope == 0
            && let Some(value) = self.consts.get(&name.text)
        {
            return Some(Term::Constant(*value));
        }
        let db = &self.ck.db;
        let module = db.module_of(self.scopes[scope].item);
        let Ok(Named::Item(constant)) = db.resolve(module, path) else {
            return None;
        };
        let ItemKind::Const(item) = &db.ast(constant).kind else {
            return None;
        };
        let reader = Reader::new(self.ck, constant, HashMap::new(), HashMap::new());
        match reader.term(&item.value, 0) {
            Term::Constant(value) => Some(Term::Constant(value)),
            _ => Some(Term::Constant(None)),
        }
    }

    /// A conjunct read in a scope as a constructive conjunct: `a op b`, `a op b + c`,
    /// `a op b - c`, `a op c` or `a != c`, with `a` and `b` drawn integers, `c` a constant and
    /// `op` a comparison.
    fn constructive(&self, expr: &'c Expr, scope: usize) -> Option<Constructive> {
        let ExprKind::Binary {
            op, left, right, ..
        } = &expr.kind
        else {
            return match &expr.kind {
                ExprKind::Paren(inner) => self.constructive(inner, scope),
                _ => None,
            };
        };
        let compared = matches!(
            op,
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Ge | BinaryOp::Gt
        );
        let Term::Drawn(left) = self.term(left, scope) else {
            return None;
        };
        if !compared {
            return None;
        }
        let mut summands = Vec::new();
        self.summands(right, scope, 1, &mut summands);
        let mut terms = summands
            .iter()
            .map(|(sign, summand, scope)| (*sign, self.term(summand, *scope)));
        let right = match terms.clone().next() {
            Some((1, Term::Drawn(drawn))) if *op != BinaryOp::Ne => {
                terms.next();
                Some(drawn)
            }
            _ => None,
        };
        let mut offset = Some(0);
        for (sign, term) in terms {
            let Term::Constant(value) = term else {
                return None;
            };
            offset = offset
                .zip(value)
                .map(|(offset, value)| offset + sign * value);
        }
        Some(Constructive {
            left,
            op: *op,
            right,
            offset,
        })
    }

    /// The summands of an expression joined by `+` and `-`, each with its sign.
    fn summands(
        &self,
        expr: &'c Expr,
        scope: usize,
        sign: i128,
        found: &mut Vec<(i128, &'c Expr, usize)>,
    ) {
        match &expr.kind {
            ExprKind::Paren(inner) => self.summands(inner, scope, sign, found),
            ExprKind::Binary {
                op: op @ (BinaryOp::Add | BinaryOp::Sub),
                left,
                right,
                ..
            } => {
                self.summands(left, scope, sign, found);
                let sign = if *op == BinaryOp::Add { sign } else { -sign };
                self.summands(right, scope, sign, found);
            }
            _ => found.push((sign, expr, scope)),
        }
    }
}

/// The receiver and member name of a call of a member without arguments.
fn called(expr: &Expr) -> Option<(&Expr, &str)> {
    let ExprKind::Call { callee, args } = &expr.kind else {
        return None;
    };
    let ExprKind::Field { base, name } = &callee.kind else {
        return None;
    };
    args.is_empty().then_some((&**base, name.text.as_str()))
}

/// Whether constructive conjuncts have a solution: their difference bounds, each `b + c` kept
/// within the type of `b`, without a negative cycle, and each `a != c` leaving `a` a value.
fn satisfiable(conjuncts: &[Constructive]) -> bool {
    let mut nodes: HashMap<&str, (usize, (i128, i128))> = HashMap::new();
    for conjunct in conjuncts {
        for (path, bounds) in std::iter::once(&conjunct.left).chain(&conjunct.right) {
            let next = nodes.len() + 1;
            nodes.entry(path).or_insert((next, *bounds));
        }
    }
    let mut edges: Vec<(usize, usize, i128)> = Vec::new();
    for (node, (low, high)) in nodes.values() {
        edges.push((0, *node, *high));
        edges.push((*node, 0, -low));
    }
    for conjunct in conjuncts {
        let Some(offset) = conjunct.offset else {
            continue;
        };
        let left = nodes[conjunct.left.0.as_str()].0;
        let right = match &conjunct.right {
            Some((path, (low, high))) => {
                let right = nodes[path.as_str()].0;
                edges.push((0, right, high - offset));
                edges.push((right, 0, offset - low));
                right
            }
            None => 0,
        };
        match conjunct.op {
            BinaryOp::Lt => edges.push((right, left, offset - 1)),
            BinaryOp::Le => edges.push((right, left, offset)),
            BinaryOp::Eq => {
                edges.push((right, left, offset));
                edges.push((left, right, -offset));
            }
            BinaryOp::Ge => edges.push((left, right, -offset)),
            BinaryOp::Gt => edges.push((left, right, -offset - 1)),
            _ => {}
        }
    }
    let mut distance = vec![0i128; nodes.len() + 1];
    let mut settled = false;
    for _ in 0..=nodes.len() {
        settled = true;
        for (from, to, weight) in &edges {
            if distance[*from] + weight < distance[*to] {
                distance[*to] = distance[*from] + weight;
                settled = false;
            }
        }
        if settled {
            break;
        }
    }
    if !settled {
        return false;
    }
    nodes.iter().all(|(path, (_, (low, high)))| {
        let mut bounds = (*low, *high);
        let mut excluded = BTreeSet::new();
        for conjunct in conjuncts
            .iter()
            .filter(|conjunct| conjunct.left.0 == *path && conjunct.right.is_none())
        {
            let Some(value) = conjunct.offset else {
                continue;
            };
            match conjunct.op {
                BinaryOp::Lt => bounds.1 = bounds.1.min(value - 1),
                BinaryOp::Le => bounds.1 = bounds.1.min(value),
                BinaryOp::Eq => bounds = (bounds.0.max(value), bounds.1.min(value)),
                BinaryOp::Ge => bounds.0 = bounds.0.max(value),
                BinaryOp::Gt => bounds.0 = bounds.0.max(value + 1),
                _ => {
                    excluded.insert(value);
                }
            }
        }
        let excluded = excluded
            .iter()
            .filter(|value| (bounds.0..=bounds.1).contains(*value))
            .count() as i128;
        bounds.1 - bounds.0 + 1 > excluded
    })
}

/// Whether a target's parameter types, its generic parameters fixed, are a test's.
fn instantiates(pattern: &Ty, found: &Ty, fixed: &mut HashMap<Param, Ty>) -> bool {
    match (pattern, found) {
        (Ty::Param(param), _) => match fixed.get(param) {
            Some(ty) => ty == found,
            None => {
                fixed.insert(*param, found.clone());
                true
            }
        },
        (Ty::Item(left, left_args), Ty::Item(right, right_args)) => {
            left == right
                && left_args.len() == right_args.len()
                && left_args
                    .iter()
                    .zip(right_args.iter())
                    .all(|pair| match pair {
                        (Arg::Ty(left), Arg::Ty(right)) => instantiates(left, right, fixed),
                        (left, right) => {
                            left == right || matches!(left, Arg::Const(Const::Param(_)))
                        }
                    })
        }
        _ => pattern == found,
    }
}

impl Checker<'_> {
    /// The generator gaps of a struct deriving `Generate`, reported once on its first line.
    pub fn check_generated(&self, item: ItemId) {
        let ty = self.own_type(item);
        if self.struct_constraints(item, &ty, "").is_none() {
            let data = self.db.item(item);
            let syntax = data.syntax();
            let line = *syntax.item_lines(&syntax.items[data.index]).start();
            self.report_line(item, line, Code::TestGeneratorGap);
        }
    }

    /// The generator gaps of a property test, reported on its head: a conjunct of its target's
    /// requires that is not constructive, when the target takes the test's parameter types, and
    /// constructive conjuncts of its generation no values satisfy.
    pub fn check_property_test(&self, item: ItemId, test: &TestItem, body: &TBody) {
        if test.params.is_none() {
            return;
        }
        let params: Vec<&Ty> = body.params.iter().map(|id| &body.local(*id).ty).collect();
        if params.iter().any(|ty| ty.has_error()) {
            return;
        }
        let mut constructive = Vec::new();
        if let Some(Target::Function(target)) = self.test_target(item, test)
            && let Some(sig) = self.fn_sig(target)
            && let ItemKind::Fn(function) = &self.db.ast(target).kind
        {
            let mut fixed = HashMap::new();
            let takes = sig.params.len() == params.len()
                && sig
                    .params
                    .iter()
                    .zip(&params)
                    .all(|(param, ty)| instantiates(&param.ty, ty, &mut fixed));
            if takes {
                let data = self.db.item(target);
                let requires: Vec<&Expr> = function
                    .clauses
                    .iter()
                    .filter(|clause| data.text(clause.token) != "todo")
                    .filter_map(|clause| match &clause.kind {
                        ClauseKind::Requires(condition) => Some(condition),
                        _ => None,
                    })
                    .collect();
                let values = sig
                    .params
                    .iter()
                    .zip(&params)
                    .enumerate()
                    .map(|(index, (param, ty))| {
                        (param.name.clone(), (format!("#{index}"), (*ty).clone()))
                    })
                    .collect();
                let mut reader = Reader::new(self, target, values, HashMap::new());
                match reader.read(&requires) {
                    Some(conjuncts) => constructive.extend(conjuncts),
                    None => {
                        self.report(item, self.db.ast(item).token, Code::TestGeneratorGap);
                        return;
                    }
                }
            }
        }
        for (index, ty) in params.iter().enumerate() {
            self.generated_constraints(
                &format!("#{index}"),
                ty,
                &mut Vec::new(),
                &mut constructive,
            );
        }
        if !satisfiable(&constructive) {
            self.report(item, self.db.ast(item).token, Code::TestGeneratorGap);
        }
    }

    /// The constructive conjuncts of the predicates of the structs a value of a type generates
    /// holds, through the fields and type arguments of each, `path` naming the value.
    fn generated_constraints(
        &self,
        path: &str,
        ty: &Ty,
        open: &mut Vec<ItemId>,
        found: &mut Vec<Constructive>,
    ) {
        let Ty::Item(item, args) = ty else {
            return;
        };
        if open.contains(item) {
            return;
        }
        open.push(*item);
        for (index, arg) in args.iter().enumerate() {
            if let Arg::Ty(arg) = arg {
                self.generated_constraints(&format!("{path}<{index}>"), arg, open, found);
            }
        }
        let generate = self.db.core_item("Generate");
        let derived = generate.is_some_and(|generate| self.derives(*item, generate));
        if derived && let SigKind::Struct { fields } = &self.sig(*item).kind {
            found.extend(self.struct_constraints(*item, ty, path).unwrap_or_default());
            for field in fields {
                if let Some(field_ty) = self.field_type(ty, &field.name) {
                    let field_path = format!("{path}.{}", field.name);
                    self.generated_constraints(&field_path, &field_ty, open, found);
                }
            }
        }
        open.pop();
    }

    /// The constructive conjuncts of a struct's predicates for an instantiation of it, its
    /// fields' drawn integers named under `path`; none when one is a gap.
    fn struct_constraints(&self, item: ItemId, ty: &Ty, path: &str) -> Option<Vec<Constructive>> {
        let ItemKind::Struct(structure) = &self.db.ast(item).kind else {
            return Some(Vec::new());
        };
        let sig = self.sig(item);
        let SigKind::Struct { fields } = &sig.kind else {
            return Some(Vec::new());
        };
        let subst = Subst::of(sig.generics.iter().map(|def| def.param), ty.args());
        let prefix = |name: &str| match path {
            "" => name.to_owned(),
            _ => format!("{path}.{name}"),
        };
        let values = fields
            .iter()
            .map(|field| {
                (
                    field.name.clone(),
                    (prefix(&field.name), subst.ty(&field.ty)),
                )
            })
            .collect();
        let consts = sig
            .generics
            .iter()
            .filter(|def| def.constant.is_some())
            .map(|def| {
                let value = match subst.get(def.param) {
                    Some(Arg::Const(Const::Int(value))) => Some(*value),
                    _ => None,
                };
                (def.name.clone(), value)
            })
            .collect();
        let conditions: Vec<&Expr> = structure
            .members
            .iter()
            .filter_map(|member| match member {
                StructMember::Field(field) => field.where_clause.as_ref(),
                StructMember::Invariant(condition) => Some(condition),
            })
            .collect();
        Reader::new(self, item, values, consts).read(&conditions)
    }
}
