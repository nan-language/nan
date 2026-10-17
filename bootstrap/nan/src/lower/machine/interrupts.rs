//! Interrupt delivery on the machine targets: the vector seam, which saves the interrupted state
//! and preempts the interrupted kernel task when told to; the dispatcher, which calls the
//! interrupt fn bound to the fired vector by an `hw::vectors` contribution or by
//! `hw::IrqShared.install`; each CPU's local APIC and timer; and the stopping of the CPUs in a
//! kernel halt.

use cranelift_codegen::binemit::Reloc;
use cranelift_codegen::ir::{
    AbiParam, InstBuilder, MemFlagsData, Signature, Value, condcodes::IntCC, types,
};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module, ModuleReloc};

use super::super::body::Held;
use super::super::call::Invoked;
use super::super::fx::{Fx, PTR};
use super::super::runtime::tasks::{self, task};
use super::super::runtime::{report, state};
use super::super::{Instance, Lowering};
use super::SEAM_FRAME;
use super::cpus::cpu;
use super::stubs::Stub;
use crate::check::ty::Ty;
use crate::interp::{FnVal, Val};

/// The vectors whose exception pushes an error code the CPU leaves on the stack; every other
/// vector's stub pushes a zero so each interrupt frame has the same shape.
const ERROR_CODE_VECTORS: [u8; 10] = [8, 10, 11, 12, 13, 14, 17, 21, 29, 30];

/// The bytes of one vector's entry stub.
const VECTOR_STUB: u32 = 12;

/// The bytes of one vector's entry stub, which normalises the error code and pushes the vector
/// number, then jumps to the common seam's entry at `entry` from the stub's start at `at`.
fn vector_stub(vector: u8, at: u32, entry: u32) -> [u8; VECTOR_STUB as usize] {
    let mut bytes = [0x90u8; VECTOR_STUB as usize];
    let number = u32::from(vector).to_le_bytes();
    let (push_at, jump_at) = if ERROR_CODE_VECTORS.contains(&vector) {
        bytes[0] = 0x68;
        (1, 5)
    } else {
        bytes[0] = 0x6a;
        bytes[1] = 0x00;
        bytes[2] = 0x68;
        (3, 7)
    };
    bytes[push_at..push_at + 4].copy_from_slice(&number);
    bytes[jump_at] = 0xe9;
    let after = at + jump_at as u32 + 5;
    let rel = entry.wrapping_sub(after);
    bytes[jump_at + 1..jump_at + 5].copy_from_slice(&rel.to_le_bytes());
    bytes
}

/// The common seam. Its first entry, for vectors of `ist` 0, pushes the 15 general-purpose
/// registers and switches to the CPU's interrupt stack, whose top and lowest address its record
/// holds, unless already on it; its second entry, for the other vectors, whose stack the CPU
/// switched to, only pushes the registers. Both then save the extended state, with `xsave64`
/// when the CPU's record says AVX is enabled and `fxsave64` otherwise, and call the dispatcher
/// with the vector and the saved frame. When the dispatcher gives zero they restore the state
/// and return from the interrupt; otherwise they copy the frame and the extended state below the
/// interrupted stack pointer, call the preemption glue there, and restore from the copies once
/// it returns.
const SEAM_COMMON: &[u8] = &[
    0x41, 0x57, 0x41, 0x56, 0x41, 0x55, 0x41, 0x54, 0x41, 0x53, 0x41, 0x52, 0x41, 0x51, 0x41, 0x50,
    0x57, 0x56, 0x55, 0x53, 0x52, 0x51, 0x50, 0xfc, 0x48, 0x89, 0xe3, 0x65, 0x48, 0x8b, 0x04, 0x25,
    0x10, 0x00, 0x00, 0x00, 0x65, 0x48, 0x3b, 0x24, 0x25, 0x18, 0x00, 0x00, 0x00, 0x72, 0x05, 0x48,
    0x39, 0xc4, 0x72, 0x20, 0x48, 0x89, 0xc4, 0xeb, 0x1b, 0x41, 0x57, 0x41, 0x56, 0x41, 0x55, 0x41,
    0x54, 0x41, 0x53, 0x41, 0x52, 0x41, 0x51, 0x41, 0x50, 0x57, 0x56, 0x55, 0x53, 0x52, 0x51, 0x50,
    0xfc, 0x48, 0x89, 0xe3, 0x48, 0x81, 0xec, 0x00, 0x04, 0x00, 0x00, 0x48, 0x83, 0xe4, 0xc0, 0x49,
    0x89, 0xe7, 0x65, 0x48, 0x83, 0x3c, 0x25, 0xe8, 0x00, 0x00, 0x00, 0x00, 0x74, 0x23, 0x48, 0x8d,
    0xbc, 0x24, 0x00, 0x02, 0x00, 0x00, 0x31, 0xc0, 0xb9, 0x08, 0x00, 0x00, 0x00, 0xf3, 0x48, 0xab,
    0xb8, 0xff, 0xff, 0xff, 0xff, 0xba, 0xff, 0xff, 0xff, 0xff, 0x48, 0x0f, 0xae, 0x24, 0x24, 0xeb,
    0x05, 0x48, 0x0f, 0xae, 0x04, 0x24, 0x48, 0x8b, 0x7b, 0x78, 0x48, 0x89, 0xde, 0xe8, 0x00, 0x00,
    0x00, 0x00, 0x48, 0x85, 0xc0, 0x75, 0x45, 0x4c, 0x89, 0xfc, 0x65, 0x48, 0x83, 0x3c, 0x25, 0xe8,
    0x00, 0x00, 0x00, 0x00, 0x74, 0x11, 0xb8, 0xff, 0xff, 0xff, 0xff, 0xba, 0xff, 0xff, 0xff, 0xff,
    0x48, 0x0f, 0xae, 0x2c, 0x24, 0xeb, 0x05, 0x48, 0x0f, 0xae, 0x0c, 0x24, 0x48, 0x89, 0xdc, 0x58,
    0x59, 0x5a, 0x5b, 0x5d, 0x5e, 0x5f, 0x41, 0x58, 0x41, 0x59, 0x41, 0x5a, 0x41, 0x5b, 0x41, 0x5c,
    0x41, 0x5d, 0x41, 0x5e, 0x41, 0x5f, 0x48, 0x83, 0xc4, 0x10, 0x48, 0xcf, 0x48, 0x8b, 0xbb, 0xa0,
    0x00, 0x00, 0x00, 0x48, 0x81, 0xef, 0xb0, 0x00, 0x00, 0x00, 0x48, 0x83, 0xe7, 0xf0, 0x48, 0x89,
    0xde, 0x48, 0x89, 0xfb, 0xb9, 0x16, 0x00, 0x00, 0x00, 0xf3, 0x48, 0xa5, 0x48, 0x8d, 0xbb, 0x00,
    0xfc, 0xff, 0xff, 0x48, 0x83, 0xe7, 0xc0, 0x4c, 0x89, 0xfe, 0x49, 0x89, 0xff, 0xb9, 0x80, 0x00,
    0x00, 0x00, 0xf3, 0x48, 0xa5, 0x4c, 0x89, 0xfc, 0xe8, 0x00, 0x00, 0x00, 0x00, 0xe9, 0x75, 0xff,
    0xff, 0xff,
];

/// The offsets within `SEAM_COMMON` of its entry for vectors on another stack, and of the
/// displacements of its calls of the dispatcher and the preemption glue.
const SEAM_STAY: u32 = 0x39;
const SEAM_DISPATCH: u32 = 0x9e;
const SEAM_PREEMPT: u32 = 0x129;

// The common seam's code reads the CPU record and the saved frame at these offsets.
const _: () = assert!(cpu::STACK_TOP == 0x10 && cpu::STACK_BASE == 0x18 && cpu::AVX == 0xe8);
const _: () = assert!(frame::CODE == 0x80 && frame::RSP == 0xa0);

/// The offsets in the frame the seam saves of what its readers read: after the 15 saved
/// registers and the vector, the error code, then the interrupted instruction pointer, flags and
/// stack pointer.
pub mod frame {
    pub const CODE: u64 = 128;
    pub const RIP: u64 = 136;
    pub const RFLAGS: u64 = 152;
    pub const RSP: u64 = 160;
}

/// The vector of page faults, whose trap frame holds the faulting address.
const PAGE_FAULT: i64 = 14;

/// The interrupt flag of the saved flags.
const INTERRUPTS_ENABLED: i64 = 0x200;

/// The size of an interrupt descriptor table, and the type and privilege level of its gates.
pub const IDT_BYTES: u64 = 256 * 16;
const GATE: i64 = 0x8E00;

/// The local APIC, in the memory the entry root maps uncached, and the registers the kernel
/// programs: its id, the spurious-interrupt vector, the end-of-interrupt register, the
/// interrupt command register's two halves, the timer's local vector table entry, its divide
/// configuration and its initial count.
const APIC: u64 = 0xFEE0_0000;
const APIC_ID: u64 = 0x20;
const APIC_SPURIOUS: u64 = 0xF0;
const APIC_EOI: u64 = 0xB0;
const APIC_COMMAND: u64 = 0x300;
const APIC_DESTINATION: u64 = 0x310;
const APIC_LVT_TIMER: u64 = 0x320;
const APIC_TIMER_DIVIDE: u64 = 0x3E0;
const APIC_TIMER_COUNT: u64 = 0x380;

/// The interrupt command register's delivery-pending bit, and the commands the kernel sends: an
/// INIT and a startup IPI to one CPU, and a non-maskable interrupt to every other CPU.
const IPI_PENDING: i64 = 0x1000;
pub const IPI_INIT: u64 = 0x4500;
pub const IPI_STARTUP: u64 = 0x4600;
const IPI_NMI_OTHERS: u64 = 0xC_4400;

/// The local APIC's software-enable bit in its spurious-interrupt register, the divide
/// configuration slowing its timer's clock by 16, and the timer mode firing periodically.
const APIC_ENABLED: u64 = 0x100;
const DIVIDE_BY_16: u64 = 0x3;
const PERIODIC: u64 = 0x2_0000;

/// The interrupt vector each CPU's local APIC timer fires, periodically, the initial count that
/// paces it on the emulator, and the vector of the local APIC's spurious interrupts.
const TIMER_VECTOR: i64 = 0x20;
const TIMER_COUNT: u64 = 0x10_0000;
const SPURIOUS_VECTOR: i64 = 0xFF;

/// The vector of non-maskable interrupts, and those whose interrupt fn's return halts the
/// kernel.
const NMI_VECTOR: i64 = 2;
const ABORT_VECTORS: [i64; 2] = [8, 18];

/// The stack the dispatcher and its glue use beside the interrupt fns they call, a halt report
/// included.
const DISPATCH_RESERVE: u64 = 16_384;

/// A vector's binding by `hw::IrqShared.install`: the record of its device, zero while unbound,
/// then the number of CPUs running the device's glue for the vector.
pub mod binding {
    pub const RUNNING: u64 = 8;
    pub const SIZE: u64 = 16;
}

/// The offsets of a device record's fields, which `hw::IrqShared.install` fills: the lock word,
/// the vector, the glue the dispatcher calls with the record, the interrupt fn as a function
/// value, and the device from `VALUE` on, at its alignment.
pub mod device {
    pub const LOCK: u64 = 0;
    pub const VECTOR: u64 = 8;
    pub const GLUE: u64 = 16;
    pub const HANDLER: u64 = 24;
    pub const VALUE: u64 = 40;
}

/// An interrupt vector the final build binds by contribution: its number, its `ist`, the class
/// its handler's variant names and the interrupt fn bound to it.
struct Bound {
    vector: i64,
    ist: u8,
    class: &'static str,
    handler: Instance,
}

impl Lowering<'_, '_> {
    /// The interrupt vectors the final build binds, in vector order: each entry of the
    /// `hw::vectors` table whose handler holds an interrupt fn.
    fn bound_vectors(&mut self) -> Vec<Bound> {
        let Some(registry) = self.ck.db.std_item("hw", "vectors") else {
            return Vec::new();
        };
        let classes = ["Irq", "Trap", "TrapWithCode", "PageFault"];
        let mut bound = Vec::new();
        for value in self.table_entries(registry) {
            let Val::Struct(_, fields) = &value.value else {
                continue;
            };
            let (Val::Int(vector), Val::Int(ist)) = (&fields[0], &fields[1]) else {
                continue;
            };
            let Val::Variant(_, variant, payload) = &fields[2] else {
                continue;
            };
            let Some(Val::Fn(function)) = payload.first() else {
                continue;
            };
            let FnVal::Function(fn_ref) = function.as_ref() else {
                continue;
            };
            bound.push(Bound {
                vector: *vector as i64,
                ist: *ist as u8,
                class: classes[*variant as usize],
                handler: self.callable_instance(fn_ref, false),
            });
        }
        bound
    }

    /// The handler of the `hw::preempt` table's entry, a landing fn the platform package
    /// contributes.
    fn preempt_handler(&mut self) -> Option<Instance> {
        let registry = self.ck.db.std_item("hw", "preempt")?;
        let value = self.table_entries(registry).into_iter().next()?;
        let Val::Struct(_, fields) = &value.value else {
            return None;
        };
        let Some(Val::Fn(function)) = fields.get(1) else {
            return None;
        };
        let FnVal::Function(fn_ref) = function.as_ref() else {
            return None;
        };
        Some(self.callable_instance(fn_ref, false))
    }

    /// Sets up interrupt delivery for a kernel's executor: builds the interrupt descriptor table,
    /// masks the legacy interrupt controller, and gives the glue each CPU runs to take
    /// interrupts: it draws its interrupt stacks, loads the table and starts its local APIC and
    /// timer. Maskable interrupts stay disabled until the executor enables them.
    pub fn interrupt_setup(&mut self, fx: &mut Fx) -> FuncId {
        let idt = self.reserved(fx, "_NSidt", IDT_BYTES, 16);
        self.fill_idt(fx, idt);
        let masked = fx.ptr(0xFF);
        for port in [0x21, 0xA1] {
            let port = fx.ptr(port);
            self.port_out(fx, 1, port, masked);
        }
        let mut stacks: Vec<u8> = self.ists().into_iter().filter(|ist| *ist != 0).collect();
        stacks.sort_unstable();
        stacks.dedup();
        self.cpu_interrupts(&stacks)
    }

    /// Writes an interrupt descriptor table at `idt` whose gate n is an interrupt gate of
    /// privilege level 0 entering the vector seam's stub for n, with the `ist` of n's binding.
    pub fn fill_idt(&mut self, fx: &mut Fx, idt: Value) {
        let seam = self.seam();
        let seam = self.func_ref(fx, seam);
        let base = fx.b.ins().func_addr(PTR, seam);
        let zero = fx.ptr(0);
        let count = fx.ptr(256);
        fx.each(zero, count, |fx, vector| {
            let gate = fx.b.ins().imul_imm_s(vector, 16);
            let gate = fx.b.ins().iadd(idt, gate);
            let handler = fx.b.ins().imul_imm_s(vector, i64::from(VECTOR_STUB));
            let handler = fx.b.ins().iadd(base, handler);
            let low = fx.b.ins().ireduce(types::I16, handler);
            fx.b.ins()
                .store(super::super::fx::unaligned(), low, gate, 0);
            let selector = fx.int(types::I16, 0x08);
            fx.b.ins()
                .store(super::super::fx::unaligned(), selector, gate, 2);
            let attributes = fx.int(types::I16, GATE);
            fx.b.ins()
                .store(super::super::fx::unaligned(), attributes, gate, 4);
            let mid = fx.b.ins().ushr_imm_s(handler, 16);
            let mid = fx.b.ins().ireduce(types::I16, mid);
            fx.b.ins()
                .store(super::super::fx::unaligned(), mid, gate, 6);
            let high = fx.b.ins().ushr_imm_s(handler, 32);
            let high = fx.b.ins().ireduce(types::I32, high);
            fx.b.ins()
                .store(super::super::fx::unaligned(), high, gate, 8);
            let reserved = fx.int(types::I32, 0);
            fx.b.ins()
                .store(super::super::fx::unaligned(), reserved, gate, 12);
        });
        for (vector, ist) in self.ists().into_iter().enumerate() {
            if ist != 0 {
                let attributes = fx.int(types::I16, GATE | i64::from(ist));
                fx.b.ins().store(
                    super::super::fx::unaligned(),
                    attributes,
                    idt,
                    (16 * vector + 4) as i32,
                );
            }
        }
    }

    /// The `ist` each vector's binding gives, 0 for an unbound vector.
    fn ists(&mut self) -> [u8; 256] {
        let mut ists = [0u8; 256];
        for entry in self.bound_vectors() {
            ists[entry.vector as usize] = entry.ist;
        }
        ists
    }

    /// The final build's vector seam, emitted once with its dispatcher and preemption glue.
    fn seam(&mut self) -> FuncId {
        if let Some(seam) = self.vector_seam {
            return seam;
        }
        let bound = self.bound_vectors();
        let dispatch = self.dispatcher(&bound);
        let preempt = self.preempt_glue();
        let ists = self.ists();
        let seam = self.vector_seam(dispatch, preempt, &ists);
        self.vector_seam = Some(seam);
        seam
    }

    /// The glue a CPU runs to take interrupts: it draws its interrupt stack and one for each of
    /// `stacks`, the nonzero `ist` values bindings use, loads the interrupt descriptor table and
    /// enables its local APIC, its timer firing `TIMER_VECTOR`.
    fn cpu_interrupts(&mut self, stacks: &[u8]) -> FuncId {
        let signature = Signature::new(super::super::CALL_CONV);
        let id = self
            .module
            .declare_function("_NScpu_interrupts", Linkage::Local, &signature)
            .expect("the CPU interrupt glue");
        let sizes = self.interrupt_stack_sizes();
        let stacks = stacks.to_vec();
        self.define(id, signature, super::super::lines::HIDDEN, |lo, fx| {
            fx.entry();
            let record = lo.running_cpu(fx);
            let sizes = lo.data_address(fx, sizes);
            let size = fx.load(PTR, sizes, 0);
            let stack = lo.interrupt_stack(fx, size);
            let top = fx.b.ins().iadd(stack, size);
            fx.store(top, record, cpu::STACK_TOP);
            fx.store(stack, record, cpu::STACK_BASE);
            for ist in &stacks {
                let size = fx.load(PTR, sizes, 8);
                let stack = lo.interrupt_stack(fx, size);
                let top = fx.b.ins().iadd(stack, size);
                lo.set_interrupt_stack(fx, record, *ist, top);
            }
            let idt = lo.reserved(fx, "_NSidt", IDT_BYTES, 16);
            lo.load_idt(fx, idt);
            lo.apic_write(fx, APIC_SPURIOUS, APIC_ENABLED | SPURIOUS_VECTOR as u64);
            lo.apic_write(fx, APIC_TIMER_DIVIDE, DIVIDE_BY_16);
            lo.apic_write(fx, APIC_LVT_TIMER, TIMER_VECTOR as u64 | PERIODIC);
            lo.apic_write(fx, APIC_TIMER_COUNT, TIMER_COUNT);
            fx.b.ins().return_(&[]);
        });
        id
    }

    /// The read-only sizes of the interrupt stacks, which `finish_interrupts` defines once every
    /// interrupt fn is lowered: that of `ist` 0, then that of each other `ist`.
    fn interrupt_stack_sizes(&mut self) -> DataId {
        let id = self
            .module
            .declare_data("_NSirq_stacks", Linkage::Local, false, false)
            .expect("the interrupt stack sizes");
        self.irq_stacks = Some(id);
        id
    }

    /// Defines the interrupt stack sizes from the largest `limits stack N` among the lowered
    /// interrupt fns: the stack of `ist` 0 holds two of them, one for an interrupt vector and
    /// one for an exception interrupting it, and two seam frames; every other stack holds one
    /// and one seam frame; each also holds what the dispatcher uses.
    pub fn finish_interrupts(&mut self) {
        let Some(id) = self.irq_stacks else {
            return;
        };
        let largest = self
            .instances
            .keys()
            .filter_map(|instance| match instance {
                Instance::Nir { item, .. } => self.ck.fn_sig(*item),
                _ => None,
            })
            .filter(|sig| sig.modifier == Some(crate::syntax::ast::FnModifier::Interrupt))
            .filter_map(|sig| sig.limits.stack)
            .max()
            .unwrap_or(0);
        let first = 2 * (largest + SEAM_FRAME) + DISPATCH_RESERVE;
        let other = largest + SEAM_FRAME + DISPATCH_RESERVE;
        let mut bytes = first.next_multiple_of(16).to_le_bytes().to_vec();
        bytes.extend_from_slice(&other.next_multiple_of(16).to_le_bytes());
        let mut description = DataDescription::new();
        description.define(bytes.into_boxed_slice());
        description.set_align(8);
        self.module
            .define_data(id, &description)
            .expect("the interrupt stack sizes");
    }

    /// Emits the vector seam: a stub per vector, each entered through its interrupt gate, then
    /// the common seam they jump to, which calls `dispatch` and, to preempt, `preempt`. Gives the
    /// seam's symbol, whose stub for vector n lies at `n * VECTOR_STUB` bytes.
    fn vector_seam(&mut self, dispatch: FuncId, preempt: FuncId, ists: &[u8; 256]) -> FuncId {
        let common = 256 * VECTOR_STUB;
        let mut bytes = Vec::with_capacity(common as usize + SEAM_COMMON.len());
        for vector in 0..=255u8 {
            let at = u32::from(vector) * VECTOR_STUB;
            let entry = match ists[vector as usize] {
                0 => common,
                _ => common + SEAM_STAY,
            };
            bytes.extend_from_slice(&vector_stub(vector, at, entry));
        }
        bytes.extend_from_slice(SEAM_COMMON);
        let seam = self
            .module
            .declare_function(
                "_NSseam",
                Linkage::Local,
                &Signature::new(super::super::CALL_CONV),
            )
            .expect("the vector seam");
        let call = |offset: u32, target: FuncId| ModuleReloc {
            offset: common + offset,
            kind: Reloc::X86PCRel4,
            name: target.into(),
            addend: -4,
        };
        let relocs = [call(SEAM_DISPATCH, dispatch), call(SEAM_PREEMPT, preempt)];
        self.module
            .define_function_bytes(seam, 16, &bytes, &relocs)
            .expect("the vector seam");
        seam
    }

    /// The glue the seam calls on a preempted task's stack: it calls the `hw::preempt` entry's
    /// landing fn, which returns once a later switch resumes the task.
    fn preempt_glue(&mut self) -> FuncId {
        let signature = Signature::new(super::super::CALL_CONV);
        let id = self
            .module
            .declare_function("_NSseam_preempt", Linkage::Local, &signature)
            .expect("the preemption glue");
        let preempt = self.preempt_handler();
        self.define(id, signature, super::super::lines::HIDDEN, |lo, fx| {
            fx.entry();
            if let Some(preempt) = &preempt {
                let abi = lo.abi(preempt);
                let id = lo.instance(preempt.clone());
                lo.invoke(fx, Invoked::Direct(id), &abi, &[]);
            }
            fx.b.ins().return_(&[]);
        });
        id
    }

    /// The interrupt dispatcher the vector seam calls with the fired vector and the saved frame;
    /// it gives nonzero when the seam is to preempt the interrupted task. It records both in the
    /// running CPU's record while it dispatches the vector, then restores those of the interrupt
    /// it nested in.
    fn dispatcher(&mut self, bound: &[Bound]) -> FuncId {
        let mut signature = Signature::new(super::super::CALL_CONV);
        signature.params.push(AbiParam::new(PTR));
        signature.params.push(AbiParam::new(PTR));
        signature.returns.push(AbiParam::new(PTR));
        let id = self
            .module
            .declare_function("_NSseam_dispatch", Linkage::Local, &signature)
            .expect("the dispatcher");
        let dispatch = self.dispatch_vector(bound, signature.clone());
        self.define(id, signature, super::super::lines::HIDDEN, |lo, fx| {
            let params = fx.entry();
            let record = lo.running_cpu(fx);
            let outer = [cpu::VECTOR, cpu::FRAME].map(|field| fx.load(PTR, record, field));
            fx.store(params[0], record, cpu::VECTOR);
            fx.store(params[1], record, cpu::FRAME);
            let dispatch = lo.func_ref(fx, dispatch);
            let call = fx.b.ins().call(dispatch, &params);
            let preempt = fx.b.inst_results(call)[0];
            fx.store(outer[0], record, cpu::VECTOR);
            fx.store(outer[1], record, cpu::FRAME);
            fx.b.ins().return_(&[preempt]);
        });
        id
    }

    /// Loads the interrupt descriptor table at `table` on the running CPU.
    pub fn load_idt(&mut self, fx: &mut Fx, table: Value) {
        let idtr = fx.slot(16, 8);
        let limit = fx.int(types::I16, IDT_BYTES as i64 - 1);
        fx.b.ins()
            .store(super::super::fx::unaligned(), limit, idtr, 0);
        fx.b.ins()
            .store(super::super::fx::unaligned(), table, idtr, 2);
        self.call_stub(fx, Stub::Lidt, &[idtr]);
    }

    /// An interrupt stack of `size` bytes drawn as a runtime record, trapping on exhaustion.
    fn interrupt_stack(&mut self, fx: &mut Fx, size: Value) -> Value {
        let stack = self.record_alloc(fx, size);
        let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, stack, 0);
        fx.when(missing, |fx| report::raise_kind(self, fx, "trap.exhausted"));
        stack
    }

    /// The location of the instruction the interrupt the running CPU serves interrupted, zero
    /// outside one.
    pub fn interrupted_location(&mut self, fx: &mut Fx) -> Value {
        let record = self.running_cpu(fx);
        let saved = fx.load(PTR, record, cpu::FRAME);
        let serving = fx.b.ins().icmp_imm_s(IntCC::NotEqual, saved, 0);
        fx.choose_in(
            self,
            serving,
            &[PTR],
            |lo, fx| {
                let rip = fx.load(PTR, saved, frame::RIP);
                vec![report::location_at(lo, fx, rip)]
            },
            |_, fx| vec![fx.ptr(0)],
        )[0]
    }

    /// Dispatches a fired vector with its saved frame: calls the interrupt fn the final build
    /// binds to the vector, an interrupt vector's with a new `hw::IrqContext` and an exception
    /// vector's with the `hw::TrapFrame` of the interrupted code, or the one an `hw::IrqShared`
    /// binds. When the fn gives `hw::IrqExit.Reschedule` while a kernel task ran with maskable
    /// interrupts enabled and the anchor is installed, the task is preempted. After an interrupt
    /// fn for vector 8 or 18 returns, and on an unbound vector, the kernel halts at the
    /// interrupted instruction.
    fn dispatch_vector(&mut self, bound: &[Bound], signature: Signature) -> FuncId {
        let id = self
            .module
            .declare_function("_NSseam_dispatch_vector", Linkage::Local, &signature)
            .expect("the vector dispatch");
        let preempting = self.preempt_handler().is_some();
        self.define(id, signature, super::super::lines::HIDDEN, |lo, fx| {
            let params = fx.entry();
            let (vector, saved) = (params[0], params[1]);
            let nmi = fx.b.ins().icmp_imm_s(IntCC::Equal, vector, NMI_VECTOR);
            fx.when(nmi, |fx| lo.stop_if_halting(fx));
            let zero = fx.ptr(0);
            for entry in bound {
                let this = fx.b.ins().icmp_imm_s(IntCC::Equal, vector, entry.vector);
                fx.when(this, |fx| {
                    let reschedule = lo.dispatch_bound(fx, entry, saved);
                    if ABORT_VECTORS.contains(&entry.vector) {
                        let rip = fx.load(PTR, saved, frame::RIP);
                        let location = report::location_at(lo, fx, rip);
                        report::halt(lo, fx, "abort-exception", location, None);
                        return;
                    }
                    let preempt = lo.preempts(fx, reschedule, saved, preempting);
                    fx.b.ins().return_(&[preempt]);
                });
            }
            let installable =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, vector, 32);
            fx.when(installable, |fx| {
                let slot = lo.irq_binding(fx, vector);
                let running = fx.offset(slot, binding::RUNNING);
                let one = fx.ptr(1);
                super::super::runtime::fetch_add(fx, running, one);
                let device = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), slot);
                let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, device, 0);
                let reschedule = fx.choose(
                    present,
                    &[types::I8],
                    |fx| {
                        let glue = fx.load(PTR, device, device::GLUE);
                        let mut call = Signature::new(super::super::CALL_CONV);
                        call.params.push(AbiParam::new(PTR));
                        call.returns.push(AbiParam::new(types::I8));
                        let call = fx.b.import_signature(call);
                        let inst = fx.b.ins().call_indirect(call, glue, &[device]);
                        vec![fx.b.inst_results(inst)[0]]
                    },
                    |fx| vec![fx.int(types::I8, 0)],
                )[0];
                let minus = fx.int(PTR, -1);
                super::super::runtime::fetch_add(fx, running, minus);
                fx.when(present, |fx| {
                    let preempt = lo.preempts(fx, reschedule, saved, preempting);
                    fx.b.ins().return_(&[preempt]);
                });
            });
            let timer = fx.b.ins().icmp_imm_s(IntCC::Equal, vector, TIMER_VECTOR);
            fx.when(timer, |fx| {
                lo.apic_eoi(fx);
                fx.b.ins().return_(&[zero]);
            });
            let spurious = fx.b.ins().icmp_imm_s(IntCC::Equal, vector, SPURIOUS_VECTOR);
            fx.when(spurious, |fx| {
                fx.b.ins().return_(&[zero]);
            });
            let rip = fx.load(PTR, saved, frame::RIP);
            let location = report::location_at(lo, fx, rip);
            report::halt(lo, fx, "unbound-vector", location, Some(("vector", vector)));
        });
        id
    }

    /// Calls the interrupt fn a contribution binds to the fired vector: an `Irq` handler with a
    /// new `hw::IrqContext`, an exception handler with an `hw::TrapFrame` naming the saved frame
    /// and, for a page fault, holding the faulting address.
    /// Gives whether its `hw::IrqExit` is `Reschedule`.
    fn dispatch_bound(&mut self, fx: &mut Fx, entry: &Bound, saved: Value) -> Value {
        let abi = self.abi(&entry.handler);
        let id = self.instance(entry.handler.clone());
        let arg = match entry.class {
            "Irq" => Held::Empty,
            _ => {
                let ty = abi.params[0].0.ty().clone();
                let layout = self.layout(&ty);
                let trap_frame = fx.slot(layout.size, layout.align);
                let at = self.layouts.field(self.ck, &ty, None, "saved").offset;
                fx.store(saved, trap_frame, at);
                let fault = match entry.vector {
                    PAGE_FAULT => self.call_stub(fx, Stub::ReadCr2, &[])[0],
                    _ => fx.ptr(0),
                };
                let at = self.layouts.field(self.ck, &ty, None, "fault").offset;
                fx.store(fault, trap_frame, at);
                Held::Address(trap_frame)
            }
        };
        let called = self.invoke(fx, Invoked::Direct(id), &abi, &[arg]);
        let exit_ty = abi.result.clone();
        self.reschedules(fx, &exit_ty, called.result)
    }

    /// Whether an `hw::IrqExit` an interrupt fn gave is `Reschedule`.
    pub fn reschedules(&mut self, fx: &mut Fx, exit_ty: &Ty, exit: Held) -> Value {
        let exit = match exit {
            Held::Address(address) => address,
            other => {
                let layout = self.layout(exit_ty);
                let slot = fx.slot(layout.size.max(1), layout.align.max(1));
                self.store_held(fx, slot, other, exit_ty);
                slot
            }
        };
        let Ty::Item(item, _) = exit_ty else {
            panic!("an interrupt fn giving hw::IrqExit");
        };
        let reschedule = self
            .variant_names(*item)
            .iter()
            .position(|name| name == "Reschedule")
            .expect("the Reschedule variant");
        let index = self.variant_index(fx, exit_ty, exit);
        fx.b.ins()
            .icmp_imm_s(IntCC::Equal, index, reschedule as i64)
    }

    /// Whether the seam preempts the interrupted code: its interrupt fn rescheduled, the final
    /// build has a `hw::preempt` entry, the code is a kernel task that ran with maskable
    /// interrupts enabled on its own stack, and the anchor is installed. Gives a word.
    fn preempts(
        &mut self,
        fx: &mut Fx,
        reschedule: Value,
        saved: Value,
        preempting: bool,
    ) -> Value {
        if !preempting {
            return fx.ptr(0);
        }
        let flags = fx.load(PTR, saved, frame::RFLAGS);
        let enabled = fx.b.ins().band_imm_s(flags, INTERRUPTS_ENABLED);
        let enabled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, enabled, 0);
        let state_address = self.state(fx);
        let anchor = fx.load(PTR, state_address, state::ANCHOR);
        let installed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, anchor, 0);
        let record = tasks::current_task(self, fx);
        let running = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
        let on_stack = fx.choose(
            running,
            &[types::I8],
            |fx| {
                let rsp = fx.load(PTR, saved, frame::RSP);
                let base = fx.load(PTR, record, task::STACK);
                let size = fx.load(PTR, record, task::STACK_SIZE);
                let top = fx.b.ins().iadd(base, size);
                let above =
                    fx.b.ins()
                        .icmp(IntCC::UnsignedGreaterThanOrEqual, rsp, base);
                let below = fx.b.ins().icmp(IntCC::UnsignedLessThan, rsp, top);
                vec![fx.b.ins().band(above, below)]
            },
            |fx| vec![fx.int(types::I8, 0)],
        )[0];
        let preempting = fx.b.ins().band(enabled, installed);
        let preempting = fx.b.ins().band(preempting, on_stack);
        let preempting = fx.b.ins().band(preempting, reschedule);
        fx.b.ins().uextend(PTR, preempting)
    }

    /// The binding of a vector by `hw::IrqShared.install`, in the image's table of them.
    pub fn irq_binding(&mut self, fx: &mut Fx, vector: Value) -> Value {
        let table = self.reserved(fx, "_NSirq_bindings", 256 * binding::SIZE, 8);
        let offset = fx.b.ins().imul_imm_s(vector, binding::SIZE as i64);
        fx.b.ins().iadd(table, offset)
    }

    /// The vectors the final build binds by contribution.
    pub fn statically_bound(&mut self) -> Vec<i64> {
        self.bound_vectors()
            .iter()
            .map(|entry| entry.vector)
            .collect()
    }

    /// The vector the running CPU's interrupt fn serves.
    pub fn current_vector(&mut self, fx: &mut Fx) -> Value {
        let record = self.running_cpu(fx);
        let vector = fx.load(PTR, record, cpu::VECTOR);
        fx.b.ins().ireduce(types::I8, vector)
    }

    /// Writes a 32-bit register of the running CPU's local APIC.
    fn apic_write(&mut self, fx: &mut Fx, register: u64, value: u64) {
        let at = fx.ptr(APIC + register);
        let value = fx.ptr(value);
        self.mmio_write(fx, 4, at, value);
    }

    /// Writes the local APIC's end-of-interrupt register, ending the interrupt in service.
    pub fn apic_eoi(&mut self, fx: &mut Fx) {
        self.apic_write(fx, APIC_EOI, 0);
    }

    /// The running CPU's local APIC id.
    pub fn local_apic_id(&mut self, fx: &mut Fx) -> Value {
        let at = fx.ptr(APIC + APIC_ID);
        let id = self.mmio_read(fx, 4, at);
        fx.b.ins().ushr_imm_s(id, 24)
    }

    /// Sends an IPI of a command to the CPU whose local APIC has the id `destination` and waits
    /// until it is delivered.
    pub fn send_ipi(&mut self, fx: &mut Fx, destination: Value, command: u64) {
        let target = fx.b.ins().ishl_imm_s(destination, 24);
        let at = fx.ptr(APIC + APIC_DESTINATION);
        self.mmio_write(fx, 4, at, target);
        self.apic_write(fx, APIC_COMMAND, command);
        let at = fx.ptr(APIC + APIC_COMMAND);
        fx.repeat(&[], |fx, _| {
            let status = self.mmio_read(fx, 4, at);
            let pending = fx.b.ins().band_imm_s(status, IPI_PENDING);
            let pending = fx.b.ins().icmp_imm_s(IntCC::NotEqual, pending, 0);
            fx.when(pending, |fx| {
                self.call_stub(fx, Stub::Pause, &[]);
            });
            (pending, Vec::new())
        });
    }

    /// The first step of a kernel halt, with maskable interrupts disabled so that nothing else
    /// runs on the halting CPU: the first CPU to begin one sends every other started CPU a
    /// non-maskable interrupt and waits until each has stopped; a CPU beginning a halt after it
    /// stops instead.
    pub fn stop_other_cpus(&mut self, fx: &mut Fx) {
        self.call_stub(fx, Stub::Cli, &[]);
        let state_address = self.state(fx);
        let flag = fx.offset(state_address, state::HALTING);
        let zero = fx.ptr(0);
        let one = fx.ptr(1);
        let seen =
            fx.b.ins()
                .atomic_cas(MemFlagsData::trusted(), flag, zero, one);
        let later = fx.b.ins().icmp_imm_s(IntCC::NotEqual, seen, 0);
        fx.when(later, |fx| {
            self.call_stub(fx, Stub::Halt, &[]);
        });
        let started = fx.load(PTR, state_address, state::STARTED_CPUS);
        let others = fx.b.ins().iadd_imm_s(started, -1);
        let running = fx.b.ins().icmp_imm_s(IntCC::SignedGreaterThan, others, 0);
        fx.when(running, |fx| {
            let all = fx.ptr(0);
            self.send_ipi(fx, all, IPI_NMI_OTHERS);
            let stopped = fx.offset(state_address, state::STOPPED_CPUS);
            fx.repeat(&[], |fx, _| {
                let count =
                    fx.b.ins()
                        .atomic_load(PTR, MemFlagsData::trusted(), stopped);
                let waiting = fx.b.ins().icmp(IntCC::SignedLessThan, count, others);
                fx.when(waiting, |fx| {
                    self.call_stub(fx, Stub::Pause, &[]);
                });
                (waiting, Vec::new())
            });
        });
    }

    /// On a non-maskable interrupt during a kernel halt, stops the running CPU, counting it.
    fn stop_if_halting(&mut self, fx: &mut Fx) {
        let state_address = self.state(fx);
        let halting = fx.load(PTR, state_address, state::HALTING);
        let halting = fx.b.ins().icmp_imm_s(IntCC::NotEqual, halting, 0);
        fx.when(halting, |fx| {
            let stopped = fx.offset(state_address, state::STOPPED_CPUS);
            let one = fx.ptr(1);
            super::super::runtime::fetch_add(fx, stopped, one);
            self.call_stub(fx, Stub::Halt, &[]);
        });
    }
}
