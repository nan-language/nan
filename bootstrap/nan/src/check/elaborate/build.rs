//! The builder of one NIR function: blocks and values as elaboration creates them, numbered at the
//! end in depth-first preorder from the entry, a region's blocks when the walk reaches its
//! instruction, values along the walk.

use std::collections::{HashMap, HashSet};

use crate::nir::{
    Block, BlockId, Call, Callee, Inst, Jump, NTy, Op, Operand, Payload, Place, Region, Span,
    Terminator, Value,
};

struct Draft {
    params: Vec<Value>,
    insts: Vec<Inst>,
    term: Option<(Terminator, Span)>,
}

pub struct Builder {
    blocks: Vec<Draft>,
    current: Option<BlockId>,
    types: Vec<NTy>,
    slots: HashSet<Value>,
    /// The blocks created while each open region is elaborated, innermost last.
    regions: Vec<Vec<BlockId>>,
}

impl Builder {
    /// A builder whose current block is the entry.
    pub fn new() -> Builder {
        let mut builder = Builder {
            blocks: Vec::new(),
            current: None,
            types: Vec::new(),
            slots: HashSet::new(),
            regions: Vec::new(),
        };
        let entry = builder.block();
        builder.current = Some(entry);
        builder
    }

    pub fn value(&mut self, ty: NTy) -> Value {
        self.types.push(ty);
        Value(self.types.len() as u32 - 1)
    }

    pub fn ty(&self, value: Value) -> &NTy {
        &self.types[value.0 as usize]
    }

    pub fn is_slot(&self, value: Value) -> bool {
        self.slots.contains(&value)
    }

    pub fn block(&mut self) -> BlockId {
        self.blocks.push(Draft {
            params: Vec::new(),
            insts: Vec::new(),
            term: None,
        });
        let id = BlockId(self.blocks.len() as u32 - 1);
        if let Some(region) = self.regions.last_mut() {
            region.push(id);
        }
        id
    }

    pub fn block_param(&mut self, block: BlockId, ty: NTy) -> Value {
        let value = self.value(ty);
        self.blocks[block.0 as usize].params.push(value);
        value
    }

    pub fn current(&self) -> Option<BlockId> {
        self.current
    }

    /// Whether code emitted now is reachable: a current block is open.
    pub fn open(&self) -> bool {
        self.current.is_some()
    }

    pub fn at(&mut self, block: BlockId) {
        self.current = Some(block);
    }

    /// Emits an instruction into the current block, with a result of `result`'s type.
    pub fn emit(
        &mut self,
        op: Op,
        result: Option<NTy>,
        span: Span,
        trusted: Option<u32>,
    ) -> Option<Value> {
        let current = self.current?;
        let slot = matches!(op, Op::Slot { .. });
        let result = result.map(|ty| self.value(ty));
        if slot && let Some(value) = result {
            self.slots.insert(value);
        }
        self.blocks[current.0 as usize].insts.push(Inst {
            result,
            trusted,
            op,
            span,
        });
        result
    }

    /// Ends the current block with a terminator; nothing is current afterwards.
    pub fn end(&mut self, term: Terminator, span: Span) {
        if let Some(current) = self.current.take() {
            self.blocks[current.0 as usize].term = Some((term, span));
        }
    }

    pub fn jump(&mut self, block: BlockId, args: Vec<Operand>, span: Span) {
        self.end(Terminator::Jump(Jump { block, args }), span);
    }

    /// A block of the code enclosing the region being collected.
    pub fn outer_block(&mut self) -> BlockId {
        let region = self.regions.pop().expect("an open region");
        let block = self.block();
        self.regions.push(region);
        block
    }

    /// Starts collecting the blocks of a region.
    pub fn open_region(&mut self) {
        self.regions.push(Vec::new());
    }

    pub fn close_region(&mut self) -> Region {
        Region {
            blocks: self.regions.pop().expect("an open region"),
        }
    }

    /// The finished blocks and value types, renumbered; `params` are the function's parameter
    /// values, numbered first.
    pub fn finish(self, params: &[Value]) -> (Vec<Block>, Vec<NTy>, Vec<Value>) {
        let mut walk = Walk {
            drafts: &self.blocks,
            blocks: HashMap::new(),
            order: Vec::new(),
            values: HashMap::new(),
        };
        for param in params {
            walk.number(*param);
        }
        walk.block(BlockId(0));
        let Walk {
            blocks: block_map,
            order,
            values: value_map,
            ..
        } = walk;
        let renumber = Renumber {
            blocks: &block_map,
            values: &value_map,
        };
        let mut types = vec![NTy::Owned(crate::check::ty::Ty::Unit); value_map.len()];
        for (old, new) in &value_map {
            types[new.0 as usize] = self.types[old.0 as usize].clone();
        }
        let blocks = order
            .iter()
            .map(|old| {
                let draft = &self.blocks[old.0 as usize];
                let (term, term_span) = draft
                    .term
                    .clone()
                    .unwrap_or((Terminator::Unreachable, Span::at(0)));
                Block {
                    params: draft
                        .params
                        .iter()
                        .map(|value| renumber.value(*value))
                        .collect(),
                    insts: draft.insts.iter().map(|inst| renumber.inst(inst)).collect(),
                    term: renumber.term(&term),
                    term_span,
                }
            })
            .collect();
        let params = params.iter().map(|value| renumber.value(*value)).collect();
        (blocks, types, params)
    }
}

struct Walk<'d> {
    drafts: &'d [Draft],
    blocks: HashMap<BlockId, BlockId>,
    order: Vec<BlockId>,
    values: HashMap<Value, Value>,
}

impl Walk<'_> {
    fn number(&mut self, value: Value) {
        let next = Value(self.values.len() as u32);
        self.values.entry(value).or_insert(next);
    }

    fn block(&mut self, id: BlockId) {
        if self.blocks.contains_key(&id) {
            return;
        }
        self.blocks.insert(id, BlockId(self.order.len() as u32));
        self.order.push(id);
        let draft = &self.drafts[id.0 as usize];
        for param in &draft.params {
            self.number(*param);
        }
        for inst in &draft.insts {
            if let Some(result) = inst.result {
                self.number(result);
            }
            if let Op::Quantifier { region, .. } | Op::When { region, .. } = &inst.op
                && let Some(first) = region.blocks.first()
            {
                self.block(*first);
            }
        }
        if let Some((term, _)) = &draft.term {
            for successor in term.successors() {
                self.block(successor);
            }
        }
    }
}

struct Renumber<'r> {
    blocks: &'r HashMap<BlockId, BlockId>,
    values: &'r HashMap<Value, Value>,
}

impl Renumber<'_> {
    fn value(&self, value: Value) -> Value {
        self.values[&value]
    }

    fn block(&self, block: BlockId) -> BlockId {
        self.blocks[&block]
    }

    fn place(&self, place: &Place) -> Place {
        Place {
            root: self.value(place.root),
            steps: place.steps.clone(),
        }
    }

    fn operand(&self, operand: &Operand) -> Operand {
        match operand {
            Operand::Value(value) => Operand::Value(self.value(*value)),
            Operand::Literal(literal) => Operand::Literal(literal.clone()),
            Operand::Place(mode, place) => Operand::Place(*mode, self.place(place)),
        }
    }

    fn operands(&self, operands: &[Operand]) -> Vec<Operand> {
        operands
            .iter()
            .map(|operand| self.operand(operand))
            .collect()
    }

    fn labelled(&self, fields: &[(String, Operand)]) -> Vec<(String, Operand)> {
        fields
            .iter()
            .map(|(name, operand)| (name.clone(), self.operand(operand)))
            .collect()
    }

    fn region(&self, region: &Region) -> Region {
        let mut blocks: Vec<BlockId> = region
            .blocks
            .iter()
            .filter_map(|block| self.blocks.get(block).copied())
            .collect();
        blocks.sort();
        Region { blocks }
    }

    fn call(&self, call: &Call) -> Call {
        Call {
            awaited: call.awaited,
            callee: match &call.callee {
                Callee::Fn(fn_ref) => Callee::Fn(fn_ref.clone()),
                Callee::Value(value) => Callee::Value(self.value(*value)),
                Callee::Dyn {
                    place,
                    interface,
                    method,
                } => Callee::Dyn {
                    place: self.place(place),
                    interface: *interface,
                    method: *method,
                },
            },
            args: self.operands(&call.args),
        }
    }

    fn inst(&self, inst: &Inst) -> Inst {
        let op = match &inst.op {
            Op::Slot { name, ty } => Op::Slot {
                name: name.clone(),
                ty: ty.clone(),
            },
            Op::Load(place) => Op::Load(self.place(place)),
            Op::Move(place) => Op::Move(self.place(place)),
            Op::Ref(place) => Op::Ref(self.place(place)),
            Op::RefMut(place) => Op::RefMut(self.place(place)),
            Op::Init(place, operand) => Op::Init(self.place(place), self.operand(operand)),
            Op::Assign(place, operand) => Op::Assign(self.place(place), self.operand(operand)),
            Op::Release(place) => Op::Release(self.place(place)),
            Op::FnRef(fn_ref) => Op::FnRef(fn_ref.clone()),
            Op::Constant(constant) => Op::Constant(*constant),
            Op::Construct { ty, fields } => Op::Construct {
                ty: ty.clone(),
                fields: self.labelled(fields),
            },
            Op::Variant {
                ty,
                variant,
                payload,
            } => Op::Variant {
                ty: ty.clone(),
                variant: variant.clone(),
                payload: match payload {
                    Payload::None => Payload::None,
                    Payload::Single(operand) => Payload::Single(self.operand(operand)),
                    Payload::Labelled(fields) => Payload::Labelled(self.labelled(fields)),
                },
            },
            Op::Is { place, ty, variant } => Op::Is {
                place: self.place(place),
                ty: ty.clone(),
                variant: variant.clone(),
            },
            Op::Closure { index, captures } => Op::Closure {
                index: *index,
                captures: self.operands(captures),
            },
            Op::Convert { value, to, via } => Op::Convert {
                value: self.operand(value),
                to: to.clone(),
                via: via.clone(),
            },
            Op::Unary(op, operand) => Op::Unary(*op, self.operand(operand)),
            Op::Binary(op, left, right) => Op::Binary(*op, self.operand(left), self.operand(right)),
            Op::Call(call) => Op::Call(self.call(call)),
            Op::Check {
                kind,
                operand,
                open,
            } => Op::Check {
                kind: *kind,
                operand: self.operand(operand),
                open: *open,
            },
            Op::Assume(operand) => Op::Assume(self.operand(operand)),
            Op::Quantifier {
                all,
                low,
                high,
                inclusive,
                region,
            } => Op::Quantifier {
                all: *all,
                low: self.operand(low),
                high: self.operand(high),
                inclusive: *inclusive,
                region: self.region(region),
            },
            Op::When {
                attempt,
                accesses,
                region,
            } => Op::When {
                attempt: *attempt,
                accesses: self.operands(accesses),
                region: self.region(region),
            },
        };
        Inst {
            result: inst.result.map(|value| self.value(value)),
            trusted: inst.trusted,
            op,
            span: inst.span,
        }
    }

    fn jump(&self, jump: &Jump) -> Jump {
        Jump {
            block: self.block(jump.block),
            args: self.operands(&jump.args),
        }
    }

    fn term(&self, term: &Terminator) -> Terminator {
        match term {
            Terminator::Jump(jump) => Terminator::Jump(self.jump(jump)),
            Terminator::Branch(operand, then, otherwise) => {
                Terminator::Branch(self.operand(operand), self.jump(then), self.jump(otherwise))
            }
            Terminator::Call { call, ok, fail } => Terminator::Call {
                call: self.call(call),
                ok: self.block(*ok),
                fail: self.block(*fail),
            },
            Terminator::Return(operand) => {
                Terminator::Return(operand.as_ref().map(|operand| self.operand(operand)))
            }
            Terminator::Fail(operand) => Terminator::Fail(self.operand(operand)),
            Terminator::Todo(intent) => Terminator::Todo(intent.clone()),
            Terminator::Unreachable => Terminator::Unreachable,
            Terminator::Yield(operand) => Terminator::Yield(self.operand(operand)),
        }
    }
}
