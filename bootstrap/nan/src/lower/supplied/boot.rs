//! The root grants of the machine targets, `boot::Kernel` and `boot::Machine`: their root
//! allocator, clocks, `hw::Ring0`, the usable memory of the machine and the modules of the start
//! information, the firmware services of the UEFI targets, the anchor a kernel installs before
//! its executor starts, and what the start information and the firmware's tables give std. The
//! installed anchor lies in storage the image reserves, whose address and the location of the
//! `anchor` call the runtime state records.

pub mod firmware;

use cranelift_codegen::ir::{InstBuilder, TrapCode, Value, condcodes::IntCC, types};

use super::super::body::Held;
use super::super::fx::{Fx, PTR};
use super::super::machine::{Stub, start};
use super::super::runtime::tasks::task;
use super::super::runtime::{Routine, report, state};
use super::super::{Glue, Lowering};
use super::Supply;
use super::hw::region;
use super::mem::vector::{self, Element};
use super::tasks::scopes;
use crate::check::ty::Ty;
use crate::workspace::{Kind, Target};

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    match supply.path.as_str() {
        "boot::Kernel.alloc" | "boot::Machine.alloc" => {
            let grant = supply.param(0).address();
            let at = field(lo, supply, "alloc");
            let alloc = fx.offset(grant, at);
            supply.ret(lo, fx, Held::Address(alloc));
        }
        "boot::Kernel.clock" => {
            let grant = supply.param(0).address();
            let at = field(lo, supply, "clock");
            let clock = fx.offset(grant, at);
            supply.ret(lo, fx, Held::Address(clock));
        }
        "boot::Kernel.wall_clock" => wall_clock(lo, fx, supply),
        "boot::Kernel.ring0" | "boot::Machine.ring0" => ring0(lo, fx, supply),
        "boot::Kernel.regions" => regions(lo, fx, supply),
        "boot::Kernel.modules" => modules(lo, fx, supply),
        "boot::Kernel.anchor" => anchor(lo, fx, supply),
        "boot::Kernel.policy" => lo.install_policy(fx, supply),
        "boot::Module.contents" => module_contents(lo, fx, supply),
        "boot::start_info" => {
            let info = lo.start_info(fx);
            supply.ret(lo, fx, Held::Scalar(info));
        }
        "boot::tree_address" => {
            let tree = match lo.ck.db.target {
                Target::Aarch64Bare => {
                    let state_address = lo.state(fx);
                    fx.load(PTR, state_address, state::PLATFORM)
                }
                _ => fx.ptr(0),
            };
            supply.ret(lo, fx, Held::Scalar(tree));
        }
        "boot::rsdp_address" => {
            let rsdp = lo.rsdp(fx);
            supply.ret(lo, fx, Held::Scalar(rsdp));
        }
        "boot::system_table_address" => {
            let table = match lo.ck.db.target.kind() {
                Some(Kind::Uefi) => {
                    let state_address = lo.state(fx);
                    fx.load(PTR, state_address, state::PLATFORM_TABLE)
                }
                _ => fx.ptr(0),
            };
            supply.ret(lo, fx, Held::Scalar(table));
        }
        "boot::Machine.with_services" => firmware::with_services(lo, fx, supply),
        "boot::Machine.exit_services" => firmware::exit_services(lo, fx, supply),
        "boot::Machine.enter" => firmware::enter(lo, fx, supply),
        "bare_platform::yield_task" | "uefi_platform::yield_task" => {
            super::super::runtime::tasks::yield_preempted(lo, fx);
            supply.done(lo, fx);
        }
        "boot::Services.free" => firmware::free(lo, fx, supply),
        path if path.starts_with("boot::Services.") => firmware::service(lo, fx, supply),
        path if path.starts_with("uefi_platform::") => firmware::platform(lo, fx, supply),
        _ => return false,
    }
    true
}

/// The offset of a field of the root grant the operation is named after.
pub(super) fn field(lo: &mut Lowering, supply: &Supply, name: &str) -> u64 {
    let grant = supply.owner.clone().expect("a root grant");
    lo.layouts.field(lo.ck, &grant, None, name).offset
}

/// `k.wall_clock()`: `Some` of the kernel's wall clock when its source gave a time at the
/// grant's minting, `None` otherwise.
fn wall_clock(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let grant = supply.param(0).address();
    let at = field(lo, supply, "wall");
    let wall = fx.load(PTR, grant, at);
    let option = supply.abi.result.clone();
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, wall, -1);
    fx.when(present, |fx| {
        let slot = fx.slot(8, 8);
        fx.store(wall, slot, 0);
        let some = lo.variant_value(fx, &option, "Some", &[Held::Address(slot)], None);
        supply.ret(lo, fx, some);
    });
    let none = lo.none_value(fx, &option);
    supply.ret(lo, fx, none);
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
        let some = lo.variant_value(fx, &option, "Some", &[Held::Empty], None);
        supply.ret(lo, fx, some);
    });
    let none = lo.none_value(fx, &option);
    supply.ret(lo, fx, none);
}

/// `k.regions()`: the first time, a region for each range of usable memory outside the image, the
/// boot area and the tables extending the entry root, in address order, drawn through the root
/// allocator; an empty list after.
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
        let image = lo.image_extent(fx);
        let area = lo.boot_area_extent(fx);
        let tables = lo.entry_tables_extent(fx);
        lo.usable_memory(fx, &[image, area, tables], &mut |lo, fx, base, end| {
            push_region(lo, fx, list, element, base, end);
        });
    });
    supply.ret(lo, fx, Held::Address(list));
}

/// `k.modules()`: the first time, the modules the start information lists, in its order, drawn
/// through the root allocator; an empty list after, and on a target without a start information.
fn modules(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let grant = supply.param(0).address();
    let at = field(lo, supply, "modules");
    let taken = fx.load(types::I8, grant, at);
    let yes = fx.int(types::I8, 1);
    fx.store(yes, grant, at);
    let list = supply.sret.expect("the modules' address");
    let module_ty = supply
        .abi
        .result
        .type_arg(0)
        .cloned()
        .expect("the module type");
    let element = Element::of(lo, &module_ty);
    let cell = fx.load(PTR, grant, field(lo, supply, "alloc"));
    vector::empty(fx, list, cell, element);
    let info = lo.start_info(fx);
    let untaken = fx.b.ins().icmp_imm_s(IntCC::Equal, taken, 0);
    let given = fx.b.ins().icmp_imm_s(IntCC::NotEqual, info, 0);
    let first = fx.b.ins().band(untaken, given);
    fx.when(first, |fx| {
        let count = start::count(fx, info, start::info::MODULE_COUNT);
        let entries = fx.load(PTR, info, start::info::MODULES);
        let room = vector::reserve(lo, fx, list, count, element);
        let short = fx.b.ins().icmp_imm_s(IntCC::Equal, room, 0);
        fx.when(short, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
        let data = fx.load(PTR, list, vector::DATA);
        let region_at = lo.layouts.field(lo.ck, &module_ty, None, "region").offset;
        let name_at = lo.layouts.field(lo.ck, &module_ty, None, "name_at").offset;
        let zero = fx.ptr(0);
        fx.each(zero, count, |fx, index| {
            let offset = fx.b.ins().imul_imm_s(index, start::module::BYTES);
            let entry = fx.b.ins().iadd(entries, offset);
            let at = element.at(fx, data, index);
            let region = fx.offset(at, region_at);
            let address = fx.load(PTR, entry, start::module::ADDRESS);
            fx.store(address, region, region::BASE);
            let size = fx.load(PTR, entry, start::module::SIZE);
            fx.store(size, region, region::LEN);
            let name = fx.load(PTR, entry, start::module::NAME);
            fx.store(name, at, name_at);
        });
        fx.store(count, list, vector::LEN);
    });
    supply.ret(lo, fx, Held::Address(list));
}

/// `module.contents()`: a view of the module's memory.
fn module_contents(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let module = supply.param(0).address();
    let module_ty = supply.owner.clone().expect("the module type");
    let region_at = lo.layouts.field(lo.ck, &module_ty, None, "region").offset;
    let region = fx.offset(module, region_at);
    let base = fx.load(PTR, region, region::BASE);
    let len = fx.load(PTR, region, region::LEN);
    let sret = supply.sret.expect("a view's address");
    fx.store(base, sret, 0);
    fx.store(len, sret, 8);
    supply.done(lo, fx);
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
/// the anchor and installs it, then starts the kernel executor on every CPU, floating point
/// disabled, and runs `f` as its first kernel task, given the executor. The kernel halts once
/// that task ends.
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
    lo.call_stub(fx, Stub::DisableFloats, &[]);
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
        lo, fx, &run_ty, function, exec, cell, process, root, process, None, None, done,
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
