//! The physical memory of a machine image: the usable memory its platform lists, the boot area,
//! which holds the boot heap and then the runtime tables `Init` sets up, and the tables extending
//! the entry root over usable memory above 4 GiB. A bare target carves the boot area, with those
//! tables at its end, from usable memory as it starts; a UEFI target's image reserves the boot
//! area, and `exit_services` carves the tables alone.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};
use cranelift_frontend::Variable;

use super::super::Lowering;
use super::super::fx::{Fx, PTR};
use super::super::runtime::memory::CHUNK;
use super::super::runtime::{report, state, system};
use super::heap::{self, BOOT_HEAP};
use super::{
    A64_ACCESS, A64_ATTR_NORMAL, A64_INNER, A64_TABLE, A64_VALID, LARGE, MAPPED, PRESENT, Stub,
    WRITABLE,
};
use crate::workspace::{Arch, Kind, Target};

/// The bytes of the boot area: the boot heap, then the runtime tables, to a whole page.
pub const BOOT_AREA: u64 = (BOOT_HEAP + system::TABLE_BYTES).next_multiple_of(TABLE);

/// The bytes of a page table, and the bytes an entry of its upper tables maps: a gigabyte.
const TABLE: u64 = 4_096;
const GIGABYTE: u64 = 1 << 30;

/// The memory the entry root can map at its physical addresses: the lower half of the 48-bit
/// address space, below 128 TiB.
const REACHABLE: u64 = 1 << 47;

/// The first mebibyte, which usable memory leaves out on x86_64.
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

/// What a walk of usable memory does with each range it finds, given the range's first address
/// and last address plus one; the range may be empty.
type Visit<'a> = &'a mut dyn FnMut(&mut Lowering, &mut Fx, Value, Value);

/// The extension of the entry root over usable memory above 4 GiB: the gigabytes it maps from
/// address zero, the upper tables it adds past the root's own, on x86_64 whether the CPU maps
/// 1 GiB pages, and the bytes of the tables it adds.
#[derive(Clone, Copy)]
struct Extension {
    gigabytes: Value,
    uppers: Value,
    pages: Option<Value>,
    bytes: Value,
}

impl Lowering<'_, '_> {
    /// `Init`'s placement of the boot area, aligned to a chunk, whose address it records in the
    /// runtime state and gives: carved on a bare target, reserved by the image on a UEFI target.
    pub fn boot_area(&mut self, fx: &mut Fx) -> Value {
        let area = match self.ck.db.target.kind() {
            Some(Kind::Bare) => self.carve_boot_area(fx),
            _ => self.reserve_boot_area(fx),
        };
        let state_address = self.state(fx);
        fx.store(area, state_address, state::BOOT_AREA);
        area
    }

    /// The boot area's first address and last address plus one.
    pub fn boot_area_extent(&mut self, fx: &mut Fx) -> (Value, Value) {
        let state_address = self.state(fx);
        let area = fx.load(PTR, state_address, state::BOOT_AREA);
        (area, fx.offset(area, BOOT_AREA))
    }

    /// The first address and last address plus one of the tables extending the entry root.
    pub fn entry_tables_extent(&mut self, fx: &mut Fx) -> (Value, Value) {
        let state_address = self.state(fx);
        let tables = fx.load(PTR, state_address, state::ENTRY_TABLES);
        let end = fx.load(PTR, state_address, state::ENTRY_TABLES_END);
        (tables, end)
    }

    /// The boot area with the tables extending the entry root at its end, carved outside the
    /// image, its runtime tables zero-filled; `Init` returns 1 when no usable memory holds it.
    /// Extends the entry root, which the entry seam installed, and on AArch64 installs it again so
    /// that its walks see the tables written.
    fn carve_boot_area(&mut self, fx: &mut Fx) -> Value {
        let extension = self.root_extension(fx);
        let image = self.image_extent(fx);
        let bytes = fx.b.ins().iadd_imm_s(extension.bytes, BOOT_AREA as i64);
        let area = self.carve(fx, bytes, &[image]);
        let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, area, -1);
        system::unavailable_if(fx, missing);
        let tables = fx.offset(area, BOOT_HEAP);
        let len = fx.ptr(system::TABLE_BYTES);
        heap::clear(self, fx, tables, len);
        let root_tables = fx.offset(area, BOOT_AREA);
        self.write_root_extension(fx, extension, root_tables);
        if self.ck.db.target.arch() == Some(Arch::Aarch64) {
            let root = self.entry_root();
            let root = self.data_address(fx, root);
            self.call_stub(fx, Stub::WriteTtbr0, &[root]);
        }
        area
    }

    /// `exit_services`'s extension of the entry root before installing it, with its tables carved
    /// from usable memory outside the image, trapping (`trap.exhausted`) when none holds them.
    pub fn extend_entry_root(&mut self, fx: &mut Fx) {
        let extension = self.root_extension(fx);
        let image = self.image_extent(fx);
        let tables = self.carve(fx, extension.bytes, &[image]);
        let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, tables, -1);
        fx.when(missing, |fx| report::raise_kind(self, fx, "trap.exhausted"));
        self.write_root_extension(fx, extension, tables);
    }

    /// The lowest chunk-aligned address of usable memory outside the `held` ranges from which
    /// `bytes` lie wholly in that usable memory below 4 GiB, where the entry root's own tables
    /// map them; all ones when there is none.
    fn carve(&mut self, fx: &mut Fx, bytes: Value, held: &[(Value, Value)]) -> Value {
        let none = fx.ptr(u64::MAX);
        let lowest = fx.var(PTR, none);
        let mapped = fx.ptr(MAPPED);
        self.usable_memory(fx, held, &mut |_, fx, base, end| {
            let bumped = fx.b.ins().iadd_imm_s(base, CHUNK as i64 - 1);
            let aligned = fx.b.ins().band_imm_s(bumped, -(CHUNK as i64));
            let top = fx.b.ins().iadd(aligned, bytes);
            let below = fx.b.ins().umin(end, mapped);
            let fits = fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, top, below);
            let held = fx.b.use_var(lowest);
            let lower = fx.b.ins().icmp(IntCC::UnsignedLessThan, aligned, held);
            let chosen = fx.b.ins().band(fits, lower);
            let next = fx.b.ins().select(chosen, aligned, held);
            fx.b.def_var(lowest, next);
        });
        fx.b.use_var(lowest)
    }

    /// The extension of the entry root over the usable memory the platform lists above 4 GiB, up
    /// to its end rounded up to a gigabyte: its upper tables, one per 512 GiB past the root's own,
    /// and on x86_64, when the CPU maps no 1 GiB pages, a page directory per gigabyte past 4 GiB.
    fn root_extension(&mut self, fx: &mut Fx) -> Extension {
        let floor = fx.ptr(MAPPED);
        let top = fx.var(PTR, floor);
        self.usable_memory(fx, &[], &mut |_, fx, base, end| {
            let held = fx.b.use_var(top);
            let present = fx.b.ins().icmp(IntCC::UnsignedLessThan, base, end);
            let higher = fx.b.ins().umax(held, end);
            let next = fx.b.ins().select(present, higher, held);
            fx.b.def_var(top, next);
        });
        let top = fx.b.use_var(top);
        let bumped = fx.b.ins().iadd_imm_s(top, GIGABYTE as i64 - 1);
        let gigabytes = fx.b.ins().ushr_imm_s(bumped, 30);
        let bumped = fx.b.ins().iadd_imm_s(gigabytes, 511);
        let spanned = fx.b.ins().ushr_imm_s(bumped, 9);
        let uppers = fx.b.ins().iadd_imm_s(spanned, -1);
        let (pages, tables) = match self.ck.db.target.arch() {
            Some(Arch::Aarch64) => (None, uppers),
            _ => {
                let pages = self.call_stub(fx, Stub::GigabytePages, &[])[0];
                let zero = fx.ptr(0);
                let past =
                    fx.b.ins()
                        .iadd_imm_s(gigabytes, -((MAPPED / GIGABYTE) as i64));
                let directories = fx.b.ins().select(pages, zero, past);
                (Some(pages), fx.b.ins().iadd(uppers, directories))
            }
        };
        let bytes = fx.b.ins().ishl_imm_s(tables, 12);
        Extension {
            gigabytes,
            uppers,
            pages,
            bytes,
        }
    }

    /// Extends the entry root in place over the gigabytes past the first 4 that `extension` maps,
    /// with its tables at `tables`, zero-filled first, whose extent it records in the runtime
    /// state. Each gigabyte gets an entry in its upper table, the root's own for the first
    /// 512 GiB: on AArch64 a 1 GiB block, on x86_64 a 1 GiB page or, when the CPU maps none, a
    /// page directory of 2 MiB pages, the directories following the upper tables.
    fn write_root_extension(&mut self, fx: &mut Fx, extension: Extension, tables: Value) {
        let state_address = self.state(fx);
        fx.store(tables, state_address, state::ENTRY_TABLES);
        let end = fx.b.ins().iadd(tables, extension.bytes);
        fx.store(end, state_address, state::ENTRY_TABLES_END);
        heap::clear(self, fx, tables, extension.bytes);
        let root = self.entry_root();
        let root = self.data_address(fx, root);
        let own = fx.offset(root, TABLE);
        let skipped = fx.b.ins().ishl_imm_s(extension.uppers, 12);
        let directories = fx.b.ins().iadd(tables, skipped);
        let (link, block) = match self.ck.db.target.arch() {
            Some(Arch::Aarch64) => (
                A64_VALID | A64_TABLE,
                A64_VALID | A64_ACCESS | A64_INNER | A64_ATTR_NORMAL,
            ),
            _ => (WRITABLE | PRESENT, LARGE | WRITABLE | PRESENT),
        };
        let first = fx.ptr(MAPPED / GIGABYTE);
        fx.each(first, extension.gigabytes, |fx, gigabyte| {
            let index = fx.b.ins().ushr_imm_s(gigabyte, 9);
            let offset = fx.b.ins().ishl_imm_s(index, 12);
            let added = fx.b.ins().iadd(tables, offset);
            let added = fx.b.ins().iadd_imm_s(added, -(TABLE as i64));
            let first_upper = fx.b.ins().icmp_imm_s(IntCC::Equal, index, 0);
            let upper = fx.b.ins().select(first_upper, own, added);
            let linked = fx.b.ins().bor_imm_s(upper, link as i64);
            let slot = fx.b.ins().ishl_imm_s(index, 3);
            let slot = fx.b.ins().iadd(root, slot);
            fx.store(linked, slot, 0);
            let address = fx.b.ins().ishl_imm_s(gigabyte, 30);
            let mut entry = fx.b.ins().bor_imm_s(address, block as i64);
            if let Some(pages) = extension.pages {
                let past =
                    fx.b.ins()
                        .iadd_imm_s(gigabyte, -((MAPPED / GIGABYTE) as i64));
                let past = fx.b.ins().ishl_imm_s(past, 12);
                let directory = fx.b.ins().iadd(directories, past);
                let small = fx.b.ins().icmp_imm_s(IntCC::Equal, pages, 0);
                fx.when(small, |fx| {
                    let zero = fx.ptr(0);
                    let count = fx.ptr(TABLE / 8);
                    fx.each(zero, count, |fx, page| {
                        let offset = fx.b.ins().ishl_imm_s(page, 21);
                        let at = fx.b.ins().iadd(address, offset);
                        let large = fx.b.ins().bor_imm_s(at, block as i64);
                        let slot = fx.b.ins().ishl_imm_s(page, 3);
                        let slot = fx.b.ins().iadd(directory, slot);
                        fx.store(large, slot, 0);
                    });
                });
                let linked = fx.b.ins().bor_imm_s(directory, link as i64);
                entry = fx.b.ins().select(pages, entry, linked);
            }
            let within = fx.b.ins().band_imm_s(gigabyte, 511);
            let within = fx.b.ins().ishl_imm_s(within, 3);
            let slot = fx.b.ins().iadd(upper, within);
            fx.store(entry, slot, 0);
        });
    }

    /// The boot area in the zeroed data the image reserves for it with one chunk more, so that it
    /// aligns to a chunk wherever the image is loaded.
    fn reserve_boot_area(&mut self, fx: &mut Fx) -> Value {
        let reserved = self.reserved(fx, "_NSboot_area", BOOT_AREA + CHUNK, 4_096);
        let bumped = fx.b.ins().iadd_imm_s(reserved, CHUNK as i64 - 1);
        fx.b.ins().band_imm_s(bumped, -(CHUNK as i64))
    }

    /// Visits the usable memory the platform lists below 128 TiB, in its parts outside the `held`
    /// ranges, outside the first MiB on x86_64 and outside the device tree and the ranges it
    /// reserves on `aarch64-bare`. Usable memory is, on `x86_64-bare`, the usable entries of the
    /// start information's memory map, on `aarch64-bare`, the ranges of the device tree's memory
    /// nodes and, on a UEFI target, the usable entries of the firmware's final memory map.
    pub fn usable_memory(&mut self, fx: &mut Fx, held: &[(Value, Value)], visit: Visit) {
        let state_address = self.state(fx);
        match self.ck.db.target {
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
                        visit_unheld(self, fx, held, None, (base, end), visit);
                    });
                });
            }
            Target::Aarch64Bare => tree_memory(self, fx, state_address, held, visit),
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
                            visit_unheld(self, fx, held, None, (base, end), visit);
                        });
                    });
                });
            }
        }
    }
}

/// Visits the parts of the usable memory `range` outside the `held` ranges and the ranges the
/// device tree's memory reservation block at `reserved` lists, clipped to start past the first
/// MiB on x86_64 and to end below 128 TiB. Each pass visits the part before
/// the one of those ranges overlapping the rest that starts lowest, then skips past that range.
fn visit_unheld(
    lo: &mut Lowering,
    fx: &mut Fx,
    held: &[(Value, Value)],
    reserved: Option<Value>,
    range: (Value, Value),
    visit: Visit,
) {
    let base = match lo.ck.db.target.arch() {
        Some(Arch::Amd64) => {
            let low = fx.ptr(LOW_MEMORY as u64);
            fx.b.ins().umax(range.0, low)
        }
        _ => range.0,
    };
    let reachable = fx.ptr(REACHABLE);
    let end = fx.b.ins().umin(range.1, reachable);
    fx.repeat(&[base], |fx, state| {
        let rest = (state[0], end);
        let mut lowest = (end, end);
        for range in held {
            lowest = lowest_overlap(fx, lowest, *range, rest);
        }
        if let Some(reserved) = reserved {
            let found = fx.repeat(&[reserved, lowest.0, lowest.1], |fx, state| {
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
            lowest = (found[1], found[2]);
        }
        visit(lo, fx, rest.0, lowest.0);
        let after = fx.b.ins().umax(rest.0, lowest.1);
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

/// A walk of the device tree's structure block: the strings block naming its properties, the
/// memory reservation block, and the variables holding the depth of the node it is in, whether
/// that node is a memory node, and the root's cells of an address and of a size.
#[derive(Clone, Copy)]
struct TreeWalk {
    strings: Value,
    reserved: Value,
    depth: Variable,
    memory: Variable,
    address_cells: Variable,
    size_cells: Variable,
}

/// The usable memory of `aarch64-bare`: the ranges in the `reg` property of each memory node of
/// the device tree, a child of its root named `memory` or `memory@` and a unit address, read in
/// the root's cells, visited outside the device tree and the `held` ranges.
fn tree_memory(
    lo: &mut Lowering,
    fx: &mut Fx,
    state_address: Value,
    held: &[(Value, Value)],
    visit: Visit,
) {
    let tree = fx.load(PTR, state_address, state::PLATFORM);
    let structure = tree_address(fx, tree, device_tree::STRUCTURE);
    let strings = tree_address(fx, tree, device_tree::STRINGS);
    let reserved = tree_address(fx, tree, device_tree::RESERVED);
    let tree_end = tree_address(fx, tree, device_tree::SIZE);
    let held = [&[(tree, tree_end)], held].concat();
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
            let after = tree_property(lo, fx, walk, &held, visit, at);
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
/// memory node visits the parts of the ranges it lists outside the `held` ranges and the ranges
/// the memory reservation block lists.
fn tree_property(
    lo: &mut Lowering,
    fx: &mut Fx,
    walk: TreeWalk,
    held: &[(Value, Value)],
    visit: Visit,
    at: Value,
) -> Value {
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
            visit_unheld(lo, fx, held, Some(walk.reserved), (base, end), visit);
        });
    });
    let padded = fx.b.ins().iadd_imm_s(len, 3);
    let padded = fx.b.ins().band_imm_s(padded, -4);
    fx.b.ins().iadd(value, padded)
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
