//! Queries: the weakest precondition of each obligation site of a NIR function over its
//! control-flow graph with back edges removed. The walk gives a term to each value and to the
//! content of each place at each point, a reach condition to each block, and records the facts
//! and obligation sites of each point.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

use super::canon::Origin;
use super::term::{Op, Sort, Term, TermId, Terms, sort_of};
use crate::check::db::ItemId;
use crate::check::sig::{Payload as PayloadSig, SigKind};
use crate::check::ty::{Arg, Const, Scalar, Subst, Ty};
use crate::check::{Checker, EnsuresClause};
use crate::interp::Val;
use crate::nir::{
    BinaryOp, Block, BlockId, Builtin, Call, Callable, Callee, CheckKind, Constant, Conversion,
    FnRef, Function, Inst, Literal, NTy, Op as NirOp, Operand, Payload, Place, Region, Role, Span,
    Terminator, UnaryOp, Unit, Value,
};
use crate::syntax::ast::Mode;

/// What an obligation site requires, by the rule defining it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Obligation {
    Check(CheckKind),
    Overflow,
    DivisionByZero,
    Shift,
    Conversion,
    Bounds,
}

/// An obligation site of the function walked: its point, what it requires and its goal.
#[derive(Clone, Debug)]
pub struct Site {
    pub block: BlockId,
    /// Its instruction's position, or the block's number of instructions for its terminator.
    pub position: usize,
    pub obligation: Obligation,
    pub goal: TermId,
    pub open: bool,
    pub span: Span,
    /// The index of the ensures clause a `check ensures` checks.
    pub clause: Option<u32>,
}

/// A query: its premises with their origins, then its goal.
pub struct Query {
    pub premises: Vec<(TermId, Origin)>,
    pub goal: TermId,
}

/// The sites of a function walked within another, an invariant or assume function, each with
/// its query.
pub struct Statement {
    pub role: Role,
    pub index: u32,
    pub sites: Vec<(Site, Query)>,
}

/// A fact of a block, holding from the point before its instruction at `from`.
#[derive(Clone, Debug)]
struct Fact {
    block: BlockId,
    from: usize,
    term: TermId,
    origin: Origin,
}

/// The root of a place: a slot, or the place a reference designates.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Root {
    Slot(Value),
    /// The place a reference parameter or a reference-returning call designates.
    Referent(Value),
}

/// A place the walk tracks: its root and the steps from it, or a part of an owned value.
#[derive(Clone, Debug)]
enum Located {
    Place(Root, Vec<String>),
    Owned(TermId),
}

/// The content of each root place at a point.
type Contents = HashMap<Root, TermId>;

/// An edge of the walk: its target, condition, the arguments it passes and the contents on it.
struct Edge {
    target: BlockId,
    condition: TermId,
    args: Vec<TermId>,
    contents: Contents,
}

pub struct Walk<'w, 'a> {
    pub ck: &'w Checker<'a>,
    /// The item whose code is judged, whose visibility decides unfolding.
    holder: ItemId,
    pub terms: &'w mut Terms,
    unit: Rc<Unit>,
    function: usize,
    subst: Subst,
    /// The entry facts, the first premises of each query.
    entry: Vec<(TermId, Origin)>,
    values: Vec<Option<TermId>>,
    /// The place each reference value designates.
    referents: HashMap<Value, Located>,
    /// The places each closure value captures by `mut` or `take`.
    captured: HashMap<Value, Vec<Located>>,
    /// The receiver place of each `mut` projection's result, which a write through the result
    /// changes.
    projected: HashMap<Root, Located>,
    reach: Vec<Option<TermId>>,
    edges: Vec<Vec<Edge>>,
    /// The forward predecessors of each block.
    preds: Vec<Vec<BlockId>>,
    /// The block and position of the instruction whose region each region's first block
    /// starts.
    region_parent: HashMap<BlockId, (BlockId, usize)>,
    /// The block holding the instruction of each region block's region.
    region_holder: HashMap<BlockId, BlockId>,
    /// The blocks of the enclosing code that the regions of each block's instructions jump to.
    region_exits: HashMap<BlockId, Vec<BlockId>>,
    back: BTreeSet<(BlockId, BlockId)>,
    dominators: Vec<Vec<bool>>,
    /// The content each root had when the loop whose header last gave it a fresh symbol
    /// started.
    loop_start: Contents,
    facts: Vec<Fact>,
    pub sites: Vec<Site>,
    /// The invariant and assume functions walked at their calls.
    pub statements: Vec<Statement>,
    /// The returned terms of a function walked for its value, with their reach conditions.
    returns: Vec<(TermId, TermId)>,
    /// The contents at each `yield` of the region being walked, with its reach condition.
    yields: Vec<(TermId, Contents)>,
    /// Whether the walk is of a function unfolded or called for its term, recording nothing.
    nested: bool,
    /// The functions being unfolded, outermost first, which no call within unfolds again.
    unfolding: Vec<ItemId>,
    /// The index of the unit's own ensures function each value is the result of a call to.
    ensures: HashMap<Value, u32>,
    /// Within the facts of one call, one requires site or the entry facts, the fresh symbol
    /// standing for each opaque subterm: one naming a field or item the holder cannot see.
    opaque: Option<HashMap<TermId, TermId>>,
}

impl<'w, 'a> Walk<'w, 'a> {
    /// Walks a function of a unit, its parameters having the given terms: for a reference
    /// parameter, the content of the place it designates.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ck: &'w Checker<'a>,
        holder: ItemId,
        terms: &'w mut Terms,
        unit: Rc<Unit>,
        function: usize,
        type_args: &[Arg],
        params: Vec<TermId>,
        entry: Vec<(TermId, Origin)>,
    ) -> Walk<'w, 'a> {
        let blocks = unit.functions[function].blocks.len();
        let mut walk = Walk {
            ck,
            holder,
            terms,
            subst: ck.unit_subst(unit.item, type_args),
            entry: Vec::new(),
            values: vec![None; unit.functions[function].types.len()],
            referents: HashMap::new(),
            captured: HashMap::new(),
            projected: HashMap::new(),
            reach: vec![None; blocks],
            edges: (0..blocks).map(|_| Vec::new()).collect(),
            preds: vec![Vec::new(); blocks],
            region_parent: HashMap::new(),
            region_holder: HashMap::new(),
            region_exits: HashMap::new(),
            back: BTreeSet::new(),
            dominators: Vec::new(),
            loop_start: Contents::new(),
            facts: Vec::new(),
            sites: Vec::new(),
            statements: Vec::new(),
            returns: Vec::new(),
            yields: Vec::new(),
            nested: false,
            unfolding: Vec::new(),
            ensures: HashMap::new(),
            opaque: None,
            unit,
            function,
        };
        let declared: Vec<Value> = walk.nir().params.iter().map(|param| param.value).collect();
        for value in declared {
            if walk.nir().ty(value).is_reference() {
                walk.referents
                    .insert(value, Located::Place(Root::Referent(value), Vec::new()));
            }
        }
        walk.restart(params, entry);
        walk.shape();
        walk
    }

    /// Gives the parameters their terms and the walk its entry facts, before it walks.
    pub fn restart(&mut self, params: Vec<TermId>, entry: Vec<(TermId, Origin)>) {
        let declared: Vec<Value> = self.nir().params.iter().map(|param| param.value).collect();
        for (value, term) in declared.into_iter().zip(params) {
            self.values[value.0 as usize] = Some(term);
        }
        self.entry = entry;
    }

    fn nir(&self) -> &Function {
        &self.unit.functions[self.function]
    }

    /// The forward graph: predecessors, back edges and dominators, each region's first block
    /// dominated by its instruction's block, and the region blocks and exits of each block.
    fn shape(&mut self) {
        let unit = self.unit.clone();
        let function = &unit.functions[self.function];
        let count = function.blocks.len();
        let mut all_preds: Vec<Vec<BlockId>> = vec![Vec::new(); count];
        for (index, block) in function.blocks.iter().enumerate() {
            for target in block.term.successors() {
                all_preds[target.0 as usize].push(BlockId(index as u32));
            }
        }
        let mut dominating = all_preds.clone();
        for (index, block) in function.blocks.iter().enumerate() {
            let holder = BlockId(index as u32);
            for (position, inst) in block.insts.iter().enumerate() {
                if let NirOp::Quantifier { region, .. } | NirOp::When { region, .. } = &inst.op {
                    let entry = region.blocks[0];
                    self.region_parent.insert(entry, (holder, position));
                    dominating[entry.0 as usize].push(holder);
                    for inner in &region.blocks {
                        self.region_holder.insert(*inner, holder);
                        for target in function.block(*inner).term.successors() {
                            if !region.blocks.contains(&target) {
                                self.region_exits.entry(holder).or_default().push(target);
                            }
                        }
                    }
                }
            }
        }
        let mut dominators = vec![vec![true; count]; count];
        dominators[0] = (0..count).map(|index| index == 0).collect();
        let mut changed = true;
        while changed {
            changed = false;
            for block in 1..count {
                let mut next = match dominating[block].is_empty() {
                    true => vec![false; count],
                    false => vec![true; count],
                };
                for pred in &dominating[block] {
                    for (index, slot) in next.iter_mut().enumerate() {
                        *slot &= dominators[pred.0 as usize][index];
                    }
                }
                next[block] = true;
                if next != dominators[block] {
                    dominators[block] = next;
                    changed = true;
                }
            }
        }
        for (index, preds) in all_preds.iter().enumerate() {
            for pred in preds {
                if dominators[pred.0 as usize][index] {
                    self.back.insert((*pred, BlockId(index as u32)));
                } else if !self.preds[index].contains(pred) {
                    self.preds[index].push(*pred);
                }
            }
            self.preds[index].sort();
        }
        self.dominators = dominators;
    }

    /// Walks every block reachable from the entry, each after its forward predecessors.
    pub fn walk(&mut self) {
        let unit = self.unit.clone();
        let function = &unit.functions[self.function];
        for (index, block) in self.order(BlockId(0), None).into_iter().enumerate() {
            let data = function.block(block);
            let contents = match index {
                0 => self.start(),
                _ => self.entry(block, data),
            };
            self.block(block, data, contents);
        }
    }

    /// The contents at the function's entry: each reference parameter's place holds its term.
    fn start(&mut self) -> Contents {
        self.reach[0] = Some(self.terms.bool(true));
        let mut contents = Contents::new();
        for param in &self.nir().params {
            if self.nir().ty(param.value).is_reference() {
                contents.insert(Root::Referent(param.value), self.value(param.value));
            }
        }
        contents
    }

    /// The blocks reachable from `start` along forward edges, within a region's blocks when
    /// given, in an order where each follows its forward predecessors; a block a region jumps
    /// to is reached from, and follows, the block holding the region's instruction.
    fn order(&self, start: BlockId, within: Option<&[BlockId]>) -> Vec<BlockId> {
        let function = self.nir();
        let mut seen = BTreeSet::from([start]);
        let mut pending = vec![start];
        while let Some(block) = pending.pop() {
            let exits = self.region_exits.get(&block).into_iter().flatten().copied();
            for successor in function
                .block(block)
                .term
                .successors()
                .into_iter()
                .chain(exits)
            {
                if !self.back.contains(&(block, successor))
                    && within.is_none_or(|blocks| blocks.contains(&successor))
                    && seen.insert(successor)
                {
                    pending.push(successor);
                }
            }
        }
        let outside = |mut block: BlockId| {
            while !seen.contains(&block)
                && let Some(holder) = self.region_holder.get(&block)
            {
                block = *holder;
            }
            block
        };
        let mut order = vec![start];
        let mut done = BTreeSet::from([start]);
        while order.len() < seen.len() {
            let next = seen
                .iter()
                .copied()
                .find(|block| {
                    !done.contains(block)
                        && self.preds[block.0 as usize]
                            .iter()
                            .map(|pred| outside(*pred))
                            .filter(|pred| seen.contains(pred))
                            .all(|pred| done.contains(&pred))
                })
                .expect("an acyclic forward graph");
            done.insert(next);
            order.push(next);
        }
        order
    }

    /// The term of a value.
    pub fn value(&self, value: Value) -> TermId {
        self.values[value.0 as usize].unwrap_or_else(|| panic!("value %{} has no term", value.0))
    }

    fn ty(&self, value: Value) -> Ty {
        self.subst.ty(self.nir().ty(value).ty())
    }

    /// Walks a block's instructions from its entry contents, then records its edges.
    fn block(&mut self, block: BlockId, data: &Block, mut contents: Contents) {
        for (position, inst) in data.insts.iter().enumerate() {
            self.inst(block, position, inst, &mut contents);
        }
        let edges = self.terminator(block, data, contents);
        self.edges[block.0 as usize] = edges;
    }

    /// The contents at a block's entry, its reach condition and parameters, merged from its
    /// incoming forward edges; at a loop's header, each place the loop changes, and each
    /// parameter, has a fresh symbol instead.
    fn entry(&mut self, block: BlockId, data: &Block) -> Contents {
        let incoming: Vec<(TermId, Vec<TermId>, Contents)> = self.preds[block.0 as usize]
            .iter()
            .flat_map(|pred| {
                self.edges[pred.0 as usize]
                    .iter()
                    .filter(|edge| edge.target == block)
                    .map(|edge| (edge.condition, edge.args.clone(), edge.contents.clone()))
                    .collect::<Vec<_>>()
            })
            .collect();
        let conditions: Vec<TermId> = incoming.iter().map(|(condition, ..)| *condition).collect();
        self.reach[block.0 as usize] = Some(self.terms.or(conditions.clone()));
        let mut contents = Contents::new();
        if let Some((_, _, first)) = incoming.first() {
            let mut roots: Vec<&Root> = first.keys().collect();
            roots.sort();
            for root in roots {
                let arms: Option<Vec<TermId>> = incoming
                    .iter()
                    .map(|(_, _, contents)| contents.get(root).copied())
                    .collect();
                if let Some(arms) = arms {
                    let merged = self.ites(&conditions, &arms);
                    contents.insert(root.clone(), merged);
                }
            }
        }
        let header = self.back.iter().any(|(_, target)| *target == block);
        for (index, param) in data.params.iter().enumerate() {
            let term = match header || incoming.is_empty() {
                true => {
                    let ty = self.ty(*param);
                    self.terms.fresh(ty)
                }
                false => {
                    let arms: Vec<TermId> =
                        incoming.iter().map(|(_, args, _)| args[index]).collect();
                    self.ites(&conditions, &arms)
                }
            };
            self.values[param.0 as usize] = Some(term);
        }
        if header {
            for root in self.changed_in_loop(block) {
                if let Some(old) = contents.get(&root).copied() {
                    self.loop_start.insert(root.clone(), old);
                    let ty = self.terms.ty(old).clone();
                    let fresh = self.terms.fresh(ty);
                    contents.insert(root, fresh);
                }
            }
        }
        contents
    }

    /// `ite(c1, a1, ite(c2, a2, ... an))`, or the one term all arms are.
    fn ites(&mut self, conditions: &[TermId], arms: &[TermId]) -> TermId {
        let mut term = *arms.last().expect("an arm");
        for index in (0..arms.len() - 1).rev() {
            term = self.terms.ite(conditions[index], arms[index], term);
        }
        term
    }

    /// The roots whose content an instruction or call of a loop's blocks, or of the regions
    /// they hold, changes.
    fn changed_in_loop(&self, header: BlockId) -> Vec<Root> {
        let function = self.nir();
        let mut blocks = BTreeSet::from([header]);
        let mut pending: Vec<BlockId> = self
            .back
            .iter()
            .filter(|(_, target)| *target == header)
            .map(|(source, _)| *source)
            .collect();
        while let Some(block) = pending.pop() {
            if blocks.insert(block) {
                pending.extend(self.preds[block.0 as usize].iter().copied());
                pending.extend(self.region_parent.get(&block).map(|(holder, _)| *holder));
            }
        }
        let held: Vec<BlockId> = self
            .region_holder
            .keys()
            .copied()
            .filter(|block| self.held_by(*block, &blocks))
            .collect();
        blocks.extend(held);
        let mut roots = Vec::new();
        for block in blocks {
            let data = function.block(block);
            let mut places: Vec<Place> = data
                .insts
                .iter()
                .flat_map(|inst| written(&inst.op))
                .collect();
            if let Terminator::Call { call, .. } = &data.term {
                places.extend(changed(call));
            }
            roots.extend(places.iter().filter_map(|place| self.root_of(place)));
        }
        roots.sort();
        roots.dedup();
        roots
    }

    /// The root a NIR place lies in, as far as the walk knows it.
    fn root_of(&self, place: &Place) -> Option<Root> {
        if let Some(Located::Place(root, _)) = self.referents.get(&place.root) {
            return Some(root.clone());
        }
        self.is_slot(place.root).then_some(Root::Slot(place.root))
    }

    fn is_slot(&self, value: Value) -> bool {
        self.nir().blocks.iter().any(|block| {
            block
                .insts
                .iter()
                .any(|inst| inst.result == Some(value) && matches!(inst.op, NirOp::Slot { .. }))
        })
    }

    /// Where a NIR place lies: a root and steps, or a part of an owned value.
    fn locate(&mut self, place: &Place) -> Located {
        let base = match self.referents.get(&place.root) {
            Some(located) => located.clone(),
            None if self.is_slot(place.root) => Located::Place(Root::Slot(place.root), Vec::new()),
            None => Located::Owned(self.value(place.root)),
        };
        match base {
            Located::Place(root, mut steps) => {
                steps.extend(place.steps.iter().cloned());
                Located::Place(root, steps)
            }
            Located::Owned(mut term) => {
                for step in &place.steps {
                    term = self.field(term, step);
                }
                Located::Owned(term)
            }
        }
    }

    /// The content of a place at a point.
    fn content(&mut self, located: &Located, contents: &Contents) -> TermId {
        match located {
            Located::Owned(term) => *term,
            Located::Place(root, steps) => {
                let mut term = match contents.get(root) {
                    Some(term) => *term,
                    None => self.terms.fresh(Ty::Unit),
                };
                for step in steps {
                    term = self.field(term, step);
                }
                term
            }
        }
    }

    /// The field or payload a step takes of a term: the operand of a construction or variant,
    /// the ite of the arms' fields for an ite, otherwise the field step application.
    fn field(&mut self, term: TermId, step: &str) -> TermId {
        match self.terms.get(term).clone() {
            Term::Ite(condition, then, otherwise) => {
                let then = self.field(then, step);
                let otherwise = self.field(otherwise, step);
                return self.terms.ite(condition, then, otherwise);
            }
            Term::App(Op::Construct(ty), operands) => {
                if let Some(index) = self.field_index(&ty, step) {
                    return operands[index];
                }
            }
            Term::App(Op::Variant(_, _), operands)
                if crate::nir::is_variant_step(step) && operands.len() == 1 =>
            {
                return operands[0];
            }
            _ => {}
        }
        let ty = self.step_ty(term, step);
        let field = self.terms.intern(Term::Field(term, step.to_owned()), ty);
        let hidden = match self.terms.ty(term) {
            Ty::Item(owner, _) => match &self.ck.sig(*owner).kind {
                SigKind::Struct { fields } | SigKind::Layout { fields, .. } => fields
                    .iter()
                    .find(|declared| declared.name == step)
                    .is_some_and(|declared| {
                        !self
                            .ck
                            .db
                            .field_visible(self.module(), *owner, declared.visibility)
                    }),
                _ => false,
            },
            _ => false,
        };
        self.opaque_term(field, hidden)
    }

    /// The module of the code judged.
    fn module(&self) -> crate::check::db::ModuleId {
        self.ck.db.module_of(self.holder)
    }

    /// A subterm, or within a scope of opacity the fresh symbol standing for it when `hidden`.
    fn opaque_term(&mut self, term: TermId, hidden: bool) -> TermId {
        if !hidden || self.opaque.is_none() {
            return term;
        }
        if let Some(symbol) = self.opaque.as_ref().and_then(|opaque| opaque.get(&term)) {
            return *symbol;
        }
        let ty = self.terms.ty(term).clone();
        let symbol = self.terms.fresh(ty);
        if let Some(opaque) = self.opaque.as_mut() {
            opaque.insert(term, symbol);
        }
        symbol
    }

    /// Computes within its own scope of opacity.
    pub fn opaquely<T>(&mut self, compute: impl FnOnce(&mut Self) -> T) -> T {
        let outer = self.opaque.replace(HashMap::new());
        let computed = compute(self);
        self.opaque = outer;
        computed
    }

    fn field_index(&self, ty: &Ty, name: &str) -> Option<usize> {
        let Ty::Item(item, _) = ty else {
            return None;
        };
        match &self.ck.sig(*item).kind {
            SigKind::Struct { fields } | SigKind::Layout { fields, .. } => {
                fields.iter().position(|field| field.name == name)
            }
            _ => None,
        }
    }

    /// The type of a field or payload step of a term.
    fn step_ty(&self, term: TermId, step: &str) -> Ty {
        let ty = self.terms.ty(term).clone();
        let Ty::Item(item, args) = &ty else {
            return Ty::Error;
        };
        let sig = self.ck.sig(*item);
        let subst = Subst::of(sig.generics.iter().map(|def| def.param), args);
        match &sig.kind {
            SigKind::Struct { fields } | SigKind::Layout { fields, .. } => fields
                .iter()
                .find(|field| field.name == step)
                .map_or(Ty::Error, |field| subst.ty(&field.ty)),
            SigKind::Enum { variants } => {
                match variants.iter().find(|variant| variant.name == step) {
                    Some(variant) => match &variant.payload {
                        PayloadSig::Single(ty) => subst.ty(ty),
                        _ => ty.clone(),
                    },
                    None => variants
                        .iter()
                        .find_map(|variant| match &variant.payload {
                            PayloadSig::Labelled(fields) => fields
                                .iter()
                                .find(|(name, _)| name == step)
                                .map(|(_, ty)| subst.ty(ty)),
                            _ => None,
                        })
                        .unwrap_or(Ty::Error),
                }
            }
            _ => Ty::Error,
        }
    }

    /// Stores a term into a place: its root's content becomes the term, or the root's former
    /// content with the place's field replaced. A store through a `mut` projection's result
    /// gives its receiver a fresh symbol.
    fn store(&mut self, located: &Located, term: TermId, contents: &mut Contents) {
        let Located::Place(root, steps) = located else {
            return;
        };
        let updated = match contents.get(root).copied() {
            Some(old) => self.replace(old, steps, term),
            None if steps.is_empty() => term,
            None => {
                let ty = self.terms.ty(term).clone();
                self.terms.fresh(ty)
            }
        };
        contents.insert(root.clone(), updated);
        if let Some(receiver) = self.projected.get(root).cloned() {
            self.havoc(&receiver, contents);
        }
    }

    /// A term with the part at a path replaced: a struct's construction of its fields with the
    /// replaced one new; a fresh symbol for a path through another kind of value.
    fn replace(&mut self, term: TermId, steps: &[String], new: TermId) -> TermId {
        let Some((first, rest)) = steps.split_first() else {
            return new;
        };
        let ty = self.terms.ty(term).clone();
        let fields: Vec<String> = match &ty {
            Ty::Item(item, _) => match &self.ck.sig(*item).kind {
                SigKind::Struct { fields } => {
                    fields.iter().map(|field| field.name.clone()).collect()
                }
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        if !fields.contains(first) {
            return self.terms.fresh(ty);
        }
        let operands: Vec<TermId> = fields
            .iter()
            .map(|name| {
                let part = self.field(term, name);
                match name == first {
                    true => self.replace(part, rest, new),
                    false => part,
                }
            })
            .collect();
        self.terms
            .intern(Term::App(Op::Construct(ty.clone()), operands), ty)
    }

    /// Gives a place a fresh symbol as content, as the frame rule does for a place a call may
    /// change.
    fn havoc(&mut self, located: &Located, contents: &mut Contents) {
        let current = self.content(located, contents);
        let ty = self.terms.ty(current).clone();
        let fresh = self.terms.fresh(ty);
        self.store(located, fresh, contents);
    }

    /// The term of an operand; a reference stands for the content of the place it designates.
    fn operand(&mut self, operand: &Operand, contents: &Contents) -> TermId {
        match operand {
            Operand::Value(value) => match self.referents.get(value).cloned() {
                Some(located) => self.content(&located, contents),
                None => self.value(*value),
            },
            Operand::Literal(literal) => self.terms.literal(literal.clone()),
            Operand::Place(_, place) => {
                let located = self.locate(place);
                self.content(&located, contents)
            }
        }
    }

    /// Records a fact holding after the instruction at `position` of a block.
    fn fact(&mut self, block: BlockId, position: usize, term: TermId, origin: Origin) {
        self.fact_from(block, position + 1, term, origin);
    }

    fn fact_from(&mut self, block: BlockId, from: usize, term: TermId, origin: Origin) {
        if !self.nested {
            self.facts.push(Fact {
                block,
                from,
                term,
                origin,
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn site(
        &mut self,
        block: BlockId,
        position: usize,
        obligation: Obligation,
        goal: TermId,
        open: bool,
        span: Span,
    ) {
        self.clause_site(block, position, obligation, goal, open, span, None);
    }

    #[allow(clippy::too_many_arguments)]
    fn clause_site(
        &mut self,
        block: BlockId,
        position: usize,
        obligation: Obligation,
        goal: TermId,
        open: bool,
        span: Span,
        clause: Option<u32>,
    ) {
        if self.nested {
            return;
        }
        self.sites.push(Site {
            block,
            position,
            obligation,
            goal,
            open,
            span,
            clause,
        });
        if !open {
            self.fact(block, position, goal, Origin::Site(self.sites.len() - 1));
        }
    }

    fn inst(&mut self, block: BlockId, position: usize, inst: &Inst, contents: &mut Contents) {
        let result_ty = inst.result.map(|value| self.ty(value));
        let term = match &inst.op {
            NirOp::Slot { .. } | NirOp::Release(_) => None,
            NirOp::Load(place) | NirOp::Move(place) => {
                let located = self.locate(place);
                Some(self.content(&located, contents))
            }
            NirOp::Ref(place) | NirOp::RefMut(place) => {
                let located = self.locate(place);
                if let Some(result) = inst.result {
                    self.referents.insert(result, located);
                }
                None
            }
            NirOp::Init(place, operand) | NirOp::Assign(place, operand) => {
                let term = self.operand(operand, contents);
                let located = self.locate(place);
                self.store(&located, term, contents);
                None
            }
            NirOp::FnRef(fn_ref) => {
                let fn_ref = self.substituted(fn_ref);
                let ty = result_ty.clone().expect("a function value");
                Some(
                    self.terms
                        .intern(Term::App(Op::FnRef(fn_ref), Vec::new()), ty),
                )
            }
            NirOp::Constant(constant) => {
                Some(self.constant(constant, result_ty.clone().expect("a constant's type")))
            }
            NirOp::Construct { ty, fields } => {
                let operands = fields
                    .iter()
                    .map(|(_, operand)| self.operand(operand, contents))
                    .collect();
                let ty = self.subst.ty(ty);
                Some(
                    self.terms
                        .intern(Term::App(Op::Construct(ty.clone()), operands), ty),
                )
            }
            NirOp::Variant {
                ty,
                variant,
                payload,
            } => {
                let operands = match payload {
                    Payload::None => Vec::new(),
                    Payload::Single(operand) => vec![self.operand(operand, contents)],
                    Payload::Labelled(fields) => fields
                        .iter()
                        .map(|(_, operand)| self.operand(operand, contents))
                        .collect(),
                };
                let ty = self.subst.ty(ty);
                let index = self.variant_index(&ty, variant);
                Some(
                    self.terms
                        .intern(Term::App(Op::Variant(ty.clone(), index), operands), ty),
                )
            }
            NirOp::Is { place, ty, variant } => {
                let located = self.locate(place);
                let scrutinee = self.content(&located, contents);
                let ty = self.subst.ty(ty);
                Some(self.tag_is(scrutinee, self.variant_index(&ty, variant)))
            }
            NirOp::Closure { index, captures } => {
                let operands: Vec<TermId> = captures
                    .iter()
                    .map(|operand| self.operand(operand, contents))
                    .collect();
                if let Some(result) = inst.result {
                    let places = captures
                        .iter()
                        .filter_map(|operand| match operand {
                            Operand::Place(Mode::Mut | Mode::Take, place) => {
                                Some(self.locate(place))
                            }
                            _ => None,
                        })
                        .collect();
                    self.captured.insert(result, places);
                }
                let ty = result_ty.clone().expect("a closure value");
                Some(
                    self.terms
                        .intern(Term::App(Op::Closure(*index), operands), ty),
                )
            }
            NirOp::Convert { value, to, via } => {
                let operand = self.operand(value, contents);
                let to = self.subst.ty(to);
                Some(self.terms.intern(
                    Term::App(Op::Convert(to.clone(), via.clone()), vec![operand]),
                    to,
                ))
            }
            NirOp::Unary(op, operand) => {
                let operand = self.operand(operand, contents);
                let ty = result_ty.clone().expect("a unary result");
                Some(self.unary(block, position, *op, operand, ty, inst.span))
            }
            NirOp::Binary(op, left, right) => {
                let left = self.operand(left, contents);
                let right = self.operand(right, contents);
                let ty = result_ty.clone().expect("a binary result");
                Some(self.binary(block, position, *op, left, right, ty, inst.span))
            }
            NirOp::Call(call) => {
                if let (Callee::Fn(fn_ref), Some(result)) = (&call.callee, inst.result)
                    && let Callable::Item {
                        item,
                        role: Role::Ensures,
                        index,
                    } = &fn_ref.callable
                    && *item == self.unit.item
                {
                    self.ensures.insert(result, *index);
                }
                self.call(
                    block,
                    position,
                    None,
                    call,
                    result_ty.clone(),
                    inst.result,
                    inst.span,
                    contents,
                )
            }
            NirOp::Check {
                kind,
                operand,
                open,
            } => {
                let goal = self.operand(operand, contents);
                let clause = match operand {
                    Operand::Value(value) => self.ensures.get(value).copied(),
                    _ => None,
                };
                self.clause_site(
                    block,
                    position,
                    Obligation::Check(*kind),
                    goal,
                    *open,
                    inst.span,
                    clause,
                );
                None
            }
            NirOp::Dbg(operand) => Some(self.operand(operand, contents)),
            NirOp::Assume(operand) => {
                let fact = self.operand(operand, contents);
                self.fact(block, position, fact, Origin::Assume);
                None
            }
            NirOp::Quantifier {
                all,
                low,
                high,
                inclusive,
                region,
            } => {
                let low = self.operand(low, contents);
                let high = self.operand(high, contents);
                Some(self.quantifier(block, *all, low, high, *inclusive, region, contents))
            }
            NirOp::When {
                attempt, region, ..
            } => {
                let unit = self.unit.clone();
                let mut acquired = contents.clone();
                let mut params = Vec::new();
                for param in &unit.functions[self.function].block(region.blocks[0]).params {
                    let ty = self.ty(*param);
                    let value = self.terms.fresh(ty);
                    acquired.insert(Root::Referent(*param), value);
                    self.referents
                        .insert(*param, Located::Place(Root::Referent(*param), Vec::new()));
                    params.push(value);
                }
                let (yielded, after) = self.region(block, region, params, &acquired);
                for (root, term) in after {
                    if let Some(content) = contents.get_mut(&root) {
                        *content = term;
                    }
                }
                match (attempt, yielded) {
                    (false, Some(yielded)) if result_ty.is_some() => Some(yielded),
                    _ => result_ty.clone().map(|ty| self.terms.fresh(ty)),
                }
            }
        };
        if let (Some(result), Some(term)) = (inst.result, term) {
            self.values[result.0 as usize] = Some(term);
        }
    }

    /// A function reference under the walk's type arguments, a call through a bound whose
    /// `Self` they fix calling the implementing function.
    fn substituted(&self, fn_ref: &FnRef) -> FnRef {
        let substituted = FnRef {
            callable: fn_ref.callable.clone(),
            args: self.subst.args(&fn_ref.args),
        };
        let Callable::Method { interface, method } = &substituted.callable else {
            return substituted;
        };
        let (Some(Arg::Ty(self_ty)), SigKind::Interface { methods }) =
            (substituted.args.first(), &self.ck.sig(*interface).kind)
        else {
            return substituted;
        };
        if matches!(self_ty, Ty::Param(_)) {
            return substituted;
        }
        match self
            .ck
            .implementation(self_ty, *interface, &methods[*method as usize].name)
        {
            Some(implementation) if !matches!(implementation.callable, Callable::Method { .. }) => {
                implementation
            }
            _ => substituted,
        }
    }

    fn variant_index(&self, ty: &Ty, name: &str) -> u32 {
        let Ty::Item(item, _) = ty else {
            return 0;
        };
        match &self.ck.sig(*item).kind {
            SigKind::Enum { variants } => variants
                .iter()
                .position(|variant| variant.name == name)
                .unwrap_or(0) as u32,
            SigKind::Layout { tags, .. } => {
                tags.iter().position(|tag| tag == name).unwrap_or(0) as u32
            }
            _ => 0,
        }
    }

    /// The atom that the tag of a term equals a variant's index, the ite of the arms' atoms
    /// for an ite.
    fn tag_is(&mut self, scrutinee: TermId, index: u32) -> TermId {
        if let Term::Ite(condition, then, otherwise) = self.terms.get(scrutinee).clone() {
            let then = self.tag_is(then, index);
            let otherwise = self.tag_is(otherwise, index);
            return self.terms.ite(condition, then, otherwise);
        }
        let tag = match self.terms.get(scrutinee).clone() {
            Term::App(Op::Variant(_, variant), _) => {
                self.terms.int(i128::from(variant), Scalar::U32)
            }
            _ => self
                .terms
                .intern(Term::Tag(scrutinee), Ty::Scalar(Scalar::U32)),
        };
        let index = self.terms.int(i128::from(index), Scalar::U32);
        self.terms.compare(BinaryOp::Eq, tag, index)
    }

    /// The term of a const or const parameter: its value for an integer, `bool` or `char`
    /// const, its application otherwise.
    fn constant(&mut self, constant: &Constant, ty: Ty) -> TermId {
        if let Constant::Item(item) = constant
            && !self.ck.db.visible(self.module(), *item)
        {
            let term = self
                .terms
                .intern(Term::App(Op::Constant(*constant), Vec::new()), ty);
            return self.opaque_term(term, true);
        }
        let value = match constant {
            Constant::Item(item) if sort_of(&ty) != Sort::Other => {
                self.ck.const_value(*item).map(|value| value.value.clone())
            }
            Constant::Param(param) => match self.subst.get(*param) {
                Some(Arg::Const(Const::Int(value))) => Some(Val::Int(*value)),
                Some(Arg::Const(Const::Bool(value))) => Some(Val::Bool(*value)),
                _ => None,
            },
            _ => None,
        };
        let literal = match (value, &ty) {
            (Some(Val::Int(value)), Ty::Scalar(scalar)) => Some(Literal::Int(value, *scalar)),
            (Some(Val::Bool(value)), _) => Some(Literal::Bool(value)),
            (Some(Val::Char(c)), _) => Some(Literal::Char(c)),
            _ => None,
        };
        match literal {
            Some(literal) => self.terms.literal(literal),
            None => self
                .terms
                .intern(Term::App(Op::Constant(*constant), Vec::new()), ty),
        }
    }

    fn unary(
        &mut self,
        block: BlockId,
        position: usize,
        op: UnaryOp,
        operand: TermId,
        ty: Ty,
        span: Span,
    ) -> TermId {
        match op {
            UnaryOp::Not if sort_of(&ty) == Sort::Bool => self.terms.not(operand),
            UnaryOp::Neg if sort_of(&ty) == Sort::Int => {
                let term = self
                    .terms
                    .intern(Term::App(Op::Unary(op), vec![operand]), ty.clone());
                let goal = self.in_range(term, &ty);
                self.site(block, position, Obligation::Overflow, goal, false, span);
                term
            }
            _ => self
                .terms
                .intern(Term::App(Op::Unary(op), vec![operand]), ty),
        }
    }

    /// The condition that a term is a value of an integer type.
    fn in_range(&mut self, term: TermId, ty: &Ty) -> TermId {
        let Some(scalar) = ty.scalar().filter(|scalar| scalar.is_integer()) else {
            return self.terms.bool(true);
        };
        let (low, high) = scalar.range();
        let low = self.terms.int(low, scalar);
        let high = self.terms.int(high, scalar);
        let above = self.terms.compare(BinaryOp::Le, low, term);
        let below = self.terms.compare(BinaryOp::Le, term, high);
        self.terms.and(vec![above, below])
    }

    #[allow(clippy::too_many_arguments)]
    fn binary(
        &mut self,
        block: BlockId,
        position: usize,
        op: BinaryOp,
        left: TermId,
        right: TermId,
        ty: Ty,
        span: Span,
    ) -> TermId {
        let sort = self.terms.sort(left);
        if op.is_comparison() {
            if sort == Sort::Bool && matches!(op, BinaryOp::Eq | BinaryOp::Ne) {
                let negated = self.terms.not(right);
                let equal = self.terms.ite(left, right, negated);
                return match op {
                    BinaryOp::Eq => equal,
                    _ => self.terms.not(equal),
                };
            }
            return self.terms.compare(op, left, right);
        }
        let term = self
            .terms
            .intern(Term::App(Op::Binary(op), vec![left, right]), ty.clone());
        let Some(scalar) = ty
            .scalar()
            .filter(|scalar| scalar.is_integer() && sort == Sort::Int)
        else {
            return term;
        };
        match op {
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul => {
                let goal = self.in_range(term, &ty);
                self.site(block, position, Obligation::Overflow, goal, false, span);
            }
            BinaryOp::Div | BinaryOp::Rem => {
                let zero = self.terms.int(0, scalar);
                let goal = self.terms.compare(BinaryOp::Ne, right, zero);
                self.site(
                    block,
                    position,
                    Obligation::DivisionByZero,
                    goal,
                    false,
                    span,
                );
                if scalar.is_signed() && op == BinaryOp::Div {
                    let goal = self.in_range(term, &ty);
                    self.site(block, position, Obligation::Overflow, goal, false, span);
                }
                if !scalar.is_signed()
                    && self.terms.as_int(right).is_some_and(|divisor| divisor >= 1)
                {
                    return self.division(op, left, right, ty);
                }
            }
            BinaryOp::Shl | BinaryOp::Shr => {
                let width = self.terms.int(i128::from(scalar.bits()), Scalar::U32);
                let goal = self.terms.compare(BinaryOp::Lt, right, width);
                self.site(block, position, Obligation::Shift, goal, false, span);
                if op == BinaryOp::Shl && scalar.is_signed() {
                    let goal = self.in_range(term, &ty);
                    self.site(block, position, Obligation::Overflow, goal, false, span);
                }
                if let Some(amount) = self.terms.as_int(right)
                    && (0..i128::from(scalar.bits())).contains(&amount)
                {
                    let power = self.terms.int(1i128 << amount, scalar);
                    match op {
                        BinaryOp::Shr if !scalar.is_signed() => {
                            return self.division(BinaryOp::Div, left, power, ty);
                        }
                        BinaryOp::Shl if scalar.is_signed() => {
                            return self.terms.intern(
                                Term::App(Op::Binary(BinaryOp::Mul), vec![left, power]),
                                ty,
                            );
                        }
                        _ => {}
                    }
                }
            }
            BinaryOp::Band if !scalar.is_signed() => {
                for (value, mask) in [(left, right), (right, left)] {
                    if let Some(mask) = self.terms.as_int(mask)
                        && mask >= 0
                        && (mask + 1).count_ones() == 1
                    {
                        let power = self.terms.int(mask + 1, scalar);
                        return self.division(BinaryOp::Rem, value, power, ty);
                    }
                }
            }
            _ => {}
        }
        term
    }

    /// The quotient or remainder of an unsigned division by a literal of at least 1.
    fn division(&mut self, op: BinaryOp, dividend: TermId, divisor: TermId, ty: Ty) -> TermId {
        match op {
            BinaryOp::Div => self.terms.intern(Term::Quot(dividend, divisor), ty),
            _ => self.terms.intern(Term::Rem(dividend, divisor), ty),
        }
    }

    /// A call: the obligation of a built-in callee, its term, the frame rule on what it may
    /// change, and the facts it gives, those of a normal return holding from `ok` for a call
    /// that may fail.
    #[allow(clippy::too_many_arguments)]
    fn call(
        &mut self,
        block: BlockId,
        position: usize,
        ok: Option<BlockId>,
        call: &Call,
        result_ty: Option<Ty>,
        result: Option<Value>,
        span: Span,
        contents: &mut Contents,
    ) -> Option<TermId> {
        let args: Vec<TermId> = call
            .args
            .iter()
            .map(|operand| self.operand(operand, contents))
            .collect();
        let callee = match &call.callee {
            Callee::Fn(fn_ref) => Some(self.substituted(fn_ref)),
            _ => None,
        };
        if let Some(fn_ref) = &callee {
            if let Some(obligation) = builtin_obligation(self.ck, fn_ref) {
                let goal = self.builtin_goal(fn_ref, &args, obligation);
                self.site(block, position, obligation, goal, false, span);
            }
            if let Some(term) = self.interpreted(fn_ref, &args) {
                return Some(term);
            }
            if let Callable::Item { item, role, index } = &fn_ref.callable
                && role.is_contract()
            {
                if matches!(role, Role::Invariant | Role::Assume)
                    && *item == self.unit.item
                    && !self.nested
                {
                    self.statement(block, position, *role, *index, &args);
                }
                let ty = result_ty.unwrap_or(Ty::Unit);
                let returned = match role {
                    Role::Requires => self.opaquely(|walk| {
                        walk.returned(*item, *role, *index, &fn_ref.args, args.clone())
                    }),
                    _ => self.returned(*item, *role, *index, &fn_ref.args, args.clone()),
                };
                return Some(returned.unwrap_or_else(|| self.terms.fresh(ty)));
            }
        }
        let before = contents.clone();
        for operand in &call.args {
            match operand {
                Operand::Place(Mode::Mut | Mode::Take, place) => {
                    let located = self.locate(place);
                    self.havoc(&located, contents);
                }
                Operand::Value(value) => {
                    for located in self.captured.get(value).cloned().unwrap_or_default() {
                        self.havoc(&located, contents);
                    }
                }
                _ => {}
            }
        }
        let term = result_ty
            .as_ref()
            .filter(|ty| **ty != Ty::Unit && **ty != Ty::Never)
            .map(|ty| self.callee_term(call, callee.as_ref(), &args, ty.clone()));
        if let (Some(term), Some(result)) = (term, result)
            && self.nir().ty(result).is_reference()
        {
            contents.insert(Root::Referent(result), term);
            self.referents
                .insert(result, Located::Place(Root::Referent(result), Vec::new()));
            if let (NTy::RefMut(_), Some(Operand::Place(Mode::Mut, receiver))) =
                (self.nir().ty(result), call.args.first())
            {
                let receiver = self.locate(receiver);
                self.projected.insert(Root::Referent(result), receiver);
            }
        }
        if !self.nested {
            self.opaquely(|walk| {
                walk.call_facts(
                    (block, position, ok),
                    call,
                    callee.as_ref(),
                    &args,
                    term,
                    &before,
                    contents,
                )
            });
        }
        term
    }

    /// The term of a call by callee knowledge: the transparent body the holder unfolds, the
    /// application of a pure callee, or a fresh symbol.
    fn callee_term(
        &mut self,
        call: &Call,
        callee: Option<&FnRef>,
        args: &[TermId],
        ty: Ty,
    ) -> TermId {
        let fn_ref = match (callee, &call.callee) {
            (Some(fn_ref), _) => fn_ref.clone(),
            (
                None,
                Callee::Dyn {
                    interface, method, ..
                },
            ) => FnRef {
                callable: Callable::Method {
                    interface: *interface,
                    method: *method,
                },
                args: Rc::from(Vec::new()),
            },
            _ => return self.terms.fresh(ty),
        };
        if let Callable::Item {
            item,
            role: Role::Body,
            ..
        } = &fn_ref.callable
            && !self.unfolding.contains(item)
            && self.ck.unfolds(*item, self.holder)
        {
            self.unfolding.push(*item);
            let term = self.returned(*item, Role::Body, 0, &fn_ref.args, args.to_vec());
            self.unfolding.pop();
            if let Some(term) = term {
                return term;
            }
        }
        if self.pure_call(&fn_ref, args) {
            let hidden = match &fn_ref.callable {
                Callable::Item { item, .. } => !self.ck.db.visible(self.module(), *item),
                _ => false,
            };
            let application = self.terms.intern(Term::Call(fn_ref, args.to_vec()), ty);
            return self.opaque_term(application, hidden);
        }
        self.terms.fresh(ty)
    }

    /// Whether the prover knows a call as the application of its callee: a direct call of a
    /// pure function, or a call through a bound or `Dyn` whose method head demands `pure`, with
    /// no function-typed argument that is impure and no argument with interior mutability.
    fn pure_call(&self, fn_ref: &FnRef, args: &[TermId]) -> bool {
        let pure = match &fn_ref.callable {
            Callable::Item { item, .. } => self.ck.digest(*item).pure(),
            Callable::Method { interface, method } => match &self.ck.sig(*interface).kind {
                SigKind::Interface { methods } => methods[*method as usize].sig.limits.pure,
                _ => false,
            },
            Callable::Derived { .. } | Callable::Builtin(_) => true,
        };
        pure && args.iter().all(|arg| {
            let ty = self.terms.ty(*arg);
            !self.ck.has_interior(ty)
                && match ty {
                    Ty::Fn(fn_ty) => fn_ty.limits.pure,
                    _ => true,
                }
        })
    }

    /// The term a NIR function of a unit returns for parameter terms, walked without recording
    /// facts or sites; none when the unit or function is missing.
    pub fn returned(
        &mut self,
        item: ItemId,
        role: Role,
        index: u32,
        type_args: &[Arg],
        params: Vec<TermId>,
    ) -> Option<TermId> {
        let unit = self.ck.unit(item)?;
        let function = unit
            .functions
            .iter()
            .position(|function| function.role == role && function.index == index)?;
        let unfolding = self.unfolding.clone();
        let mut nested = Walk::new(
            self.ck,
            self.holder,
            &mut *self.terms,
            unit,
            function,
            type_args,
            params,
            Vec::new(),
        );
        nested.nested = true;
        nested.unfolding = unfolding;
        nested.opaque = self.opaque.take();
        nested.walk();
        let result = nested.result();
        self.opaque = nested.opaque.take();
        result
    }

    /// The returned term of a function walked for its value: ite over its returns by their
    /// reach conditions.
    fn result(&mut self) -> Option<TermId> {
        let returns = std::mem::take(&mut self.returns);
        let (conditions, arms): (Vec<TermId>, Vec<TermId>) = returns.into_iter().unzip();
        (!arms.is_empty()).then(|| self.ites(&conditions, &arms))
    }

    /// Walks an invariant or assume function at its call, its queries having the premises of
    /// the call's point as entry facts.
    fn statement(
        &mut self,
        block: BlockId,
        position: usize,
        role: Role,
        index: u32,
        args: &[TermId],
    ) {
        let Some(function) = self
            .unit
            .functions
            .iter()
            .position(|function| function.role == role && function.index == index)
        else {
            return;
        };
        let entry: Vec<(TermId, Origin)> = self
            .premises_at(block, position)
            .into_iter()
            .map(|(term, origin)| match origin {
                Origin::Assume => (term, Origin::Assume),
                _ => (term, Origin::Fact),
            })
            .collect();
        let type_args: Vec<Arg> = self.subst_args();
        let mut walk = Walk::new(
            self.ck,
            self.holder,
            &mut *self.terms,
            self.unit.clone(),
            function,
            &type_args,
            args.to_vec(),
            entry,
        );
        walk.walk();
        let sites = walk.queries();
        self.statements.push(Statement { role, index, sites });
    }

    /// The type arguments the walk's substitution gives the unit's generic parameters.
    fn subst_args(&self) -> Vec<Arg> {
        self.ck
            .sig(self.unit.item)
            .generics
            .iter()
            .map(|def| {
                self.subst
                    .get(def.param)
                    .cloned()
                    .unwrap_or_else(|| crate::check::sig::param_arg(def))
            })
            .collect()
    }

    /// The term of a call the decidable fragment interprets: a widening or fitting conversion
    /// between integer types, which is its argument.
    fn interpreted(&mut self, fn_ref: &FnRef, args: &[TermId]) -> Option<TermId> {
        let Callable::Builtin(Builtin::Conversion(conversion, target)) = &fn_ref.callable else {
            return None;
        };
        let source = self.terms.ty(*args.first()?).scalar()?;
        let integers = target.is_integer() && source.is_integer();
        match conversion {
            Conversion::Widen | Conversion::Fit if integers => Some(args[0]),
            _ => None,
        }
    }

    /// The condition under which a built-in operation does not trap.
    fn builtin_goal(&mut self, fn_ref: &FnRef, args: &[TermId], obligation: Obligation) -> TermId {
        match (obligation, &fn_ref.callable) {
            (Obligation::Conversion, Callable::Builtin(Builtin::Conversion(_, target))) => {
                let source = self.terms.ty(args[0]).scalar();
                if target.is_integer() && source.is_some_and(Scalar::is_integer) {
                    return self.in_range(args[0], &Ty::Scalar(*target));
                }
                self.terms.intern(
                    Term::Call(fn_ref.clone(), args.to_vec()),
                    Ty::Scalar(Scalar::Bool),
                )
            }
            (Obligation::Bounds, _) => {
                let len = self.len_of(args[0]);
                self.terms.compare(BinaryOp::Lt, args[1], len)
            }
            _ => self.terms.bool(true),
        }
    }

    /// The `len` application of an indexed value's type.
    fn len_of(&mut self, indexed: TermId) -> TermId {
        let ty = self.terms.ty(indexed).clone();
        let Ty::Item(item, args) = &ty else {
            return self.terms.fresh(Ty::Scalar(Scalar::Usize));
        };
        let owner = crate::check::db::TypeKey::Item(*item);
        let Some(len) = self.ck.db.members(owner, "len").first().copied() else {
            return self.terms.fresh(Ty::Scalar(Scalar::Usize));
        };
        let fn_ref = FnRef::item(len, args.clone());
        let ty = Ty::Scalar(Scalar::Usize);
        let call = Call {
            awaited: false,
            callee: Callee::Fn(fn_ref.clone()),
            args: Vec::new(),
        };
        self.callee_term(&call, Some(&fn_ref), &[indexed], ty)
    }

    /// The facts after a call at a point: the ensures of a direct callee, from the `ok` block of
    /// a call that may fail; the predicates of the values reachable from its result and `mut`
    /// arguments where they are facts; and the element range a range cursor yields.
    #[allow(clippy::too_many_arguments)]
    fn call_facts(
        &mut self,
        (block, position, ok): (BlockId, usize, Option<BlockId>),
        call: &Call,
        callee: Option<&FnRef>,
        args: &[TermId],
        result: Option<TermId>,
        before: &Contents,
        after: &Contents,
    ) {
        let after_args: Vec<TermId> = call
            .args
            .iter()
            .zip(args)
            .map(|(operand, term)| match operand {
                Operand::Place(Mode::Mut, place) => {
                    let located = self.locate(place);
                    self.content(&located, after)
                }
                _ => *term,
            })
            .collect();
        let through = match (&call.callee, callee) {
            (
                _,
                Some(FnRef {
                    callable: Callable::Method { interface, method },
                    args: type_args,
                }),
            ) => type_args.split_first().and_then(|(self_ty, rest)| {
                let unit_args = self.ck.interface_args(*interface, self_ty.ty()?, rest);
                Some((*interface, *method, unit_args))
            }),
            (
                Callee::Dyn {
                    interface, method, ..
                },
                _,
            ) => {
                let self_ty = self.terms.ty(args[0]).clone();
                let bound_args = match &self_ty {
                    Ty::Dyn(bound) => bound.args.clone(),
                    _ => Rc::from(Vec::new()),
                };
                let unit_args = self.ck.interface_args(*interface, &self_ty, &bound_args);
                Some((*interface, *method, unit_args))
            }
            _ => None,
        };
        if let Some((interface, method, unit_args)) = through {
            let clauses = self.ck.ensures_clauses(interface, Some(method as usize));
            let modes = self.method_modes(interface, method as usize);
            let ensures = self.ensures_facts(
                interface,
                &clauses,
                &modes,
                &unit_args,
                args,
                &after_args,
                result,
            );
            for fact in ensures {
                self.fact_after(block, position, ok, fact);
            }
            return;
        }
        let Some(fn_ref) = callee else {
            return;
        };
        if let Callable::Item {
            item,
            role: Role::Body,
            ..
        } = &fn_ref.callable
        {
            if self.ck.db.canonical(*item) == "core::Range.next"
                && let (Some(result), Some(Operand::Place(_, place))) = (result, call.args.first())
            {
                self.range_fact(block, position, place, result, before);
            }
            let clauses = self.ck.ensures_clauses(*item, None);
            let modes = self.param_modes(fn_ref);
            let ensures = self.ensures_facts(
                *item,
                &clauses,
                &modes,
                &fn_ref.args,
                args,
                &after_args,
                result,
            );
            for fact in ensures {
                self.fact_after(block, position, ok, fact);
            }
            if let Some(implemented) = self.ck.implemented_method(*item) {
                let generics = &self.ck.sig(*item).generics;
                let unit_args = Subst::of(generics.iter().map(|def| def.param), &fn_ref.args)
                    .args(&implemented.args);
                let held: Vec<TermId> = self
                    .ck
                    .method_requires(implemented.interface, implemented.method)
                    .into_iter()
                    .filter(|(_, disputed)| !disputed)
                    .filter_map(|(index, _)| {
                        self.returned(
                            implemented.interface,
                            Role::Requires,
                            index,
                            &unit_args,
                            args.to_vec(),
                        )
                    })
                    .collect();
                let guard = self.terms.and(held);
                let clauses = self
                    .ck
                    .ensures_clauses(implemented.interface, Some(implemented.method));
                let inherited = self.ensures_facts(
                    implemented.interface,
                    &clauses,
                    &modes,
                    &unit_args,
                    args,
                    &after_args,
                    result,
                );
                for fact in inherited {
                    let fact = self.terms.implies(guard, fact);
                    self.fact_after(block, position, ok, fact);
                }
            }
            let helper = !self.ck.is_boundary(*item)
                && self.ck.db.module_of(*item) == self.ck.db.module_of(self.unit.item);
            let mut facts = Vec::new();
            if let (Some(result), Some(ty)) = (result, self.result_ty(fn_ref)) {
                self.predicate_facts(result, &ty, !helper, &mut facts);
            }
            for ((operand, term), ty) in call
                .args
                .iter()
                .zip(&after_args)
                .zip(self.param_types(fn_ref))
            {
                if matches!(operand, Operand::Place(Mode::Mut, _)) {
                    self.predicate_facts(*term, &ty, !helper, &mut facts);
                }
            }
            for fact in facts {
                self.fact(block, position, fact, Origin::Fact);
            }
        }
    }

    /// Records a fact of a call's normal return: from its `ok` block for a call that may fail.
    fn fact_after(&mut self, block: BlockId, position: usize, ok: Option<BlockId>, fact: TermId) {
        match ok {
            Some(ok) => self.fact_from(ok, 0, fact, Origin::Fact),
            None => self.fact(block, position, fact, Origin::Fact),
        }
    }

    fn result_ty(&self, fn_ref: &FnRef) -> Option<Ty> {
        let Callable::Item { item, .. } = &fn_ref.callable else {
            return None;
        };
        let sig = self.ck.fn_sig(*item)?;
        Some(Subst::of(sig.generics.iter().map(|def| def.param), &fn_ref.args).ty(&sig.result))
    }

    /// The modes of a direct callee's parameters, as its head declares them.
    fn param_modes(&self, fn_ref: &FnRef) -> Vec<Mode> {
        let sig = match &fn_ref.callable {
            Callable::Item { item, .. } => self.ck.fn_sig(*item),
            _ => None,
        };
        sig.map(|sig| sig.params.iter().map(|param| param.mode).collect())
            .unwrap_or_default()
    }

    /// The modes of an interface method's parameters.
    fn method_modes(&self, interface: ItemId, method: usize) -> Vec<Mode> {
        match &self.ck.sig(interface).kind {
            SigKind::Interface { methods } => methods[method]
                .sig
                .params
                .iter()
                .map(|param| param.mode)
                .collect(),
            _ => Vec::new(),
        }
    }

    fn param_types(&self, fn_ref: &FnRef) -> Vec<Ty> {
        let Callable::Item { item, .. } = &fn_ref.callable else {
            return Vec::new();
        };
        let Some(sig) = self.ck.fn_sig(*item) else {
            return Vec::new();
        };
        let subst = Subst::of(sig.generics.iter().map(|def| def.param), &fn_ref.args);
        sig.params.iter().map(|param| subst.ty(&param.ty)).collect()
    }

    /// The ensures of a callee after it returns, computed by the functions of `unit`'s unit:
    /// each read or `take` parameter its argument's term at the call, each `mut` parameter its
    /// place's content after the call, each `old(e)` the term of e over the arguments at the
    /// call, and `result` the call's term.
    #[allow(clippy::too_many_arguments)]
    fn ensures_facts(
        &mut self,
        unit: ItemId,
        clauses: &[EnsuresClause],
        modes: &[Mode],
        unit_args: &[Arg],
        args: &[TermId],
        after_args: &[TermId],
        result: Option<TermId>,
    ) -> Vec<TermId> {
        let kept: Vec<TermId> = modes
            .iter()
            .copied()
            .zip(after_args)
            .filter(|(mode, _)| *mode != Mode::Take)
            .map(|(_, term)| *term)
            .collect();
        let result = match result {
            Some(result) => result,
            None => self.terms.literal(Literal::Unit),
        };
        let mut facts = Vec::new();
        for clause in clauses.iter().filter(|clause| !clause.disputed) {
            let mut params = kept.clone();
            params.push(result);
            for old in &clause.olds {
                let term = self.returned(unit, Role::Old, *old, unit_args, args.to_vec());
                params.push(term.unwrap_or_else(|| self.terms.fresh(Ty::Unit)));
            }
            if let Some(fact) = self.returned(unit, Role::Ensures, clause.index, unit_args, params)
            {
                facts.push(fact);
            }
        }
        facts
    }

    /// The predicates of the struct values reachable from a term of a type, as facts where
    /// they are: outside their type's module always, and inside it when `inside` says so.
    pub fn predicate_facts(
        &mut self,
        term: TermId,
        ty: &Ty,
        inside: bool,
        facts: &mut Vec<TermId>,
    ) {
        let Ty::Item(item, args) = ty else {
            return;
        };
        if !self.ck.has_predicates_within(ty) {
            return;
        }
        let SigKind::Struct { fields } = self.ck.sig(*item).kind.clone() else {
            return;
        };
        if inside || self.ck.db.module_of(*item) != self.ck.db.module_of(self.holder) {
            for index in 0..self.ck.predicate_count(*item) {
                if let Some(fact) = self.returned(*item, Role::Predicate, index, args, vec![term]) {
                    facts.push(fact);
                }
            }
        }
        let subst = Subst::of(
            self.ck.sig(*item).generics.iter().map(|def| def.param),
            args,
        );
        for field in fields {
            let field_ty = subst.ty(&field.ty);
            if self.ck.has_predicates_within(&field_ty) {
                let part = self.field(term, &field.name);
                self.predicate_facts(part, &field_ty, inside, facts);
            }
        }
    }

    /// The fact that the element a range cursor yields lies within the bounds the range had
    /// when its loop started.
    fn range_fact(
        &mut self,
        block: BlockId,
        position: usize,
        place: &Place,
        yielded: TermId,
        before: &Contents,
    ) {
        let located = self.locate(place);
        let range = match &located {
            Located::Place(root, steps) if steps.is_empty() => {
                match self.loop_start.get(root).or_else(|| before.get(root)) {
                    Some(range) => *range,
                    None => return,
                }
            }
            _ => return,
        };
        let low = self.field(range, "low");
        let high = self.field(range, "high");
        let inclusive = self.field(range, "inclusive");
        let some = self.tag_is(yielded, 0);
        let element = self.field(yielded, "Some");
        let above = self.terms.compare(BinaryOp::Le, low, element);
        let le = self.terms.compare(BinaryOp::Le, element, high);
        let lt = self.terms.compare(BinaryOp::Lt, element, high);
        let below = self.terms.ite(inclusive, le, lt);
        let within = self.terms.and(vec![above, below]);
        let fact = self.terms.implies(some, within);
        self.fact(block, position, fact, Origin::Fact);
    }

    /// A quantifier's term: its region walked with a bound symbol for its element, which lies
    /// within the range there.
    #[allow(clippy::too_many_arguments)]
    fn quantifier(
        &mut self,
        block: BlockId,
        all: bool,
        low: TermId,
        high: TermId,
        inclusive: bool,
        region: &Region,
        contents: &Contents,
    ) -> TermId {
        let element = self.terms.ty(low).clone();
        let bound = self.terms.bound(element);
        let above = self.terms.compare(BinaryOp::Le, low, bound);
        let op = if inclusive {
            BinaryOp::Le
        } else {
            BinaryOp::Lt
        };
        let below = self.terms.compare(op, bound, high);
        let within = self.terms.and(vec![above, below]);
        self.fact_from(region.blocks[0], 0, within, Origin::Fact);
        let (body, _) = self.region(block, region, vec![bound], contents);
        let body = body.unwrap_or_else(|| self.terms.bool(true));
        self.terms.intern(
            Term::Quant {
                all,
                low,
                high,
                inclusive,
                bound,
                body,
            },
            Ty::Scalar(Scalar::Bool),
        )
    }

    /// Walks a region from its first block, which receives `params` and the reach condition of
    /// its instruction's block; returns the term it yields, if it yields, and the contents of
    /// `contents`' roots after it yields, the ite of theirs at each `yield`.
    fn region(
        &mut self,
        block: BlockId,
        region: &Region,
        params: Vec<TermId>,
        contents: &Contents,
    ) -> (Option<TermId>, Contents) {
        let entry = region.blocks[0];
        self.reach[entry.0 as usize] = self.reach[block.0 as usize];
        let unit = self.unit.clone();
        let function = &unit.functions[self.function];
        for (param, term) in function.block(entry).params.iter().zip(params) {
            self.values[param.0 as usize] = Some(term);
        }
        let saved = std::mem::take(&mut self.returns);
        let saved_yields = std::mem::take(&mut self.yields);
        for (index, current) in self
            .order(entry, Some(&region.blocks))
            .into_iter()
            .enumerate()
        {
            let data = function.block(current);
            let current_contents = match index {
                0 => contents.clone(),
                _ => self.entry(current, data),
            };
            self.block(current, data, current_contents);
        }
        let yielded = self.result();
        self.returns = saved;
        let yields = std::mem::replace(&mut self.yields, saved_yields);
        let conditions: Vec<TermId> = yields.iter().map(|(condition, _)| *condition).collect();
        let mut roots: Vec<&Root> = contents.keys().collect();
        roots.sort();
        let mut after = Contents::new();
        for root in roots {
            let arms: Option<Vec<TermId>> =
                yields.iter().map(|(_, at)| at.get(root).copied()).collect();
            if let Some(arms) = arms.filter(|arms| !arms.is_empty()) {
                after.insert(root.clone(), self.ites(&conditions, &arms));
            }
        }
        (yielded, after)
    }

    /// The edges a terminator leaves by, with their conditions; a return or yield records its
    /// term with the block's reach condition.
    fn terminator(&mut self, block: BlockId, data: &Block, mut contents: Contents) -> Vec<Edge> {
        let reach = self.reach[block.0 as usize].expect("a reached block");
        let edge = |walk: &mut Self,
                    target: BlockId,
                    condition: TermId,
                    args: Vec<TermId>,
                    contents: Contents| {
            let condition = walk.terms.and(vec![reach, condition]);
            Edge {
                target,
                condition,
                args,
                contents,
            }
        };
        match &data.term {
            Terminator::Jump(jump) => {
                let args = jump
                    .args
                    .iter()
                    .map(|operand| self.operand(operand, &contents))
                    .collect();
                let truth = self.terms.bool(true);
                vec![edge(self, jump.block, truth, args, contents)]
            }
            Terminator::Branch(condition, then, otherwise) => {
                let test = self.operand(condition, &contents);
                let negated = self.terms.not(test);
                let then_args = then
                    .args
                    .iter()
                    .map(|operand| self.operand(operand, &contents))
                    .collect();
                let else_args = otherwise
                    .args
                    .iter()
                    .map(|operand| self.operand(operand, &contents))
                    .collect();
                vec![
                    edge(self, then.block, test, then_args, contents.clone()),
                    edge(self, otherwise.block, negated, else_args, contents),
                ]
            }
            Terminator::Call { call, ok, fail } => {
                let position = data.insts.len();
                let ok_ty = self.ty(self.nir().block(*ok).params[0]);
                let term = self.call(
                    block,
                    position,
                    Some(*ok),
                    call,
                    Some(ok_ty.clone()),
                    None,
                    data.term_span,
                    &mut contents,
                );
                let result = term.unwrap_or_else(|| self.terms.fresh(ok_ty));
                let succeeded = self.terms.fresh(Ty::Scalar(Scalar::Bool));
                let failed = self.terms.not(succeeded);
                let fail_ty = self.ty(self.nir().block(*fail).params[0]);
                let failure = self.terms.fresh(fail_ty);
                vec![
                    edge(self, *ok, succeeded, vec![result], contents.clone()),
                    edge(self, *fail, failed, vec![failure], contents),
                ]
            }
            Terminator::Return(value) => {
                let term = match value {
                    Some(value) => self.operand(value, &contents),
                    None => self.terms.literal(Literal::Unit),
                };
                self.returns.push((reach, term));
                Vec::new()
            }
            Terminator::Yield(value) => {
                let term = self.operand(value, &contents);
                self.returns.push((reach, term));
                self.yields.push((reach, contents));
                Vec::new()
            }
            Terminator::Fail(_) | Terminator::Todo(_) | Terminator::Unreachable => Vec::new(),
        }
    }

    /// The queries of the function's sites, in site order.
    pub fn queries(&mut self) -> Vec<(Site, Query)> {
        let sites = self.sites.clone();
        sites
            .into_iter()
            .map(|site| {
                let premises = self.premises_at(site.block, site.position);
                let query = Query {
                    premises,
                    goal: site.goal,
                };
                (site, query)
            })
            .collect()
    }

    /// The premises at a point: the entry facts, the reach condition of its block, then the
    /// facts of each block from which it is reachable, in block order, those holding on the way
    /// to the point, guarded by their block's reach condition unless that block dominates its
    /// block.
    fn premises_at(&mut self, target: BlockId, position: usize) -> Vec<(TermId, Origin)> {
        let reach = self.reach[target.0 as usize].expect("a reached point");
        let mut premises = self.entry.clone();
        premises.push((reach, Origin::Fact));
        let mut reaching = self.reaching(target);
        reaching.insert(target, position);
        let facts = self.facts.clone();
        for (block, before) in reaching {
            for fact in facts.iter().filter(|fact| fact.block == block) {
                if fact.from > before {
                    continue;
                }
                let term = match self.dominators[target.0 as usize][block.0 as usize] {
                    true => fact.term,
                    false => {
                        let guard = self.reach[block.0 as usize].expect("a reached block");
                        self.terms.implies(guard, fact.term)
                    }
                };
                premises.push((term, fact.origin));
            }
        }
        premises
    }

    /// The blocks from which a block is reachable, each with the last position of its own whose
    /// facts hold on the way: all of them through a forward edge, and those before a region's
    /// instruction on the way from its block to the region's first block.
    fn reaching(&self, target: BlockId) -> BTreeMap<BlockId, usize> {
        let mut seen = BTreeMap::new();
        let mut pending = vec![target];
        while let Some(block) = pending.pop() {
            let whole = self.preds[block.0 as usize]
                .iter()
                .map(|pred| (*pred, usize::MAX));
            let parent = self.region_parent.get(&block).copied();
            for (pred, before) in whole.chain(parent) {
                let known = seen.get(&pred).copied();
                if known.is_none_or(|known| known < before) {
                    seen.insert(pred, before);
                    if known.is_none() {
                        pending.push(pred);
                    }
                }
            }
        }
        seen
    }

    /// Whether a region block lies, through the blocks holding the instructions of its region
    /// and of those enclosing it, in one of `blocks`.
    fn held_by(&self, mut block: BlockId, blocks: &BTreeSet<BlockId>) -> bool {
        while let Some(holder) = self.region_holder.get(&block) {
            if blocks.contains(holder) {
                return true;
            }
            block = *holder;
        }
        false
    }
}

/// The places an instruction writes: by `init`, `assign`, `ref mut` or a call's `mut` or
/// `take` operands.
fn written(op: &NirOp) -> Vec<Place> {
    match op {
        NirOp::Init(place, _) | NirOp::Assign(place, _) | NirOp::RefMut(place) => {
            vec![place.clone()]
        }
        NirOp::Call(call) => changed(call),
        _ => Vec::new(),
    }
}

/// The places a call receives by `mut` or `take`.
fn changed(call: &Call) -> Vec<Place> {
    call.args
        .iter()
        .filter_map(|operand| match operand {
            Operand::Place(Mode::Mut | Mode::Take, place) => Some(place.clone()),
            _ => None,
        })
        .collect()
}

/// The obligation a call of a built-in operation carries: `fit`, and the `at` and `at_mut` of
/// an array, slice, list, bytes or bounded list.
fn builtin_obligation(ck: &Checker, fn_ref: &FnRef) -> Option<Obligation> {
    match &fn_ref.callable {
        Callable::Builtin(Builtin::Conversion(Conversion::Fit, _)) => Some(Obligation::Conversion),
        Callable::Item {
            item,
            role: Role::Body,
            ..
        } => {
            let path = ck.db.canonical(*item);
            let (owner, member) = path.rsplit_once('.')?;
            let indexed = matches!(
                owner,
                "core::Array"
                    | "core::Slice"
                    | "core::SliceMut"
                    | "mem::List"
                    | "mem::Bytes"
                    | "mem::Bounded"
            ) && matches!(member, "at" | "at_mut");
            indexed.then_some(Obligation::Bounds)
        }
        _ => None,
    }
}
