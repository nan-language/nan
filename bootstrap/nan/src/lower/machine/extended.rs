//! Extended state owned by threads. A task owning extended state records its save area, from
//! which the scheduler restores the state before the task runs and into which it saves the
//! state once the task switches away, so the state is live only while its owner runs. Each CPU's
//! executor thread records whether the CPU's extended state is enabled and holds an owner's;
//! before it runs a task owning none it puts that state in its initial state and disables it,
//! so no task sees another thread's registers and an instruction using them faults. The
//! `Extended` routine changes what the running task owns.
use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::Lowering;
use super::super::data::Reserved;
use super::super::fx::{Fx, PTR};
use super::super::runtime::Routine;
use super::super::runtime::tasks::{task, thread};
use super::cpus::cpu;
use super::stubs::Stub;
use crate::workspace::Arch;

/// The bits of an x86_64 CPU record's features that the `CpuFeatures` stub sets where the CPU
/// enables XSAVE, and where it saves in the compacted form, through `xsavec` or `xsaves`.
const XSAVE: i64 = 0x200;
const COMPACTED: i64 = 0x1800;

/// The offsets in an x86_64 save area of its vector registers, which an AArch64 one begins with,
/// of the x87 control word, of `MXCSR`, and of the XSAVE header's state and compaction bitmaps;
/// the bit of the SSE state in those bitmaps and the compaction bitmap's bit marking the
/// compacted form.
const X64_VECTORS: u64 = 160;
const X87_CONTROL: u64 = 0;
const MXCSR: u64 = 24;
const STATE_BITMAP: u64 = 512;
const COMPACTION_BITMAP: u64 = 520;
const SSE_STATE: i64 = 2;
const COMPACTED_FORM: i64 = 1 << 63;

/// The initial x87 control word and `MXCSR`, and the bytes of a save area that its initial
/// state and the XSAVE header span.
const INITIAL_X87_CONTROL: i64 = 0x037F;
const INITIAL_MXCSR: i64 = 0x1F80;
const AREA_HEAD: u64 = 576;

/// The alignment of a save area, and the bytes of a frame's slot holding one of a frame's size.
const AREA_ALIGN: i64 = 64;
const OWNED_SLOT: u64 = 4_096 + AREA_ALIGN as u64;

/// The x86_64 area an `xrstor` or `fxrstor` of which puts the extended state in its initial
/// state: a legacy region and header of zeros.
const INITIAL: Reserved = Reserved {
    name: "_NSextended_initial",
    size: AREA_HEAD,
    align: 64,
};

impl Lowering<'_, '_> {
    /// Before the scheduler runs a task: makes the CPU's extended state the one the task owns.
    pub fn extended_in(&mut self, fx: &mut Fx, record: Value) {
        let area = fx.load(PTR, record, task::EXTENDED);
        self.adopt_extended(fx, area);
    }

    /// Once a task has switched away: saves the extended state it owns.
    pub fn extended_out(&mut self, fx: &mut Fx, record: Value) {
        let area = fx.load(PTR, record, task::EXTENDED);
        let owns = fx.b.ins().icmp_imm_s(IntCC::NotEqual, area, 0);
        fx.when(owns, |fx| {
            self.call_stub(fx, Stub::ExtendedSave, &[area]);
        });
    }

    /// `Extended(area) -> previous`: with maskable interrupts disabled, saves the extended state
    /// the running task owns, makes the task own the one saved at `area`, none for zero, and
    /// makes it the CPU's, giving the area the task owned before.
    pub(crate) fn define_extended(&mut self, fx: &mut Fx) {
        let area = fx.entry()[0];
        let flags = self.call_stub(fx, Stub::IrqSave, &[])[0];
        let running = self.thread(fx);
        let record = fx.load(PTR, running, thread::CURRENT);
        let previous = fx.load(PTR, record, task::EXTENDED);
        self.extended_out(fx, record);
        fx.store(area, record, task::EXTENDED);
        self.adopt_extended(fx, area);
        self.call_stub(fx, Stub::IrqRestore, &[flags]);
        fx.b.ins().return_(&[previous]);
    }

    /// Makes the running CPU's extended state the one saved at `area`, enabling it first where
    /// it is disabled; for zero, puts an owner's state the CPU holds in its initial state and
    /// disables it.
    fn adopt_extended(&mut self, fx: &mut Fx, area: Value) {
        let own = self.thread(fx);
        let live = fx.load(PTR, own, thread::EXTENDED);
        let disabled = fx.b.ins().icmp_imm_s(IntCC::Equal, live, 0);
        let owns = fx.b.ins().icmp_imm_s(IntCC::NotEqual, area, 0);
        fx.choose_in(
            self,
            owns,
            &[],
            |lo, fx| {
                fx.when(disabled, |fx| {
                    lo.call_stub(fx, Stub::EnableFloats, &[]);
                    let one = fx.ptr(1);
                    fx.store(one, own, thread::EXTENDED);
                });
                lo.call_stub(fx, Stub::ExtendedRestore, &[area]);
                Vec::new()
            },
            |lo, fx| {
                let enabled = fx.b.ins().bxor_imm_s(disabled, 1);
                fx.when(enabled, |fx| {
                    match lo.ck.db.target.arch() {
                        Some(Arch::Aarch64) => lo.call_stub(fx, Stub::ExtendedClear, &[]),
                        _ => {
                            let initial = lo.reserved(fx, INITIAL);
                            lo.call_stub(fx, Stub::ExtendedClear, &[initial])
                        }
                    };
                    lo.call_stub(fx, Stub::DisableFloats, &[]);
                    let zero = fx.ptr(0);
                    fx.store(zero, own, thread::EXTENDED);
                });
                Vec::new()
            },
        );
    }

    /// On a nanos target, whose code holds floats and lanes in the registers of extended state,
    /// at an entry that code of another target calls: makes the running task own extended state
    /// of its own, in its initial state and saved in the frame, giving the state it owned before.
    pub fn own_extended(&mut self, fx: &mut Fx) -> Option<Value> {
        if !self.ck.db.target.is_nanos() {
            return None;
        }
        let slot = fx.slot(OWNED_SLOT, 16);
        let area = fx.b.ins().iadd_imm_s(slot, AREA_ALIGN - 1);
        let area = fx.b.ins().band_imm_s(area, -AREA_ALIGN);
        Some(self.call_routine(fx, Routine::ExtendedOwn, &[area])[0])
    }

    /// `ExtendedOwn(area) -> previous`: makes the running task own extended state in its initial
    /// state, saved at `area`, giving the area it owned before.
    pub(crate) fn define_extended_own(&mut self, fx: &mut Fx) {
        let area = fx.entry()[0];
        self.initial_extended(fx, area);
        let previous = self.call_routine(fx, Routine::Extended, &[area])[0];
        fx.b.ins().return_(&[previous]);
    }

    /// Makes the running task own again the extended state `own_extended` gave.
    pub fn disown_extended(&mut self, fx: &mut Fx, previous: Option<Value>) {
        if let Some(previous) = previous {
            self.call_routine(fx, Routine::Extended, &[previous]);
        }
    }

    /// The bytes of the stack an entry of the target's code uses for the extended state it
    /// owns.
    pub fn owned_stack(&self) -> u64 {
        match self.ck.db.target.is_nanos() {
            true => OWNED_SLOT,
            false => 0,
        }
    }

    /// Writes the initial extended state into the save area at `base`, in the form the running
    /// CPU saves.
    pub fn initial_extended(&mut self, fx: &mut Fx, base: Value) {
        let zero = fx.ptr(0);
        let len = fx.ptr(AREA_HEAD);
        self.call_routine(fx, Routine::Fill, &[base, zero, len]);
        if self.ck.db.target.arch() == Some(Arch::Aarch64) {
            return;
        }
        let control = fx.int(types::I16, INITIAL_X87_CONTROL);
        fx.store(control, base, X87_CONTROL);
        let mxcsr = fx.int(types::I32, INITIAL_MXCSR);
        fx.store(mxcsr, base, MXCSR);
        let features = self.features(fx);
        let compacted = fx.b.ins().band_imm_s(features, COMPACTED);
        let compacted = fx.b.ins().icmp_imm_s(IntCC::NotEqual, compacted, 0);
        let form = fx.int(PTR, COMPACTED_FORM);
        let form = fx.b.ins().select(compacted, form, zero);
        fx.store(form, base, COMPACTION_BITMAP);
    }

    /// The address in the save area at `base` of the vector register at `index`, and whether
    /// it holds that register's value rather than its initial zeros, which an x86_64 area in
    /// the XSAVE form with the SSE state initial leaves out.
    pub fn extended_vector(&mut self, fx: &mut Fx, base: Value, index: Value) -> (Value, Value) {
        let offset = fx.b.ins().imul_imm_s(index, 16);
        let at = fx.b.ins().iadd(base, offset);
        if self.ck.db.target.arch() == Some(Arch::Aarch64) {
            let held = fx.int(types::I8, 1);
            return (at, held);
        }
        let at = fx.offset(at, X64_VECTORS);
        let features = self.features(fx);
        let xsave = fx.b.ins().band_imm_s(features, XSAVE);
        let legacy = fx.b.ins().icmp_imm_s(IntCC::Equal, xsave, 0);
        let bitmap = fx.load(PTR, base, STATE_BITMAP);
        let sse = fx.b.ins().band_imm_s(bitmap, SSE_STATE);
        let sse = fx.b.ins().icmp_imm_s(IntCC::NotEqual, sse, 0);
        let held = fx.b.ins().bor(legacy, sse);
        (at, held)
    }

    /// Makes the save area at `base` hold every vector register's value: on x86_64, an area in
    /// the XSAVE form whose SSE state is initial gets that state's zeros and marks it saved.
    pub fn hold_extended_vectors(&mut self, fx: &mut Fx, base: Value) {
        if self.ck.db.target.arch() == Some(Arch::Aarch64) {
            return;
        }
        let zero = fx.ptr(0);
        let index = fx.ptr(0);
        let (vectors, held) = self.extended_vector(fx, base, index);
        let initial = fx.b.ins().bxor_imm_s(held, 1);
        fx.when(initial, |fx| {
            let len = fx.ptr(16 * 16);
            self.call_routine(fx, Routine::Fill, &[vectors, zero, len]);
            let bitmap = fx.load(PTR, base, STATE_BITMAP);
            let bitmap = fx.b.ins().bor_imm_s(bitmap, SSE_STATE);
            fx.store(bitmap, base, STATE_BITMAP);
            let compaction = fx.load(PTR, base, COMPACTION_BITMAP);
            let compacted = fx.b.ins().band_imm_s(compaction, COMPACTED_FORM);
            let compacted = fx.b.ins().icmp_imm_s(IntCC::NotEqual, compacted, 0);
            let marked = fx.b.ins().bor_imm_s(compaction, SSE_STATE);
            let compaction = fx.b.ins().select(compacted, marked, compaction);
            fx.store(compaction, base, COMPACTION_BITMAP);
        });
    }

    /// The features the running x86_64 CPU's record holds.
    fn features(&mut self, fx: &mut Fx) -> Value {
        let own = self.running_cpu(fx);
        fx.load(PTR, own, cpu::FEATURES)
    }
}
