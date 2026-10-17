//! The boot heap a machine image reserves, from which its runtime draws its records and the
//! chunks of cells: a map of its 64 KiB chunks hands out runs of them, and a record is a block of
//! a power-of-two size class behind a 16-byte header naming the class, carved from chunks the map
//! hands out. A freed block joins its class's free list, from which a later record of the class
//! takes it. Every record and chunk is cleared once its lock is released.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, condcodes::IntCC, types};

use super::super::Lowering;
use super::super::fx::{Fx, PTR};
use super::super::runtime::Routine;
use super::super::runtime::memory::CHUNK;
use super::stubs::Stub;

/// The bytes of the boot heap.
const BOOT_HEAP: u64 = 16 * 1024 * 1024;

/// The chunks of the boot heap.
const CHUNKS: u64 = BOOT_HEAP / CHUNK;

/// The header before a block, holding its size class; it keeps usable bytes 16-aligned.
const HEADER: u64 = 16;

/// The smallest size class, whose usable bytes hold a free-list link.
const MIN_CLASS: i64 = 5;

/// The size class of a chunk; blocks of smaller classes are carved from chunks, blocks of it and
/// larger ones are runs of chunks.
const CHUNK_CLASS: i64 = 16;

/// The size classes, from 2^0 to 2^63 bytes, of which `MIN_CLASS` up are used.
const CLASSES: u64 = 64;

/// The offsets of the allocators' state: their spin lock, the next free byte and the end of the
/// chunk blocks are carved from, the chunk map holding a bit per chunk set while it is handed
/// out, and each size class's free-list head.
mod allocators {
    pub const LOCK: u64 = 0;
    pub const CARVE: u64 = 8;
    pub const CARVE_END: u64 = 16;
    pub const MAP: u64 = 24;
    pub const FREE: u64 = MAP + super::CHUNKS / 8;
    pub const SIZE: u64 = FREE + 8 * super::CLASSES;
}

/// A run of chunks handed out: its first chunk's address, its chunk count and a map holding a
/// bit per chunk, set while the chunk is handed out.
#[derive(Clone, Copy)]
pub struct ChunkSupply {
    pub base: Value,
    pub count: Value,
    pub map: Value,
}

/// `RecordAlloc`, `RecordFree` and `RecordResize` over the boot heap.
pub fn define(lo: &mut Lowering, fx: &mut Fx, routine: Routine) {
    let params = fx.entry();
    match routine {
        Routine::RecordAlloc => {
            let address = record_alloc(lo, fx, params[0]);
            fx.b.ins().return_(&[address]);
        }
        Routine::RecordFree => {
            record_free(lo, fx, params[0]);
            fx.b.ins().return_(&[]);
        }
        _ => {
            let address = record_resize(lo, fx, params[0], params[1]);
            fx.b.ins().return_(&[address]);
        }
    }
}

/// `count` zero-filled chunks aligned to their size, zero when the boot heap runs out.
pub fn map_chunks(lo: &mut Lowering, fx: &mut Fx, count: Value) -> Value {
    let state = heap_state(lo, fx);
    let flags = lock(lo, fx, state);
    let supply = boot_supply(lo, fx, state);
    let base = take_chunks(fx, supply, count);
    unlock(lo, fx, state, flags);
    let taken = fx.b.ins().icmp_imm_s(IntCC::NotEqual, base, 0);
    fx.when(taken, |fx| {
        let len = fx.b.ins().ishl_imm_s(count, CHUNK_CLASS);
        clear(lo, fx, base, len);
    });
    base
}

/// Returns `count` chunks from `base` to the boot heap.
pub fn unmap_chunks(lo: &mut Lowering, fx: &mut Fx, base: Value, count: Value) {
    let state = heap_state(lo, fx);
    let flags = lock(lo, fx, state);
    let supply = boot_supply(lo, fx, state);
    give_chunks(fx, supply, base, count);
    unlock(lo, fx, state, flags);
}

/// Gives `count` chunks from `base` back to a supply.
pub fn give_chunks(fx: &mut Fx, supply: ChunkSupply, base: Value, count: Value) {
    let offset = fx.b.ins().isub(base, supply.base);
    let first = fx.b.ins().ushr_imm_s(offset, CHUNK_CLASS);
    let free = fx.int(types::I8, 0);
    let end = fx.b.ins().iadd(first, count);
    fx.each(first, end, |fx, index| mark(fx, supply.map, index, free));
}

/// Takes the first run of `count` chunks a supply has free, or gives zero when none is free. The
/// caller holds the supply's lock and clears the chunks.
pub fn take_chunks(fx: &mut Fx, supply: ChunkSupply, count: Value) -> Value {
    let zero = fx.ptr(0);
    let end = supply.count;
    let found = fx.repeat(&[zero, zero, zero], |fx, held| {
        let (index, run, first) = (held[0], held[1], held[2]);
        let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, index, end);
        let next = fx.choose(
            inside,
            &[PTR, PTR, PTR, types::I8],
            |fx| {
                let (word, mask) = map_bit(fx, supply.map, index);
                let bits = fx.load(PTR, word, 0);
                let taken = fx.b.ins().band(bits, mask);
                let free = fx.b.ins().icmp_imm_s(IntCC::Equal, taken, 0);
                let grown = fx.b.ins().iadd_imm_s(run, 1);
                let none = fx.ptr(0);
                let run = fx.b.ins().select(free, grown, none);
                let started = fx.b.ins().icmp_imm_s(IntCC::Equal, run, 1);
                let first = fx.b.ins().select(started, index, first);
                let done = fx.b.ins().icmp(IntCC::Equal, run, count);
                let more = fx.b.ins().bxor_imm_s(done, 1);
                let index = fx.b.ins().iadd_imm_s(index, 1);
                vec![index, run, first, more]
            },
            |fx| vec![index, fx.ptr(0), first, fx.int(types::I8, 0)],
        );
        (next[3], next[..3].to_vec())
    });
    let (run, first) = (found[1], found[2]);
    let fits = fx.b.ins().icmp(IntCC::Equal, run, count);
    fx.choose(
        fits,
        &[PTR],
        |fx| {
            let taken = fx.int(types::I8, 1);
            let last = fx.b.ins().iadd(first, count);
            fx.each(first, last, |fx, index| mark(fx, supply.map, index, taken));
            let offset = fx.b.ins().ishl_imm_s(first, CHUNK_CLASS);
            vec![fx.b.ins().iadd(supply.base, offset)]
        },
        |fx| vec![fx.ptr(0)],
    )[0]
}

/// Zero-fills `len` bytes from `base`.
pub fn clear(lo: &mut Lowering, fx: &mut Fx, base: Value, len: Value) {
    let zero = fx.ptr(0);
    lo.call_routine(fx, Routine::Fill, &[base, zero, len]);
}

/// The address of the first chunk of the boot heap: the heap is reserved with one chunk more, so
/// that its chunks align to their size wherever the image is loaded.
fn boot_heap(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let reserved = lo.reserved(fx, "_NSboot_heap", BOOT_HEAP + CHUNK, 4_096);
    let bumped = fx.b.ins().iadd_imm_s(reserved, CHUNK as i64 - 1);
    fx.b.ins().band_imm_s(bumped, -(CHUNK as i64))
}

/// The boot heap as a supply of chunks, its map in the allocators' state at `state`.
fn boot_supply(lo: &mut Lowering, fx: &mut Fx, state: Value) -> ChunkSupply {
    ChunkSupply {
        base: boot_heap(lo, fx),
        count: fx.ptr(CHUNKS),
        map: fx.offset(state, allocators::MAP),
    }
}

/// The address of the allocators' state.
fn heap_state(lo: &mut Lowering, fx: &mut Fx) -> Value {
    lo.reserved(fx, "_NSheap_state", allocators::SIZE, 64)
}

/// Takes the allocators' spin lock, which no interrupt fn takes. A kernel's holder disables
/// maskable interrupts, so no preemption leaves the lock held; gives the flags to restore.
fn lock(lo: &mut Lowering, fx: &mut Fx, state: Value) -> Option<Value> {
    let flags = lo.kernel().then(|| lo.call_stub(fx, Stub::IrqSave, &[])[0]);
    let word = fx.offset(state, allocators::LOCK);
    fx.repeat(&[], |fx, _| {
        let zero = fx.int(types::I32, 0);
        let one = fx.int(types::I32, 1);
        let seen =
            fx.b.ins()
                .atomic_cas(MemFlagsData::trusted(), word, zero, one);
        let busy = fx.b.ins().icmp_imm_s(IntCC::NotEqual, seen, 0);
        fx.when(busy, |fx| lo.relax(fx));
        (busy, Vec::new())
    });
    flags
}

fn unlock(lo: &mut Lowering, fx: &mut Fx, state: Value, flags: Option<Value>) {
    let word = fx.offset(state, allocators::LOCK);
    let zero = fx.int(types::I32, 0);
    fx.b.ins().atomic_store(MemFlagsData::trusted(), zero, word);
    if let Some(flags) = flags {
        lo.call_stub(fx, Stub::IrqRestore, &[flags]);
    }
}

/// The word of a chunk map holding a chunk's bit, and the bit's mask.
fn map_bit(fx: &mut Fx, map: Value, index: Value) -> (Value, Value) {
    let word = fx.b.ins().ushr_imm_s(index, 6);
    let word = fx.b.ins().imul_imm_s(word, 8);
    let word = fx.b.ins().iadd(map, word);
    let bit = fx.b.ins().band_imm_s(index, 63);
    let one = fx.ptr(1);
    (word, fx.b.ins().ishl(one, bit))
}

/// Sets a chunk's bit of a chunk map when `taken` is nonzero, clears it otherwise.
fn mark(fx: &mut Fx, map: Value, index: Value, taken: Value) {
    let (word, mask) = map_bit(fx, map, index);
    let bits = fx.load(PTR, word, 0);
    let set = fx.b.ins().bor(bits, mask);
    let cleared = fx.b.ins().band_not(bits, mask);
    let bits = fx.b.ins().select(taken, set, cleared);
    fx.store(bits, word, 0);
}

/// The least size class holding `total` bytes.
fn class_of(fx: &mut Fx, total: Value) -> Value {
    let size = fx.ptr(1 << MIN_CLASS);
    let class = fx.ptr(MIN_CLASS as u64);
    fx.repeat(&[size, class], |fx, state| {
        let (size, class) = (state[0], state[1]);
        let small = fx.b.ins().icmp(IntCC::UnsignedLessThan, size, total);
        let doubled = fx.b.ins().ishl_imm_s(size, 1);
        let next = fx.b.ins().iadd_imm_s(class, 1);
        let size = fx.b.ins().select(small, doubled, size);
        let class = fx.b.ins().select(small, next, class);
        (small, vec![size, class])
    })[1]
}

/// The bytes of a size class's block.
fn block_bytes(fx: &mut Fx, class: Value) -> Value {
    let one = fx.ptr(1);
    fx.b.ins().ishl(one, class)
}

/// A zeroed record of at least `size` usable bytes, zero when the boot heap is exhausted: a block
/// of its class's free list, or one carved anew, from the chunk blocks are carved from or, for a
/// class of a chunk or more, as a run of chunks.
fn record_alloc(lo: &mut Lowering, fx: &mut Fx, size: Value) -> Value {
    let total = fx.b.ins().iadd_imm_s(size, HEADER as i64);
    let class = class_of(fx, total);
    let state = heap_state(lo, fx);
    let flags = lock(lo, fx, state);
    let slot = fx.b.ins().imul_imm_s(class, 8);
    let slot = fx.b.ins().iadd(state, slot);
    let head = fx.load(PTR, slot, allocators::FREE);
    let reuse = fx.b.ins().icmp_imm_s(IntCC::NotEqual, head, 0);
    let bytes = block_bytes(fx, class);
    let block = fx.choose_in(
        lo,
        reuse,
        &[PTR],
        |_, fx| {
            let next = fx.load(PTR, head, HEADER);
            fx.store(next, slot, allocators::FREE);
            vec![head]
        },
        |lo, fx| {
            let large =
                fx.b.ins()
                    .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, class, CHUNK_CLASS);
            fx.choose_in(
                lo,
                large,
                &[PTR],
                |lo, fx| {
                    let count = fx.b.ins().ushr_imm_s(bytes, CHUNK_CLASS);
                    let supply = boot_supply(lo, fx, state);
                    vec![take_chunks(fx, supply, count)]
                },
                |lo, fx| vec![carve(lo, fx, state, bytes)],
            )
        },
    )[0];
    unlock(lo, fx, state, flags);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, block, 0);
    fx.choose_in(
        lo,
        present,
        &[PTR],
        |lo, fx| {
            fx.store(class, block, 0);
            let usable = fx.b.ins().iadd_imm_s(block, HEADER as i64);
            let cleared = fx.b.ins().iadd_imm_s(bytes, -(HEADER as i64));
            clear(lo, fx, usable, cleared);
            vec![usable]
        },
        |_, fx| vec![fx.ptr(0)],
    )[0]
}

/// A block of `bytes`, smaller than a chunk, carved from the chunk blocks are carved from, which
/// a new chunk replaces when the block does not fit in the rest of it; zero when the boot heap is
/// exhausted. The caller holds the lock.
fn carve(lo: &mut Lowering, fx: &mut Fx, state: Value, bytes: Value) -> Value {
    let at = fx.load(PTR, state, allocators::CARVE);
    let end = fx.load(PTR, state, allocators::CARVE_END);
    let after = fx.b.ins().iadd(at, bytes);
    let fits = fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, after, end);
    let started = fx.b.ins().icmp_imm_s(IntCC::NotEqual, at, 0);
    let fits = fx.b.ins().band(fits, started);
    let at = fx.choose_in(
        lo,
        fits,
        &[PTR],
        |_, _| vec![at],
        |lo, fx| {
            let one = fx.ptr(1);
            let supply = boot_supply(lo, fx, state);
            let chunk = take_chunks(fx, supply, one);
            let end = fx.b.ins().iadd_imm_s(chunk, CHUNK as i64);
            let taken = fx.b.ins().icmp_imm_s(IntCC::NotEqual, chunk, 0);
            fx.when(taken, |fx| fx.store(end, state, allocators::CARVE_END));
            vec![chunk]
        },
    )[0];
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, at, 0);
    fx.when(present, |fx| {
        let next = fx.b.ins().iadd(at, bytes);
        fx.store(next, state, allocators::CARVE);
    });
    at
}

/// Returns a block to its size class's free list.
fn record_free(lo: &mut Lowering, fx: &mut Fx, address: Value) {
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, address, 0);
    fx.when(present, |fx| {
        let block = fx.b.ins().iadd_imm_s(address, -(HEADER as i64));
        let class = fx.load(PTR, block, 0);
        let state = heap_state(lo, fx);
        let flags = lock(lo, fx, state);
        let slot = fx.b.ins().imul_imm_s(class, 8);
        let slot = fx.b.ins().iadd(state, slot);
        let head = fx.load(PTR, slot, allocators::FREE);
        fx.store(head, address, 0);
        fx.store(block, slot, allocators::FREE);
        unlock(lo, fx, state, flags);
    });
}

/// A block of at least `size` usable bytes holding the leading bytes of the block at
/// `address`, which is freed; zero on exhaustion, the old block kept.
fn record_resize(lo: &mut Lowering, fx: &mut Fx, address: Value, size: Value) -> Value {
    let grown = record_alloc(lo, fx, size);
    let moved = fx.b.ins().icmp_imm_s(IntCC::NotEqual, grown, 0);
    let held = fx.b.ins().icmp_imm_s(IntCC::NotEqual, address, 0);
    let both = fx.b.ins().band(moved, held);
    fx.when(both, |fx| {
        let block = fx.b.ins().iadd_imm_s(address, -(HEADER as i64));
        let class = fx.load(PTR, block, 0);
        let bytes = block_bytes(fx, class);
        let old = fx.b.ins().iadd_imm_s(bytes, -(HEADER as i64));
        let kept = fx.b.ins().umin(old, size);
        lo.call_routine(fx, Routine::Copy, &[grown, address, kept]);
        record_free(lo, fx, address);
    });
    grown
}
