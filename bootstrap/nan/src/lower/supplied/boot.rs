//! The root grants of the machine targets, `boot::Kernel` and `boot::Machine`: their root
//! allocator, `hw::Ring0` and the usable memory of the machine, the firmware services of
//! `x86_64-uefi`, and the anchor a kernel installs before its executor starts. The installed
//! anchor lies in storage the image reserves, whose address and the location of the `anchor`
//! call the runtime state records.

mod firmware;

use cranelift_codegen::ir::{InstBuilder, TrapCode, Value, condcodes::IntCC, types};

use super::super::body::Held;
use super::super::fx::{Fx, PTR};
use super::super::machine::{MAPPED, Stub};
use super::super::runtime::tasks::task;
use super::super::runtime::{Routine, report, state};
use super::super::{Glue, Lowering};
use super::Supply;
use super::hw::region;
use super::mem::vector::{self, Element};
use super::tasks::scopes;
use crate::check::ty::Ty;
use crate::workspace::Target;

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    match supply.path.as_str() {
        "boot::Kernel.alloc" | "boot::Machine.alloc" => {
            let grant = supply.param(0).address();
            let at = field(lo, supply, "alloc");
            let alloc = fx.offset(grant, at);
            supply.ret(lo, fx, Held::Address(alloc));
        }
        "boot::Kernel.ring0" => ring0(lo, fx, supply),
        "boot::Kernel.regions" => regions(lo, fx, supply),
        "boot::Kernel.anchor" => anchor(lo, fx, supply),
        "boot::Machine.with_services" => firmware::with_services(lo, fx, supply),
        "boot::Machine.exit_services" => firmware::exit_services(lo, fx, supply),
        "boot::Services.write_console" => firmware::write_console(lo, fx, supply),
        "boot::Services.read_file" => firmware::read_file(lo, fx, supply),
        "bare_platform::yield_task" | "uefi_platform::yield_task" => {
            super::super::runtime::tasks::yield_preempted(lo, fx);
            supply.done(lo, fx);
        }
        _ => return false,
    }
    true
}

/// The offset of a field of the root grant the operation is named after.
fn field(lo: &mut Lowering, supply: &Supply, name: &str) -> u64 {
    let grant = supply.owner.clone().expect("a root grant");
    lo.layouts.field(lo.ck, &grant, None, name).offset
}

/// `k.ring0()`: `Some` of the machine's one `hw::Ring0` the first time, `None` after.
fn ring0(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let grant = supply.param(0).address();
    let at = field(lo, supply, "ring0");
    let taken = fx.load(types::I8, grant, at);
    let yes = fx.int(types::I8, 1);
    fx.store(yes, grant, at);
    let option = supply.abi.result.clone();
    let first = fx.b.ins().icmp_imm_s(IntCC::Equal, taken, 0);
    fx.when(first, |fx| {
        let some = lo.variant_value(fx, &option, "Some", &[Held::Empty], false);
        supply.ret(lo, fx, some);
    });
    let none = lo.none_value(fx, &option);
    supply.ret(lo, fx, none);
}

/// The first mebibyte, which `k.regions()` leaves out.
const LOW_MEMORY: i64 = 0x10_0000;

/// The usable entries of the `x86_64-bare` start information's memory map, which version 1 of
/// it has: the offsets of the map's address and entry count, an entry's size, and in an entry
/// the offsets of its address, size and type, and the type of usable memory.
mod start_map {
    pub const VERSION: u64 = 4;
    pub const ADDRESS: u64 = 40;
    pub const COUNT: u64 = 48;
    pub const ENTRY: i64 = 24;
    pub const BASE: u64 = 0;
    pub const SIZE: u64 = 8;
    pub const KIND: u64 = 16;
    pub const USABLE: i64 = 1;
}

/// The usable entries of a UEFI memory map: in a descriptor the offsets of its type, physical
/// start and page count, and the types of conventional memory and of boot services' code and
/// data, which `exit_services` frees.
mod firmware_map {
    pub const KIND: u64 = 0;
    pub const BASE: u64 = 8;
    pub const PAGES: u64 = 24;
    pub const USABLE: [i64; 3] = [3, 4, 7];
}

/// `k.regions()`: the first time, a region for each range of usable memory outside the first
/// MiB and outside the image, below the 4 GiB the entry root maps, in address order, drawn
/// through the root allocator; an empty list after.
fn regions(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let grant = supply.param(0).address();
    let at = field(lo, supply, "regions");
    let taken = fx.load(types::I8, grant, at);
    let yes = fx.int(types::I8, 1);
    fx.store(yes, grant, at);
    let list = supply.sret.expect("the regions' address");
    let region_ty = supply
        .abi
        .result
        .type_arg(0)
        .cloned()
        .expect("the region type");
    let element = Element::of(lo, &region_ty);
    let cell = fx.load(PTR, grant, field(lo, supply, "alloc"));
    vector::empty(fx, list, cell, element);
    let first = fx.b.ins().icmp_imm_s(IntCC::Equal, taken, 0);
    fx.when(first, |fx| {
        let (image, image_end) = lo.image_extent(fx);
        let usable = |lo: &mut Lowering, fx: &mut Fx, base: Value, end: Value| {
            let low = fx.ptr(LOW_MEMORY as u64);
            let base = fx.b.ins().umax(base, low);
            let mapped = fx.ptr(MAPPED);
            let end = fx.b.ins().umin(end, mapped);
            let before = fx.b.ins().umin(end, image);
            push_region(lo, fx, list, element, base, before);
            let after = fx.b.ins().umax(base, image_end);
            push_region(lo, fx, list, element, after, end);
        };
        let state_address = lo.state(fx);
        match lo.ck.db.target {
            Target::Uefi => {
                let map = fx.load(PTR, state_address, state::MEMORY_MAP);
                let size = fx.load(PTR, state_address, state::MAP_SIZE);
                let stride = fx.load(PTR, state_address, state::MAP_STRIDE);
                let count = fx.b.ins().udiv(size, stride);
                let zero = fx.ptr(0);
                fx.each(zero, count, |fx, index| {
                    let offset = fx.b.ins().imul(index, stride);
                    let entry = fx.b.ins().iadd(map, offset);
                    let kind = fx.load(types::I32, entry, firmware_map::KIND);
                    let mut free = fx.int(types::I8, 0);
                    for usable_kind in firmware_map::USABLE {
                        let same = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, usable_kind);
                        free = fx.b.ins().bor(free, same);
                    }
                    fx.when(free, |fx| {
                        let base = fx.load(PTR, entry, firmware_map::BASE);
                        let pages = fx.load(PTR, entry, firmware_map::PAGES);
                        let bytes = fx.b.ins().ishl_imm_s(pages, 12);
                        let end = fx.b.ins().iadd(base, bytes);
                        usable(lo, fx, base, end);
                    });
                });
            }
            _ => {
                let info = fx.load(PTR, state_address, state::PLATFORM);
                let version = fx.load(types::I32, info, start_map::VERSION);
                let mapped =
                    fx.b.ins()
                        .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, version, 1);
                fx.when(mapped, |fx| {
                    let map = fx.load(PTR, info, start_map::ADDRESS);
                    let count = fx.load(types::I32, info, start_map::COUNT);
                    let count = fx.b.ins().uextend(PTR, count);
                    let zero = fx.ptr(0);
                    fx.each(zero, count, |fx, index| {
                        let offset = fx.b.ins().imul_imm_s(index, start_map::ENTRY);
                        let entry = fx.b.ins().iadd(map, offset);
                        let kind = fx.load(types::I32, entry, start_map::KIND);
                        let free = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, start_map::USABLE);
                        fx.when(free, |fx| {
                            let base = fx.load(PTR, entry, start_map::BASE);
                            let size = fx.load(PTR, entry, start_map::SIZE);
                            let end = fx.b.ins().iadd(base, size);
                            usable(lo, fx, base, end);
                        });
                    });
                });
            }
        }
    });
    supply.ret(lo, fx, Held::Address(list));
}

/// Inserts the region from `base` to `end`, when it is not empty, into a list of regions in
/// address order, trapping when the list cannot grow.
fn push_region(
    lo: &mut Lowering,
    fx: &mut Fx,
    list: Value,
    element: Element,
    base: Value,
    end: Value,
) {
    let present = fx.b.ins().icmp(IntCC::UnsignedLessThan, base, end);
    fx.when(present, |fx| {
        let len = fx.load(PTR, list, vector::LEN);
        let needed = fx.b.ins().iadd_imm_s(len, 1);
        let room = vector::reserve(lo, fx, list, needed, element);
        let short = fx.b.ins().icmp_imm_s(IntCC::Equal, room, 0);
        fx.when(short, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
        let data = fx.load(PTR, list, vector::DATA);
        let zero = fx.ptr(0);
        let index = fx.repeat(&[zero], |fx, state| {
            let index = state[0];
            let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, index, len);
            let below = fx.choose(
                inside,
                &[types::I8],
                |fx| {
                    let at = element.at(fx, data, index);
                    let other = fx.load(PTR, at, region::BASE);
                    vec![fx.b.ins().icmp(IntCC::UnsignedLessThan, other, base)]
                },
                |fx| vec![fx.int(types::I8, 0)],
            )[0];
            let next = fx.b.ins().iadd_imm_s(index, 1);
            let index = fx.b.ins().select(below, next, index);
            (below, vec![index])
        })[0];
        let at = vector::open_gap(lo, fx, list, index, element);
        let len = fx.b.ins().isub(end, base);
        fx.store(base, at, region::BASE);
        fx.store(len, at, region::LEN);
    });
}

/// `take k.anchor(take state, run: f)`: moves `state` into the storage the image reserves for
/// the anchor and installs it, then starts the kernel executor on every CPU and runs `f` as its
/// first kernel task, given the executor. The kernel halts once that task ends.
fn anchor(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let state_ty = supply.type_arg(0);
    let layout = lo.layout(&state_ty);
    let storage = lo.reserved(fx, "_NSanchor", layout.size.max(1), layout.align.max(1));
    lo.store_held(fx, storage, supply.param(1), &state_ty);
    let state_address = lo.state(fx);
    let location = report::caller_location(lo, fx);
    fx.store(location, state_address, state::ANCHOR_CALL);
    fx.store(storage, state_address, state::ANCHOR);
    let prepare = lo.interrupt_setup(fx);
    let call = lo.func_ref(fx, prepare);
    fx.b.ins().call(call, &[]);
    let boot = lo.cpu_records(fx);
    lo.cpu_started(fx, boot);
    lo.start_cpus(fx, prepare);
    let grant = supply.param(0).address();
    let cell = fx.load(PTR, grant, field(lo, supply, "alloc"));
    let exec = fx.load(PTR, grant, field(lo, supply, "exec"));
    let Ty::Fn(run_ty) = supply.abi.params[2].0.ty() else {
        panic!("the anchor's task fn");
    };
    let run_ty = (**run_ty).clone();
    let function = supply.param(2).address();
    let process = fx.load(PTR, state_address, state::PROCESS);
    let root = fx.load(PTR, state_address, state::ROOT);
    let done = first_task_done(lo);
    let first = scopes::start_task(
        lo, fx, &run_ty, function, cell, process, root, process, None, done,
    );
    fx.store(exec, first, task::ARGUMENT);
    fx.store(first, state_address, state::ENTRY_TASK);
    lo.call_routine(fx, Routine::Wake, &[first]);
    lo.call_stub(fx, Stub::Sti, &[]);
    lo.call_routine(fx, Routine::Schedule, &[]);
    fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
}

/// What the executor thread does once the anchor's first kernel task has ended: the kernel
/// halts, at the `anchor` call.
fn first_task_done(lo: &mut Lowering) -> cranelift_module::FuncId {
    lo.glue_function(
        Glue::AnchorDone,
        super::tasks::record_signature(),
        |lo, fx| {
            fx.entry();
            let state_address = lo.state(fx);
            let location = fx.load(PTR, state_address, state::ANCHOR_CALL);
            report::halt(lo, fx, "first-task-ended", location, None);
        },
    )
}

/// Whether releasing a boot type does anything, none for a type std declares in full.
pub fn needs_release(path: &str) -> Option<bool> {
    matches!(path, "boot::Kernel" | "boot::Machine").then_some(false)
}
