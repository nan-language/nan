//! Hardware primitives, by kind: the synchronisation primitives, register windows and DMA
//! buffers, the CPU-facing operations, and physical memory.

mod cpu;
mod physical;
mod sync;
mod windows;

use cranelift_codegen::ir::{InstBuilder, TrapCode, Value};

use super::super::Lowering;
use super::super::fx::{Fx, PTR};
use super::super::machine::Stub;
use super::Supply;
use crate::check::ty::Ty;

pub use physical::{region, region_cell};
pub use windows::dma_access;

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    let owner = supply
        .path
        .rsplit_once('.')
        .map_or(supply.path.as_str(), |(owner, _)| owner);
    match owner {
        "hw::Anchor" => sync::anchor_with(lo, fx, supply),
        "hw::without_irqs" => sync::without_irqs(lo, fx, supply),
        "hw::SpinLock" => sync::spin_lock(lo, fx, supply),
        "hw::IrqShared" => sync::irq_shared(lo, fx, supply),
        "hw::PerCpu" => sync::per_cpu(lo, fx, supply),
        "hw::Signal" => sync::signal(lo, fx, supply),
        "hw::Dma" | "hw::DmaShared" => windows::dma(lo, fx, supply),
        "hw::dma_fence" => {
            if lo.machine() {
                lo.call_stub(fx, Stub::Mfence, &[]);
            }
            supply.done(lo, fx);
        }
        "hw::Mmio" | "hw::Ports" => windows::window(lo, fx, supply),
        "hw::sim::Registers" => windows::registers(lo, fx, supply),
        "hw::IrqContext" => cpu::irq_context(lo, fx, supply),
        "hw::cpu_hardware_id" => cpu::hardware_id(lo, fx, supply),
        "hw::Ring0" => cpu::ring0(lo, fx, supply),
        "hw::Idt" => cpu::idt(lo, fx, supply),
        "hw::TrapFrame" => cpu::trap_frame(lo, fx, supply),
        "hw::UserAddr" => cpu::user_addr(lo, fx, supply),
        "hw::Region" => physical::lower_region(lo, fx, supply),
        "hw::PhysFrame" => physical::lower_frame(lo, fx, supply),
        "hw::PageTableRoot" => physical::page_table_root(lo, fx, supply),
        // Only the kernel executor creates the values these take, and it switches threads
        // through its own routines, so their code never runs.
        "hw::SavedContext" | "hw::switch_to" | "hw::SpinGuard" => {
            fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
        }
        _ => return false,
    }
    true
}

/// Returns a called body's result as the operation's, its failure becoming the operation's.
pub(super) fn forward(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    called: super::super::call::Called,
) {
    supply.forward_failure(lo, fx, &called);
    supply.ret(lo, fx, called.result);
}

/// Whether moving an hw type into another domain's reach does anything, none for a type std
/// declares in full.
pub fn transfers(lo: &mut Lowering, path: &str, ty: &Ty) -> Option<bool> {
    Some(match path {
        "hw::Signal" | "hw::SpinLock" => {
            let value = ty.type_arg(0).cloned().expect("a wrapped type");
            lo.transfers(&value)
        }
        "hw::IrqShared" | "hw::PerCpu" | "hw::Dma" | "hw::DmaShared" | "hw::IdtInstalled" => true,
        "hw::Mmio" | "hw::Ports" | "hw::Idt" | "hw::Region" | "hw::PhysFrame"
        | "hw::PageTableRoot" | "hw::SpinGuard" => false,
        _ => return None,
    })
}

/// Moves an hw type at an address into another domain's reach; false for a type std declares in
/// full.
pub fn transfer(
    lo: &mut Lowering,
    fx: &mut Fx,
    path: &str,
    ty: &Ty,
    address: Value,
    lift: Value,
    owner: Value,
) -> bool {
    let wrapped = || ty.type_arg(0).cloned().expect("a wrapped type");
    match path {
        "hw::Signal" => sync::transfer_signal(lo, fx, ty, address, lift, owner),
        "hw::SpinLock" => {
            let value = lo.layouts.field(lo.ck, ty, None, "value").offset;
            let value = fx.offset(address, value);
            lo.transfer(fx, &wrapped(), value, lift, owner);
        }
        "hw::IrqShared" => {
            let record = fx.load(PTR, address, 0);
            let holding = fx.offset(record, super::super::machine::device::HOLDING);
            super::super::transfer::rehome(lo, fx, holding, owner);
            let value = sync::device_value(lo, &wrapped());
            let value = fx.offset(record, value);
            lo.transfer(fx, &wrapped(), value, lift, owner);
        }
        "hw::PerCpu" => sync::transfer_per_cpu(lo, fx, &wrapped(), address, lift, owner),
        "hw::Dma" | "hw::DmaShared" | "hw::IdtInstalled" => {
            let contents = fx.load(PTR, address, 0);
            super::super::transfer::lift(lo, fx, contents, lift);
        }
        _ => return false,
    }
    true
}

/// Whether releasing an hw type does anything, none for a type std declares in full. A linear
/// type's values end only through its own operations.
pub fn needs_release(lo: &mut Lowering, path: &str, ty: &Ty) -> Option<bool> {
    Some(match path {
        "hw::Signal" | "hw::SpinLock" => {
            let value = ty.type_arg(0).cloned().expect("a wrapped type");
            lo.needs_release(&value)
        }
        "hw::PerCpu" => true,
        "hw::Mmio" | "hw::Ports" | "hw::Dma" | "hw::DmaShared" | "hw::Idt" | "hw::IdtInstalled"
        | "hw::Region" | "hw::PhysFrame" | "hw::PageTableRoot" | "hw::SpinGuard"
        | "hw::IrqShared" => false,
        _ => return None,
    })
}

/// Releases an hw type at an address; false for a type std declares in full.
pub fn release(lo: &mut Lowering, fx: &mut Fx, path: &str, ty: &Ty, address: Value) -> bool {
    let wrapped = || ty.type_arg(0).cloned().expect("a wrapped type");
    match path {
        "hw::Signal" => sync::release_signal(lo, fx, ty, address),
        "hw::SpinLock" => {
            let value = lo.layouts.field(lo.ck, ty, None, "value").offset;
            let value = fx.offset(address, value);
            lo.release(fx, &wrapped(), value);
        }
        "hw::PerCpu" => sync::release_per_cpu(lo, fx, &wrapped(), address),
        _ => return false,
    }
    true
}
