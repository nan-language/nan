//! The operations std's `hw` supplies to its IOMMU and PCI code: the unit record the runtime
//! state names, its lock and invalidation, chunks, runtime records and holdings, cache line
//! write-backs, the platform's tables, PCI configuration accesses, and the calls of that code
//! the runtime makes for the kernel's accessors, DMA buffers and settlement.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, condcodes::IntCC};

use super::super::super::body::Held;
use super::super::super::call::Invoked;
use super::super::super::fx::{Fx, PTR};
use super::super::super::machine::Stub;
use super::super::super::runtime::system::map_chunks;
use super::super::super::runtime::{Routine, report, state};
use super::super::super::{Instance, Lowering};
use super::super::Supply;
use crate::nir::{Callable, FnRef, Role};
use crate::workspace::Arch;

/// The configuration ports of PCI configuration mechanism #1: the address, whose high bit
/// enables it, and the data.
const CONFIG_ADDRESS: u64 = 0xCF8;
const CONFIG_DATA: u64 = 0xCFC;
const CONFIG_ENABLE: i64 = 0x8000_0000;

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    let name = match supply.path.strip_prefix("hw::") {
        Some(name) => name,
        None => return false,
    };
    match name {
        "iommu_unit" => {
            let unit = lo.iommu_unit(fx);
            supply.ret(lo, fx, Held::Scalar(unit));
        }
        "iommu_ready" => {
            let state_address = lo.state(fx);
            fx.store(supply.param(0).scalar(), state_address, state::IOMMU);
            supply.done(lo, fx);
        }
        "iommu_lock" => {
            let unit = lo.iommu_unit(fx);
            let flags = lo.iommu_lock(fx, unit);
            supply.ret(lo, fx, Held::Scalar(flags));
        }
        "iommu_unlock" => {
            let unit = lo.iommu_unit(fx);
            lo.iommu_unlock(fx, unit, supply.param(0).scalar());
            supply.done(lo, fx);
        }
        "iommu_flush" => {
            lo.call_routine(fx, Routine::IommuFlush, &[]);
            supply.done(lo, fx);
        }
        "chunks" => {
            let chunks = map_chunks(lo, fx, supply.param(0).scalar());
            supply.ret(lo, fx, Held::Scalar(chunks));
        }
        "runtime_record" => {
            let record = lo.record_alloc(fx, supply.param(0).scalar());
            let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
            fx.when(missing, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
            supply.ret(lo, fx, Held::Scalar(record));
        }
        "free_record" => {
            lo.record_free(fx, supply.param(0).scalar());
            supply.done(lo, fx);
        }
        "hold" => {
            let args = [supply.param(0).scalar(), supply.param(1).scalar()];
            let holding = lo.call_routine(fx, Routine::Hold, &args)[0];
            supply.ret(lo, fx, Held::Scalar(holding));
        }
        "unhold" => {
            lo.call_routine(fx, Routine::Unhold, &[supply.param(0).scalar()]);
            supply.done(lo, fx);
        }
        "flush_line" => {
            lo.call_stub(fx, Stub::FlushLine, &[supply.param(0).scalar()]);
            supply.done(lo, fx);
        }
        "exhausted" => report::raise_kind(lo, fx, "trap.exhausted"),
        "x2apic" => {
            let state_address = lo.state(fx);
            let mode = fx.load(PTR, state_address, state::X2APIC);
            let on = fx.b.ins().icmp_imm_s(IntCC::NotEqual, mode, 0);
            supply.ret(lo, fx, Held::Scalar(on));
        }
        "rsdp_address" => {
            let rsdp = lo.rsdp(fx);
            supply.ret(lo, fx, Held::Scalar(rsdp));
        }
        "tree_address" => {
            let tree = super::super::boot::tree_address(lo, fx);
            supply.ret(lo, fx, Held::Scalar(tree));
        }
        "on_aarch64" => {
            let aarch64 = fx.int(
                cranelift_codegen::ir::types::I8,
                i64::from(lo.ck.db.target.arch() == Some(Arch::Aarch64)),
            );
            supply.ret(lo, fx, Held::Scalar(aarch64));
        }
        "config_read" => {
            let value = config(lo, fx, supply, None);
            supply.ret(lo, fx, Held::Scalar(value));
        }
        "config_write" => {
            config(lo, fx, supply, Some(supply.param(3).scalar()));
            supply.done(lo, fx);
        }
        _ => return false,
    }
    true
}

/// A PCI configuration access of a double word: through the ECAM a nonzero first parameter
/// names, or on x86_64 through the configuration ports, which a lock keeps to one CPU at a time.
/// Reads give the double word.
fn config(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, write: Option<Value>) -> Value {
    let ecam = supply.param(0).scalar();
    let requester = fx.b.ins().uextend(PTR, supply.param(1).scalar());
    let offset = fx.b.ins().uextend(PTR, supply.param(2).scalar());
    let value = write.map(|value| fx.b.ins().uextend(PTR, value));
    let mapped = |lo: &mut Lowering, fx: &mut Fx| {
        let at = fx.b.ins().ishl_imm_s(requester, 12);
        let at = fx.b.ins().iadd(ecam, at);
        let at = fx.b.ins().iadd(at, offset);
        match value {
            Some(value) => {
                lo.mmio_write(fx, 4, at, value);
                vec![fx.ptr(0)]
            }
            None => vec![lo.mmio_read(fx, 4, at)],
        }
    };
    let read = match lo.ck.db.target.arch() {
        Some(Arch::Amd64) => {
            let ported = fx.b.ins().icmp_imm_s(IntCC::Equal, ecam, 0);
            fx.choose_in(
                lo,
                ported,
                &[PTR],
                |lo, fx| {
                    let state_address = lo.state(fx);
                    let lock = fx.offset(state_address, state::PCI_LOCK);
                    let flags = lo.call_stub(fx, Stub::IrqSave, &[])[0];
                    fx.repeat(&[], |fx, _| {
                        let free = fx.ptr(0);
                        let held = fx.ptr(1);
                        let seen =
                            fx.b.ins()
                                .atomic_cas(MemFlagsData::trusted(), lock, free, held);
                        let busy = fx.b.ins().icmp_imm_s(IntCC::NotEqual, seen, 0);
                        (busy, Vec::new())
                    });
                    let address = fx.b.ins().ishl_imm_s(requester, 8);
                    let low = fx.b.ins().band_imm_s(offset, 0xFC);
                    let address = fx.b.ins().bor(address, low);
                    let address = fx.b.ins().bor_imm_s(address, CONFIG_ENABLE);
                    let port = fx.ptr(CONFIG_ADDRESS);
                    lo.port_out(fx, 4, port, address);
                    let port = fx.ptr(CONFIG_DATA);
                    let read = match value {
                        Some(value) => {
                            lo.port_out(fx, 4, port, value);
                            fx.ptr(0)
                        }
                        None => lo.port_in(fx, 4, port),
                    };
                    let free = fx.ptr(0);
                    fx.b.ins().atomic_store(MemFlagsData::trusted(), free, lock);
                    lo.call_stub(fx, Stub::IrqRestore, &[flags]);
                    vec![read]
                },
                mapped,
            )[0]
        }
        _ => mapped(lo, fx)[0],
    };
    fx.b.ins().ireduce(cranelift_codegen::ir::types::I32, read)
}

/// Calls a function std's `hw` declares, of no type parameters, with values held as its
/// parameters take them, giving its result.
pub fn call_hw(lo: &mut Lowering, fx: &mut Fx, name: &str, args: &[Held]) -> Held {
    let item = lo
        .ck
        .db
        .std_item("hw", name)
        .unwrap_or_else(|| panic!("the std function hw::{name}"));
    let fn_ref = FnRef {
        callable: Callable::Item {
            item,
            role: Role::Body,
            index: 0,
        },
        args: Vec::new().into(),
    };
    let instance: Instance = lo.callable_instance(&fn_ref, false);
    let abi = lo.abi(&instance);
    let id = lo.instance(instance);
    lo.invoke(fx, Invoked::Direct(id), &abi, args).result
}
