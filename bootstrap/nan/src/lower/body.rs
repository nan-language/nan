//! The lowering of a NIR function: its values as registers or addresses, its blocks as
//! Cranelift blocks, regions as loops or cown acquisitions around their blocks, and each
//! instruction and terminator as the code that behaves as NIR defines.

use std::collections::HashMap;
use std::rc::Rc;

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{self, InstBuilder, Type, Value as IrValue, types};

use super::abi::{Abi, Repr, Returned, substituted};
use super::fx::{Fx, PTR};
use super::runtime::Routine;
use super::{Lowering, trap};
use crate::check::db::ItemId;
use crate::check::ty::{Args, Scalar, Subst, Ty};
use crate::nir::{
    BinaryOp, BlockId, CheckKind, Function, Inst, Jump, Literal, NTy, Op, Operand, Payload, Place,
    Region, Role, Span, Terminator, UnaryOp, Unit, Value,
};
use crate::prover::Status;
use crate::syntax::ast::Mode;

/// How a NIR value is held in lowered code.
#[derive(Clone, Copy, Debug)]
pub enum Held {
    /// A scalar in a register.
    Scalar(IrValue),
    /// The address of a value's bytes or of the place a reference designates.
    Address(IrValue),
    /// The value of an empty type.
    Empty,
}

impl Held {
    pub fn address(self) -> IrValue {
        match self {
            Held::Address(address) => address,
            other => panic!("an address expected, found {other:?}"),
        }
    }

    pub fn scalar(self) -> IrValue {
        match self {
            Held::Scalar(value) => value,
            other => panic!("a scalar expected, found {other:?}"),
        }
    }

    /// The registers holding it.
    pub fn values(self) -> Vec<IrValue> {
        match self {
            Held::Scalar(value) | Held::Address(value) => vec![value],
            Held::Empty => Vec::new(),
        }
    }
}

/// Where the code after a region continues: the block its `yield` jumps to, and the cowns a
/// `when` region holds, which every exit from it releases.
struct RegionExit {
    yield_to: ir::Block,
    held: Option<IrValue>,
}

pub struct Body<'b, 'c, 'a, 'f> {
    pub lo: &'b mut Lowering<'c, 'a>,
    pub fx: &'b mut Fx<'f>,
    pub item: ItemId,
    pub unit: Rc<Unit>,
    pub function: usize,
    pub subst: Subst,
    pub args: Args,
    values: Vec<Option<Held>>,
    /// The type of each value, its generic parameters substituted; a wrapped part's is the
    /// type whose non-Copy payloads are pointers.
    types: Vec<NTy>,
    /// Whether each value is a slot, whose held address is the slot's storage.
    slots: Vec<bool>,
    /// Whether each value holds a wrapped part.
    wrapped: Vec<bool>,
    blocks: Vec<ir::Block>,
    /// The region holding each block, by the region's first block.
    region_of: HashMap<BlockId, BlockId>,
    exits: HashMap<BlockId, RegionExit>,
    sret: Option<IrValue>,
    failure: Option<IrValue>,
    pub(super) abi: Abi,
    /// The status of each obligation site of the function, none when the statuses are not
    /// decided and every site is checked.
    statuses: Option<HashMap<(BlockId, usize), Status>>,
    /// The span of the instruction being lowered.
    pub(super) current: Span,
    /// Whether the function's returns write the bits of the two sides of the comparison it
    /// returns at the result address, as its `Sides` instance.
    sides: bool,
}

/// Lowers a NIR function of an item's unit, instantiated with `args`.
pub fn lower(
    lo: &mut Lowering,
    fx: &mut Fx,
    abi: &Abi,
    (item, role, index): (ItemId, Role, u32),
    args: &Args,
    sides: bool,
) {
    let unit = lo.ck.unit(item).expect("a unit lowered");
    let position = unit
        .functions
        .iter()
        .position(|function| function.role == role && function.index == index)
        .expect("a function of the unit");
    let subst = lo.subst(item, args);
    let function = &unit.functions[position];
    let statuses = statuses(lo, item, role, index);
    let mut body = Body {
        item,
        function: position,
        subst,
        args: args.clone(),
        values: vec![None; function.types.len()],
        types: Vec::new(),
        slots: vec![false; function.types.len()],
        wrapped: vec![false; function.types.len()],
        blocks: Vec::new(),
        region_of: HashMap::new(),
        exits: HashMap::new(),
        sret: None,
        failure: None,
        abi: abi.clone(),
        statuses,
        current: function.span,
        sides,
        unit: unit.clone(),
        lo,
        fx,
    };
    body.prepare();
    body.entry();
    for block in 0..body.function().blocks.len() {
        body.block(BlockId(block as u32));
    }
}

/// The statuses of a function's obligation sites, none for an item whose statuses the checker
/// does not decide.
fn statuses(
    lo: &Lowering,
    item: ItemId,
    role: Role,
    index: u32,
) -> Option<HashMap<(BlockId, usize), Status>> {
    if lo.ck.db.is_carried(item) {
        return None;
    }
    Some(
        lo.ck
            .obligations(item)
            .iter()
            .filter(|site| site.role == role && site.index == index)
            .map(|site| ((site.block, site.position), site.status))
            .collect(),
    )
}

impl<'b, 'c, 'a, 'f> Body<'b, 'c, 'a, 'f> {
    pub fn function(&self) -> &Function {
        &self.unit.functions[self.function]
    }

    /// Whether the function is the body of a test item, from which failures escape.
    fn test_body(&self) -> bool {
        self.function().role == Role::Body
            && matches!(
                self.lo.ck.db.ast(self.item).kind,
                crate::syntax::ast::ItemKind::Test(_)
            )
    }

    /// Computes the types, slots, wrapped parts and regions of the function, and creates its
    /// blocks.
    fn prepare(&mut self) {
        let unit = self.unit.clone();
        let function = &unit.functions[self.function];
        self.types = function
            .types
            .iter()
            .map(|nty| substituted(&self.subst, nty))
            .collect();
        for block in &function.blocks {
            for inst in &block.insts {
                if let (Some(result), Op::Slot { .. }) = (inst.result, &inst.op) {
                    self.slots[result.0 as usize] = true;
                }
                match &inst.op {
                    Op::Quantifier { region, .. } | Op::When { region, .. } => {
                        let first = region.blocks[0];
                        for block in &region.blocks {
                            self.region_of.entry(*block).or_insert(first);
                        }
                    }
                    _ => {}
                }
            }
        }
        self.mark_wrapped(function);
        for block in &function.blocks {
            let ir_block = self.fx.b.create_block();
            for param in &block.params {
                let ty = match self.value_repr(*param) {
                    Repr::Scalar(ty) => ty,
                    Repr::Address => PTR,
                    Repr::Empty => continue,
                };
                self.fx.b.append_block_param(ir_block, ty);
            }
            self.blocks.push(ir_block);
        }
    }

    /// Gives each value holding a wrapped part its wrapped type: the results of projections
    /// returning an `Option` or `Result`, variants built from references, and block
    /// parameters receiving one of these.
    fn mark_wrapped(&mut self, function: &Function) {
        let mut wrapped = vec![false; function.types.len()];
        let projection_result = self.wraps_result();
        loop {
            let mut changed = false;
            for block in &function.blocks {
                for inst in &block.insts {
                    let Some(result) = inst.result else {
                        continue;
                    };
                    let marks = match &inst.op {
                        Op::Call(call) => self.call_wraps(call),
                        Op::Variant { payload, ty, .. } => {
                            (projection_result.as_ref() == Some(&self.subst.ty(ty)))
                                || payload_operands(payload)
                                    .iter()
                                    .any(|operand| self.operand_is_reference(operand, &wrapped))
                        }
                        Op::Dbg(Operand::Value(value)) => wrapped[value.0 as usize],
                        _ => false,
                    };
                    if marks && !wrapped[result.0 as usize] {
                        wrapped[result.0 as usize] = true;
                        changed = true;
                    }
                }
                let mut incoming: Vec<(BlockId, &[Operand])> = Vec::new();
                match &block.term {
                    Terminator::Jump(jump) => incoming.push((jump.block, &jump.args)),
                    Terminator::Branch(_, then, otherwise) => {
                        incoming.push((then.block, &then.args));
                        incoming.push((otherwise.block, &otherwise.args));
                    }
                    Terminator::Call { call, ok, .. } => {
                        let target = &function.blocks[ok.0 as usize];
                        if self.call_wraps(call) && !wrapped[target.params[0].0 as usize] {
                            wrapped[target.params[0].0 as usize] = true;
                            changed = true;
                        }
                    }
                    _ => {}
                }
                for (target, args) in incoming {
                    let params = &function.blocks[target.0 as usize].params;
                    for (param, arg) in params.iter().zip(args) {
                        let carries =
                            matches!(arg, Operand::Value(value) if wrapped[value.0 as usize]);
                        if carries && !wrapped[param.0 as usize] {
                            wrapped[param.0 as usize] = true;
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
        self.wrapped = wrapped;
    }

    /// Whether a value holds a wrapped part, laid out as its type's wrapped type.
    pub fn wrapped_flag(&self, value: Value) -> bool {
        self.wrapped[value.0 as usize]
    }

    /// The Cranelift block of a NIR block.
    pub fn block_of(&self, block: BlockId) -> ir::Block {
        self.blocks[block.0 as usize]
    }

    /// The source text a value's definition spans: a parameter's name, or the text of the
    /// construct an instruction elaborates.
    pub fn value_text(&self, value: Value) -> String {
        let function = self.function();
        let span = function
            .params
            .iter()
            .find(|param| param.value == value)
            .map(|param| param.span)
            .or_else(|| {
                function.blocks.iter().find_map(|block| {
                    block
                        .insts
                        .iter()
                        .find(|inst| inst.result == Some(value))
                        .map(|inst| inst.span)
                })
            });
        let Some(span) = span else {
            return "_".to_owned();
        };
        let data = self.lo.ck.db.item(self.item);
        let tokens = &data.file.syntax.tokens;
        let (first, last) = (&tokens[span.first as usize], &tokens[span.last as usize]);
        let source = &data.file.syntax.source;
        String::from_utf8_lossy(&source[first.start as usize..last.end as usize]).into_owned()
    }

    /// The result type of a projection whose results are wrapped parts, when this function is
    /// the body of one.
    fn wraps_result(&self) -> Option<Ty> {
        let function = self.function();
        if !matches!(function.role, Role::Body | Role::Entry) {
            return None;
        }
        let sig = self.lo.ck.fn_sig(self.item)?;
        let result = self.subst.ty(&function.result);
        (sig.is_proj() && self.lo.wraps(&result)).then_some(result)
    }

    /// Whether a call's result is a wrapped part: a projection's `Option` or `Result`.
    fn call_wraps(&self, call: &crate::nir::Call) -> bool {
        let projection = match &call.callee {
            crate::nir::Callee::Fn(fn_ref) => self.lo.callable_is_proj(&fn_ref.callable),
            crate::nir::Callee::Dyn {
                interface, method, ..
            } => self.lo.method_is_proj(*interface, *method),
            crate::nir::Callee::Value(_) => false,
        };
        projection
            && self
                .call_result_ty(call)
                .is_some_and(|result| self.lo.wraps(&result))
    }

    /// The type a call's result has in this function.
    fn call_result_ty(&self, call: &crate::nir::Call) -> Option<Ty> {
        let function = self.function();
        for block in &function.blocks {
            for inst in &block.insts {
                if let Op::Call(candidate) = &inst.op
                    && std::ptr::eq(candidate, call)
                {
                    return inst
                        .result
                        .map(|result| self.subst.ty(function.ty(result).ty()));
                }
            }
            if let Terminator::Call {
                call: candidate,
                ok,
                ..
            } = &block.term
                && std::ptr::eq(candidate, call)
            {
                let param = function.blocks[ok.0 as usize].params[0];
                return Some(self.subst.ty(function.ty(param).ty()));
            }
        }
        None
    }

    fn operand_is_reference(&self, operand: &Operand, wrapped: &[bool]) -> bool {
        match operand {
            Operand::Value(value) => {
                self.function().ty(*value).is_reference() || wrapped[value.0 as usize]
            }
            Operand::Place(Mode::Read | Mode::Mut, _) => true,
            _ => false,
        }
    }

    pub fn value_repr(&mut self, value: Value) -> Repr {
        if self.slots[value.0 as usize] {
            return Repr::Address;
        }
        let nty = self.types[value.0 as usize].clone();
        self.lo.repr(&nty)
    }

    pub fn nty(&self, value: Value) -> &NTy {
        &self.types[value.0 as usize]
    }

    pub fn held(&self, value: Value) -> Held {
        self.values[value.0 as usize]
            .unwrap_or_else(|| panic!("value %{} used before its definition", value.0))
    }

    fn define(&mut self, value: Value, held: Held) {
        self.values[value.0 as usize] = Some(held);
    }

    /// Binds the function's parameters: the environment of a closure and its captures, the
    /// result and failure addresses, then each parameter adapted to its NIR type.
    fn entry(&mut self) {
        let params = {
            let entry = self.blocks[0];
            self.fx.b.switch_to_block(entry);
            self.fx.b.append_block_params_for_function_params(entry);
            self.fx.mark_ran();
            self.fx.b.block_params(entry).to_vec()
        };
        let mut incoming = params.into_iter();
        let env = self
            .abi
            .env
            .then(|| incoming.next().expect("an environment"));
        if self.abi.returned == Returned::Indirect {
            self.sret = incoming.next();
        }
        if self.abi.flagged {
            self.failure = incoming.next();
        }
        let function_params = self.function().params.clone();
        let captures = match env {
            Some(_) if self.function().role == Role::Closure => {
                self.lo.closure_captures(&self.unit, self.function().index)
            }
            _ => 0,
        };
        if let Some(env) = env
            && captures > 0
        {
            let task = self
                .function()
                .closure
                .as_ref()
                .is_some_and(|head| head.task);
            let start = if task { super::transfer::ENV_HEADER } else { 0 };
            let layout = self
                .lo
                .capture_layout(&self.types, &function_params[..captures], start);
            for (param, field) in function_params[..captures].iter().zip(layout) {
                let held = match self.value_repr(param.value) {
                    Repr::Empty => Held::Empty,
                    Repr::Scalar(ty) => {
                        if self.nty(param.value).is_reference() {
                            Held::Address(self.fx.load(PTR, env, field))
                        } else {
                            Held::Scalar(self.fx.load(ty, env, field))
                        }
                    }
                    Repr::Address => {
                        if self.nty(param.value).is_reference() {
                            Held::Address(self.fx.load(PTR, env, field))
                        } else {
                            Held::Address(self.fx.offset(env, field))
                        }
                    }
                };
                self.define(param.value, held);
            }
        }
        let own = &function_params[captures..];
        let abi_params = self.abi.params.clone();
        for (param, (abi_nty, repr)) in own.iter().zip(abi_params) {
            let received = match repr {
                Repr::Scalar(_) => Held::Scalar(incoming.next().expect("a parameter")),
                Repr::Address => Held::Address(incoming.next().expect("a parameter")),
                Repr::Empty => Held::Empty,
            };
            let wanted = self.value_repr(param.value);
            let held = self.adapt(received, &abi_nty, wanted);
            self.define(param.value, held);
        }
    }

    /// A value held one way, held as `wanted`: a scalar spilled to a slot for an address, or
    /// loaded from its address for a register.
    pub fn adapt(&mut self, held: Held, nty: &NTy, wanted: Repr) -> Held {
        match (held, wanted) {
            (Held::Scalar(value), Repr::Address) => {
                let ty = nty.ty().clone();
                let layout = self.lo.layout(&ty);
                let slot = self.fx.slot(layout.size, layout.align);
                self.store_scalar(value, slot, 0, &ty);
                Held::Address(slot)
            }
            (Held::Address(address), Repr::Scalar(_)) => {
                let ty = nty.ty().clone();
                Held::Scalar(self.load_scalar(address, 0, &ty))
            }
            (_, Repr::Empty) => Held::Empty,
            (held, _) => held,
        }
    }

    fn block(&mut self, block: BlockId) {
        let unit = self.unit.clone();
        let function = &unit.functions[self.function];
        let data = &function.blocks[block.0 as usize];
        let ir_block = self.blocks[block.0 as usize];
        if block.0 != 0 {
            self.fx.b.switch_to_block(ir_block);
            let params = self.fx.b.block_params(ir_block).to_vec();
            let mut params = params.into_iter();
            for param in &data.params {
                let held = match self.value_repr(*param) {
                    Repr::Scalar(_) => Held::Scalar(params.next().expect("a block parameter")),
                    Repr::Address => Held::Address(params.next().expect("a block parameter")),
                    Repr::Empty => Held::Empty,
                };
                self.define(*param, held);
            }
        }
        for (position, inst) in data.insts.iter().enumerate() {
            self.lo.locate(self.fx, self.item, inst.span);
            self.current = inst.span;
            self.inst(block, position, inst);
        }
        self.lo.locate(self.fx, self.item, data.term_span);
        self.current = data.term_span;
        self.terminator(block, data.insts.len(), &data.term);
    }

    fn status(&self, block: BlockId, position: usize) -> Status {
        match &self.statuses {
            None => Status::Checked,
            Some(statuses) => statuses
                .get(&(block, position))
                .copied()
                .unwrap_or(Status::Checked),
        }
    }

    /// Whether the obligation of a site is checked at run time.
    pub fn checked(&self, block: BlockId, position: usize) -> bool {
        self.status(block, position) == Status::Checked
    }

    fn inst(&mut self, block: BlockId, position: usize, inst: &Inst) {
        let held = match &inst.op {
            Op::Slot { ty, .. } => {
                let ty = self.subst.ty(ty);
                let layout = self.lo.layout(&ty);
                Some(Held::Address(self.fx.slot(layout.size, layout.align)))
            }
            Op::Load(place) | Op::Move(place) => Some(self.read_place(place)),
            Op::Ref(place) | Op::RefMut(place) => Some(Held::Address(self.place_address(place))),
            Op::Init(place, operand) => {
                self.store_place(place, operand, false, block, position);
                None
            }
            Op::Assign(place, operand) => {
                self.store_place(place, operand, true, block, position);
                None
            }
            Op::Release(place) => {
                let ty = self.place_type(place);
                let address = self.place_address(place);
                self.lo.release(self.fx, &ty, address);
                None
            }
            Op::FnRef(fn_ref) => Some(self.fn_value(fn_ref)),
            Op::Constant(constant) => {
                let result = inst.result.expect("a constant value");
                let ty = self.nty(result).ty().clone();
                Some(self.constant(constant, &ty))
            }
            Op::Construct { ty, fields } => {
                let ty = self.subst.ty(ty);
                Some(self.construct(&ty, fields, block, position))
            }
            Op::Variant {
                variant, payload, ..
            } => {
                let result = inst.result.expect("a variant value");
                let ty = self.nty(result).ty().clone();
                let wrapped = self.is_wrapped(result);
                Some(self.variant(&ty, variant, payload, wrapped))
            }
            Op::Is { place, variant, .. } => {
                let (address, ty) = self.place_storage(place);
                Some(Held::Scalar(
                    self.lo.variant_is(self.fx, address, &ty, variant),
                ))
            }
            Op::Closure { index, captures } => Some(self.closure(*index, captures)),
            Op::Convert { value, to, via } => {
                let to = self.subst.ty(to);
                Some(self.convert(value, &to, via.as_deref()))
            }
            Op::Unary(op, operand) => {
                let result = inst.result.expect("a unary result");
                let ty = self.nty(result).ty().clone();
                Some(self.unary(*op, operand, &ty, block, position))
            }
            Op::Binary(op, left, right) => Some(self.binary(*op, left, right, block, position)),
            Op::Call(call) => self.call(call, inst.result, block, position, None),
            Op::Check {
                kind,
                operand,
                open,
            } => {
                if !*open && *kind != CheckKind::Decreases && self.checked(block, position) {
                    let (held, _) = self.operand(operand);
                    let holds = held.scalar();
                    let condition = match &operand {
                        Operand::Value(value) => Some(*value),
                        _ => None,
                    };
                    let kind = trap::check_kind(*kind);
                    self.trap_unless(holds, kind, trap::Operands::Condition(condition));
                }
                None
            }
            Op::Assume(_) => None,
            Op::Dbg(operand) => {
                let (held, nty) = self.operand(operand);
                if self.lo.seeded() {
                    trap::dbg_line(self, held, &nty);
                }
                Some(held)
            }
            Op::Quantifier {
                all,
                low,
                high,
                inclusive,
                region,
            } => Some(self.quantifier(*all, low, high, *inclusive, region)),
            Op::When {
                attempt,
                accesses,
                region,
            } => {
                let result = inst.result;
                super::runtime::seeded::scheduling_point(self.lo, self.fx);
                Some(self.when(*attempt, accesses, region, result))
            }
        };
        if let (Some(result), Some(held)) = (inst.result, held) {
            self.define(result, held);
        }
    }

    fn terminator(&mut self, block: BlockId, position: usize, term: &Terminator) {
        match term {
            Terminator::Jump(jump) => self.jump(block, jump),
            Terminator::Branch(condition, then, otherwise) => {
                let (held, _) = self.operand(condition);
                let condition = held.scalar();
                let then_args = self.jump_args(then);
                let otherwise_args = self.jump_args(otherwise);
                let (then_block, otherwise_block) = (
                    self.exit_block(block, then.block, then_args),
                    self.exit_block(block, otherwise.block, otherwise_args),
                );
                self.fx.b.ins().brif(
                    condition,
                    then_block.0,
                    &then_block.1,
                    otherwise_block.0,
                    &otherwise_block.1,
                );
            }
            Terminator::Call { call, ok, fail } => {
                self.call(call, None, block, position, Some((*ok, *fail)));
            }
            Terminator::Return(operand) if self.sides => {
                let sides = match operand {
                    Some(Operand::Value(value)) => trap::comparison(self.function(), *value),
                    _ => None,
                };
                if let Some((_, sides, _)) = sides {
                    let unit = self.unit.clone();
                    let function = &unit.functions[self.function];
                    let types = trap::side_types(self.lo, function, &self.subst, &sides);
                    let (offsets, _) = trap::sides_layout(self.lo, &types);
                    let record = self.sret.expect("the sides' record");
                    for ((side, ty), offset) in sides.iter().zip(&types).zip(offsets) {
                        let (held, nty) = self.operand(side);
                        trap::keep_side(self, held, &nty, ty, record, offset);
                    }
                }
                self.fx.b.ins().return_(&[]);
            }
            Terminator::Return(operand) => {
                let wraps = self.wraps_result().is_some();
                let held = operand.as_ref().map(|operand| match wraps {
                    true => self.operand(operand),
                    false => self.owned_operand(operand),
                });
                self.ret(held);
            }
            Terminator::Fail(operand) => {
                let (held, nty) = self.operand(operand);
                let ty = nty.ty().clone();
                match self.test_body() {
                    true => super::test_image::escape(self.lo, self.fx, held, &ty),
                    false => {
                        let failure = self.failure.expect("a failing function");
                        self.write_value(failure, held, &ty);
                    }
                }
                let failed = self.fx.int(types::I8, 1);
                let mut returns = vec![failed];
                if let Returned::Scalar(ty) = self.abi.returned {
                    returns.push(zero(self.fx, ty));
                } else if self.abi.returned == Returned::Part {
                    returns.push(self.fx.ptr(0));
                }
                self.fx.b.ins().return_(&returns);
            }
            Terminator::Todo(_) => {
                self.trap_now("trap.todo", trap::Operands::None);
            }
            Terminator::Unreachable => {
                self.fx
                    .b
                    .ins()
                    .trap(ir::TrapCode::user(1).expect("a trap code"));
            }
            Terminator::Yield(operand) => {
                let (held, _) = self.operand(operand);
                let region = self.region_of[&block];
                let exit = &self.exits[&region];
                let target = exit.yield_to;
                let args: Vec<ir::BlockArg> = held.values().into_iter().map(Into::into).collect();
                self.fx.b.ins().jump(target, &args);
            }
        }
    }

    /// The registers a jump passes to its target's parameters.
    fn jump_args(&mut self, jump: &Jump) -> Vec<IrValue> {
        let unit = self.unit.clone();
        let params = &unit.functions[self.function].blocks[jump.block.0 as usize].params;
        let mut args = Vec::new();
        for (param, operand) in params.iter().zip(&jump.args) {
            let (held, nty) = match self.is_wrapped(*param) {
                true => self.operand(operand),
                false => self.owned_operand(operand),
            };
            let wanted = self.value_repr(*param);
            args.extend(self.adapt(held, &nty, wanted).values());
        }
        args
    }

    fn jump(&mut self, from: BlockId, jump: &Jump) {
        let args = self.jump_args(jump);
        let (target, args) = self.exit_block(from, jump.block, args);
        self.fx.b.ins().jump(target, &args);
    }

    /// The block a jump from `from` to `to` enters: `to` itself, or a block releasing the cowns
    /// of each `when` region the jump leaves before entering it.
    fn exit_block(
        &mut self,
        from: BlockId,
        to: BlockId,
        args: Vec<IrValue>,
    ) -> (ir::Block, Vec<ir::BlockArg>) {
        let target = self.blocks[to.0 as usize];
        let left: Vec<IrValue> = self
            .regions_left(from, to)
            .into_iter()
            .filter_map(|region| self.exits.get(&region).and_then(|exit| exit.held))
            .collect();
        if left.is_empty() {
            return (target, args.into_iter().map(Into::into).collect());
        }
        let current = self.fx.b.current_block().expect("a current block");
        let release = self.fx.b.create_block();
        let types: Vec<Type> = args
            .iter()
            .map(|value| self.fx.b.func.dfg.value_type(*value))
            .collect();
        for ty in &types {
            self.fx.b.append_block_param(release, *ty);
        }
        self.fx.b.switch_to_block(release);
        let passed = self.fx.b.block_params(release).to_vec();
        for held in left {
            self.lo.call_routine(self.fx, Routine::CownRelease, &[held]);
        }
        let passed: Vec<ir::BlockArg> = passed.into_iter().map(Into::into).collect();
        self.fx.b.ins().jump(target, &passed);
        self.fx.b.switch_to_block(current);
        (release, args.into_iter().map(Into::into).collect())
    }

    /// The `when` regions holding `from` that do not hold `to`, innermost first.
    fn regions_left(&self, from: BlockId, to: BlockId) -> Vec<BlockId> {
        let mut left = Vec::new();
        let mut current = self.region_of.get(&from).copied();
        while let Some(region) = current {
            let holds_target = self.region_holds(region, to);
            if holds_target {
                break;
            }
            left.push(region);
            current = self.enclosing_region(region);
        }
        left
    }

    fn region_holds(&self, region: BlockId, block: BlockId) -> bool {
        let mut current = self.region_of.get(&block).copied();
        while let Some(found) = current {
            if found == region {
                return true;
            }
            current = self.enclosing_region(found);
        }
        false
    }

    /// The region holding the instruction that opens a region.
    fn enclosing_region(&self, region: BlockId) -> Option<BlockId> {
        let function = self.function();
        for (index, block) in function.blocks.iter().enumerate() {
            for inst in &block.insts {
                if let Op::Quantifier { region: opened, .. } | Op::When { region: opened, .. } =
                    &inst.op
                    && opened.blocks[0] == region
                {
                    return self.region_of.get(&BlockId(index as u32)).copied();
                }
            }
        }
        None
    }

    /// Returns a value, or `Unit`.
    fn ret(&mut self, held: Option<(Held, NTy)>) {
        let mut returns = Vec::new();
        if self.abi.flagged {
            returns.push(self.fx.int(types::I8, 0));
        }
        match (self.abi.returned, held) {
            (Returned::Scalar(_), Some((held, nty))) => {
                let repr = self.lo.owned_repr(nty.ty());
                let held = self.adapt(held, &nty, repr);
                returns.push(held.scalar());
            }
            (Returned::Indirect, Some((held, nty))) => {
                let sret = self.sret.expect("a result address");
                let ty = self.abi.result.clone();
                let held = self.adapt(held, &nty, Repr::Address);
                self.copy_to(sret, held, &ty);
            }
            (Returned::Part, Some((held, nty))) => {
                let held = self.adapt(held, &nty, Repr::Address);
                returns.push(held.address());
            }
            _ => {}
        }
        self.fx.b.ins().return_(&returns);
    }

    /// The value and NIR type of an operand.
    pub fn operand(&mut self, operand: &Operand) -> (Held, NTy) {
        match operand {
            Operand::Value(value) => (self.held(*value), self.nty(*value).clone()),
            Operand::Literal(literal) => self.literal(literal),
            Operand::Place(mode, place) => {
                let ty = self.place_type(place);
                match mode {
                    Mode::Take => {
                        let held = self.take_place(place, &ty);
                        (held, NTy::Owned(ty))
                    }
                    Mode::Mut => (Held::Address(self.place_address(place)), NTy::RefMut(ty)),
                    Mode::Read => (Held::Address(self.place_address(place)), NTy::Ref(ty)),
                }
            }
        }
    }

    fn literal(&mut self, literal: &Literal) -> (Held, NTy) {
        match literal {
            Literal::Int(value, scalar) => {
                let ty = super::layout::scalar_type(*scalar);
                let held = Held::Scalar(self.fx.b.ins().iconst(ty, *value as i64));
                (held, NTy::Owned(Ty::Scalar(*scalar)))
            }
            Literal::Float(bits, scalar) => {
                let held = Held::Scalar(match scalar {
                    Scalar::F32 => self.fx.b.ins().f32const(f32::from_bits(*bits as u32)),
                    _ => self.fx.b.ins().f64const(f64::from_bits(*bits)),
                });
                (held, NTy::Owned(Ty::Scalar(*scalar)))
            }
            Literal::Bool(value) => (
                Held::Scalar(self.fx.int(types::I8, i64::from(*value))),
                NTy::Owned(Ty::Scalar(Scalar::Bool)),
            ),
            Literal::Char(c) => (
                Held::Scalar(self.fx.int(types::I32, i64::from(u32::from(*c)))),
                NTy::Owned(Ty::Scalar(Scalar::Char)),
            ),
            Literal::Unit => (Held::Empty, NTy::Owned(Ty::Unit)),
            Literal::Text(text) => {
                let ty = self.lo.std_ty("core", "Str", Vec::new());
                let held = self.lo.view_literal(self.fx, text.as_bytes());
                (held, NTy::Owned(ty))
            }
            Literal::Bytes(bytes) => {
                let ty = self.lo.slice_of(Ty::Scalar(Scalar::U8));
                let held = self.lo.view_literal(self.fx, bytes);
                (held, NTy::Owned(ty))
            }
        }
    }

    /// Stores an operand's value at a place: `init` into an empty place, `assign` releasing
    /// what it holds first; a ranged or rest field of a layout checks that the value fits.
    fn store_place(
        &mut self,
        place: &Place,
        operand: &Operand,
        assign: bool,
        block: BlockId,
        position: usize,
    ) {
        let (held, nty) = self.owned_operand(operand);
        let nty = &nty;
        let location = self.locate_place(place);
        if let super::place::Location::Bits { .. } = &location {
            let value = self.adapt(held, nty, Repr::Scalar(types::I64)).scalar();
            let fits = self.bits_fit(&location, value);
            if let Some(fits) = fits
                && self.checked(block, position)
            {
                let field = place.steps.last().expect("a field").clone();
                let stored = vec![(operand.clone(), held, nty.clone())];
                let operands = trap::Operands::Values(trap::Form::Field(field), stored);
                self.trap_unless(fits, "trap.conversion", operands);
            }
            self.write_location(&location, Held::Scalar(value), nty);
            return;
        }
        if assign {
            let ty = self.location_type(&location);
            let address = self.location_address(&location);
            self.lo.release(self.fx, &ty, address);
        }
        self.write_location(&location, held, nty);
    }

    /// Lowers the rest of a function after a call that never returns.
    pub fn unreachable_after(&mut self) {
        self.fx
            .b
            .ins()
            .trap(ir::TrapCode::user(1).expect("a trap code"));
        let next = self.fx.b.create_block();
        self.fx.b.switch_to_block(next);
    }

    fn quantifier(
        &mut self,
        all: bool,
        low: &Operand,
        high: &Operand,
        inclusive: bool,
        region: &Region,
    ) -> Held {
        let (low_held, low_nty) = self.operand(low);
        let (high_held, _) = self.operand(high);
        let scalar = low_nty.ty().scalar().expect("an integer range");
        let (low, high) = (low_held.scalar(), high_held.scalar());
        let ty = super::layout::scalar_type(scalar);
        let less = if scalar.is_signed() {
            IntCC::SignedLessThan
        } else {
            IntCC::UnsignedLessThan
        };
        let header = self.fx.b.create_block();
        let yielded = self.fx.b.create_block();
        let done = self.fx.b.create_block();
        self.fx.b.append_block_param(header, ty);
        self.fx.b.append_block_param(yielded, types::I8);
        self.fx.b.append_block_param(done, types::I8);
        let entry = self.blocks[region.blocks[0].0 as usize];
        self.exits.insert(
            region.blocks[0],
            RegionExit {
                yield_to: yielded,
                held: None,
            },
        );
        let counter = self.fx.b.declare_var(ty);
        self.fx.b.def_var(counter, low);
        self.fx.b.ins().jump(header, &[low.into()]);
        self.fx.b.switch_to_block(header);
        let current = self.fx.b.block_params(header)[0];
        self.fx.b.def_var(counter, current);
        let inside = if inclusive {
            let cc = if scalar.is_signed() {
                IntCC::SignedLessThanOrEqual
            } else {
                IntCC::UnsignedLessThanOrEqual
            };
            self.fx.b.ins().icmp(cc, current, high)
        } else {
            self.fx.b.ins().icmp(less, current, high)
        };
        let decided_none = self.fx.int(types::I8, i64::from(all));
        self.fx.b.ins().brif(
            inside,
            entry,
            &[current.into()],
            done,
            &[decided_none.into()],
        );
        self.fx.b.switch_to_block(yielded);
        let result = self.fx.b.block_params(yielded)[0];
        let wanted = self.fx.int(types::I8, i64::from(all));
        let same = self.fx.b.ins().icmp(IntCC::Equal, result, wanted);
        let at = self.fx.b.use_var(counter);
        let one = self.fx.b.ins().iconst(ty, 1);
        let next = self.fx.b.ins().iadd(at, one);
        let decided = self.fx.int(types::I8, i64::from(!all));
        let wraps = if inclusive {
            Some(self.fx.b.ins().icmp(IntCC::Equal, at, high))
        } else {
            None
        };
        match wraps {
            Some(last) => {
                let go_on = self.fx.b.create_block();
                self.fx
                    .b
                    .ins()
                    .brif(same, go_on, &[], done, &[decided.into()]);
                self.fx.b.switch_to_block(go_on);
                let finished = self.fx.int(types::I8, i64::from(all));
                self.fx
                    .b
                    .ins()
                    .brif(last, done, &[finished.into()], header, &[next.into()]);
            }
            None => {
                self.fx
                    .b
                    .ins()
                    .brif(same, header, &[next.into()], done, &[decided.into()]);
            }
        }
        self.fx.b.switch_to_block(done);
        Held::Scalar(self.fx.b.block_params(done)[0])
    }

    fn when(
        &mut self,
        attempt: bool,
        accesses: &[Operand],
        region: &Region,
        result: Option<Value>,
    ) -> Held {
        super::supplied::tasks::when(self, attempt, accesses, region, result)
    }

    /// Opens a region whose `yield` jumps to `yield_to`, holding the cowns at `held`.
    pub fn open_region(&mut self, region: &Region, yield_to: ir::Block, held: Option<IrValue>) {
        self.exits
            .insert(region.blocks[0], RegionExit { yield_to, held });
    }

    pub fn region_entry(&self, region: &Region) -> ir::Block {
        self.blocks[region.blocks[0].0 as usize]
    }

    fn unary(
        &mut self,
        op: UnaryOp,
        operand: &Operand,
        ty: &Ty,
        block: BlockId,
        position: usize,
    ) -> Held {
        let (held, nty) = self.operand(operand);
        let repr = self.lo.owned_repr(ty);
        let value = self.adapt(held, &nty, repr).scalar();
        let scalar = ty.scalar();
        Held::Scalar(match (op, scalar) {
            (UnaryOp::Not, _) | (UnaryOp::Inv, Some(Scalar::Bool)) => {
                self.fx.b.ins().bxor_imm_s(value, 1)
            }
            (UnaryOp::Inv, _) => self.fx.b.ins().bnot(value),
            (UnaryOp::Neg, Some(scalar)) if scalar.is_float() => self.fx.b.ins().fneg(value),
            (UnaryOp::Neg, Some(_)) => {
                let zero = zero(self.fx, self.fx.b.func.dfg.value_type(value));
                let (result, overflow) = self.fx.b.ins().ssub_overflow(zero, value);
                if self.checked(block, position) {
                    let fits = self.fx.b.ins().bxor_imm_s(overflow, 1);
                    let operands = trap::Operands::Values(
                        trap::Form::Unary,
                        vec![(operand.clone(), held, nty)],
                    );
                    self.trap_unless(fits, "trap.overflow", operands);
                }
                result
            }
            _ => panic!("{op:?} on {ty:?}"),
        })
    }

    fn binary(
        &mut self,
        op: BinaryOp,
        left: &Operand,
        right: &Operand,
        block: BlockId,
        position: usize,
    ) -> Held {
        let (left_held, left_nty) = self.operand(left);
        let (right_held, right_nty) = self.operand(right);
        let operand_ty = left_nty.ty().clone();
        if op.is_comparison() && operand_ty.scalar().is_none() {
            let equal = self.lo.views_equal(self.fx, left_held, right_held);
            return Held::Scalar(match op {
                BinaryOp::Eq => equal,
                _ => self.fx.b.ins().bxor_imm_s(equal, 1),
            });
        }
        let scalar = operand_ty.scalar().expect("a scalar operand");
        let repr = self.lo.owned_repr(&operand_ty);
        let a = self.adapt(left_held, &left_nty, repr).scalar();
        let b = self.adapt(right_held, &right_nty, repr).scalar();
        let operands = trap::Operands::Values(
            trap::Form::Binary(op),
            vec![
                (
                    left.clone(),
                    Held::Scalar(a),
                    NTy::Owned(operand_ty.clone()),
                ),
                (
                    right.clone(),
                    Held::Scalar(b),
                    NTy::Owned(operand_ty.clone()),
                ),
            ],
        );
        let checked = self.checked(block, position);
        if scalar.is_float() {
            return Held::Scalar(self.float_binary(op, a, b));
        }
        let value = match op {
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge => {
                let cc = int_cc(op, scalar.is_signed());
                let flag = self.fx.b.ins().icmp(cc, a, b);
                return Held::Scalar(flag);
            }
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul => {
                let (result, overflow) = match (op, scalar.is_signed()) {
                    (BinaryOp::Add, true) => self.fx.b.ins().sadd_overflow(a, b),
                    (BinaryOp::Add, false) => self.fx.b.ins().uadd_overflow(a, b),
                    (BinaryOp::Sub, true) => self.fx.b.ins().ssub_overflow(a, b),
                    (BinaryOp::Sub, false) => self.fx.b.ins().usub_overflow(a, b),
                    (_, true) => self.fx.b.ins().smul_overflow(a, b),
                    _ => self.fx.b.ins().umul_overflow(a, b),
                };
                if checked {
                    let fits = self.fx.b.ins().bxor_imm_s(overflow, 1);
                    self.trap_unless(fits, "trap.overflow", operands);
                }
                result
            }
            BinaryOp::Div | BinaryOp::Rem => {
                let ty = self.fx.b.func.dfg.value_type(b);
                let zero = zero(self.fx, ty);
                let nonzero = self.fx.b.ins().icmp(IntCC::NotEqual, b, zero);
                self.trap_unless(nonzero, "trap.division-by-zero", operands);
                match op {
                    BinaryOp::Div => self.fx.b.ins().udiv(a, b),
                    _ => self.fx.b.ins().urem(a, b),
                }
            }
            BinaryOp::Shl | BinaryOp::Shr => {
                let ty = self.fx.b.func.dfg.value_type(a);
                let width = i64::from(ty.bits());
                let amount = widen(self.fx, b);
                let limit = self.fx.int(types::I64, width);
                let below = self.fx.b.ins().icmp(IntCC::UnsignedLessThan, amount, limit);
                let negative = if right_nty.ty().scalar().is_some_and(Scalar::is_signed) {
                    let zero = zero(self.fx, self.fx.b.func.dfg.value_type(b));
                    Some(self.fx.b.ins().icmp(IntCC::SignedLessThan, b, zero))
                } else {
                    None
                };
                let valid = match negative {
                    Some(negative) => {
                        let positive = self.fx.b.ins().bxor_imm_s(negative, 1);
                        self.fx.b.ins().band(below, positive)
                    }
                    None => below,
                };
                if checked {
                    self.trap_unless(valid, "trap.shift", operands.clone());
                }
                match (op, scalar.is_signed()) {
                    (BinaryOp::Shr, true) => self.fx.b.ins().sshr(a, b),
                    (BinaryOp::Shr, false) => self.fx.b.ins().ushr(a, b),
                    (_, false) => self.fx.b.ins().ishl(a, b),
                    (_, true) => {
                        let shifted = self.fx.b.ins().ishl(a, b);
                        let back = self.fx.b.ins().sshr(shifted, b);
                        let same = self.fx.b.ins().icmp(IntCC::Equal, back, a);
                        if checked {
                            self.trap_unless(same, "trap.overflow", operands);
                        }
                        shifted
                    }
                }
            }
            BinaryOp::Band => self.fx.b.ins().band(a, b),
            BinaryOp::Bor => self.fx.b.ins().bor(a, b),
            BinaryOp::Bxor => self.fx.b.ins().bxor(a, b),
        };
        Held::Scalar(value)
    }

    fn float_binary(&mut self, op: BinaryOp, a: IrValue, b: IrValue) -> IrValue {
        let compare = |cc: FloatCC, body: &mut Self| body.fx.b.ins().fcmp(cc, a, b);
        match op {
            BinaryOp::Add => self.fx.b.ins().fadd(a, b),
            BinaryOp::Sub => self.fx.b.ins().fsub(a, b),
            BinaryOp::Mul => self.fx.b.ins().fmul(a, b),
            BinaryOp::Div => self.fx.b.ins().fdiv(a, b),
            BinaryOp::Rem => super::supplied::floats::remainder(self.lo, self.fx, a, b),
            BinaryOp::Eq => compare(FloatCC::Equal, self),
            BinaryOp::Ne => compare(FloatCC::NotEqual, self),
            BinaryOp::Lt => compare(FloatCC::LessThan, self),
            BinaryOp::Le => compare(FloatCC::LessThanOrEqual, self),
            BinaryOp::Gt => compare(FloatCC::GreaterThan, self),
            BinaryOp::Ge => compare(FloatCC::GreaterThanOrEqual, self),
            _ => panic!("{op:?} on floats"),
        }
    }

    /// Traps with `kind` unless `holds` is nonzero.
    pub fn trap_unless(&mut self, holds: IrValue, kind: &'static str, operands: trap::Operands) {
        let fail = self.fx.b.create_block();
        let next = self.fx.b.create_block();
        self.fx.b.set_cold_block(fail);
        self.fx.b.ins().brif(holds, next, &[], fail, &[]);
        self.fx.b.switch_to_block(fail);
        self.trap_now(kind, operands);
        self.fx.b.switch_to_block(next);
    }

    /// Traps with `kind` here.
    pub fn trap_now(&mut self, kind: &'static str, operands: trap::Operands) {
        trap::raise(self, kind, operands);
    }
}

/// The comparison of integers an operator makes.
pub fn int_cc(op: BinaryOp, signed: bool) -> IntCC {
    match (op, signed) {
        (BinaryOp::Eq, _) => IntCC::Equal,
        (BinaryOp::Ne, _) => IntCC::NotEqual,
        (BinaryOp::Lt, true) => IntCC::SignedLessThan,
        (BinaryOp::Lt, false) => IntCC::UnsignedLessThan,
        (BinaryOp::Le, true) => IntCC::SignedLessThanOrEqual,
        (BinaryOp::Le, false) => IntCC::UnsignedLessThanOrEqual,
        (BinaryOp::Gt, true) => IntCC::SignedGreaterThan,
        (BinaryOp::Gt, false) => IntCC::UnsignedGreaterThan,
        (BinaryOp::Ge, true) => IntCC::SignedGreaterThanOrEqual,
        (BinaryOp::Ge, false) => IntCC::UnsignedGreaterThanOrEqual,
        _ => panic!("{op:?} is no comparison"),
    }
}

/// An integer register widened to 64 bits without its sign.
pub fn widen(fx: &mut Fx, value: IrValue) -> IrValue {
    match fx.b.func.dfg.value_type(value) {
        types::I64 => value,
        _ => fx.b.ins().uextend(types::I64, value),
    }
}

/// The zero of a register type.
pub fn zero(fx: &mut Fx, ty: Type) -> IrValue {
    match ty {
        types::F32 => fx.b.ins().f32const(0.0),
        types::F64 => fx.b.ins().f64const(0.0),
        _ => fx.b.ins().iconst(ty, 0),
    }
}

fn payload_operands(payload: &Payload) -> Vec<&Operand> {
    match payload {
        Payload::None => Vec::new(),
        Payload::Single(operand) => vec![operand],
        Payload::Labelled(fields) => fields.iter().map(|(_, operand)| operand).collect(),
    }
}
