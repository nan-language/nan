//! Memory: byte copies, fills and comparisons; the cell table with each cell's limit, charge,
//! parent and state; chunks of 64 KiB drawn through a cell and charged to its chain; and the
//! allocations carved from the chunks a domain draws through one cell.
//!
//! A cell handle is the cell's index in its low 32 bits and its generation in its high 32. A
//! cell record is its limit, its charge, its parent's index plus one, its generation, whether it
//! is open and, for a chain whose root cell is over a region, the supply of the region's chunks,
//! which each cell of the chain copies from its parent and draws from instead of the platform. Each drawn run of chunks has a record,
//! found from the address of its first chunk through the chunk map: its cell handle, its domain,
//! its live allocation count, the offset of its free bytes, its chunk count, its links in its
//! domain's list and its address. A run passes to an enclosing domain when a value it holds
//! leaves the domain owning it, and the runs a domain still owns are freed when it ends.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::Lowering;
use super::super::fx::{Fx, PTR};
use super::super::machine::Stub;
use super::super::machine::heap;
use super::{Routine, state, system, tasks};

pub const CHUNK: u64 = 65_536;

/// The cell table's capacity.
pub const CELLS: u64 = 65_536;
pub const CELL_RECORD: u64 = 40;

/// The chunk map: a table of 2^15 pointers, indexed by bits 47 to 32 of a chunk's address, to
/// tables of 2^16 chunk records, indexed by bits 31 to 16.
pub const MAP_ROOTS: u64 = 32_768;
const MAP_LEAVES: u64 = 65_536;

mod cell {
    pub const LIMIT: u64 = 0;
    pub const CHARGE: u64 = 8;
    pub const PARENT: u64 = 16;
    pub const GENERATION: u64 = 20;
    pub const OPEN: u64 = 24;
    pub const SUPPLY: u64 = 32;
}

/// The offsets of a region's supply record: its first chunk's address, its chunk count, then its
/// map of a bit per chunk.
mod supply {
    pub const BASE: u64 = 0;
    pub const COUNT: u64 = 8;
    pub const MAP: u64 = 16;
}

pub mod chunk {
    pub const CELL: u64 = 0;
    pub const DOMAIN: u64 = 8;
    pub const LIVE: u64 = 16;
    pub const USED: u64 = 24;
    pub const COUNT: u64 = 32;
    pub const NEXT: u64 = 40;
    pub const PREVIOUS: u64 = 48;
    pub const BASE: u64 = 56;
    pub const SIZE: u64 = 64;
}

/// A domain's pool of the chunk it carves for one cell: the next pool, the cell handle and the
/// chunk's record.
mod pool {
    pub const NEXT: u64 = 0;
    pub const CELL: u64 = 8;
    pub const CHUNK: u64 = 16;
    pub const SIZE: u64 = 24;
}

/// The byte routines of `mem_op`: the C library's on `x86_64-host`; on a machine target, which
/// has no C library, the copy and fill compiler stubs and a comparison loop.
pub fn bytes(lo: &mut Lowering, fx: &mut Fx, routine: Routine) {
    let params = fx.entry();
    if lo.machine() {
        machine_bytes(lo, fx, routine, &params);
        return;
    }
    match routine {
        Routine::Copy => {
            lo.call_libc(fx, "memmove", &params, &[PTR]);
            fx.b.ins().return_(&[]);
        }
        Routine::Fill => {
            let byte = fx.b.ins().ireduce(types::I32, params[1]);
            lo.call_libc(fx, "memset", &[params[0], byte, params[2]], &[PTR]);
            fx.b.ins().return_(&[]);
        }
        _ => {
            let order = lo.call_libc(fx, "memcmp", &params, &[types::I32]);
            fx.b.ins().return_(&order);
        }
    }
}

/// The byte routines of a machine target.
fn machine_bytes(lo: &mut Lowering, fx: &mut Fx, routine: Routine, params: &[Value]) {
    let flags = super::super::fx::unaligned();
    let zero = fx.ptr(0);
    match routine {
        Routine::Copy => {
            lo.call_stub(fx, Stub::Copy, params);
            fx.b.ins().return_(&[]);
        }
        Routine::Fill => {
            lo.call_stub(fx, Stub::Fill, params);
            fx.b.ins().return_(&[]);
        }
        _ => {
            let (left, right, len) = (params[0], params[1], params[2]);
            let equal = fx.int(types::I32, 0);
            let order = fx.repeat(&[zero, equal], |fx, state| {
                let index = state[0];
                let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, index, len);
                let order = fx.choose(
                    inside,
                    &[types::I32],
                    |fx| {
                        let a = fx.b.ins().iadd(left, index);
                        let b = fx.b.ins().iadd(right, index);
                        let a = fx.b.ins().load(types::I8, flags, a, 0);
                        let b = fx.b.ins().load(types::I8, flags, b, 0);
                        let a = fx.b.ins().uextend(types::I32, a);
                        let b = fx.b.ins().uextend(types::I32, b);
                        vec![fx.b.ins().isub(a, b)]
                    },
                    |fx| vec![fx.int(types::I32, 0)],
                )[0];
                let same = fx.b.ins().icmp_imm_s(IntCC::Equal, order, 0);
                let more = fx.b.ins().band(inside, same);
                let next = fx.b.ins().iadd_imm_s(index, 1);
                (more, vec![next, order])
            });
            fx.b.ins().return_(&[order[1]]);
        }
    }
}

pub fn define(lo: &mut Lowering, fx: &mut Fx, routine: Routine) {
    let params = fx.entry();
    match routine {
        Routine::Alloc | Routine::AllocIn => {
            let domain = match routine {
                Routine::Alloc => tasks::current_domain(lo, fx),
                _ => params[3],
            };
            let state_address = lo.state(fx);
            let lock = fx.offset(state_address, state::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let address = allocate(lo, fx, params[0], params[1], params[2], domain);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            fx.b.ins().return_(&[address]);
        }
        Routine::Free => {
            let state_address = lo.state(fx);
            let lock = fx.offset(state_address, state::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            release(lo, fx, params[0]);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            fx.b.ins().return_(&[]);
        }
        Routine::Split => {
            let state_address = lo.state(fx);
            let lock = fx.offset(state_address, state::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let handle = new_cell(lo, fx, params[1], params[0]);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            fx.b.ins().return_(&[handle]);
        }
        Routine::Close => {
            let state_address = lo.state(fx);
            let lock = fx.offset(state_address, state::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let record = live_cell(lo, fx, params[0]);
            let found = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
            fx.when(found, |fx| {
                let closed = fx.int(types::I32, 0);
                fx.store(closed, record, cell::OPEN);
                let generation = fx.load(types::I32, record, cell::GENERATION);
                let next = fx.b.ins().iadd_imm_s(generation, 1);
                fx.store(next, record, cell::GENERATION);
            });
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            fx.b.ins().return_(&[]);
        }
        Routine::Charged | Routine::Limit => {
            let record = cell_record(lo, fx, params[0]);
            let field = match routine {
                Routine::Charged => cell::CHARGE,
                _ => cell::LIMIT,
            };
            let bytes = fx.load(PTR, record, field);
            fx.b.ins().return_(&[bytes]);
        }
        Routine::TaskEnv => {
            let size = params[0];
            let address = lo.record_alloc(fx, size);
            let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, address, 0);
            fx.when(failed, |fx| {
                super::report::raise_kind(lo, fx, "trap.exhausted");
            });
            fx.b.ins().return_(&[address]);
        }
        Routine::TaskEnvFree => {
            lo.record_free(fx, params[0]);
            fx.b.ins().return_(&[]);
        }
        Routine::Lift => {
            let state_address = lo.state(fx);
            let lock = fx.offset(state_address, state::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let lifted = lift(lo, fx, params[0], params[1]);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            fx.b.ins().return_(&[lifted]);
        }
        _ => unreachable!("a memory routine"),
    }
}

/// The record of a cell by handle, whatever its generation.
fn cell_record(lo: &mut Lowering, fx: &mut Fx, handle: Value) -> Value {
    let index = fx.b.ins().band_imm_s(handle, 0xFFFF_FFFF);
    record_at(lo, fx, index)
}

fn record_at(lo: &mut Lowering, fx: &mut Fx, index: Value) -> Value {
    let state_address = lo.state(fx);
    let cells = fx.load(PTR, state_address, state::CELLS);
    let offset = fx.b.ins().imul_imm_s(index, CELL_RECORD as i64);
    fx.b.ins().iadd(cells, offset)
}

/// The record of a cell whose handle is live, zero for a stale one.
fn live_cell(lo: &mut Lowering, fx: &mut Fx, handle: Value) -> Value {
    let record = cell_record(lo, fx, handle);
    let generation = fx.load(types::I32, record, cell::GENERATION);
    let wanted = fx.b.ins().ushr_imm_s(handle, 32);
    let wanted = fx.b.ins().ireduce(types::I32, wanted);
    let live = fx.b.ins().icmp(IntCC::Equal, generation, wanted);
    let zero = fx.ptr(0);
    fx.b.ins().select(live, record, zero)
}

/// Adds a cell with a limit under a parent handle, or a root cell when the parent is the
/// all-ones handle; gives its handle.
pub fn new_cell(lo: &mut Lowering, fx: &mut Fx, limit: Value, parent: Value) -> Value {
    let state_address = lo.state(fx);
    let count = fx.load(PTR, state_address, state::CELL_COUNT);
    let full =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, count, CELLS as i64);
    fx.when(full, |fx| {
        super::report::raise_kind(lo, fx, "trap.exhausted")
    });
    let next = fx.b.ins().iadd_imm_s(count, 1);
    fx.store(next, state_address, state::CELL_COUNT);
    let record = record_at(lo, fx, count);
    fx.store(limit, record, cell::LIMIT);
    let zero = fx.ptr(0);
    fx.store(zero, record, cell::CHARGE);
    let root = fx.b.ins().icmp_imm_s(IntCC::Equal, parent, -1);
    let parent_index = fx.b.ins().band_imm_s(parent, 0xFFFF_FFFF);
    let parent_index = fx.b.ins().iadd_imm_s(parent_index, 1);
    let parent_index = fx.b.ins().ireduce(types::I32, parent_index);
    let none = fx.int(types::I32, 0);
    let parent_index = fx.b.ins().select(root, none, parent_index);
    fx.store(parent_index, record, cell::PARENT);
    fx.store(none, record, cell::GENERATION);
    let open = fx.int(types::I32, 1);
    fx.store(open, record, cell::OPEN);
    let supply = fx.choose_in(
        lo,
        root,
        &[PTR],
        |_, fx| vec![fx.ptr(0)],
        |lo, fx| {
            let index = fx.b.ins().band_imm_s(parent, 0xFFFF_FFFF);
            let parent_record = record_at(lo, fx, index);
            vec![fx.load(PTR, parent_record, cell::SUPPLY)]
        },
    )[0];
    fx.store(supply, record, cell::SUPPLY);
    count
}

/// Adds a root cell whose supply is the `count` chunks from `first` and whose limit is their
/// size; gives its handle. The caller holds the lock.
pub fn region_cell(lo: &mut Lowering, fx: &mut Fx, first: Value, count: Value) -> Value {
    let limit = fx.b.ins().imul_imm_s(count, CHUNK as i64);
    let root = fx.int(PTR, -1);
    let handle = new_cell(lo, fx, limit, root);
    let words = fx.b.ins().iadd_imm_s(count, 63);
    let words = fx.b.ins().ushr_imm_s(words, 6);
    let size = fx.b.ins().imul_imm_s(words, 8);
    let size = fx.b.ins().iadd_imm_s(size, supply::MAP as i64);
    let record = lo.record_alloc(fx, size);
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
    fx.when(missing, |fx| {
        super::report::raise_kind(lo, fx, "trap.exhausted")
    });
    fx.store(first, record, supply::BASE);
    fx.store(count, record, supply::COUNT);
    let cell = cell_record(lo, fx, handle);
    fx.store(record, cell, cell::SUPPLY);
    handle
}

/// A region's supply by its record.
fn region_supply(fx: &mut Fx, record: Value) -> heap::ChunkSupply {
    heap::ChunkSupply {
        base: fx.load(PTR, record, supply::BASE),
        count: fx.load(PTR, record, supply::COUNT),
        map: fx.offset(record, supply::MAP),
    }
}

/// Draws `count` chunks through a cell: charges every cell of its chain and maps them, or gives
/// zero when a cell of the chain is closed or would pass its limit, or the supply runs out.
fn draw(lo: &mut Lowering, fx: &mut Fx, handle: Value, count: Value) -> Value {
    let first = live_cell(lo, fx, handle);
    let bytes = fx.b.ins().imul_imm_s(count, CHUNK as i64);
    let fits = fx.b.ins().icmp_imm_s(IntCC::NotEqual, first, 0);
    let fits = chain_fits(lo, fx, first, bytes, fits);
    let mapped = fx.choose(
        fits,
        &[PTR],
        |fx| {
            let region = fx.load(PTR, first, cell::SUPPLY);
            let platform = fx.b.ins().icmp_imm_s(IntCC::Equal, region, 0);
            fx.choose_in(
                lo,
                platform,
                &[PTR],
                |lo, fx| vec![system::map_chunks(lo, fx, count)],
                |lo, fx| {
                    let supply = region_supply(fx, region);
                    let base = heap::take_chunks(fx, supply, count);
                    let taken = fx.b.ins().icmp_imm_s(IntCC::NotEqual, base, 0);
                    fx.when(taken, |fx| {
                        let len = fx.b.ins().imul_imm_s(count, CHUNK as i64);
                        heap::clear(lo, fx, base, len);
                    });
                    vec![base]
                },
            )
        },
        |fx| vec![fx.ptr(0)],
    )[0];
    let drawn = fx.b.ins().icmp_imm_s(IntCC::NotEqual, mapped, 0);
    fx.when(drawn, |fx| {
        walk_chain(lo, fx, first, |fx, record| {
            let charge = fx.load(PTR, record, cell::CHARGE);
            let charged = fx.b.ins().iadd(charge, bytes);
            fx.store(charged, record, cell::CHARGE);
        });
    });
    mapped
}

/// Whether every cell of the chain from a record is open and stays within its limit with
/// `bytes` more charged.
fn chain_fits(lo: &mut Lowering, fx: &mut Fx, first: Value, bytes: Value, start: Value) -> Value {
    let state_address = lo.state(fx);
    let cells = fx.load(PTR, state_address, state::CELLS);
    let result = fx.repeat(&[first, start], |fx, state| {
        let (record, fits) = (state[0], state[1]);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
        let checked = fx.choose(
            present,
            &[PTR, types::I8],
            |fx| {
                let open = fx.load(types::I32, record, cell::OPEN);
                let open = fx.b.ins().icmp_imm_s(IntCC::NotEqual, open, 0);
                let limit = fx.load(PTR, record, cell::LIMIT);
                let charge = fx.load(PTR, record, cell::CHARGE);
                let room = fx.b.ins().isub(limit, charge);
                let within = fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, bytes, room);
                let below =
                    fx.b.ins()
                        .icmp(IntCC::UnsignedLessThanOrEqual, charge, limit);
                let ok = fx.b.ins().band(open, within);
                let ok = fx.b.ins().band(ok, below);
                let fits = fx.b.ins().band(fits, ok);
                let parent = fx.load(types::I32, record, cell::PARENT);
                let parent = fx.b.ins().uextend(PTR, parent);
                let has_parent = fx.b.ins().icmp_imm_s(IntCC::NotEqual, parent, 0);
                let index = fx.b.ins().iadd_imm_s(parent, -1);
                let offset = fx.b.ins().imul_imm_s(index, CELL_RECORD as i64);
                let next = fx.b.ins().iadd(cells, offset);
                let zero = fx.ptr(0);
                let next = fx.b.ins().select(has_parent, next, zero);
                vec![next, fits]
            },
            |fx| vec![fx.ptr(0), fits],
        );
        let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, checked[0], 0);
        (more, checked)
    });
    result[1]
}

/// Runs `visit` on every cell record of the chain from a record.
fn walk_chain(lo: &mut Lowering, fx: &mut Fx, first: Value, visit: impl FnOnce(&mut Fx, Value)) {
    let state_address = lo.state(fx);
    let cells = fx.load(PTR, state_address, state::CELLS);
    let mut visit = Some(visit);
    fx.repeat(&[first], |fx, state| {
        let record = state[0];
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
        let next = fx.choose(
            present,
            &[PTR],
            |fx| {
                if let Some(visit) = visit.take() {
                    visit(fx, record);
                }
                let parent = fx.load(types::I32, record, cell::PARENT);
                let parent = fx.b.ins().uextend(PTR, parent);
                let has_parent = fx.b.ins().icmp_imm_s(IntCC::NotEqual, parent, 0);
                let index = fx.b.ins().iadd_imm_s(parent, -1);
                let offset = fx.b.ins().imul_imm_s(index, CELL_RECORD as i64);
                let next = fx.b.ins().iadd(cells, offset);
                let zero = fx.ptr(0);
                vec![fx.b.ins().select(has_parent, next, zero)]
            },
            |fx| vec![fx.ptr(0)],
        );
        let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next[0], 0);
        (more, next)
    });
}

/// Subtracts a returned chunk's bytes from every open cell of the chain from a record.
fn uncharge(lo: &mut Lowering, fx: &mut Fx, first: Value, bytes: Value) {
    walk_chain(lo, fx, first, |fx, record| {
        let open = fx.load(types::I32, record, cell::OPEN);
        let open = fx.b.ins().icmp_imm_s(IntCC::NotEqual, open, 0);
        let charge = fx.load(PTR, record, cell::CHARGE);
        let less = fx.b.ins().isub(charge, bytes);
        let kept = fx.b.ins().select(open, less, charge);
        fx.store(kept, record, cell::CHARGE);
    });
}

/// An allocation of `size` bytes aligned to `align` through a cell, in a chunk of a domain;
/// zero on exhaustion. The caller holds the lock.
fn allocate(
    lo: &mut Lowering,
    fx: &mut Fx,
    handle: Value,
    size: Value,
    align: Value,
    domain: Value,
) -> Value {
    let large =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedGreaterThan, size, CHUNK as i64);
    fx.choose_in(
        lo,
        large,
        &[PTR],
        |lo, fx| {
            let total = fx.b.ins().iadd_imm_s(size, CHUNK as i64 - 1);
            let count = fx.b.ins().ushr_imm_s(total, 16);
            let record = new_chunk(lo, fx, handle, count, domain);
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
            fx.choose(
                present,
                &[PTR],
                |fx| {
                    let one = fx.ptr(1);
                    fx.store(one, record, chunk::LIVE);
                    fx.store(size, record, chunk::USED);
                    vec![fx.load(PTR, record, chunk::BASE)]
                },
                |fx| vec![fx.ptr(0)],
            )
        },
        |lo, fx| vec![carve(lo, fx, handle, size, align, domain)],
    )[0]
}

/// Draws a run of chunks through a cell for a domain, with its record written, mapped and
/// linked; zero on exhaustion.
fn new_chunk(lo: &mut Lowering, fx: &mut Fx, handle: Value, count: Value, domain: Value) -> Value {
    let base = draw(lo, fx, handle, count);
    let drawn = fx.b.ins().icmp_imm_s(IntCC::NotEqual, base, 0);
    fx.choose(
        drawn,
        &[PTR],
        |fx| {
            let size = fx.ptr(chunk::SIZE);
            let record = lo.record_alloc(fx, size);
            let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
            fx.when(missing, |fx| {
                super::report::raise_kind(lo, fx, "trap.exhausted")
            });
            fx.store(handle, record, chunk::CELL);
            fx.store(domain, record, chunk::DOMAIN);
            fx.store(count, record, chunk::COUNT);
            fx.store(base, record, chunk::BASE);
            let head = fx.load(PTR, domain, tasks::domain::CHUNKS);
            fx.store(head, record, chunk::NEXT);
            let has_head = fx.b.ins().icmp_imm_s(IntCC::NotEqual, head, 0);
            fx.when(has_head, |fx| fx.store(record, head, chunk::PREVIOUS));
            fx.store(record, domain, tasks::domain::CHUNKS);
            let leaf = map_leaf(lo, fx, base);
            fx.store(record, leaf, 0);
            vec![record]
        },
        |fx| vec![fx.ptr(0)],
    )[0]
}

/// The address of the chunk map's slot for the chunk at an address, its table made when
/// missing.
fn map_leaf(lo: &mut Lowering, fx: &mut Fx, base: Value) -> Value {
    let state_address = lo.state(fx);
    let roots = fx.load(PTR, state_address, state::CHUNK_MAP);
    let root = fx.b.ins().ushr_imm_s(base, 32);
    let root = fx.b.ins().imul_imm_s(root, 8);
    let root = fx.b.ins().iadd(roots, root);
    let table = fx.load(PTR, root, 0);
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, table, 0);
    let table = fx.choose(
        missing,
        &[PTR],
        |fx| {
            let size = fx.ptr(MAP_LEAVES * 8);
            let table = lo.record_alloc(fx, size);
            let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, table, 0);
            fx.when(failed, |fx| {
                super::report::raise_kind(lo, fx, "trap.exhausted")
            });
            fx.store(table, root, 0);
            vec![table]
        },
        |_| vec![table],
    )[0];
    let leaf = fx.b.ins().ushr_imm_s(base, 16);
    let leaf = fx.b.ins().band_imm_s(leaf, MAP_LEAVES as i64 - 1);
    let leaf = fx.b.ins().imul_imm_s(leaf, 8);
    fx.b.ins().iadd(table, leaf)
}

/// The least alignment of an allocation carved from a chunk on a machine target, which suits
/// devices reaching a DMA buffer of any type.
const MIN_ALIGN: u64 = 16;

/// Carves a small allocation from the chunk a domain's pool for a cell holds, on a machine
/// target at least `MIN_ALIGN`-aligned, drawing a new chunk when it lacks room.
fn carve(
    lo: &mut Lowering,
    fx: &mut Fx,
    handle: Value,
    size: Value,
    align: Value,
    domain: Value,
) -> Value {
    let pool = domain_pool(lo, fx, domain, handle);
    let current = fx.load(PTR, pool, pool::CHUNK);
    let align = match lo.machine() {
        true => {
            let least = fx.ptr(MIN_ALIGN);
            fx.b.ins().umax(align, least)
        }
        false => align,
    };
    let start = |fx: &mut Fx, record: Value| {
        let used = fx.load(PTR, record, chunk::USED);
        let mask = fx.b.ins().iadd_imm_s(align, -1);
        let bumped = fx.b.ins().iadd(used, mask);
        let inverse = fx.b.ins().bnot(mask);
        fx.b.ins().band(bumped, inverse)
    };
    let has_chunk = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
    let fits = fx.choose(
        has_chunk,
        &[types::I8],
        |fx| {
            let offset = start(fx, current);
            let end = fx.b.ins().iadd(offset, size);
            vec![
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedLessThanOrEqual, end, CHUNK as i64),
            ]
        },
        |fx| vec![fx.int(types::I8, 0)],
    )[0];
    let record = fx.choose(
        fits,
        &[PTR],
        |_| vec![current],
        |fx| {
            let one = fx.ptr(1);
            let record = new_chunk(lo, fx, handle, one, domain);
            let drawn = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
            fx.when(drawn, |fx| fx.store(record, pool, pool::CHUNK));
            vec![record]
        },
    )[0];
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
    fx.choose(
        present,
        &[PTR],
        |fx| {
            let offset = start(fx, record);
            let end = fx.b.ins().iadd(offset, size);
            fx.store(end, record, chunk::USED);
            let live = fx.load(PTR, record, chunk::LIVE);
            let live = fx.b.ins().iadd_imm_s(live, 1);
            fx.store(live, record, chunk::LIVE);
            let base = fx.load(PTR, record, chunk::BASE);
            vec![fx.b.ins().iadd(base, offset)]
        },
        |fx| vec![fx.ptr(0)],
    )[0]
}

/// The pool of a domain for a cell handle, added when missing.
fn domain_pool(lo: &mut Lowering, fx: &mut Fx, domain: Value, handle: Value) -> Value {
    let head = fx.load(PTR, domain, tasks::domain::POOLS);
    let found = fx.repeat(&[head], |fx, state| {
        let pool = state[0];
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, pool, 0);
        let next = fx.choose(
            present,
            &[PTR, types::I8],
            |fx| {
                let cell = fx.load(PTR, pool, pool::CELL);
                let same = fx.b.ins().icmp(IntCC::Equal, cell, handle);
                let next = fx.load(PTR, pool, pool::NEXT);
                let next = fx.b.ins().select(same, pool, next);
                let again = fx.b.ins().bxor_imm_s(same, 1);
                vec![next, again]
            },
            |fx| vec![fx.ptr(0), fx.int(types::I8, 0)],
        );
        (next[1], vec![next[0]])
    })[0];
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, found, 0);
    fx.choose(
        missing,
        &[PTR],
        |fx| {
            let size = fx.ptr(pool::SIZE);
            let pool = lo.record_alloc(fx, size);
            let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, pool, 0);
            fx.when(failed, |fx| {
                super::report::raise_kind(lo, fx, "trap.exhausted")
            });
            fx.store(handle, pool, pool::CELL);
            let head = fx.load(PTR, domain, tasks::domain::POOLS);
            fx.store(head, pool, pool::NEXT);
            fx.store(pool, domain, tasks::domain::POOLS);
            vec![pool]
        },
        |_| vec![found],
    )[0]
}

/// Frees an allocation: its chunk returns once none of its allocations is live. The caller
/// holds the lock.
fn release(lo: &mut Lowering, fx: &mut Fx, address: Value) {
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, address, 0);
    fx.when(present, |fx| {
        let base = fx.b.ins().band_imm_s(address, !(CHUNK as i64 - 1));
        let leaf = map_leaf(lo, fx, base);
        let record = fx.load(PTR, leaf, 0);
        let live = fx.load(PTR, record, chunk::LIVE);
        let live = fx.b.ins().iadd_imm_s(live, -1);
        fx.store(live, record, chunk::LIVE);
        let empty = fx.b.ins().icmp_imm_s(IntCC::Equal, live, 0);
        fx.when(empty, |fx| return_chunk(lo, fx, record));
    });
}

/// Returns a run of chunks by its record: takes it out of its domain, uncharges its chain,
/// returns it to its chain's supply and drops its record. The caller holds the lock.
pub fn return_chunk(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    detach(fx, record);
    let handle = fx.load(PTR, record, chunk::CELL);
    let cell = cell_record(lo, fx, handle);
    let count = fx.load(PTR, record, chunk::COUNT);
    let bytes = fx.b.ins().imul_imm_s(count, CHUNK as i64);
    uncharge(lo, fx, cell, bytes);
    let base = fx.load(PTR, record, chunk::BASE);
    let leaf = map_leaf(lo, fx, base);
    let zero = fx.ptr(0);
    fx.store(zero, leaf, 0);
    let region = fx.load(PTR, cell, cell::SUPPLY);
    let platform = fx.b.ins().icmp_imm_s(IntCC::Equal, region, 0);
    fx.choose_in(
        lo,
        platform,
        &[],
        |lo, fx| {
            system::unmap_chunks(lo, fx, base, count);
            Vec::new()
        },
        |_, fx| {
            let supply = region_supply(fx, region);
            heap::give_chunks(fx, supply, base, count);
            Vec::new()
        },
    );
    lo.record_free(fx, record);
}

/// Takes a run of chunks out of its domain's list and out of the pool carving it.
fn detach(fx: &mut Fx, record: Value) {
    let domain = fx.load(PTR, record, chunk::DOMAIN);
    let next = fx.load(PTR, record, chunk::NEXT);
    let previous = fx.load(PTR, record, chunk::PREVIOUS);
    let has_previous = fx.b.ins().icmp_imm_s(IntCC::NotEqual, previous, 0);
    fx.choose(
        has_previous,
        &[],
        |fx| {
            fx.store(next, previous, chunk::NEXT);
            Vec::new()
        },
        |fx| {
            fx.store(next, domain, tasks::domain::CHUNKS);
            Vec::new()
        },
    );
    let has_next = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next, 0);
    fx.when(has_next, |fx| fx.store(previous, next, chunk::PREVIOUS));
    let head = fx.load(PTR, domain, tasks::domain::POOLS);
    fx.repeat(&[head], |fx, state| {
        let pool = state[0];
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, pool, 0);
        let next = fx.choose(
            present,
            &[PTR],
            |fx| {
                let held = fx.load(PTR, pool, pool::CHUNK);
                let same = fx.b.ins().icmp(IntCC::Equal, held, record);
                fx.when(same, |fx| {
                    let zero = fx.ptr(0);
                    fx.store(zero, pool, pool::CHUNK);
                });
                vec![fx.load(PTR, pool, pool::NEXT)]
            },
            |fx| vec![fx.ptr(0)],
        );
        let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next[0], 0);
        (more, next)
    });
}

/// The record of the run of chunks an allocation lies at the start of, zero for an address no
/// chunk holds.
fn chunk_of(lo: &mut Lowering, fx: &mut Fx, address: Value) -> Value {
    let root = fx.b.ins().ushr_imm_s(address, 32);
    let mapped =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedLessThan, root, MAP_ROOTS as i64);
    fx.choose_in(
        lo,
        mapped,
        &[PTR],
        |lo, fx| {
            let state_address = lo.state(fx);
            let roots = fx.load(PTR, state_address, state::CHUNK_MAP);
            let slot = fx.b.ins().imul_imm_s(root, 8);
            let slot = fx.b.ins().iadd(roots, slot);
            let table = fx.load(PTR, slot, 0);
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, table, 0);
            fx.choose(
                present,
                &[PTR],
                |fx| {
                    let leaf = fx.b.ins().ushr_imm_s(address, 16);
                    let leaf = fx.b.ins().band_imm_s(leaf, MAP_LEAVES as i64 - 1);
                    let leaf = fx.b.ins().imul_imm_s(leaf, 8);
                    let leaf = fx.b.ins().iadd(table, leaf);
                    vec![fx.load(PTR, leaf, 0)]
                },
                |fx| vec![fx.ptr(0)],
            )
        },
        |_, fx| vec![fx.ptr(0)],
    )[0]
}

/// `Lift(address, domain) -> lifted`: passes the run of chunks an allocation lies in to a
/// domain when a domain nested in it owns the run; nonzero when it passed.
fn lift(lo: &mut Lowering, fx: &mut Fx, address: Value, to: Value) -> Value {
    let record = chunk_of(lo, fx, address);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
    fx.choose(
        present,
        &[types::I8],
        |fx| {
            let owner = fx.load(PTR, record, chunk::DOMAIN);
            let inside = tasks::nested_in(fx, owner, to);
            fx.when(inside, |fx| {
                detach(fx, record);
                fx.store(to, record, chunk::DOMAIN);
                let zero = fx.ptr(0);
                fx.store(zero, record, chunk::PREVIOUS);
                let head = fx.load(PTR, to, tasks::domain::CHUNKS);
                fx.store(head, record, chunk::NEXT);
                let has_head = fx.b.ins().icmp_imm_s(IntCC::NotEqual, head, 0);
                fx.when(has_head, |fx| fx.store(record, head, chunk::PREVIOUS));
                fx.store(record, to, tasks::domain::CHUNKS);
            });
            vec![inside]
        },
        |fx| vec![fx.int(types::I8, 0)],
    )[0]
}

/// Frees every run of chunks a domain owns, and its pools.
pub fn free_chunks(lo: &mut Lowering, fx: &mut Fx, domain: Value) {
    let state_address = lo.state(fx);
    let lock = fx.offset(state_address, state::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    fx.repeat(&[], |fx, _| {
        let head = fx.load(PTR, domain, tasks::domain::CHUNKS);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, head, 0);
        fx.when(present, |fx| return_chunk(lo, fx, head));
        (present, Vec::new())
    });
    let pools = fx.load(PTR, domain, tasks::domain::POOLS);
    fx.repeat(&[pools], |fx, state| {
        let pool = state[0];
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, pool, 0);
        let next = fx.choose_in(
            lo,
            present,
            &[PTR],
            |lo, fx| {
                let next = fx.load(PTR, pool, pool::NEXT);
                lo.record_free(fx, pool);
                vec![next]
            },
            |_, fx| vec![fx.ptr(0)],
        );
        (present, next)
    });
    lo.call_routine(fx, Routine::Unlock, &[lock]);
}
