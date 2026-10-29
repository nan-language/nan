//! The root grants of the machine targets, `boot::Kernel` and `boot::Machine`: their root
//! allocator, `hw::Ring0` and the usable memory of the machine, the firmware services of the
//! UEFI targets, and the anchor a kernel installs before its executor starts. The installed
//! anchor lies in storage the image reserves, whose address and the location of the `anchor`
//! call the runtime state records.

mod firmware;

use cranelift_codegen::ir::{InstBuilder, TrapCode, Value, condcodes::IntCC, types};
use cranelift_frontend::Variable;

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
use crate::workspace::{Arch, Target};

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
        let some = lo.variant_value(fx, &option, "Some", &[Held::Empty], None);
        supply.ret(lo, fx, some);
    });
    let none = lo.none_value(fx, &option);
    supply.ret(lo, fx, none);
}

/// The first mebibyte, which `k.regions()` leaves out on x86_64.
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

/// The flattened device tree `aarch64-bare` enters with, whose words are big-endian: the offsets
/// in its header of its size and of the offsets of its structure, strings and memory reservation
/// blocks; the structure block's tokens opening and closing a node, starting a property and
/// ending the block, any other being one word; in a property the offsets of its value's length,
/// of its name's offset in the strings block and of its value; a reservation's size and the
/// offset of its length; and the root's cells of an address and of a size when it states none.
mod device_tree {
    pub const SIZE: u64 = 4;
    pub const STRUCTURE: u64 = 8;
    pub const STRINGS: u64 = 12;
    pub const RESERVED: u64 = 16;
    pub const BEGIN_NODE: i64 = 1;
    pub const END_NODE: i64 = 2;
    pub const PROPERTY: i64 = 3;
    pub const END: i64 = 9;
    pub const LEN: u64 = 4;
    pub const NAME: u64 = 8;
    pub const VALUE: i64 = 12;
    pub const RESERVATION: i64 = 16;
    pub const RESERVATION_LEN: u64 = 8;
    pub const ADDRESS_CELLS: u64 = 2;
    pub const SIZE_CELLS: u64 = 1;
}

/// `k.regions()`: the first time, a region for each range of usable memory outside the image,
/// outside the first MiB on x86_64 and outside the device tree and the ranges it reserves on
/// `aarch64-bare`, below the 4 GiB the entry root maps, in address order, drawn through the root
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
    let target = lo.ck.db.target;
    fx.when(first, |fx| {
        let (image, image_end) = lo.image_extent(fx);
        let usable = |lo: &mut Lowering, fx: &mut Fx, base: Value, end: Value| {
            let base = match target.arch() {
                Some(Arch::Amd64) => {
                    let low = fx.ptr(LOW_MEMORY as u64);
                    fx.b.ins().umax(base, low)
                }
                _ => base,
            };
            let mapped = fx.ptr(MAPPED);
            let end = fx.b.ins().umin(end, mapped);
            let before = fx.b.ins().umin(end, image);
            push_region(lo, fx, list, element, base, before);
            let after = fx.b.ins().umax(base, image_end);
            push_region(lo, fx, list, element, after, end);
        };
        let state_address = lo.state(fx);
        match target {
            Target::X64Uefi | Target::Aarch64Uefi => {
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
            Target::Aarch64Bare => {
                tree_regions(lo, fx, state_address, list, element, (image, image_end))
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

/// A walk of the device tree's structure block: the strings block naming its properties, the
/// memory reservation block, the memory the device tree and the image hold, the list of regions
/// it fills, and the variables holding the depth of the node it is in, whether that node is a
/// memory node, and the root's cells of an address and of a size.
#[derive(Clone, Copy)]
struct TreeWalk {
    strings: Value,
    reserved: Value,
    held: [(Value, Value); 2],
    list: Value,
    element: Element,
    depth: Variable,
    memory: Variable,
    address_cells: Variable,
    size_cells: Variable,
}

/// The usable memory of `aarch64-bare`: the ranges in the `reg` property of each memory node of
/// the device tree, a child of its root named `memory` or `memory@` and a unit address, read in
/// the root's cells.
fn tree_regions(
    lo: &mut Lowering,
    fx: &mut Fx,
    state_address: Value,
    list: Value,
    element: Element,
    image: (Value, Value),
) {
    let tree = fx.load(PTR, state_address, state::PLATFORM);
    let structure = tree_address(fx, tree, device_tree::STRUCTURE);
    let strings = tree_address(fx, tree, device_tree::STRINGS);
    let reserved = tree_address(fx, tree, device_tree::RESERVED);
    let tree_end = tree_address(fx, tree, device_tree::SIZE);
    let zero = fx.ptr(0);
    let depth = fx.var(PTR, zero);
    let no = fx.int(types::I8, 0);
    let memory = fx.var(types::I8, no);
    let address_cells = fx.ptr(device_tree::ADDRESS_CELLS);
    let address_cells = fx.var(PTR, address_cells);
    let size_cells = fx.ptr(device_tree::SIZE_CELLS);
    let size_cells = fx.var(PTR, size_cells);
    let walk = TreeWalk {
        strings,
        reserved,
        held: [(tree, tree_end), image],
        list,
        element,
        depth,
        memory,
        address_cells,
        size_cells,
    };
    fx.repeat(&[structure], |fx, state| {
        let at = state[0];
        let token = tree_cell(fx, at, 0);
        let name = fx.b.ins().iadd_imm_s(at, 4);
        let next = fx.var(PTR, name);
        let open =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, token, device_tree::BEGIN_NODE);
        fx.when(open, |fx| {
            let parent = fx.b.use_var(depth);
            let child = fx.b.ins().icmp_imm_s(IntCC::Equal, parent, 1);
            let named = memory_node(fx, name);
            let found = fx.b.ins().band(child, named);
            fx.b.def_var(memory, found);
            let inner = fx.b.ins().iadd_imm_s(parent, 1);
            fx.b.def_var(depth, inner);
            let after = name_end(fx, name);
            fx.b.def_var(next, after);
        });
        let close =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, token, device_tree::END_NODE);
        fx.when(close, |fx| {
            let inner = fx.b.use_var(depth);
            let outer = fx.b.ins().iadd_imm_s(inner, -1);
            fx.b.def_var(depth, outer);
        });
        let property =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, token, device_tree::PROPERTY);
        fx.when(property, |fx| {
            let after = tree_property(lo, fx, walk, at);
            fx.b.def_var(next, after);
        });
        let more =
            fx.b.ins()
                .icmp_imm_s(IntCC::NotEqual, token, device_tree::END);
        (more, vec![fx.b.use_var(next)])
    });
}

/// Reads the property at `at` of the device tree's structure block and gives the address after
/// it: the root's `#address-cells` and `#size-cells` set the walk's cells, and the `reg` of a
/// memory node inserts the usable parts of the ranges it lists.
fn tree_property(lo: &mut Lowering, fx: &mut Fx, walk: TreeWalk, at: Value) -> Value {
    let len = tree_cell(fx, at, device_tree::LEN);
    let offset = tree_cell(fx, at, device_tree::NAME);
    let name = fx.b.ins().iadd(walk.strings, offset);
    let value = fx.b.ins().iadd_imm_s(at, device_tree::VALUE);
    let depth = fx.b.use_var(walk.depth);
    let root = fx.b.ins().icmp_imm_s(IntCC::Equal, depth, 1);
    let stated = tree_cell(fx, value, 0);
    let named = starts_with(fx, name, b"#address-cells\0");
    let addresses = fx.b.ins().band(root, named);
    fx.when(addresses, |fx| fx.b.def_var(walk.address_cells, stated));
    let named = starts_with(fx, name, b"#size-cells\0");
    let sizes = fx.b.ins().band(root, named);
    fx.when(sizes, |fx| fx.b.def_var(walk.size_cells, stated));
    let node = fx.b.ins().icmp_imm_s(IntCC::Equal, depth, 2);
    let memory = fx.b.use_var(walk.memory);
    let inside = fx.b.ins().band(node, memory);
    let named = starts_with(fx, name, b"reg\0");
    let reg = fx.b.ins().band(inside, named);
    fx.when(reg, |fx| {
        let address_cells = fx.b.use_var(walk.address_cells);
        let size_cells = fx.b.use_var(walk.size_cells);
        let cells = fx.b.ins().iadd(address_cells, size_cells);
        let pair = fx.b.ins().ishl_imm_s(cells, 2);
        let size_offset = fx.b.ins().ishl_imm_s(address_cells, 2);
        let count = fx.b.ins().udiv(len, pair);
        let zero = fx.ptr(0);
        fx.each(zero, count, |fx, index| {
            let offset = fx.b.ins().imul(index, pair);
            let base_at = fx.b.ins().iadd(value, offset);
            let base = tree_number(fx, base_at, address_cells);
            let size_at = fx.b.ins().iadd(base_at, size_offset);
            let size = tree_number(fx, size_at, size_cells);
            let end = fx.b.ins().iadd(base, size);
            push_unreserved(lo, fx, walk, base, end);
        });
    });
    let padded = fx.b.ins().iadd_imm_s(len, 3);
    let padded = fx.b.ins().band_imm_s(padded, -4);
    fx.b.ins().iadd(value, padded)
}

/// Inserts the usable memory from `base` to `end`, below the 4 GiB the entry root maps, into the
/// walk's list of regions in address order: its parts outside the memory the walk holds and the
/// ranges the memory reservation block lists. Each pass inserts the part before the one of those
/// ranges overlapping the rest that starts lowest, then skips past that range.
fn push_unreserved(lo: &mut Lowering, fx: &mut Fx, walk: TreeWalk, base: Value, end: Value) {
    let mapped = fx.ptr(MAPPED);
    let end = fx.b.ins().umin(end, mapped);
    fx.repeat(&[base], |fx, state| {
        let rest = (state[0], end);
        let mut lowest = (end, end);
        for held in walk.held {
            lowest = lowest_overlap(fx, lowest, held, rest);
        }
        let found = fx.repeat(&[walk.reserved, lowest.0, lowest.1], |fx, state| {
            let entry = state[0];
            let start = fx.load_ordered(PTR, entry, 0, false, true);
            let len = fx.load_ordered(PTR, entry, device_tree::RESERVATION_LEN, false, true);
            let stop = fx.b.ins().iadd(start, len);
            let lowest = lowest_overlap(fx, (state[1], state[2]), (start, stop), rest);
            let listed = fx.b.ins().bor(start, len);
            let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, listed, 0);
            let next = fx.b.ins().iadd_imm_s(entry, device_tree::RESERVATION);
            (more, vec![next, lowest.0, lowest.1])
        });
        push_region(lo, fx, walk.list, walk.element, rest.0, found[1]);
        let after = fx.b.ins().umax(rest.0, found[2]);
        let more = fx.b.ins().icmp(IntCC::UnsignedLessThan, after, end);
        (more, vec![after])
    });
}

/// Whichever of `lowest` and `range` starts lower, `range` only where it overlaps `rest`.
fn lowest_overlap(
    fx: &mut Fx,
    lowest: (Value, Value),
    range: (Value, Value),
    rest: (Value, Value),
) -> (Value, Value) {
    let start = fx.b.ins().umax(range.0, rest.0);
    let stop = fx.b.ins().umin(range.1, rest.1);
    let overlaps = fx.b.ins().icmp(IntCC::UnsignedLessThan, start, stop);
    let below = fx.b.ins().icmp(IntCC::UnsignedLessThan, range.0, lowest.0);
    let lower = fx.b.ins().band(overlaps, below);
    let start = fx.b.ins().select(lower, range.0, lowest.0);
    let stop = fx.b.ins().select(lower, range.1, lowest.1);
    (start, stop)
}

/// The address `tree` plus the device tree's header word at `field`: that of one of its blocks,
/// or its end.
fn tree_address(fx: &mut Fx, tree: Value, field: u64) -> Value {
    let offset = tree_cell(fx, tree, field);
    fx.b.ins().iadd(tree, offset)
}

/// The big-endian cell at `offset` past `at`, widened to an address.
fn tree_cell(fx: &mut Fx, at: Value, offset: u64) -> Value {
    let cell = fx.load_ordered(types::I32, at, offset, false, true);
    fx.b.ins().uextend(PTR, cell)
}

/// The number the `count` cells at `at` hold, the most significant first, to 64 bits.
fn tree_number(fx: &mut Fx, at: Value, count: Value) -> Value {
    let zero = fx.ptr(0);
    let number = fx.var(PTR, zero);
    fx.each(zero, count, |fx, index| {
        let offset = fx.b.ins().ishl_imm_s(index, 2);
        let cell_at = fx.b.ins().iadd(at, offset);
        let cell = tree_cell(fx, cell_at, 0);
        let high = fx.b.use_var(number);
        let high = fx.b.ins().ishl_imm_s(high, 32);
        let joined = fx.b.ins().bor(high, cell);
        fx.b.def_var(number, joined);
    });
    fx.b.use_var(number)
}

/// Whether the node name at `name` is `memory`, alone or followed by `@` and a unit address.
fn memory_node(fx: &mut Fx, name: Value) -> Value {
    let memory = starts_with(fx, name, b"memory");
    let after = fx.load(types::I8, name, 6);
    let alone = fx.b.ins().icmp_imm_s(IntCC::Equal, after, 0);
    let unit = fx.b.ins().icmp_imm_s(IntCC::Equal, after, i64::from(b'@'));
    let either = fx.b.ins().bor(alone, unit);
    fx.b.ins().band(memory, either)
}

/// The address after the node name at `name`, which a zero byte ends and zeros pad to whole
/// words: past the first word whose last byte is zero.
fn name_end(fx: &mut Fx, name: Value) -> Value {
    fx.repeat(&[name], |fx, state| {
        let last = fx.load(types::I8, state[0], 3);
        let next = fx.b.ins().iadd_imm_s(state[0], 4);
        let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, last, 0);
        (more, vec![next])
    })[0]
}

/// Whether the bytes at `at` start with `text`.
fn starts_with(fx: &mut Fx, at: Value, text: &[u8]) -> Value {
    let mut same = fx.int(types::I8, 1);
    for (offset, byte) in text.iter().enumerate() {
        let found = fx.load(types::I8, at, offset as u64);
        let equal = fx.b.ins().icmp_imm_s(IntCC::Equal, found, i64::from(*byte));
        same = fx.b.ins().band(same, equal);
    }
    same
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
        lo, fx, &run_ty, function, cell, process, root, process, None, None, done,
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
