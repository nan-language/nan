//! The reference interpreter: executes NIR functions at check time for const evaluation, example
//! runs and refutation confirmation. Every obligation site with a trap kind that is not open is
//! checked; a run ends at a trap, at its step limit, at an operation it cannot interpret, or at
//! a const or function that has no value or unit.

mod budget;
mod memory;
mod native;
mod value;

use std::collections::HashMap;
use std::rc::Rc;

pub use value::{FnVal, Loc, Val, View};

use crate::check::Checker;
use crate::check::db::{ItemId, TypeKey};
use crate::check::sig::{Payload as PayloadSig, SigKind, param_arg};
use crate::check::ty::{Arg, Const, Param, Scalar, Subst, Ty};
use crate::nir::{
    BinaryOp, BlockId, Builtin, Call, Callable, Callee, CheckKind, Constant, FnRef, Function, Inst,
    Jump, Literal, NTy, Op, Operand, Payload, Place, Role, Span, Terminator, UnaryOp, Unit,
};
use crate::syntax::ast::Mode;
use memory::Memory;
use value::{fits, wrap};

/// The steps every interpreter run may take.
pub const STEP_LIMIT: u64 = 1_000_000;

/// The call depth past which a call traps.
const DEPTH_LIMIT: usize = 4_096;

/// How an interpreter run ends.
#[derive(Debug)]
pub enum End {
    Value(Val),
    Failure,
    Trap(Report),
    Exhausted(Report),
    Violation(Report),
    Uninterpretable(Report),
    /// The run reached a const or contribution without a value, or a function without a unit.
    Missing,
    /// A confirmation run reached its obligation's site with the condition false.
    Confirmed,
}

/// Where a run ended abnormally: the first word of its report, the operation and the calls in
/// progress, innermost first.
#[derive(Clone, Debug)]
pub struct Report {
    pub word: String,
    pub at: (ItemId, Span),
    pub callers: Vec<(ItemId, Span)>,
}

/// An obligation site of a unit's function, by the function's role and index and the
/// instruction's block and position; the terminator of a block is at its number of
/// instructions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Site {
    pub role: Role,
    pub index: u32,
    pub block: BlockId,
    pub position: usize,
}

/// A value a confirmation run receives: a value, or a struct, enum value, array or slice of
/// inputs, a slice viewing all of a fresh extent holding its elements.
pub enum Input {
    Value(Val),
    Struct(ItemId, Vec<Input>),
    Variant(ItemId, u32, Vec<Input>),
    Array(Vec<Input>),
    /// Its elements, and whether it is writable.
    Slice(Vec<Input>, bool),
}

type Exec<T> = Result<T, End>;

/// How a function or region ends normally.
enum Flow {
    Return(Val),
    Fail(Val),
    Yield(Val),
}

/// A call in progress: its unit and function, its values, its type arguments and its slots.
struct Frame {
    unit: Rc<Unit>,
    function: usize,
    values: Vec<Option<Val>>,
    subst: Subst,
    slots: Vec<u32>,
}

impl Frame {
    fn function<'f>(&self, unit: &'f Unit) -> &'f Function {
        &unit.functions[self.function]
    }
}

pub struct Interpreter<'c, 'a> {
    pub ck: &'c Checker<'a>,
    steps: u64,
    memory: Memory,
    /// The call sites in progress, outermost first.
    calls: Vec<(ItemId, Span)>,
    /// The site a confirmation run stops at.
    stop: Option<Site>,
    /// The extents of literal data created so far.
    literals: Vec<(Literal, Val)>,
    budgets: budget::Budgets,
}

impl<'c, 'a> Interpreter<'c, 'a> {
    pub fn new(ck: &'c Checker<'a>) -> Interpreter<'c, 'a> {
        Interpreter {
            ck,
            steps: 0,
            memory: Memory::default(),
            calls: Vec::new(),
            stop: None,
            literals: Vec::new(),
            budgets: budget::Budgets::default(),
        }
    }

    /// Runs a function of an item's unit with arguments and type arguments.
    pub fn run(
        &mut self,
        item: ItemId,
        role: Role,
        index: u32,
        args: Vec<Val>,
        type_args: &[Arg],
    ) -> End {
        let Some(unit) = self.ck.unit(item) else {
            return End::Missing;
        };
        let Some(function) = unit
            .functions
            .iter()
            .position(|function| function.role == role && function.index == index)
        else {
            return End::Missing;
        };
        match self.execute(unit, function, args, type_args) {
            Ok(Flow::Return(value)) => End::Value(value),
            Ok(Flow::Fail(_)) => End::Failure,
            Ok(Flow::Yield(_)) => panic!("a function ending as a region"),
            Err(end) => end,
        }
    }

    /// A confirmation run: binds the inputs to the parameters of the site's function, a
    /// reference parameter to a fresh extent holding its input, evaluates the entry facts, then
    /// runs the function until it reaches the site with its obligation's condition false.
    pub fn confirm(
        &mut self,
        item: ItemId,
        site: Site,
        inputs: Vec<Input>,
        type_args: &[Arg],
    ) -> bool {
        let Some(unit) = self.ck.unit(item) else {
            return false;
        };
        let Some(function) = unit.function(site.role, site.index) else {
            return false;
        };
        let args: Vec<Val> = function
            .params
            .iter()
            .zip(inputs)
            .map(|(param, input)| {
                let value = self.input(input);
                match function.ty(param.value) {
                    NTy::Owned(_) => value,
                    NTy::Ref(ty) | NTy::RefMut(ty) => {
                        let (size, _) = memory::layout(self.ck, ty);
                        let extent = self.memory.alloc(value, true, size);
                        Val::Ref(Loc {
                            extent,
                            path: Vec::new(),
                        })
                    }
                }
            })
            .collect();
        if !self.entry_facts(item, function, &args, type_args) {
            return false;
        }
        self.stop = Some(site);
        matches!(
            self.run(item, site.role, site.index, args, type_args),
            End::Confirmed
        )
    }

    /// The kept fields of a flexible struct value that holds none; none for another item.
    pub fn kept_empty(&self, item: ItemId) -> Option<Val> {
        let Ty::Item(kept, _) = self.ck.kept_type(item)? else {
            return None;
        };
        Some(Val::Variant(kept, 0, Rc::new(Vec::new())))
    }

    fn input(&mut self, input: Input) -> Val {
        match input {
            Input::Value(value) => value,
            Input::Struct(item, fields) => {
                let mut values: Vec<Val> =
                    fields.into_iter().map(|field| self.input(field)).collect();
                values.extend(self.kept_empty(item));
                Val::Struct(item, Rc::new(values))
            }
            Input::Variant(item, index, payload) => Val::Variant(
                item,
                index,
                Rc::new(payload.into_iter().map(|field| self.input(field)).collect()),
            ),
            Input::Array(elements) => Val::Array(Rc::new(
                elements
                    .into_iter()
                    .map(|element| self.input(element))
                    .collect(),
            )),
            Input::Slice(elements, writable) => {
                let elements: Vec<Val> = elements
                    .into_iter()
                    .map(|element| self.input(element))
                    .collect();
                let len = elements.len();
                let extent = self
                    .memory
                    .alloc(Val::Array(Rc::new(elements)), writable, len as u64);
                Val::View(View {
                    array: Loc {
                        extent,
                        path: Vec::new(),
                    },
                    start: 0,
                    len,
                })
            }
        }
    }

    /// Whether a confirmation run's entry facts hold: the `where` conditions of the const
    /// parameters, the predicates that are facts at the item's entry, and the requires the
    /// function meets, those written before it for a `requires` function.
    fn entry_facts(
        &mut self,
        item: ItemId,
        function: &Function,
        args: &[Val],
        type_args: &[Arg],
    ) -> bool {
        if self.ck.unit(item).is_none() {
            return false;
        }
        let generics = self.ck.sig(item).generics.clone();
        let values: HashMap<Param, Val> = generics
            .iter()
            .zip(type_args)
            .filter_map(|(def, arg)| match arg {
                Arg::Const(Const::Int(value)) => Some((def.param, Val::Int(*value))),
                Arg::Const(Const::Bool(value)) => Some((def.param, Val::Bool(*value))),
                _ => None,
            })
            .collect();
        let owner = match self.ck.db.owners.get(&item) {
            Some(TypeKey::Item(owner)) => Some(*owner),
            _ => None,
        };
        for holder in std::iter::once(item).chain(owner) {
            let Some(unit) = self.ck.unit(holder) else {
                continue;
            };
            let holder_generics = self.ck.sig(holder).generics.clone();
            let consts: Vec<Val> = holder_generics
                .iter()
                .filter(|def| def.constant.is_some())
                .filter_map(|def| values.get(&def.param).cloned())
                .collect();
            let holder_args: Vec<Arg> = holder_generics
                .iter()
                .map(|def| {
                    generics
                        .iter()
                        .position(|own| own.param == def.param)
                        .map_or_else(|| param_arg(def), |at| type_args[at].clone())
                })
                .collect();
            for condition in unit
                .functions
                .iter()
                .filter(|other| other.role == Role::Where)
            {
                if !self.holds(
                    holder,
                    Role::Where,
                    condition.index,
                    consts.clone(),
                    &holder_args,
                ) {
                    return false;
                }
            }
        }
        let boundary = self.ck.is_boundary(item);
        for (param, arg) in function.params.iter().zip(args) {
            let ty = self
                .ck
                .unit_subst(item, type_args)
                .ty(function.ty(param.value).ty());
            let value = self.deref(arg);
            if !self.predicates_hold(item, boundary, &value, &ty) {
                return false;
            }
        }
        let before = match function.role {
            Role::Requires => function.index,
            _ => u32::MAX,
        };
        let (entry_requires, shared) = self.ck.entry_requires(item);
        for requires in entry_requires {
            let Callable::Item {
                item: holder,
                index,
                ..
            } = requires.callable
            else {
                continue;
            };
            if holder == item && index >= before {
                continue;
            }
            let Some(target) = self.ck.unit(holder) else {
                return false;
            };
            let Some(checked) = target.function(Role::Requires, index) else {
                return false;
            };
            let passed: Vec<Val> = checked
                .params
                .iter()
                .zip(&args[shared.clone()])
                .map(|(param, arg)| match checked.ty(param.value) {
                    NTy::Owned(_) => self.deref(arg),
                    _ => arg.clone(),
                })
                .collect();
            let type_args = self.ck.unit_subst(item, type_args).args(&requires.args);
            if !self.holds(holder, Role::Requires, index, passed, &type_args) {
                return false;
            }
        }
        true
    }

    /// Whether the predicates of the struct values reachable from a value hold, where they are
    /// facts at the entry of a function of `item`: outside their type's module, or at a
    /// boundary function's entry.
    fn predicates_hold(&mut self, item: ItemId, boundary: bool, value: &Val, ty: &Ty) -> bool {
        let (Ty::Item(owner, args), Val::Struct(_, fields)) = (ty, value) else {
            return true;
        };
        let SigKind::Struct { fields: declared } = &self.ck.sig(*owner).kind else {
            return true;
        };
        let declared = declared.clone();
        if boundary || self.ck.db.module_of(*owner) != self.ck.db.module_of(item) {
            for index in 0..self.ck.predicate_count(*owner) {
                if !self.holds(*owner, Role::Predicate, index, vec![value.clone()], args) {
                    return false;
                }
            }
        }
        let subst = Subst::of(
            self.ck.sig(*owner).generics.iter().map(|def| def.param),
            args,
        );
        declared.iter().zip(fields.iter()).all(|(field, value)| {
            let value = self.deref(value);
            self.predicates_hold(item, boundary, &value, &subst.ty(&field.ty))
        })
    }

    fn holds(
        &mut self,
        item: ItemId,
        role: Role,
        index: u32,
        args: Vec<Val>,
        type_args: &[Arg],
    ) -> bool {
        matches!(
            self.run(item, role, index, args, type_args),
            End::Value(Val::Bool(true))
        )
    }

    /// Whether a site of the run function's own code, at the call depth `depth` within it, is
    /// where a confirmation run stops.
    fn stops_at(&self, site: Site, depth: usize) -> bool {
        self.calls.len() == depth && self.stop == Some(site)
    }

    /// Counts a step, ending the run past its step limit.
    fn step(&mut self, at: (ItemId, Span)) -> Exec<()> {
        self.steps += 1;
        if self.steps > STEP_LIMIT {
            return Err(End::Exhausted(self.report("steps exhausted", at)));
        }
        Ok(())
    }

    fn report(&self, word: &str, at: (ItemId, Span)) -> Report {
        Report {
            word: word.to_owned(),
            at,
            callers: self.calls.iter().rev().copied().collect(),
        }
    }

    fn trap(&self, kind: &str, at: (ItemId, Span)) -> End {
        End::Trap(self.report(kind, at))
    }

    fn execute(
        &mut self,
        unit: Rc<Unit>,
        function: usize,
        args: Vec<Val>,
        type_args: &[Arg],
    ) -> Exec<Flow> {
        let item = unit.item;
        let mut frame = Frame {
            function,
            values: vec![None; unit.functions[function].types.len()],
            subst: self.ck.unit_subst(item, type_args),
            slots: Vec::new(),
            unit: unit.clone(),
        };
        let nir = frame.function(&unit);
        for (param, arg) in nir.params.iter().zip(args) {
            let arg = match (nir.ty(param.value).is_reference(), arg) {
                (false, reference @ Val::Ref(_)) => self.deref(&reference),
                (_, arg) => arg,
            };
            frame.values[param.value.0 as usize] = Some(arg);
        }
        let flow = self.blocks(&mut frame, BlockId(0));
        for slot in frame.slots.drain(..) {
            self.memory.kill(slot);
        }
        flow
    }

    /// Runs blocks from `start` until a return, a failure, or the `yield` ending a region.
    fn blocks(&mut self, frame: &mut Frame, start: BlockId) -> Exec<Flow> {
        let unit = frame.unit.clone();
        let item = unit.item;
        let function = frame.function(&unit);
        let mut current = start;
        loop {
            let block = function.block(current);
            for (position, inst) in block.insts.iter().enumerate() {
                self.step((item, inst.span))?;
                let site = Site {
                    role: function.role,
                    index: function.index,
                    block: current,
                    position,
                };
                self.inst(frame, inst, site)?;
            }
            self.step((item, block.term_span))?;
            let at = (item, block.term_span);
            let (next, args) = match &block.term {
                Terminator::Jump(jump) => (jump.block, self.jump_args(frame, jump, at)?),
                Terminator::Branch(condition, then, otherwise) => {
                    let chosen = match self.operand(frame, condition, at)?.bool() {
                        true => then,
                        false => otherwise,
                    };
                    (chosen.block, self.jump_args(frame, chosen, at)?)
                }
                Terminator::Call { call, ok, fail } => {
                    let site = Site {
                        role: function.role,
                        index: function.index,
                        block: current,
                        position: block.insts.len(),
                    };
                    match self.call(frame, call, at, site)? {
                        Flow::Return(value) => (*ok, vec![value]),
                        Flow::Fail(failure) => (*fail, vec![failure]),
                        _ => panic!("a call ending as a region"),
                    }
                }
                Terminator::Return(value) => {
                    let value = match value {
                        Some(operand) => self.operand(frame, operand, at)?,
                        None => Val::Unit,
                    };
                    return Ok(Flow::Return(value));
                }
                Terminator::Fail(failure) => {
                    return Ok(Flow::Fail(self.operand(frame, failure, at)?));
                }
                Terminator::Todo(_) => return Err(self.trap("trap.todo", at)),
                Terminator::Unreachable => panic!("an unreachable block reached"),
                Terminator::Yield(value) => {
                    return Ok(Flow::Yield(self.operand(frame, value, at)?));
                }
            };
            for (param, arg) in function.block(next).params.iter().zip(args) {
                frame.values[param.0 as usize] = Some(arg);
            }
            current = next;
        }
    }

    fn jump_args(&mut self, frame: &mut Frame, jump: &Jump, at: (ItemId, Span)) -> Exec<Vec<Val>> {
        jump.args
            .iter()
            .map(|operand| self.operand(frame, operand, at))
            .collect()
    }

    fn inst(&mut self, frame: &mut Frame, inst: &Inst, site: Site) -> Exec<()> {
        let item = frame.unit.item;
        let at = (item, inst.span);
        let unit = frame.unit.clone();
        let function = frame.function(&unit);
        let result_ty = inst.result.map(|value| function.ty(value).clone());
        let value = match &inst.op {
            Op::Slot { ty, .. } => {
                let (size, _) = memory::layout(self.ck, &frame.subst.ty(ty));
                let extent = self.memory.alloc(Val::Absent, true, size);
                frame.slots.push(extent);
                Some(Val::Ref(Loc {
                    extent,
                    path: Vec::new(),
                }))
            }
            Op::Load(place) => Some(self.read(frame, place, at)?),
            Op::Move(place) => {
                let value = self.read(frame, place, at)?;
                if let Some(loc) = self.locate(frame, place, at)? {
                    self.memory.set(&loc, Val::Absent);
                }
                Some(value)
            }
            Op::Ref(place) | Op::RefMut(place) => Some(Val::Ref(self.reference(frame, place, at)?)),
            Op::Init(place, operand) | Op::Assign(place, operand) => {
                let value = self.operand(frame, operand, at)?;
                let loc = self
                    .locate(frame, place, at)?
                    .unwrap_or_else(|| panic!("a store into a value"));
                if matches!(inst.op, Op::Assign(..)) {
                    let old = self.memory.get(&loc).clone();
                    self.release_value(&old);
                }
                self.memory.set(&loc, value);
                None
            }
            Op::Release(place) => {
                if let Some(loc) = self.locate(frame, place, at)? {
                    let old = self.memory.get(&loc).clone();
                    self.release_value(&old);
                    self.memory.set(&loc, Val::Absent);
                }
                None
            }
            Op::FnRef(fn_ref) => Some(Val::Fn(Rc::new(FnVal::Function(
                self.substituted(frame, fn_ref),
            )))),
            Op::Constant(constant) => Some(self.constant(frame, constant, at)?),
            Op::Construct { ty, fields } => {
                let values = fields
                    .iter()
                    .map(|(_, operand)| self.operand(frame, operand, at))
                    .collect::<Exec<Vec<Val>>>()?;
                let Ty::Item(item, _) = ty else {
                    panic!("a construction of no item type");
                };
                if !self.layout_fits(*item, &values) {
                    self.obligation_fails(site, "trap.conversion", at)?;
                }
                let mut values = values;
                values.extend(self.kept_empty(*item));
                Some(Val::Struct(*item, Rc::new(values)))
            }
            Op::Variant {
                ty,
                variant,
                payload,
            } => {
                let Ty::Item(item, _) = ty else {
                    panic!("a variant of no enum");
                };
                let values = match payload {
                    Payload::None => Vec::new(),
                    Payload::Single(operand) => vec![self.operand(frame, operand, at)?],
                    Payload::Labelled(fields) => fields
                        .iter()
                        .map(|(_, operand)| self.operand(frame, operand, at))
                        .collect::<Exec<Vec<Val>>>()?,
                };
                Some(Val::Variant(
                    *item,
                    self.variant_index(*item, variant),
                    Rc::new(values),
                ))
            }
            Op::Is { place, variant, .. } => {
                let value = self.read(frame, place, at)?;
                let Val::Variant(item, index, _) = value else {
                    panic!("an `is` test of no enum value");
                };
                Some(Val::Bool(index == self.variant_index(item, variant)))
            }
            Op::Closure { index, captures } => {
                let values = captures
                    .iter()
                    .map(|operand| self.operand(frame, operand, at))
                    .collect::<Exec<Vec<Val>>>()?;
                let args = self
                    .ck
                    .sig(item)
                    .generics
                    .iter()
                    .map(|def| match frame.subst.get(def.param) {
                        Some(arg) => arg.clone(),
                        None => Arg::Ty(Ty::Param(def.param)),
                    })
                    .collect();
                Some(Val::Fn(Rc::new(FnVal::Closure {
                    item,
                    index: *index,
                    args,
                    captures: values,
                })))
            }
            Op::Convert { value, to, via } => {
                let value = self.operand(frame, value, at)?;
                match (to, via) {
                    (Ty::Item(item, _), Some(variant)) => Some(Val::Variant(
                        *item,
                        self.variant_index(*item, variant),
                        Rc::new(vec![value]),
                    )),
                    _ => Some(value),
                }
            }
            Op::Unary(op, operand) => {
                let value = self.operand(frame, operand, at)?;
                let ty = result_ty.clone().expect("a unary result");
                Some(self.unary(*op, value, &frame.subst.ty(ty.ty()), at, site)?)
            }
            Op::Binary(op, left, right) => {
                let left_value = self.operand(frame, left, at)?;
                let right_value = self.operand(frame, right, at)?;
                let operand_ty = self.operand_ty(frame, left);
                let ty = result_ty.clone().expect("a binary result");
                Some(self.binary(
                    *op,
                    left_value,
                    right_value,
                    &operand_ty,
                    &frame.subst.ty(ty.ty()),
                    at,
                    site,
                )?)
            }
            Op::Call(call) => match self.call(frame, call, at, site)? {
                Flow::Return(value) => Some(value),
                _ => panic!("a call that may fail is a terminator"),
            },
            Op::Check {
                kind,
                operand,
                open,
            } => {
                let holds = self.operand(frame, operand, at)?.bool();
                if !holds && !open {
                    self.obligation_fails(site, check_trap(*kind), at)?;
                }
                None
            }
            Op::Dbg(operand) => Some(self.operand(frame, operand, at)?),
            Op::Assume(operand) => {
                if !self.operand(frame, operand, at)?.bool() {
                    return Err(End::Violation(self.report("assume violation", at)));
                }
                None
            }
            Op::Quantifier {
                all,
                low,
                high,
                inclusive,
                region,
            } => {
                let low = self.operand(frame, low, at)?.int();
                let high = self.operand(frame, high, at)?.int();
                let end = if *inclusive { high + 1 } else { high };
                let entry = region.blocks[0];
                let param = function.block(entry).params[0];
                let mut decided = *all;
                let mut value = low;
                while value < end {
                    frame.values[param.0 as usize] = Some(Val::Int(value));
                    let Flow::Yield(result) = self.blocks(frame, entry)? else {
                        panic!("a quantifier's region left");
                    };
                    if result.bool() != *all {
                        decided = !*all;
                        break;
                    }
                    value += 1;
                }
                Some(Val::Bool(decided))
            }
            Op::When { .. } => {
                return Err(End::Uninterpretable(self.report("uninterpretable", at)));
            }
        };
        if let (Some(result), Some(value)) = (inst.result, value) {
            frame.values[result.0 as usize] = Some(value);
        }
        Ok(())
    }

    /// The end of a run at a site whose obligation fails: a stop for a confirmation run at its
    /// site, a trap otherwise.
    fn obligation_fails(&self, site: Site, kind: &str, at: (ItemId, Span)) -> Exec<()> {
        if self.stops_at(site, 0) {
            return Err(End::Confirmed);
        }
        Err(self.trap(kind, at))
    }

    /// Whether the values of a layout's fields fit them: each ranged `bit` field below 2 to its
    /// width, and a `rest` field without a covered bit; any other construction fits.
    fn layout_fits(&self, item: ItemId, values: &[Val]) -> bool {
        use crate::check::lowlevel::{BitsKind, LayoutShape};
        let Some(LayoutShape::Fields(fields)) = self.ck.layout_shape(item) else {
            return true;
        };
        let covered = fields.iter().fold(0u128, |covered, field| {
            covered | (((1u128 << field.width) - 1) << field.low)
        });
        fields
            .iter()
            .zip(values)
            .all(|(field, value)| match field.kind {
                BitsKind::Ranged => value.int() < (1 << field.width),
                BitsKind::Rest => value.int() as u128 & covered == 0,
                _ => true,
            })
    }

    fn operand_ty(&self, frame: &Frame, operand: &Operand) -> Ty {
        let unit = frame.unit.clone();
        match operand {
            Operand::Value(value) => frame.subst.ty(frame.function(&unit).ty(*value).ty()),
            Operand::Literal(Literal::Int(_, scalar) | Literal::Float(_, scalar)) => {
                Ty::Scalar(*scalar)
            }
            Operand::Literal(Literal::Bool(_)) => Ty::Scalar(Scalar::Bool),
            Operand::Literal(Literal::Char(_)) => Ty::Scalar(Scalar::Char),
            _ => Ty::Unit,
        }
    }

    fn substituted(&self, frame: &Frame, fn_ref: &FnRef) -> FnRef {
        FnRef {
            callable: fn_ref.callable.clone(),
            args: frame.subst.args(&fn_ref.args),
        }
    }

    fn variant_index(&self, item: ItemId, name: &str) -> u32 {
        match &self.ck.sig(item).kind {
            SigKind::Enum { variants } | SigKind::Protocol { variants, .. } => variants
                .iter()
                .position(|variant| variant.name == name)
                .expect("a variant of the enum")
                as u32,
            SigKind::Layout { tags, .. } => {
                tags.iter().position(|tag| tag == name).expect("a tag") as u32
            }
            _ => panic!("a variant of no enum"),
        }
    }

    fn constant(&mut self, frame: &Frame, constant: &Constant, at: (ItemId, Span)) -> Exec<Val> {
        match constant {
            Constant::Item(item) => {
                if matches!(
                    self.ck.db.ast(*item).kind,
                    crate::syntax::ast::ItemKind::Registry(_)
                ) {
                    return Err(End::Uninterpretable(self.report("uninterpretable", at)));
                }
                let value = self.ck.const_value(*item).ok_or(End::Missing)?;
                Ok(self.import(&value))
            }
            Constant::Param(param) => match frame.subst.get(*param) {
                Some(Arg::Const(Const::Int(value))) => Ok(Val::Int(*value)),
                Some(Arg::Const(Const::Bool(value))) => Ok(Val::Bool(*value)),
                _ => Err(End::Missing),
            },
            Constant::Register(_, index) => Ok(Val::Int(i128::from(*index))),
        }
    }

    /// A value an earlier run computed, the data it views held in this run's memory.
    fn import(&mut self, value: &ConstValue) -> Val {
        let extents: Vec<u32> = value
            .data
            .iter()
            .map(|data| {
                let size = data.fields().len() as u64;
                self.memory.alloc_data(data.clone(), size)
            })
            .collect();
        remap(&value.value, &|index| extents[index as usize])
    }

    /// Whether the field at `index` of two const struct values compares `Equal` by `Ord.cmp`,
    /// or is equal by `Eq.eq` when `order` is false, as derived and built-in conformances
    /// compare them.
    pub fn same_field(
        &mut self,
        left: &ConstValue,
        right: &ConstValue,
        index: usize,
        order: bool,
    ) -> bool {
        if order {
            return self.field_order(left, right, index) == std::cmp::Ordering::Equal;
        }
        let (left, right) = (self.import(left), self.import(right));
        self.equal(&left.fields()[index], &right.fields()[index])
    }

    /// The order of two const struct values by the field at `index`.
    pub fn field_order(
        &mut self,
        left: &ConstValue,
        right: &ConstValue,
        index: usize,
    ) -> std::cmp::Ordering {
        let (left, right) = (self.import(left), self.import(right));
        self.order(&left.fields()[index], &right.fields()[index])
    }

    /// A run's value as a const's: the data it views copied out of the run's memory, or none
    /// when it views memory other than literal and const data.
    pub fn export(&self, value: &Val) -> Option<ConstValue> {
        let mut extents: Vec<u32> = Vec::new();
        self.collect_extents(value, &mut extents);
        let mut index = 0;
        while index < extents.len() {
            let held = self.memory.extent(extents[index]).value.clone();
            self.collect_extents(&held, &mut extents);
            index += 1;
        }
        if extents
            .iter()
            .any(|extent| !self.memory.extent(*extent).data)
        {
            return None;
        }
        let position = |extent: u32| {
            extents
                .iter()
                .position(|known| *known == extent)
                .expect("a collected extent") as u32
        };
        Some(ConstValue {
            data: extents
                .iter()
                .map(|extent| remap(&self.memory.extent(*extent).value, &position))
                .collect(),
            value: remap(value, &position),
        })
    }

    fn collect_extents(&self, value: &Val, extents: &mut Vec<u32>) {
        let mut add = |extent: u32| {
            if !extents.contains(&extent) {
                extents.push(extent);
            }
        };
        match value {
            Val::View(view) => add(view.array.extent),
            Val::Ref(loc) => add(loc.extent),
            Val::Struct(_, fields) | Val::Variant(_, _, fields) | Val::Array(fields) => {
                for field in fields.iter() {
                    self.collect_extents(field, extents);
                }
            }
            _ => {}
        }
    }

    /// The value of an operand: a value, a literal, or a place passed by its mode.
    fn operand(&mut self, frame: &mut Frame, operand: &Operand, at: (ItemId, Span)) -> Exec<Val> {
        match operand {
            Operand::Value(value) => Ok(frame.values[value.0 as usize]
                .clone()
                .unwrap_or_else(|| panic!("value %{} used before its definition", value.0))),
            Operand::Literal(literal) => Ok(self.literal(literal)),
            Operand::Place(Mode::Take, place) => {
                let value = self.read(frame, place, at)?;
                if let Some(loc) = self.locate(frame, place, at)? {
                    self.memory.set(&loc, Val::Absent);
                }
                Ok(value)
            }
            Operand::Place(_, place) => Ok(Val::Ref(self.reference(frame, place, at)?)),
        }
    }

    fn literal(&mut self, literal: &Literal) -> Val {
        match literal {
            Literal::Int(value, _) => Val::Int(*value),
            Literal::Float(bits, Scalar::F32) => Val::F32(f32::from_bits(*bits as u32)),
            Literal::Float(bits, _) => Val::F64(f64::from_bits(*bits)),
            Literal::Bool(value) => Val::Bool(*value),
            Literal::Char(c) => Val::Char(*c),
            Literal::Unit => Val::Unit,
            Literal::Text(_) | Literal::Bytes(_) => {
                if let Some((_, value)) = self.literals.iter().find(|(known, _)| known == literal) {
                    return value.clone();
                }
                let bytes: Vec<u8> = match literal {
                    Literal::Text(text) => text.as_bytes().to_vec(),
                    Literal::Bytes(bytes) => bytes.clone(),
                    _ => unreachable!("a text or byte-string literal"),
                };
                let elements: Vec<Val> = bytes
                    .iter()
                    .map(|byte| Val::Int(i128::from(*byte)))
                    .collect();
                let len = elements.len();
                let extent = self
                    .memory
                    .alloc_data(Val::Array(Rc::new(elements)), len as u64);
                let value = Val::View(View {
                    array: Loc {
                        extent,
                        path: Vec::new(),
                    },
                    start: 0,
                    len,
                });
                self.literals.push((literal.clone(), value.clone()));
                value
            }
        }
    }

    /// Where a place lies in memory, or none when it lies within an owned value.
    fn locate(&mut self, frame: &Frame, place: &Place, at: (ItemId, Span)) -> Exec<Option<Loc>> {
        let root = frame.values[place.root.0 as usize]
            .clone()
            .unwrap_or_else(|| panic!("place root %{} undefined", place.root.0));
        let mut current: (Option<Loc>, Val) = match &root {
            Val::Ref(loc) => (Some(loc.clone()), self.memory.get(loc).clone()),
            other => (None, other.clone()),
        };
        let mut labelled = false;
        for step in &place.steps {
            if let Val::Ref(loc) = &current.1 {
                current = (Some(loc.clone()), self.memory.get(loc).clone());
            }
            let (index, next_labelled) = self.step_index(&current.1, step, labelled, at)?;
            labelled = next_labelled;
            let Some(index) = index else {
                continue;
            };
            let value = current.1.fields()[index as usize].clone();
            current = (current.0.map(|loc| loc.with(index)), value);
        }
        if let Val::Ref(loc) = &current.1
            && place
                .steps
                .last()
                .is_some_and(|step| crate::nir::is_variant_step(step))
        {
            return Ok(Some(loc.clone()));
        }
        Ok(current.0)
    }

    /// The index a step takes in a value: a field's, or a payload field's; a variant step takes
    /// none and is checked against the value's variant, a labelled payload taking the next step.
    fn step_index(
        &self,
        value: &Val,
        step: &str,
        labelled: bool,
        at: (ItemId, Span),
    ) -> Exec<(Option<u32>, bool)> {
        match value {
            Val::Struct(item, _) => Ok((Some(self.field_index(*item, step)), false)),
            Val::Variant(item, variant, _) if labelled => {
                let SigKind::Enum { variants } = &self.ck.sig(*item).kind else {
                    panic!("a payload of no enum");
                };
                let PayloadSig::Labelled(fields) = &variants[*variant as usize].payload else {
                    panic!("a label of an unlabelled payload");
                };
                let index = fields
                    .iter()
                    .position(|(name, _)| name == step)
                    .expect("a payload label") as u32;
                Ok((Some(index), false))
            }
            Val::Variant(item, variant, _) => {
                let SigKind::Enum { variants } = &self.ck.sig(*item).kind else {
                    panic!("a variant step of no enum");
                };
                let current = &variants[*variant as usize];
                if current.name != step {
                    panic!("a payload of {} read from {}", step, current.name);
                }
                let _ = at;
                match current.payload {
                    PayloadSig::Labelled(_) => Ok((None, true)),
                    _ => Ok((Some(0), false)),
                }
            }
            other => panic!("a step {step} into {other:?}"),
        }
    }

    fn field_index(&self, item: ItemId, name: &str) -> u32 {
        match &self.ck.sig(item).kind {
            SigKind::Struct { fields } | SigKind::Layout { fields, .. } => fields
                .iter()
                .position(|field| field.name == name)
                .unwrap_or_else(|| panic!("a field {name}"))
                as u32,
            _ => panic!("a field of no struct"),
        }
    }

    /// The value at a place.
    fn read(&mut self, frame: &Frame, place: &Place, at: (ItemId, Span)) -> Exec<Val> {
        let root = frame.values[place.root.0 as usize]
            .clone()
            .unwrap_or_else(|| panic!("place root %{} undefined", place.root.0));
        let mut current = match &root {
            Val::Ref(loc) => self.memory.get(loc).clone(),
            other => other.clone(),
        };
        let mut labelled = false;
        for step in &place.steps {
            if let Val::Ref(loc) = &current {
                current = self.memory.get(loc).clone();
            }
            let (index, next_labelled) = self.step_index(&current, step, labelled, at)?;
            labelled = next_labelled;
            if let Some(index) = index {
                current = current.fields()[index as usize].clone();
            }
        }
        if let Val::Ref(loc) = &current
            && !place.steps.is_empty()
        {
            current = self.memory.get(loc).clone();
        }
        Ok(current)
    }

    /// A reference to a place: its location, or a new read-only extent holding the value when
    /// it lies within an owned value.
    fn reference(&mut self, frame: &Frame, place: &Place, at: (ItemId, Span)) -> Exec<Loc> {
        if let Some(loc) = self.locate(frame, place, at)? {
            return Ok(loc);
        }
        let value = self.read(frame, place, at)?;
        if let Val::Ref(loc) = value {
            return Ok(loc);
        }
        let extent = self.memory.alloc(value, false, 0);
        Ok(Loc {
            extent,
            path: Vec::new(),
        })
    }

    /// Releases what a value owns: the memory of the std containers it holds.
    fn release_value(&mut self, value: &Val) {
        native::release(self, value);
    }

    fn unary(&self, op: UnaryOp, value: Val, ty: &Ty, at: (ItemId, Span), site: Site) -> Exec<Val> {
        Ok(match (op, value) {
            (UnaryOp::Not, Val::Bool(value)) => Val::Bool(!value),
            (UnaryOp::Neg, Val::Int(value)) => {
                let scalar = ty.scalar().expect("an integer result");
                if !fits(-value, scalar) {
                    return Err(self.overflow_trap(site, at));
                }
                Val::Int(-value)
            }
            (UnaryOp::Neg, Val::F32(value)) => Val::F32(-value),
            (UnaryOp::Neg, Val::F64(value)) => Val::F64(-value),
            (UnaryOp::Inv, Val::Int(value)) => {
                let scalar = ty.scalar().expect("an integer result");
                if scalar.is_signed() {
                    Val::Int(-value - 1)
                } else {
                    Val::Int(scalar.range().1 - value)
                }
            }
            (UnaryOp::Inv, Val::Bool(value)) => Val::Bool(!value),
            (op, value) => panic!("{op:?} on {value:?}"),
        })
    }

    fn overflow_trap(&self, site: Site, at: (ItemId, Span)) -> End {
        if self.stops_at(site, 0) {
            return End::Confirmed;
        }
        self.trap("trap.overflow", at)
    }

    #[allow(clippy::too_many_arguments)]
    fn binary(
        &self,
        op: BinaryOp,
        left: Val,
        right: Val,
        operand_ty: &Ty,
        ty: &Ty,
        at: (ItemId, Span),
        site: Site,
    ) -> Exec<Val> {
        let site_trap = |kind: &str| -> End {
            if self.stops_at(site, 0) {
                End::Confirmed
            } else {
                self.trap(kind, at)
            }
        };
        if op.is_comparison() {
            return Ok(Val::Bool(self.compare(op, &left, &right)));
        }
        Ok(match (left, right) {
            (Val::Int(a), Val::Int(b)) => {
                let scalar = ty
                    .scalar()
                    .or(operand_ty.scalar())
                    .expect("an integer type");
                match op {
                    BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul => {
                        let result = match op {
                            BinaryOp::Add => a.checked_add(b),
                            BinaryOp::Sub => a.checked_sub(b),
                            _ => a.checked_mul(b),
                        };
                        match result {
                            Some(result) if fits(result, scalar) => Val::Int(result),
                            _ => return Err(site_trap("trap.overflow")),
                        }
                    }
                    BinaryOp::Div | BinaryOp::Rem => {
                        if b == 0 {
                            return Err(site_trap("trap.division-by-zero"));
                        }
                        Val::Int(if op == BinaryOp::Div { a / b } else { a % b })
                    }
                    BinaryOp::Shl | BinaryOp::Shr => {
                        if b < 0 || b >= i128::from(scalar.bits()) {
                            return Err(site_trap("trap.shift"));
                        }
                        if op == BinaryOp::Shr {
                            Val::Int(a >> b)
                        } else if scalar.is_signed() {
                            let shifted = a << b;
                            if !fits(shifted, scalar) {
                                return Err(site_trap("trap.overflow"));
                            }
                            Val::Int(shifted)
                        } else {
                            Val::Int(wrap(a << b, scalar))
                        }
                    }
                    BinaryOp::Band => Val::Int(a & b),
                    BinaryOp::Bor => Val::Int(a | b),
                    BinaryOp::Bxor => Val::Int(a ^ b),
                    _ => unreachable!("comparisons return above"),
                }
            }
            (Val::Bool(a), Val::Bool(b)) => match op {
                BinaryOp::Band => Val::Bool(a & b),
                BinaryOp::Bor => Val::Bool(a | b),
                BinaryOp::Bxor => Val::Bool(a ^ b),
                _ => panic!("{op:?} on bools"),
            },
            (Val::F32(a), Val::F32(b)) => native::canonical(Val::F32(match op {
                BinaryOp::Add => a + b,
                BinaryOp::Sub => a - b,
                BinaryOp::Mul => a * b,
                BinaryOp::Div => a / b,
                BinaryOp::Rem => a % b,
                _ => panic!("{op:?} on floats"),
            })),
            (Val::F64(a), Val::F64(b)) => native::canonical(Val::F64(match op {
                BinaryOp::Add => a + b,
                BinaryOp::Sub => a - b,
                BinaryOp::Mul => a * b,
                BinaryOp::Div => a / b,
                BinaryOp::Rem => a % b,
                _ => panic!("{op:?} on floats"),
            })),
            (left, right) => panic!("{op:?} on {left:?} and {right:?}"),
        })
    }

    /// A comparison of two scalars of one type, or of two texts or byte strings by length and
    /// bytes.
    fn compare(&self, op: BinaryOp, left: &Val, right: &Val) -> bool {
        use std::cmp::Ordering;
        let order = match (left, right) {
            (Val::F32(a), Val::F32(b)) => a.partial_cmp(b),
            (Val::F64(a), Val::F64(b)) => a.partial_cmp(b),
            (Val::View(a), Val::View(b)) => {
                let equal = self.view_values(a) == self.view_values(b);
                return match op {
                    BinaryOp::Eq => equal,
                    BinaryOp::Ne => !equal,
                    _ => panic!("an ordering of texts"),
                };
            }
            (a, b) => Some(a.int().cmp(&b.int())),
        };
        match (op, order) {
            (BinaryOp::Eq, order) => order == Some(Ordering::Equal),
            (BinaryOp::Ne, order) => order != Some(Ordering::Equal),
            (BinaryOp::Lt, order) => order == Some(Ordering::Less),
            (BinaryOp::Le, order) => matches!(order, Some(Ordering::Less | Ordering::Equal)),
            (BinaryOp::Gt, order) => order == Some(Ordering::Greater),
            (BinaryOp::Ge, order) => matches!(order, Some(Ordering::Greater | Ordering::Equal)),
            _ => unreachable!("a comparison"),
        }
    }

    /// The elements a view sees.
    pub fn view_values(&self, view: &View) -> Vec<Val> {
        let array = self.memory.get(&view.array);
        array.fields()[view.start..view.start + view.len].to_vec()
    }

    fn call(
        &mut self,
        frame: &mut Frame,
        call: &Call,
        at: (ItemId, Span),
        site: Site,
    ) -> Exec<Flow> {
        let mut args = call
            .args
            .iter()
            .map(|operand| self.operand(frame, operand, at))
            .collect::<Exec<Vec<Val>>>()?;
        if call.awaited {
            return Err(End::Uninterpretable(self.report("uninterpretable", at)));
        }
        let callee = match &call.callee {
            Callee::Fn(fn_ref) => FnVal::Function(self.substituted(frame, fn_ref)),
            Callee::Value(value) => {
                let unit = frame.unit.clone();
                if let Ty::Fn(function) = frame.function(&unit).ty(*value).ty()
                    && function.convention.is_some()
                {
                    return Err(End::Uninterpretable(self.report("uninterpretable", at)));
                }
                match frame.values[value.0 as usize].clone() {
                    Some(Val::Fn(function)) => (*function).clone(),
                    other => panic!("an indirect call of {other:?}"),
                }
            }
            Callee::Dyn { place, method, .. } => {
                let Val::Dyn(dynamic) = self.read(frame, place, at)? else {
                    panic!("a dynamic call on no Dyn value");
                };
                args[0] = dynamic.value.clone();
                FnVal::Function(dynamic.methods[*method as usize].clone())
            }
        };
        self.calls.push(at);
        if self.calls.len() > DEPTH_LIMIT {
            self.calls.pop();
            return Err(self.trap("trap.stack-overflow", at));
        }
        let flow = self.invoke(&callee, args, at, site);
        self.calls.pop();
        flow
    }

    /// Calls a function or closure value with its arguments.
    fn invoke(
        &mut self,
        callee: &FnVal,
        args: Vec<Val>,
        at: (ItemId, Span),
        site: Site,
    ) -> Exec<Flow> {
        match callee {
            FnVal::Closure {
                item,
                index,
                args: type_args,
                captures,
            } => {
                let unit = self.ck.unit(*item).ok_or(End::Missing)?;
                let function = unit
                    .functions
                    .iter()
                    .position(|function| function.role == Role::Closure && function.index == *index)
                    .ok_or(End::Missing)?;
                let mut all = captures.clone();
                all.extend(args);
                self.execute(unit, function, all, type_args)
            }
            FnVal::Function(fn_ref) => self.call_function(fn_ref, args, at, site),
        }
    }

    fn call_function(
        &mut self,
        fn_ref: &FnRef,
        args: Vec<Val>,
        at: (ItemId, Span),
        site: Site,
    ) -> Exec<Flow> {
        match &fn_ref.callable {
            Callable::Item { item, role, index } => {
                let unit = self.ck.unit(*item).ok_or(End::Missing)?;
                let function = unit
                    .functions
                    .iter()
                    .position(|function| function.role == *role && function.index == *index);
                match function {
                    Some(function) => self.execute(unit, function, args, &fn_ref.args),
                    None if *role == Role::Body && self.ck.is_foreign(*item) => {
                        Err(End::Uninterpretable(self.report("uninterpretable", at)))
                    }
                    None if *role == Role::Body && self.ck.db.is_std(*item) => {
                        native::call(self, *item, &fn_ref.args, args, at, site)
                    }
                    None => Err(End::Missing),
                }
            }
            Callable::Method { interface, method } => {
                let self_ty = fn_ref
                    .args
                    .first()
                    .and_then(Arg::ty)
                    .cloned()
                    .expect("a Self type");
                let name = match &self.ck.sig(*interface).kind {
                    SigKind::Interface { methods } => methods[*method as usize].name.clone(),
                    _ => panic!("a method of no interface"),
                };
                if matches!(self_ty, Ty::Param(_)) {
                    panic!("a method call through an unsubstituted bound");
                }
                match self.ck.implementation(&self_ty, *interface, &name) {
                    Some(implementation)
                        if !matches!(implementation.callable, Callable::Method { .. }) =>
                    {
                        let implementation =
                            self.ck
                                .with_call_args(implementation, *interface, &fn_ref.args);
                        self.call_function(&implementation, args, at, site)
                    }
                    _ => native::conformance(self, &self_ty, &name, &fn_ref.args, args, at, site),
                }
            }
            Callable::Derived { owner, method } => {
                native::derived(self, *owner, &fn_ref.args, method, args, at, site)
            }
            Callable::Builtin(Builtin::Intrinsic(path)) => {
                native::intrinsic(self, path, &fn_ref.args, args, at, site)
            }
            Callable::Builtin(builtin) => native::builtin(self, *builtin, args, at, site),
        }
    }
}

/// The trap kind of a kept check.
fn check_trap(kind: CheckKind) -> &'static str {
    match kind {
        CheckKind::Requires => "trap.requires",
        CheckKind::Ensures => "trap.ensures",
        CheckKind::Predicate => "trap.predicate",
        CheckKind::Expect | CheckKind::Invariant => "trap.expect",
        CheckKind::Decreases => "trap.decreases",
    }
}

/// A value const evaluation computed, with the data its views refer to.
#[derive(Debug)]
pub struct ConstValue {
    /// The arrays its views and references see, by index.
    pub data: Vec<Val>,
    /// The value, its views and references naming data by index as their extent.
    pub value: Val,
}

impl ConstValue {
    /// The elements a view of the value sees.
    pub fn elements(&self, view: &View) -> &[Val] {
        let array = view
            .array
            .path
            .iter()
            .fold(&self.data[view.array.extent as usize], |value, index| {
                &value.fields()[*index as usize]
            });
        &array.fields()[view.start..view.start + view.len]
    }
}

/// A value with the extent of each view and reference mapped.
fn remap(value: &Val, map: &dyn Fn(u32) -> u32) -> Val {
    match value {
        Val::View(view) => Val::View(View {
            array: Loc {
                extent: map(view.array.extent),
                path: view.array.path.clone(),
            },
            ..view.clone()
        }),
        Val::Ref(loc) => Val::Ref(Loc {
            extent: map(loc.extent),
            path: loc.path.clone(),
        }),
        Val::Struct(item, fields) => Val::Struct(
            *item,
            Rc::new(fields.iter().map(|field| remap(field, map)).collect()),
        ),
        Val::Variant(item, index, fields) => Val::Variant(
            *item,
            *index,
            Rc::new(fields.iter().map(|field| remap(field, map)).collect()),
        ),
        Val::Array(elements) => Val::Array(Rc::new(
            elements.iter().map(|element| remap(element, map)).collect(),
        )),
        other => other.clone(),
    }
}
