//! The function builder lowering uses: Cranelift's builder with the structured control flow,
//! memory accesses and stack storage that lowered code and support routines are written with.

use std::collections::HashMap;

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{
    self, Block, InstBuilder, MemFlagsData, StackSlotData, StackSlotKind, Type, Value, types,
};
use cranelift_frontend::{FunctionBuilder, Variable};

use super::stack::Site;

/// The type of addresses and sizes.
pub const PTR: Type = types::I64;

pub struct Fx<'f> {
    pub b: FunctionBuilder<'f>,
    /// The call sites lowering has classified, by call instruction.
    pub sites: Vec<(ir::Inst, Site)>,
    /// The addresses of the function's own function-typed parameters, with their positions.
    pub fn_params: HashMap<Value, usize>,
}

/// Flags of an access to memory lowered code owns: aligned and never trapping.
pub fn trusted() -> MemFlagsData {
    MemFlagsData::trusted()
}

/// Flags of an access that may be unaligned, such as a field of a bytes layout.
pub fn unaligned() -> MemFlagsData {
    MemFlagsData::new().with_notrap()
}

/// The flags of a scalar access: possibly unaligned, possibly big-endian.
pub fn access(unaligned_access: bool, big: bool) -> MemFlagsData {
    let flags = if unaligned_access {
        unaligned()
    } else {
        trusted()
    };
    if big {
        flags.with_endianness(ir::Endianness::Big)
    } else {
        flags
    }
}

impl<'f> Fx<'f> {
    pub fn new(b: FunctionBuilder<'f>) -> Fx<'f> {
        Fx {
            b,
            sites: Vec::new(),
            fn_params: HashMap::new(),
        }
    }

    /// Starts the function: its entry block holding its parameters, current and sealed.
    pub fn entry(&mut self) -> Vec<Value> {
        let entry = self.b.create_block();
        self.b.append_block_params_for_function_params(entry);
        self.b.switch_to_block(entry);
        self.b.seal_block(entry);
        self.b.block_params(entry).to_vec()
    }

    pub fn int(&mut self, ty: Type, value: i64) -> Value {
        self.b.ins().iconst(ty, value)
    }

    pub fn ptr(&mut self, value: u64) -> Value {
        self.b.ins().iconst(PTR, value as i64)
    }

    pub fn load(&mut self, ty: Type, addr: Value, offset: u64) -> Value {
        self.b.ins().load(ty, trusted(), addr, offset as i32)
    }

    pub fn store(&mut self, value: Value, addr: Value, offset: u64) {
        self.b.ins().store(trusted(), value, addr, offset as i32);
    }

    /// The address `offset` bytes past `addr`.
    pub fn offset(&mut self, addr: Value, offset: u64) -> Value {
        if offset == 0 {
            return addr;
        }
        let offset = self.ptr(offset);
        self.b.ins().iadd(addr, offset)
    }

    /// A new stack area of `size` bytes aligned to `align`, by its address.
    pub fn slot(&mut self, size: u64, align: u64) -> Value {
        let shift = align.max(1).trailing_zeros() as u8;
        let slot = self.b.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            size.max(1) as u32,
            shift,
        ));
        self.b.ins().stack_addr(PTR, slot, 0)
    }

    /// Copies `size` bytes between non-overlapping areas, word by word when both are aligned to
    /// 8, by bytes otherwise; used for sizes known small.
    pub fn copy_small(&mut self, dst: Value, src: Value, size: u64, align: u64) {
        let mut offset = 0;
        for (width, ty) in [
            (8, types::I64),
            (4, types::I32),
            (2, types::I16),
            (1, types::I8),
        ] {
            if align < width {
                continue;
            }
            while offset + width <= size {
                let value = self.b.ins().load(ty, trusted(), src, offset as i32);
                self.b.ins().store(trusted(), value, dst, offset as i32);
                offset += width;
            }
        }
        while offset < size {
            let value = self
                .b
                .ins()
                .load(types::I8, unaligned(), src, offset as i32);
            self.b.ins().store(unaligned(), value, dst, offset as i32);
            offset += 1;
        }
    }

    /// Runs `then` when `condition` is nonzero, continuing after it either way.
    pub fn when(&mut self, condition: Value, then: impl FnOnce(&mut Self)) {
        let body = self.b.create_block();
        let next = self.b.create_block();
        self.b.ins().brif(condition, body, &[], next, &[]);
        self.b.switch_to_block(body);
        self.b.seal_block(body);
        then(self);
        if !self.is_filled() {
            self.b.ins().jump(next, &[]);
        }
        self.b.switch_to_block(next);
        self.b.seal_block(next);
    }

    /// Runs `then` or `otherwise` as `condition` is nonzero or not, each yielding values of
    /// `results`' types, and gives the values the one run yields.
    pub fn choose(
        &mut self,
        condition: Value,
        results: &[Type],
        then: impl FnOnce(&mut Self) -> Vec<Value>,
        otherwise: impl FnOnce(&mut Self) -> Vec<Value>,
    ) -> Vec<Value> {
        self.choose_in(
            &mut (),
            condition,
            results,
            |_, fx| then(fx),
            |_, fx| otherwise(fx),
        )
    }

    /// `choose`, with a context both branches use.
    pub fn choose_in<C: ?Sized>(
        &mut self,
        cx: &mut C,
        condition: Value,
        results: &[Type],
        then: impl FnOnce(&mut C, &mut Self) -> Vec<Value>,
        otherwise: impl FnOnce(&mut C, &mut Self) -> Vec<Value>,
    ) -> Vec<Value> {
        let yes = self.b.create_block();
        let no = self.b.create_block();
        let next = self.b.create_block();
        for ty in results {
            self.b.append_block_param(next, *ty);
        }
        self.b.ins().brif(condition, yes, &[], no, &[]);
        self.b.switch_to_block(yes);
        self.b.seal_block(yes);
        let produced = then(cx, self);
        self.jump_unless_filled(next, &produced);
        self.b.switch_to_block(no);
        self.b.seal_block(no);
        let produced = otherwise(cx, self);
        self.jump_unless_filled(next, &produced);
        self.b.switch_to_block(next);
        self.b.seal_block(next);
        self.b.block_params(next).to_vec()
    }

    /// Ends the current block with a jump to `target` passing `args`, unless it already ends.
    pub fn jump_unless_filled(&mut self, target: Block, args: &[Value]) {
        if !self.is_filled() {
            let args: Vec<ir::BlockArg> = args.iter().map(|value| (*value).into()).collect();
            self.b.ins().jump(target, &args);
        }
    }

    /// A loop whose body runs while it returns a nonzero value, with `state` carried from each
    /// pass to the next; gives the state after the last pass.
    pub fn repeat(
        &mut self,
        state: &[Value],
        body: impl FnOnce(&mut Self, &[Value]) -> (Value, Vec<Value>),
    ) -> Vec<Value> {
        let types: Vec<Type> = state
            .iter()
            .map(|value| self.b.func.dfg.value_type(*value))
            .collect();
        let header = self.b.create_block();
        let exit = self.b.create_block();
        for ty in &types {
            self.b.append_block_param(header, *ty);
            self.b.append_block_param(exit, *ty);
        }
        let args: Vec<ir::BlockArg> = state.iter().map(|value| (*value).into()).collect();
        self.b.ins().jump(header, &args);
        self.b.switch_to_block(header);
        let current = self.b.block_params(header).to_vec();
        let (again, next) = body(self, &current);
        let next_args: Vec<ir::BlockArg> = next.iter().map(|value| (*value).into()).collect();
        self.b
            .ins()
            .brif(again, header, &next_args, exit, &next_args);
        self.b.seal_block(header);
        self.b.switch_to_block(exit);
        self.b.seal_block(exit);
        self.b.block_params(exit).to_vec()
    }

    /// A loop over `start..end`, `body` receiving each index.
    pub fn each(&mut self, start: Value, end: Value, body: impl FnOnce(&mut Self, Value)) {
        let header = self.b.create_block();
        let pass = self.b.create_block();
        let exit = self.b.create_block();
        self.b.append_block_param(header, PTR);
        self.b.ins().jump(header, &[start.into()]);
        self.b.switch_to_block(header);
        let index = self.b.block_params(header)[0];
        let more = self.b.ins().icmp(IntCC::UnsignedLessThan, index, end);
        self.b.ins().brif(more, pass, &[], exit, &[]);
        self.b.switch_to_block(pass);
        self.b.seal_block(pass);
        body(self, index);
        if !self.is_filled() {
            let one = self.ptr(1);
            let next = self.b.ins().iadd(index, one);
            self.b.ins().jump(header, &[next.into()]);
        }
        self.b.seal_block(header);
        self.b.switch_to_block(exit);
        self.b.seal_block(exit);
    }

    /// Whether the current block already ends in a terminator.
    pub fn is_filled(&self) -> bool {
        self.b.current_block().is_some_and(|block| {
            self.b
                .func
                .layout
                .last_inst(block)
                .is_some_and(|inst| self.b.func.dfg.insts[inst].opcode().is_terminator())
        })
    }

    /// A new block, not yet sealed.
    pub fn block(&mut self) -> Block {
        self.b.create_block()
    }

    pub fn var(&mut self, ty: Type, initial: Value) -> Variable {
        let var = self.b.declare_var(ty);
        self.b.def_var(var, initial);
        var
    }
}
