//! Interrupt delivery on the machine targets: the x86_64 vector seam, which saves the interrupted
//! state and preempts the interrupted kernel task when told to; the dispatcher both
//! architectures' seams reach, which calls the interrupt fn bound to the fired vector by an
//! `hw::vectors` contribution or the devices `hw::IrqShared` installs on it; each CPU's local
//! APIC, in x2APIC mode when the boot CPU has it, and timer; and the stopping of the CPUs in a
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
use super::super::runtime::{Routine, report, state, stops};
use super::super::trap::{OPERAND_RECORD, Written, kind_number};
use super::super::{Instance, Lowering};
use super::SEAM_FRAME;
use super::aarch64;
use super::cpus::cpu;
use super::signal::SIGNAL_VECTOR;
use super::stubs::Stub;
use crate::check::ty::Ty;
use crate::interp::{FnVal, Val};
use crate::workspace::Arch;

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
/// switched to, only pushes the registers. Both then call the dispatcher with the vector and the
/// saved frame. When the dispatcher gives zero they restore the registers and return from the
/// interrupt; otherwise they copy the frame below the interrupted stack pointer, leaving
/// `RESERVED` bytes between them, call the preemption glue there with the copy and what the
/// dispatcher gave, and restore from the copy once it returns.
pub(super) const SEAM_COMMON: &[u8] = &[
    0x41, 0x57, 0x41, 0x56, 0x41, 0x55, 0x41, 0x54, 0x41, 0x53, 0x41, 0x52, 0x41, 0x51, 0x41, 0x50,
    0x57, 0x56, 0x55, 0x53, 0x52, 0x51, 0x50, 0xfc, 0x48, 0x89, 0xe3, 0x65, 0x48, 0x8b, 0x04, 0x25,
    0x10, 0x00, 0x00, 0x00, 0x65, 0x48, 0x3b, 0x24, 0x25, 0x18, 0x00, 0x00, 0x00, 0x72, 0x05, 0x48,
    0x39, 0xc4, 0x72, 0x20, 0x48, 0x89, 0xc4, 0xeb, 0x1b, 0x41, 0x57, 0x41, 0x56, 0x41, 0x55, 0x41,
    0x54, 0x41, 0x53, 0x41, 0x52, 0x41, 0x51, 0x41, 0x50, 0x57, 0x56, 0x55, 0x53, 0x52, 0x51, 0x50,
    0xfc, 0x48, 0x89, 0xe3, 0x48, 0x83, 0xe4, 0xf0, 0x48, 0x8b, 0x7b, 0x78, 0x48, 0x89, 0xde, 0xe8,
    0x00, 0x00, 0x00, 0x00, 0x48, 0x85, 0xc0, 0x75, 0x20, 0x48, 0x89, 0xdc, 0x58, 0x59, 0x5a, 0x5b,
    0x5d, 0x5e, 0x5f, 0x41, 0x58, 0x41, 0x59, 0x41, 0x5a, 0x41, 0x5b, 0x41, 0x5c, 0x41, 0x5d, 0x41,
    0x5e, 0x41, 0x5f, 0x48, 0x83, 0xc4, 0x10, 0x48, 0xcf, 0x48, 0x8b, 0xbb, 0xa0, 0x00, 0x00, 0x00,
    0x48, 0x81, 0xef, 0x30, 0x01, 0x00, 0x00, 0x48, 0x83, 0xe7, 0xf0, 0x48, 0x89, 0xde, 0x48, 0x89,
    0xfb, 0xb9, 0x16, 0x00, 0x00, 0x00, 0xf3, 0x48, 0xa5, 0x48, 0x89, 0xdc, 0x48, 0x89, 0xdf, 0x48,
    0x89, 0xc6, 0xe8, 0x00, 0x00, 0x00, 0x00, 0xeb, 0xb0,
];

/// The bytes both architectures' seams leave between a frame they copy for the preemption glue
/// and the interrupted stack pointer.
pub(super) const RESERVED: u64 = 128;

// The operands of an exception's trap fit in the reserved bytes.
const _: () = assert!(3 * OPERAND_RECORD <= RESERVED);

/// The offsets within `SEAM_COMMON` of its entry for vectors on another stack, and of the
/// displacements of its calls of the dispatcher and the preemption glue.
pub(super) const SEAM_STAY: u32 = 0x39;
pub(super) const SEAM_DISPATCH: u32 = 0x60;
pub(super) const SEAM_PREEMPT: u32 = 0xb3;

// The common seam's code reads the CPU record and the saved frame at these offsets, and copies
// frames of this size.
const _: () = assert!(cpu::STACK_TOP == 0x10 && cpu::STACK_BASE == 0x18);
const _: () = assert!(frame::VECTOR == 0x78 && frame::CODE == 0x80 && frame::RSP == 0xa0);
const _: () = assert!(frame::SIZE == 0x16 * 8 && frame::SIZE + RESERVED == 0x130);

/// The offsets in the frame the seam saves of what its readers read: the saved registers the
/// first four arguments of a call and the frame pointer pass in, the vector after the 15 saved
/// registers, the error code, then the interrupted instruction pointer, flags and stack
/// pointer; and the frame's size.
mod frame {
    pub const RCX: u64 = 8;
    pub const RDX: u64 = 16;
    pub const RBP: u64 = 32;
    pub const RSI: u64 = 40;
    pub const RDI: u64 = 48;
    pub const VECTOR: u64 = 120;
    pub const CODE: u64 = 128;
    pub const RIP: u64 = 136;
    pub const RFLAGS: u64 = 152;
    pub const RSP: u64 = 160;
    pub const SIZE: u64 = 176;
}

/// The offsets in the frame an architecture's vector seam saves of what the dispatcher,
/// `hw::TrapFrame`, halt reports and stops read: the error code or syndrome, the interrupted
/// instruction's address, the processor state holding whether maskable interrupts were enabled,
/// the interrupted stack pointer, frame pointer and link register where the architecture has
/// one, and those of the registers passing the first four arguments of a call; and the frame's
/// size.
#[derive(Clone, Copy)]
pub struct SavedFrame {
    pub code: u64,
    pub ip: u64,
    pub state: u64,
    pub sp: u64,
    pub fp: u64,
    pub link: Option<u64>,
    pub args: [u64; 4],
    pub size: u64,
}

const X64_FRAME: SavedFrame = SavedFrame {
    code: frame::CODE,
    ip: frame::RIP,
    state: frame::RFLAGS,
    sp: frame::RSP,
    fp: frame::RBP,
    link: None,
    args: [frame::RDI, frame::RSI, frame::RDX, frame::RCX],
    size: frame::SIZE,
};

/// What the dispatcher knows of an architecture's vectors: the vector of the interrupt a halting
/// CPU stops the others with, where the dispatcher receives it as a vector; the vector the timer
/// pacing the kernel fires; the vector of the interrupt controller's spurious interrupts, where
/// it delivers them as one; the vectors whose interrupt fn's return halts the kernel; the vector
/// of page faults, whose trap frame holds the faulting address; where the interrupt controller
/// numbers interrupts past the last vector, the end of the vectors an `hw::IrqShared` installs
/// on; whether it delivers LPIs, which the dispatcher receives as their identifiers; and the end
/// of the exception vectors, numbered from 0.
pub(super) struct Vectors {
    pub(super) stop: Option<i64>,
    pub(super) timer: i64,
    pub(super) spurious: Option<i64>,
    pub(super) aborts: &'static [i64],
    pub(super) page_fault: i64,
    pub(super) installable_end: Option<i64>,
    pub(super) lpis: bool,
    pub(super) exceptions: i64,
}

const X64_VECTORS: Vectors = Vectors {
    stop: Some(NMI_VECTOR),
    timer: TIMER_VECTOR,
    spurious: Some(SPURIOUS_VECTOR),
    aborts: &ABORT_VECTORS,
    page_fault: 14,
    installable_end: None,
    lpis: false,
    exceptions: 32,
};

/// The interrupt flag of the saved flags.
const INTERRUPTS_ENABLED: i64 = 0x200;

/// The size of an interrupt descriptor table, and the type and privilege level of its gates.
pub const IDT_BYTES: u64 = 256 * 16;
const GATE: i64 = 0x8E00;

/// The local APIC, in the memory the entry root maps uncached, and the registers the kernel
/// programs: its id, the spurious-interrupt vector, the end-of-interrupt register, the
/// interrupt command register's two halves, the timer's local vector table entry, its divide
/// configuration, its initial count and its current count. In x2APIC mode each register is the
/// model-specific register `X2APIC_MSRS` plus its offset over 16, the interrupt command
/// register one of 64 bits.
const APIC: u64 = 0xFEE0_0000;
const X2APIC_MSRS: u64 = 0x800;
const APIC_ID: u64 = 0x20;
const APIC_SPURIOUS: u64 = 0xF0;
const APIC_EOI: u64 = 0xB0;
const APIC_COMMAND: u64 = 0x300;
const APIC_DESTINATION: u64 = 0x310;
const APIC_LVT_TIMER: u64 = 0x320;
const APIC_TIMER_DIVIDE: u64 = 0x3E0;
const APIC_TIMER_COUNT: u64 = 0x380;
const APIC_TIMER_CURRENT: u64 = 0x390;

/// The interrupt command register's delivery-pending bit, and the commands the kernel sends: an
/// INIT and a startup IPI to one CPU, and a non-maskable interrupt to every other CPU.
const IPI_PENDING: i64 = 0x1000;
pub const IPI_INIT: u64 = 0x4500;
pub const IPI_STARTUP: u64 = 0x4600;
const IPI_NMI_OTHERS: u64 = 0xC_4400;

/// The model-specific register holding the local APIC's base and mode, the bits enabling it in
/// x2APIC mode, and the `cpuid` leaf 1 bit of `ecx` saying the CPU has that mode.
const APIC_BASE_MSR: u32 = 0x1B;
const X2APIC_MODE: i64 = 0xC00;
const X2APIC_FEATURE: i64 = 1 << 21;

/// The local APIC's software-enable bit in its spurious-interrupt register, the divide
/// configuration slowing its timer's clock by 16, and the mask bit of a local vector table
/// entry.
const APIC_ENABLED: u64 = 0x100;
const DIVIDE_BY_16: u64 = 0x3;
const MASKED: u64 = 0x1_0000;

/// The interrupt vector each CPU's local APIC timer fires, one-shot, the nanoseconds of the
/// clock over which a CPU counts its timer's ticks, and the vector of the local APIC's spurious
/// interrupts.
const TIMER_VECTOR: i64 = 0x20;
const TIMER_MEASURE: u64 = 1_000_000;
const SPURIOUS_VECTOR: i64 = 0xFF;

/// The vector of non-maskable interrupts, and those whose interrupt fn's return halts the
/// kernel.
const NMI_VECTOR: i64 = 2;
const ABORT_VECTORS: [i64; 2] = [8, 18];

/// What the dispatcher gives the seam to make the interrupted task trap; it gives 1 to preempt
/// the task.
const EXCEPTION_TRAP: i64 = 2;

/// The stack the dispatcher and its glue use beside the interrupt fns they call, a halt report
/// included.
const DISPATCH_RESERVE: u64 = 16_384;

/// An interrupt vector the final build binds by contribution: its number, its `ist`, the class
/// its handler's variant names and the interrupt fn bound to it.
pub(super) struct Bound {
    pub(super) vector: i64,
    ist: u8,
    class: &'static str,
    handler: Instance,
}

impl Lowering<'_, '_> {
    /// The interrupt vectors the final build binds, in vector order: each entry of the
    /// `hw::vectors` table whose handler holds an interrupt fn.
    pub(super) fn bound_vectors(&mut self) -> Vec<Bound> {
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
    pub(super) fn preempt_handler(&mut self) -> Option<Instance> {
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

    /// The architecture of the machine target being lowered for.
    pub(super) fn machine_arch(&self) -> Arch {
        self.ck
            .db
            .target
            .arch()
            .expect("a machine target's architecture")
    }

    /// What the dispatcher knows of the target's vectors.
    pub(super) fn vectors(&self) -> Vectors {
        match self.machine_arch() {
            Arch::Amd64 => X64_VECTORS,
            Arch::Aarch64 => aarch64::VECTORS,
        }
    }

    /// The layout of the frame the target's vector seam saves.
    pub fn saved_frame(&self) -> SavedFrame {
        match self.machine_arch() {
            Arch::Amd64 => X64_FRAME,
            Arch::Aarch64 => aarch64::FRAME,
        }
    }

    /// Whether the code a saved frame interrupted ran with maskable interrupts enabled: the
    /// interrupt flag of the saved flags is set on x86_64, the IRQ mask of the saved processor
    /// state clear on AArch64.
    fn interrupts_were_enabled(&mut self, fx: &mut Fx, saved: Value) -> Value {
        let state = fx.load(PTR, saved, self.saved_frame().state);
        match self.machine_arch() {
            Arch::Amd64 => {
                let enabled = fx.b.ins().band_imm_s(state, INTERRUPTS_ENABLED);
                fx.b.ins().icmp_imm_s(IntCC::NotEqual, enabled, 0)
            }
            Arch::Aarch64 => {
                let masked = fx.b.ins().band_imm_s(state, aarch64::IRQ_MASKED);
                fx.b.ins().icmp_imm_s(IntCC::Equal, masked, 0)
            }
        }
    }

    /// The address a page fault faulted at: `cr2` on x86_64, the fault address register the
    /// AArch64 seam saved in the frame.
    fn fault_address(&mut self, fx: &mut Fx, saved: Value) -> Value {
        match self.machine_arch() {
            Arch::Amd64 => self.call_stub(fx, Stub::ReadCr2, &[])[0],
            Arch::Aarch64 => fx.load(PTR, saved, aarch64::frame::FAR),
        }
    }

    /// Sets up interrupt delivery for a kernel's executor and gives the glue each CPU runs to
    /// take interrupts. On x86_64 it builds the interrupt descriptor table, masks the legacy
    /// interrupt controller, and the glue draws the CPU's interrupt stacks, loads the table and
    /// starts its local APIC and timer. Maskable interrupts stay disabled until the executor
    /// enables them.
    pub fn interrupt_setup(&mut self, fx: &mut Fx) -> FuncId {
        if self.machine_arch() == Arch::Aarch64 {
            return self.aarch64_interrupt_setup(fx);
        }
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

    /// The glue a CPU runs to take interrupts: it puts its local APIC in the runtime's mode,
    /// draws its interrupt stack and one for each of `stacks`, the nonzero `ist` values bindings
    /// use, loads the interrupt descriptor table, enables its local APIC and starts its timer,
    /// firing `TIMER_VECTOR`.
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
            lo.enter_apic_mode(fx);
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
            let spurious = fx.ptr(APIC_ENABLED | SPURIOUS_VECTOR as u64);
            lo.apic_write(fx, APIC_SPURIOUS, spurious);
            lo.start_timer(fx);
            fx.b.ins().return_(&[]);
        });
        id
    }

    /// The read-only sizes of the interrupt stacks, which `finish_interrupts` defines once every
    /// interrupt fn is lowered: that of `ist` 0, then that of each other `ist`.
    pub(super) fn interrupt_stack_sizes(&mut self) -> DataId {
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

    /// The glue the seam calls on an interrupted task's stack with the copy of its saved frame
    /// and what the dispatcher gave: for `EXCEPTION_TRAP` it makes the frame resume in a trap of
    /// the task, and otherwise it calls the `hw::preempt` entry's landing fn, which returns once
    /// a later switch resumes the task.
    pub(super) fn preempt_glue(&mut self) -> FuncId {
        let mut signature = Signature::new(super::super::CALL_CONV);
        signature.params.push(AbiParam::new(PTR));
        signature.params.push(AbiParam::new(PTR));
        let id = self
            .module
            .declare_function("_NSseam_preempt", Linkage::Local, &signature)
            .expect("the preemption glue");
        let preempt = self.preempt_handler();
        self.define(id, signature, super::super::lines::HIDDEN, |lo, fx| {
            let params = fx.entry();
            let trap =
                fx.b.ins()
                    .icmp_imm_s(IntCC::Equal, params[1], EXCEPTION_TRAP);
            fx.when(trap, |fx| {
                lo.exception_trap(fx, params[0]);
                fx.b.ins().return_(&[]);
            });
            if let Some(preempt) = &preempt {
                let abi = lo.abi(preempt);
                let id = lo.instance(preempt.clone());
                lo.invoke(fx, Invoked::Direct(id), &abi, &[]);
            }
            fx.b.ins().return_(&[]);
        });
        id
    }

    /// Makes the interrupted task, whose saved frame the seam copied to `frame` on the task's
    /// stack, trap with `trap.exception` at the interrupted instruction: the operands, the
    /// vector, the error code or syndrome and, for a page fault, the faulting address, lie in
    /// the bytes the seam left above the copy, and the frame resumes in the trap routine as if
    /// the instruction called it.
    fn exception_trap(&mut self, fx: &mut Fx, frame: Value) {
        let layout = self.saved_frame();
        let code = fx.load(PTR, frame, layout.code);
        let vector = match self.machine_arch() {
            Arch::Amd64 => fx.load(PTR, frame, frame::VECTOR),
            Arch::Aarch64 => aarch64::exception_vector(fx, code),
        };
        let page_fault =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, vector, self.vectors().page_fault);
        let address = fx.choose_in(
            self,
            page_fault,
            &[PTR],
            |lo, fx| vec![lo.fault_address(fx, frame)],
            |_, fx| vec![fx.ptr(0)],
        )[0];
        let operands = fx.offset(frame, layout.size);
        let named = [("vector", vector), ("code", code), ("address", address)];
        for (index, (name, value)) in named.into_iter().enumerate() {
            let record = fx.offset(operands, index as u64 * OPERAND_RECORD);
            let text = self.bytes_data(name.as_bytes());
            let text = self.data_ref(fx, text);
            let text = fx.b.ins().symbol_value(PTR, text);
            fx.store(text, record, 0);
            let len = fx.ptr(name.len() as u64);
            fx.store(len, record, 8);
            fx.store(value, record, 16);
            let how = fx.ptr(Written::Unsigned as u64);
            fx.store(how, record, 24);
            let writer = fx.ptr(0);
            fx.store(writer, record, 32);
        }
        let two = fx.ptr(2);
        let three = fx.ptr(3);
        let count = fx.b.ins().select(page_fault, three, two);
        let kind = fx.ptr(kind_number("trap.exception") as u64);
        let caller = fx.ptr(0);
        for (at, value) in layout.args.into_iter().zip([kind, operands, count, caller]) {
            fx.store(value, frame, at);
        }
        let ip = fx.load(PTR, frame, layout.ip);
        let returned = fx.b.ins().iadd_imm_s(ip, 1);
        match layout.link {
            Some(link) => fx.store(returned, frame, link),
            None => {
                let top = fx.offset(frame, frame::VECTOR);
                fx.store(returned, top, 0);
                fx.store(top, frame, layout.sp);
            }
        }
        let raise = self.routine(Routine::TrapRaise);
        let raise = self.func_ref(fx, raise);
        let raise = fx.b.ins().func_addr(PTR, raise);
        fx.store(raise, frame, layout.ip);
    }

    /// The interrupt dispatcher the vector seam calls with what fired and the saved frame; it
    /// gives nonzero when the seam is to preempt the interrupted task. The x86_64 seam passes the
    /// fired vector, the AArch64 one the kind of exception, from which the dispatcher finds the
    /// vector. It records the vector and the frame in the running CPU's record while it
    /// dispatches the vector, then restores those of the interrupt it nested in.
    pub(super) fn dispatcher(&mut self, bound: &[Bound]) -> FuncId {
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
            let mut params = fx.entry();
            if lo.machine_arch() == Arch::Aarch64 {
                params[0] = lo.aarch64_vector(fx, params[0], params[1]);
            }
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
    pub(super) fn interrupt_stack(&mut self, fx: &mut Fx, size: Value) -> Value {
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
                let ip = fx.load(PTR, saved, lo.saved_frame().ip);
                vec![report::location_at(lo, fx, ip)]
            },
            |_, fx| vec![fx.ptr(0)],
        )[0]
    }

    /// Dispatches a fired vector with its saved frame: calls the interrupt fn the final build
    /// binds to the vector, an interrupt vector's with a new `hw::IrqContext` and an exception
    /// vector's with the `hw::TrapFrame` of the interrupted code, or the one an `hw::IrqShared`
    /// binds, or the devices installed on the vector or on the LPI. A timer interrupt first serves
    /// the kernel's clock, and reaches an interrupt fn on a pacing tick alone. The CPU signal
    /// first serves a shootdown on x86_64, and only ends unless bound. When a fn gives
    /// `hw::IrqExit.Reschedule` while a kernel task ran with maskable interrupts enabled and the
    /// anchor is installed, the task is preempted; a task asked to stop is preempted on any
    /// interrupt once it may stop, and only then. An unbound exception vector other than an abort
    /// vector, and an exception vector whose fn gives `hw::IrqExit.Abandon`, trap a kernel task
    /// that ran with maskable interrupts enabled on its own stack; an exception fn's `Abandon`
    /// elsewhere halts the kernel. An unbound timer interrupt only ends; after an interrupt fn
    /// for an abort vector returns, and on any other unbound vector, the kernel halts at the
    /// interrupted instruction.
    fn dispatch_vector(&mut self, bound: &[Bound], signature: Signature) -> FuncId {
        let id = self
            .module
            .declare_function("_NSseam_dispatch_vector", Linkage::Local, &signature)
            .expect("the vector dispatch");
        let preempting = self.preempt_handler().is_some();
        let vectors = self.vectors();
        self.define(id, signature, super::super::lines::HIDDEN, |lo, fx| {
            let params = fx.entry();
            let (vector, saved) = (params[0], params[1]);
            if let Some(stop) = vectors.stop {
                let stopping = fx.b.ins().icmp_imm_s(IntCC::Equal, vector, stop);
                fx.when(stopping, |fx| lo.stop_if_halting(fx));
            }
            let no = fx.int(types::I8, 0);
            lo.dispatch_signal(fx, bound, vector, saved, preempting);
            let timer = fx.b.ins().icmp_imm_s(IntCC::Equal, vector, vectors.timer);
            fx.when(timer, |fx| {
                let pacing = lo.timer_interrupt(fx);
                let early = fx.b.ins().bxor_imm_s(pacing, 1);
                fx.when(early, |fx| {
                    lo.end_of_interrupt(fx);
                    let preempt = lo.preempts(fx, no, saved, preempting);
                    fx.b.ins().return_(&[preempt]);
                });
            });
            for entry in bound {
                let this = fx.b.ins().icmp_imm_s(IntCC::Equal, vector, entry.vector);
                fx.when(this, |fx| {
                    let (reschedule, abandon) = lo.dispatch_bound(fx, entry, saved);
                    if vectors.aborts.contains(&entry.vector) {
                        let ip = fx.load(PTR, saved, lo.saved_frame().ip);
                        let location = report::location_at(lo, fx, ip);
                        report::halt(lo, fx, "abort-exception", location, None);
                        return;
                    }
                    if entry.class != "Irq" {
                        fx.when(abandon, |fx| {
                            let traps = lo.traps(fx, saved);
                            fx.when(traps, |fx| {
                                let trap = fx.ptr(EXCEPTION_TRAP as u64);
                                fx.b.ins().return_(&[trap]);
                            });
                            let ip = fx.load(PTR, saved, lo.saved_frame().ip);
                            let location = report::location_at(lo, fx, ip);
                            report::halt(lo, fx, "unabandonable", location, None);
                        });
                    }
                    let preempt = lo.preempts(fx, reschedule, saved, preempting);
                    fx.b.ins().return_(&[preempt]);
                });
            }
            if vectors.lpis {
                let lpi = fx.b.ins().icmp_imm_s(
                    IntCC::UnsignedGreaterThanOrEqual,
                    vector,
                    super::its::LPI_BASE,
                );
                fx.when(lpi, |fx| {
                    let slot = lo.lpi_binding(fx, vector);
                    let (present, reschedule) = lo.run_installed(fx, &[slot]);
                    fx.when(present, |fx| {
                        let preempt = lo.preempts(fx, reschedule, saved, preempting);
                        fx.b.ins().return_(&[preempt]);
                    });
                });
            }
            let mut installable =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, vector, 32);
            if let Some(end) = vectors.installable_end {
                let below = fx.b.ins().icmp_imm_s(IntCC::UnsignedLessThan, vector, end);
                installable = fx.b.ins().band(installable, below);
            }
            fx.when(installable, |fx| {
                let (present, reschedule) = lo.installed_reschedule(fx, vector);
                fx.when(present, |fx| {
                    let preempt = lo.preempts(fx, reschedule, saved, preempting);
                    fx.b.ins().return_(&[preempt]);
                });
            });
            let timer = fx.b.ins().icmp_imm_s(IntCC::Equal, vector, vectors.timer);
            fx.when(timer, |fx| {
                lo.end_of_interrupt(fx);
                let preempt = lo.preempts(fx, no, saved, preempting);
                fx.b.ins().return_(&[preempt]);
            });
            if let Some(spurious) = vectors.spurious {
                let spurious = fx.b.ins().icmp_imm_s(IntCC::Equal, vector, spurious);
                fx.when(spurious, |fx| {
                    let zero = fx.ptr(0);
                    fx.b.ins().return_(&[zero]);
                });
            }
            let mut exception =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedLessThan, vector, vectors.exceptions);
            for other in vectors.stop.iter().chain(vectors.aborts) {
                let this = fx.b.ins().icmp_imm_s(IntCC::NotEqual, vector, *other);
                exception = fx.b.ins().band(exception, this);
            }
            fx.when(exception, |fx| {
                let traps = lo.traps(fx, saved);
                fx.when(traps, |fx| {
                    let trap = fx.ptr(EXCEPTION_TRAP as u64);
                    fx.b.ins().return_(&[trap]);
                });
            });
            let ip = fx.load(PTR, saved, lo.saved_frame().ip);
            let location = report::location_at(lo, fx, ip);
            report::halt(lo, fx, "unbound-vector", location, Some(("vector", vector)));
        });
        id
    }

    /// Serves the CPU signal when `vector` is its: a shootdown first on x86_64, then, unless a
    /// contribution binds the vector, the end of the interrupt, returning whether a task asked to
    /// stop is preempted.
    fn dispatch_signal(
        &mut self,
        fx: &mut Fx,
        bound: &[Bound],
        vector: Value,
        saved: Value,
        preempting: bool,
    ) {
        let shooting = self.machine_arch() == Arch::Amd64;
        let signal_bound = bound.iter().any(|entry| entry.vector == SIGNAL_VECTOR);
        if !shooting && signal_bound {
            return;
        }
        let signal = fx.b.ins().icmp_imm_s(IntCC::Equal, vector, SIGNAL_VECTOR);
        fx.when(signal, |fx| {
            if shooting {
                self.serve_shootdown(fx);
            }
            if !signal_bound {
                self.end_of_interrupt(fx);
                let no = fx.int(types::I8, 0);
                let preempt = self.preempts(fx, no, saved, preempting);
                fx.b.ins().return_(&[preempt]);
            }
        });
    }

    /// Calls the interrupt fn a contribution binds to the fired vector: an `Irq` handler with a
    /// new `hw::IrqContext`, an exception handler with an `hw::TrapFrame` naming the saved frame
    /// and, for a page fault, holding the faulting address.
    /// Gives whether its `hw::IrqExit` is `Reschedule`, and whether it is `Abandon`.
    pub(super) fn dispatch_bound(
        &mut self,
        fx: &mut Fx,
        entry: &Bound,
        saved: Value,
    ) -> (Value, Value) {
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
                let fault = match entry.vector == self.vectors().page_fault {
                    true => self.fault_address(fx, saved),
                    false => fx.ptr(0),
                };
                let at = self.layouts.field(self.ck, &ty, None, "fault").offset;
                fx.store(fault, trap_frame, at);
                Held::Address(trap_frame)
            }
        };
        let called = self.invoke(fx, Invoked::Direct(id), &abi, &[arg]);
        let exit_ty = abi.result.clone();
        let exit = self.exit_variant(fx, &exit_ty, called.result);
        let reschedule = self.is_exit(fx, &exit_ty, exit, "Reschedule");
        let abandon = self.is_exit(fx, &exit_ty, exit, "Abandon");
        (reschedule, abandon)
    }

    /// Whether an `hw::IrqExit` an interrupt fn gave is `Reschedule`.
    pub fn reschedules(&mut self, fx: &mut Fx, exit_ty: &Ty, exit: Held) -> Value {
        let exit = self.exit_variant(fx, exit_ty, exit);
        self.is_exit(fx, exit_ty, exit, "Reschedule")
    }

    /// The variant index of an `hw::IrqExit` an interrupt fn gave.
    fn exit_variant(&mut self, fx: &mut Fx, exit_ty: &Ty, exit: Held) -> Value {
        let exit = match exit {
            Held::Address(address) => address,
            other => {
                let layout = self.layout(exit_ty);
                let slot = fx.slot(layout.size.max(1), layout.align.max(1));
                self.store_held(fx, slot, other, exit_ty);
                slot
            }
        };
        self.variant_index(fx, exit_ty, exit)
    }

    /// Whether an `hw::IrqExit` variant index is the variant named `name`.
    fn is_exit(&mut self, fx: &mut Fx, exit_ty: &Ty, index: Value, name: &str) -> Value {
        let Ty::Item(item, _) = exit_ty else {
            panic!("an interrupt fn giving hw::IrqExit");
        };
        let position = self
            .variant_names(*item)
            .iter()
            .position(|variant| variant == name)
            .expect("an hw::IrqExit variant");
        fx.b.ins().icmp_imm_s(IntCC::Equal, index, position as i64)
    }

    /// Whether the interrupted code is a kernel task's that may trap where it was interrupted:
    /// it ran with maskable interrupts enabled on its own stack, and the anchor is installed.
    fn traps(&mut self, fx: &mut Fx, saved: Value) -> Value {
        let enabled = self.interrupts_were_enabled(fx, saved);
        let state_address = self.state(fx);
        let anchor = fx.load(PTR, state_address, state::ANCHOR);
        let installed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, anchor, 0);
        let record = tasks::current_task(self, fx);
        let running = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
        let sp_at = self.saved_frame().sp;
        let on_stack = fx.choose(
            running,
            &[types::I8],
            |fx| {
                let sp = fx.load(PTR, saved, sp_at);
                let base = fx.load(PTR, record, task::STACK);
                let size = fx.load(PTR, record, task::STACK_SIZE);
                let top = fx.b.ins().iadd(base, size);
                let above = fx.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, sp, base);
                let below = fx.b.ins().icmp(IntCC::UnsignedLessThan, sp, top);
                vec![fx.b.ins().band(above, below)]
            },
            |fx| vec![fx.int(types::I8, 0)],
        )[0];
        let traps = fx.b.ins().band(enabled, installed);
        fx.b.ins().band(traps, on_stack)
    }

    /// Whether the seam preempts the interrupted code, giving 1 or zero: the final build
    /// has a `hw::preempt` entry, the code is a kernel task that may trap where it was
    /// interrupted, outside `hw::Ring0.wait_for_interrupt`, and the task is asked to stop and
    /// may stop there, or else is not asked and its interrupt fn rescheduled.
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
        let traps = self.traps(fx, saved);
        let cpu_record = self.running_cpu(fx);
        let waiting = fx.load(PTR, cpu_record, cpu::WAITING);
        let free = fx.b.ins().icmp_imm_s(IntCC::Equal, waiting, 0);
        let candidate = fx.b.ins().band(traps, free);
        let layout = self.saved_frame();
        let preempting = fx.choose_in(
            self,
            candidate,
            &[types::I8],
            |lo, fx| {
                let record = tasks::current_task(lo, fx);
                let asked = tasks::stop_asked(fx, record);
                fx.choose_in(
                    lo,
                    asked,
                    &[types::I8],
                    |lo, fx| {
                        let base = fx.load(PTR, record, task::STACK);
                        let size = fx.load(PTR, record, task::STACK_SIZE);
                        let at = stops::Interrupted {
                            ip: fx.load(PTR, saved, layout.ip),
                            sp: fx.load(PTR, saved, layout.sp),
                            fp: fx.load(PTR, saved, layout.fp),
                            link: layout.link.map(|link| fx.load(PTR, saved, link)),
                            low: fx.load(PTR, record, task::GUARD),
                            high: fx.b.ins().iadd(base, size),
                        };
                        vec![stops::stoppable(lo, fx, &at)]
                    },
                    |_, _| vec![reschedule],
                )
            },
            |_, fx| vec![fx.int(types::I8, 0)],
        )[0];
        fx.b.ins().uextend(PTR, preempting)
    }

    /// The vectors the final build binds by contribution, then the CPU signal's unless one binds
    /// it, which the runtime keeps.
    pub fn statically_bound(&mut self) -> Vec<i64> {
        let mut vectors: Vec<i64> = self
            .bound_vectors()
            .iter()
            .map(|entry| entry.vector)
            .collect();
        if !vectors.contains(&SIGNAL_VECTOR) {
            vectors.push(SIGNAL_VECTOR);
        }
        vectors
    }

    /// The vector the running CPU's interrupt fn serves, 0 for an AArch64 LPI.
    pub fn current_vector(&mut self, fx: &mut Fx) -> Value {
        let record = self.running_cpu(fx);
        let mut vector = fx.load(PTR, record, cpu::VECTOR);
        if self.machine_arch() == Arch::Aarch64 {
            let lpi = fx.b.ins().icmp_imm_s(
                IntCC::UnsignedGreaterThanOrEqual,
                vector,
                super::its::LPI_BASE,
            );
            let none = fx.ptr(0);
            vector = fx.b.ins().select(lpi, none, vector);
        }
        fx.b.ins().ireduce(types::I8, vector)
    }

    /// Ends the interrupt an `hw::IrqContext` serves, unless the running CPU defers it to the
    /// dispatcher running installed devices.
    pub fn complete_interrupt(&mut self, fx: &mut Fx) {
        let record = self.running_cpu(fx);
        let deferred = fx.load(PTR, record, cpu::DEFERRED);
        let now = fx.b.ins().icmp_imm_s(IntCC::Equal, deferred, 0);
        fx.when(now, |fx| self.end_of_interrupt(fx));
    }

    /// On `Init` of an x86_64 kernel, chooses x2APIC mode when the boot CPU has it, recording
    /// the choice, and puts the boot CPU's local APIC in it.
    pub(super) fn choose_apic_mode(&mut self, fx: &mut Fx) {
        let registers = fx.slot(16, 4);
        let leaf = fx.ptr(1);
        let subleaf = fx.ptr(0);
        self.call_stub(fx, Stub::Cpuid, &[leaf, subleaf, registers]);
        let ecx = fx.load(types::I32, registers, 8);
        let feature = fx.b.ins().band_imm_s(ecx, X2APIC_FEATURE);
        let x2apic = fx.b.ins().icmp_imm_s(IntCC::NotEqual, feature, 0);
        let x2apic = fx.b.ins().uextend(PTR, x2apic);
        let state_address = self.state(fx);
        fx.store(x2apic, state_address, state::X2APIC);
        self.enter_apic_mode(fx);
    }

    /// Puts the running CPU's local APIC in x2APIC mode when the runtime chose it.
    fn enter_apic_mode(&mut self, fx: &mut Fx) {
        let x2apic = self.x2apic(fx);
        fx.when(x2apic, |fx| {
            let msr = fx.ptr(u64::from(APIC_BASE_MSR));
            let base = self.call_stub(fx, Stub::Rdmsr, &[msr])[0];
            let base = fx.b.ins().bor_imm_s(base, X2APIC_MODE);
            self.write_msr(fx, APIC_BASE_MSR, base);
        });
    }

    /// Whether the local APICs run in x2APIC mode.
    fn x2apic(&mut self, fx: &mut Fx) -> Value {
        let state_address = self.state(fx);
        let mode = fx.load(PTR, state_address, state::X2APIC);
        fx.b.ins().icmp_imm_s(IntCC::NotEqual, mode, 0)
    }

    /// Writes a 32-bit register of the running CPU's local APIC.
    fn apic_write(&mut self, fx: &mut Fx, register: u64, value: Value) {
        let x2apic = self.x2apic(fx);
        fx.choose_in(
            self,
            x2apic,
            &[],
            |lo, fx| {
                let msr = fx.ptr(X2APIC_MSRS + (register >> 4));
                lo.call_stub(fx, Stub::Wrmsr, &[msr, value]);
                Vec::new()
            },
            |lo, fx| {
                let at = fx.ptr(APIC + register);
                lo.mmio_write(fx, 4, at, value);
                Vec::new()
            },
        );
    }

    /// Reads a 32-bit register of the running CPU's local APIC.
    fn apic_read(&mut self, fx: &mut Fx, register: u64) -> Value {
        let x2apic = self.x2apic(fx);
        fx.choose_in(
            self,
            x2apic,
            &[PTR],
            |lo, fx| {
                let msr = fx.ptr(X2APIC_MSRS + (register >> 4));
                lo.call_stub(fx, Stub::Rdmsr, &[msr])
            },
            |lo, fx| {
                let at = fx.ptr(APIC + register);
                vec![lo.mmio_read(fx, 4, at)]
            },
        )[0]
    }

    /// The ticks the running CPU's local APIC timer counts in a millisecond, measured with its
    /// interrupt masked, after which the timer fires `TIMER_VECTOR` one-shot.
    pub(super) fn x64_timer_rate(&mut self, fx: &mut Fx) -> Value {
        let divide = fx.ptr(DIVIDE_BY_16);
        self.apic_write(fx, APIC_TIMER_DIVIDE, divide);
        let masked = fx.ptr(TIMER_VECTOR as u64 | MASKED);
        self.apic_write(fx, APIC_LVT_TIMER, masked);
        let full = fx.ptr(0xFFFF_FFFF);
        self.apic_write(fx, APIC_TIMER_COUNT, full);
        let start = self.call_routine(fx, Routine::ClockNow, &[])[0];
        let end = fx.repeat(&[start], |fx, _| {
            let now = self.call_routine(fx, Routine::ClockNow, &[])[0];
            let elapsed = fx.b.ins().isub(now, start);
            let short =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedLessThan, elapsed, TIMER_MEASURE as i64);
            (short, vec![now])
        })[0];
        let left = self.apic_read(fx, APIC_TIMER_CURRENT);
        let ticks = fx.b.ins().isub(full, left);
        let ticks =
            fx.b.ins()
                .imul_imm_s(ticks, super::super::runtime::timers::NS_PER_MS as i64);
        let elapsed = fx.b.ins().isub(end, start);
        let unmasked = fx.ptr(TIMER_VECTOR as u64);
        self.apic_write(fx, APIC_LVT_TIMER, unmasked);
        fx.b.ins().udiv(ticks, elapsed)
    }

    /// Starts the running CPU's local APIC timer counting down `ticks`.
    pub(super) fn x64_set_timer(&mut self, fx: &mut Fx, ticks: Value) {
        self.apic_write(fx, APIC_TIMER_COUNT, ticks);
    }

    /// Ends the interrupt the running CPU serves, which `hw::IrqContext.complete` reaches: the
    /// local APIC's end-of-interrupt register on x86_64, and the GIC CPU interface's on AArch64.
    pub fn end_of_interrupt(&mut self, fx: &mut Fx) {
        match self.machine_arch() {
            Arch::Amd64 => {
                let zero = fx.ptr(0);
                self.apic_write(fx, APIC_EOI, zero);
            }
            Arch::Aarch64 => self.aarch64_eoi(fx),
        }
    }

    /// The running CPU's local APIC id, the whole x2APIC id in x2APIC mode.
    pub fn local_apic_id(&mut self, fx: &mut Fx) -> Value {
        let id = self.apic_read(fx, APIC_ID);
        let x2apic = self.x2apic(fx);
        let shifted = fx.b.ins().ushr_imm_s(id, 24);
        fx.b.ins().select(x2apic, id, shifted)
    }

    /// Sends an IPI of a command to the CPU whose local APIC has the id `destination` and, out
    /// of x2APIC mode, waits until it is delivered.
    pub fn send_ipi(&mut self, fx: &mut Fx, destination: Value, command: u64) {
        let x2apic = self.x2apic(fx);
        fx.choose_in(
            self,
            x2apic,
            &[],
            |lo, fx| {
                let target = fx.b.ins().ishl_imm_s(destination, 32);
                let value = fx.b.ins().bor_imm_s(target, command as i64);
                let msr = fx.ptr(X2APIC_MSRS + (APIC_COMMAND >> 4));
                lo.call_stub(fx, Stub::Wrmsr, &[msr, value]);
                Vec::new()
            },
            |lo, fx| {
                let target = fx.b.ins().ishl_imm_s(destination, 24);
                let at = fx.ptr(APIC + APIC_DESTINATION);
                lo.mmio_write(fx, 4, at, target);
                let command = fx.ptr(command);
                let at = fx.ptr(APIC + APIC_COMMAND);
                lo.mmio_write(fx, 4, at, command);
                fx.repeat(&[], |fx, _| {
                    let status = lo.mmio_read(fx, 4, at);
                    let pending = fx.b.ins().band_imm_s(status, IPI_PENDING);
                    let pending = fx.b.ins().icmp_imm_s(IntCC::NotEqual, pending, 0);
                    fx.when(pending, |fx| {
                        lo.call_stub(fx, Stub::Pause, &[]);
                    });
                    (pending, Vec::new())
                });
                Vec::new()
            },
        );
    }

    /// The first step of a kernel halt, with maskable interrupts disabled so that nothing else
    /// runs on the halting CPU: the first CPU to begin one sends every other started CPU the stop
    /// interrupt and waits until each has stopped; a CPU beginning a halt after it stops instead.
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
            // The non-maskable interrupt reaches a halted x86_64 CPU, which then counts itself;
            // an AArch64 CPU halts with the stop interrupt masked, so it counts itself first.
            match self.machine_arch() {
                Arch::Amd64 => {
                    self.call_stub(fx, Stub::Halt, &[]);
                }
                Arch::Aarch64 => self.stop_cpu(fx),
            }
        });
        let started = fx.load(PTR, state_address, state::STARTED_CPUS);
        let others = fx.b.ins().iadd_imm_s(started, -1);
        let running = fx.b.ins().icmp_imm_s(IntCC::SignedGreaterThan, others, 0);
        fx.when(running, |fx| {
            self.send_stop(fx);
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

    /// Sends every other CPU the interrupt that stops it in a halt, which disabled maskable
    /// interrupts do not hold back: a non-maskable interrupt on x86_64, and on AArch64 the
    /// software interrupt the interrupt controller delivers as a fast interrupt.
    fn send_stop(&mut self, fx: &mut Fx) {
        match self.machine_arch() {
            Arch::Amd64 => {
                let all = fx.ptr(0);
                self.send_ipi(fx, all, IPI_NMI_OTHERS);
            }
            Arch::Aarch64 => self.aarch64_send_stop(fx),
        }
    }

    /// On a non-maskable interrupt during a kernel halt, stops the running CPU, counting it; the
    /// interrupt has other causes outside one.
    pub(super) fn stop_if_halting(&mut self, fx: &mut Fx) {
        let state_address = self.state(fx);
        let halting = fx.load(PTR, state_address, state::HALTING);
        let halting = fx.b.ins().icmp_imm_s(IntCC::NotEqual, halting, 0);
        fx.when(halting, |fx| self.stop_cpu(fx));
    }

    /// Counts the running CPU among the stopped and halts it. An AArch64 CPU masks fast
    /// interrupts first, so that a stop interrupt arriving meanwhile cannot count it twice.
    pub(super) fn stop_cpu(&mut self, fx: &mut Fx) {
        if self.machine_arch() == Arch::Aarch64 {
            self.call_stub(fx, Stub::FiqOff, &[]);
        }
        let state_address = self.state(fx);
        let stopped = fx.offset(state_address, state::STOPPED_CPUS);
        let one = fx.ptr(1);
        super::super::runtime::fetch_add(fx, stopped, one);
        self.call_stub(fx, Stub::Halt, &[]);
    }
}
