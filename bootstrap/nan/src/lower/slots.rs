//! Stack slot sharing: the slots of a function whose contents are never needed at once share
//! one slot. A slot's contents are reached only through the addresses derived from its own, so
//! they are needed wherever such an address is live. Slots whose addresses meet, in one call's
//! arguments or one stored in the other, are needed together as one group. A slot is kept to
//! itself when its address may outlive the addresses derived from it: stored outside the
//! function's slots, or given to a callee that may keep it.

use std::collections::{BTreeSet, HashMap};

use cranelift_codegen::ir::{
    BlockArg, ExternalName, FuncRef, Function, Inst, InstructionData, StackSlot, StackSlotData,
    Value,
};

/// What a callee does with the addresses given to it once it returns.
pub enum Callee {
    /// It may keep them.
    Keeping,
    /// It keeps none.
    Plain,
    /// It keeps none, copying the bytes at its second address to its first.
    Copying,
}

/// Shares the slots of `func` whose groups are never needed at once, a shared slot taking the
/// largest size and alignment of the slots it stands for and those taking no bytes; `callee`
/// tells by function index what a callee named so does with addresses, every callee named
/// otherwise possibly keeping them.
pub fn share(func: &mut Function, callee: &dyn Fn(u32) -> Callee) {
    let count = func.sized_stack_slots.len();
    if count < 2 {
        return;
    }
    let mut groups = Groups::new(func, count, callee);
    groups.derive();
    let needed = groups.needed();
    let mut homes: Vec<Home> = Vec::new();
    let mut moved: HashMap<usize, usize> = HashMap::new();
    for slot in 0..count {
        let root = groups.root(slot);
        if groups.pinned[root] {
            continue;
        }
        let data = &func.sized_stack_slots[StackSlot::from_u32(slot as u32)];
        let fitting = homes.iter_mut().find(|home| {
            home.roots
                .iter()
                .all(|other| *other != root && !needed[*other].meets(&needed[root]))
        });
        match fitting {
            Some(home) => {
                home.size = home.size.max(data.size);
                home.align_shift = home.align_shift.max(data.align_shift);
                home.roots.push(root);
                moved.insert(slot, home.slot);
            }
            None => homes.push(Home {
                slot,
                roots: vec![root],
                size: data.size,
                align_shift: data.align_shift,
            }),
        }
    }
    if moved.is_empty() {
        return;
    }
    for home in &homes {
        let data = &mut func.sized_stack_slots[StackSlot::from_u32(home.slot as u32)];
        data.size = home.size;
        data.align_shift = home.align_shift;
    }
    for &slot in moved.keys() {
        let data = &mut func.sized_stack_slots[StackSlot::from_u32(slot as u32)];
        *data = StackSlotData::new(data.kind, 0, 0);
    }
    let blocks: Vec<_> = func.layout.blocks().collect();
    for block in blocks {
        let insts: Vec<_> = func.layout.block_insts(block).collect();
        for inst in insts {
            if let InstructionData::StackAddr { stack_slot, .. } = &mut func.dfg.insts[inst]
                && let Some(home) = moved.get(&(stack_slot.as_u32() as usize))
            {
                *stack_slot = StackSlot::from_u32(*home as u32);
            }
        }
    }
}

/// A slot others share: the groups placed in it, its size and its alignment.
struct Home {
    slot: usize,
    roots: Vec<usize>,
    size: u32,
    align_shift: u8,
}

/// Program points of a function, numbered in layout order.
struct Points(Vec<u64>);

impl Points {
    fn new(count: usize) -> Points {
        Points(vec![0; count.div_ceil(64)])
    }

    fn mark(&mut self, point: usize) {
        self.0[point / 64] |= 1 << (point % 64);
    }

    fn meets(&self, other: &Points) -> bool {
        self.0
            .iter()
            .zip(&other.0)
            .any(|(left, right)| left & right != 0)
    }
}

/// The groups of a function's slots, with the slots each address derived from slot addresses
/// may point into.
struct Groups<'f> {
    func: &'f Function,
    callee: &'f dyn Fn(u32) -> Callee,
    /// Each slot's parent in its group, a group's root being its least slot.
    parent: Vec<usize>,
    /// Whether each slot, and once derived each group by its root, is kept to itself.
    pinned: Vec<bool>,
    /// Whether each group, by its root, may hold an address derived from a slot address.
    holding: Vec<bool>,
    /// The slots each derived address may point into, ascending.
    derived: HashMap<Value, Vec<usize>>,
    changed: bool,
}

impl<'f> Groups<'f> {
    fn new(func: &'f Function, count: usize, callee: &'f dyn Fn(u32) -> Callee) -> Groups<'f> {
        Groups {
            func,
            callee,
            parent: (0..count).collect(),
            pinned: vec![false; count],
            holding: vec![false; count],
            derived: HashMap::new(),
            changed: false,
        }
    }

    fn root(&self, slot: usize) -> usize {
        let mut at = slot;
        while self.parent[at] != at {
            at = self.parent[at];
        }
        at
    }

    /// Places slots in one group, which holds an address when one of theirs does.
    fn unite(&mut self, slots: &[usize]) {
        for pair in slots.windows(2) {
            let (left, right) = (self.root(pair[0]), self.root(pair[1]));
            if left != right {
                let (low, high) = (left.min(right), left.max(right));
                self.parent[high] = low;
                self.holding[low] |= self.holding[high];
                self.changed = true;
            }
        }
    }

    /// Marks the groups of slots as holding an address.
    fn hold(&mut self, slots: &[usize]) {
        for &slot in slots {
            let root = self.root(slot);
            if !self.holding[root] {
                self.holding[root] = true;
                self.changed = true;
            }
        }
    }

    /// Keeps slots to themselves.
    fn pin(&mut self, slots: &[usize]) {
        for &slot in slots {
            if !self.pinned[slot] {
                self.pinned[slot] = true;
                self.changed = true;
            }
        }
    }

    /// The slots values may point into, ascending.
    fn slots_of(&self, values: impl IntoIterator<Item = Value>) -> Vec<usize> {
        let mut slots: Vec<usize> = values
            .into_iter()
            .filter_map(|value| self.derived.get(&value))
            .flatten()
            .copied()
            .collect();
        slots.sort_unstable();
        slots.dedup();
        slots
    }

    /// Adds slots to those a value may point into.
    fn derive_value(&mut self, value: Value, slots: &[usize]) {
        if slots.is_empty() {
            return;
        }
        let known = self.derived.entry(value).or_default();
        let before = known.len();
        known.extend_from_slice(slots);
        known.sort_unstable();
        known.dedup();
        if known.len() != before {
            self.changed = true;
        }
    }

    /// Follows slot addresses through the function until nothing changes: into the values
    /// computed from them, the results of calls given them, block parameters and what a load
    /// reads from a group holding addresses; uniting the slots that meet in a call or in
    /// memory, and pinning those that a store or a callee may keep.
    fn derive(&mut self) {
        let func = self.func;
        loop {
            self.changed = false;
            for block in func.layout.blocks() {
                for inst in func.layout.block_insts(block) {
                    let args: Vec<Value> = func
                        .dfg
                        .inst_args(inst)
                        .iter()
                        .map(|arg| func.dfg.resolve_aliases(*arg))
                        .collect();
                    let reached = match &func.dfg.insts[inst] {
                        InstructionData::StackAddr { stack_slot, .. } => {
                            vec![stack_slot.as_u32() as usize]
                        }
                        InstructionData::Load { .. } | InstructionData::LoadNoOffset { .. } => {
                            self.read(args[0])
                        }
                        InstructionData::Store { .. } | InstructionData::StoreNoOffset { .. } => {
                            self.stored(args[0], args[1]);
                            self.read(args[1])
                        }
                        InstructionData::AtomicRmw { .. } => {
                            self.stored(args[1], args[0]);
                            self.read(args[0])
                        }
                        InstructionData::AtomicCas { .. } => {
                            self.stored(args[2], args[0]);
                            self.read(args[0])
                        }
                        InstructionData::Call { func_ref, .. } => {
                            let given = self.slots_of(args.iter().copied());
                            match self.callee_of(*func_ref) {
                                Callee::Keeping => self.pin(&given),
                                Callee::Plain => self.called(&given),
                                Callee::Copying => {
                                    let held = self.read(args[1]);
                                    self.unite(&given);
                                    if !held.is_empty() {
                                        self.hold(&given);
                                    }
                                }
                            }
                            given
                        }
                        InstructionData::CallIndirect { .. } => {
                            let given = self.slots_of(args.iter().copied());
                            self.called(&given);
                            given
                        }
                        data => {
                            for call in data.branch_destination(
                                &func.dfg.jump_tables,
                                &func.dfg.exception_tables,
                            ) {
                                let target = call.block(&func.dfg.value_lists);
                                let params = func.dfg.block_params(target);
                                let passed = call.args(&func.dfg.value_lists);
                                for (param, arg) in params.iter().zip(passed) {
                                    if let BlockArg::Value(value) = arg {
                                        let slots =
                                            self.slots_of([func.dfg.resolve_aliases(value)]);
                                        self.derive_value(*param, &slots);
                                    }
                                }
                            }
                            self.slots_of(args.iter().copied())
                        }
                    };
                    for &result in func.dfg.inst_results(inst) {
                        self.derive_value(result, &reached);
                    }
                }
            }
            if !self.changed {
                break;
            }
        }
        for slot in 0..self.pinned.len() {
            if self.pinned[slot] {
                let root = self.root(slot);
                self.pinned[root] = true;
            }
        }
    }

    /// What a callee does with the addresses given to it.
    fn callee_of(&self, func_ref: FuncRef) -> Callee {
        match &self.func.dfg.ext_funcs[func_ref].name {
            ExternalName::User(name) => {
                (self.callee)(self.func.params.user_named_funcs()[*name].index)
            }
            _ => Callee::Keeping,
        }
    }

    /// The slots given to a callee that keeps none: they meet, and when several are given, the
    /// callee may store one's address in another.
    fn called(&mut self, given: &[usize]) {
        self.unite(given);
        if given.len() > 1 {
            self.hold(given);
        }
    }

    /// The slots a value read at an address may point into: those of the address's group when
    /// it holds addresses.
    fn read(&self, address: Value) -> Vec<usize> {
        let slots = self.slots_of([address]);
        match slots.iter().any(|slot| self.holding[self.root(*slot)]) {
            true => slots,
            false => Vec::new(),
        }
    }

    /// A value stored at an address: a slot address stored in a slot's memory unites them in a
    /// group holding addresses, and one stored anywhere else is kept.
    fn stored(&mut self, value: Value, address: Value) {
        let kept = self.slots_of([value]);
        if kept.is_empty() {
            return;
        }
        let mut holding = self.slots_of([address]);
        if holding.is_empty() {
            self.pin(&kept);
            return;
        }
        holding.extend(kept);
        holding.sort_unstable();
        holding.dedup();
        self.unite(&holding);
        self.hold(&holding);
    }

    /// The program points at which each group, by its root, is needed: where an address
    /// derived from one of its slots is defined, used or live.
    fn needed(&self) -> Vec<Points> {
        let func = self.func;
        let blocks: Vec<_> = func.layout.blocks().collect();
        let mut first = Vec::with_capacity(blocks.len());
        let mut points = 0;
        for &block in &blocks {
            first.push(points);
            points += func.layout.block_insts(block).count();
        }
        let index: HashMap<_, _> = blocks.iter().enumerate().map(|(at, b)| (*b, at)).collect();
        let successors: Vec<Vec<usize>> = blocks
            .iter()
            .map(|block| match func.layout.last_inst(*block) {
                Some(last) => func.dfg.insts[last]
                    .branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables)
                    .iter()
                    .map(|call| index[&call.block(&func.dfg.value_lists)])
                    .collect(),
                None => Vec::new(),
            })
            .collect();
        let mut live_in: Vec<BTreeSet<Value>> = vec![BTreeSet::new(); blocks.len()];
        loop {
            let mut changed = false;
            for at in (0..blocks.len()).rev() {
                let mut live = live_out(&successors[at], &live_in);
                let insts: Vec<_> = func.layout.block_insts(blocks[at]).collect();
                for &inst in insts.iter().rev() {
                    for result in func.dfg.inst_results(inst) {
                        live.remove(result);
                    }
                    live.extend(self.used(inst));
                }
                for param in func.dfg.block_params(blocks[at]) {
                    live.remove(param);
                }
                if live != live_in[at] {
                    live_in[at] = live;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut needed: Vec<Points> = (0..self.parent.len())
            .map(|_| Points::new(points))
            .collect();
        for (at, &block) in blocks.iter().enumerate() {
            let mut live = live_out(&successors[at], &live_in);
            let insts: Vec<_> = func.layout.block_insts(block).collect();
            for (offset, &inst) in insts.iter().enumerate().rev() {
                let results = self.tracked(func.dfg.inst_results(inst).iter().copied());
                let used = self.used(inst);
                for value in live.iter().chain(&results).chain(&used) {
                    for slot in &self.derived[value] {
                        needed[self.root(*slot)].mark(first[at] + offset);
                    }
                }
                for result in &results {
                    live.remove(result);
                }
                live.extend(used);
            }
        }
        needed
    }

    /// The derived addresses among values.
    fn tracked(&self, values: impl Iterator<Item = Value>) -> BTreeSet<Value> {
        values
            .filter(|value| self.derived.contains_key(value))
            .collect()
    }

    /// The derived addresses an instruction reads, its branch arguments included.
    fn used(&self, inst: Inst) -> BTreeSet<Value> {
        let dfg = &self.func.dfg;
        self.tracked(
            dfg.inst_values(inst)
                .map(|value| dfg.resolve_aliases(value)),
        )
    }
}

/// The values live out of a block: those live into its successors.
fn live_out(successors: &[usize], live_in: &[BTreeSet<Value>]) -> BTreeSet<Value> {
    successors
        .iter()
        .flat_map(|successor| live_in[*successor].iter().copied())
        .collect()
}
