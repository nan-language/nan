//! Capability tables: the entry each Copy capability's handle names, with its generation, its
//! kind, the entry it derives from and the authority it records, and the minting of the root
//! grant's capabilities. The outbox table's slots are entries of the same table.
//!
//! A capability handle is the entry's index in its low 32 bits and its generation in its high
//! 32. An entry record is its generation, whether it is live (2 while its revocation spreads),
//! its kind, its source's index plus one (the next free entry's for a free one), and four words
//! of state; the second and third words of a `Fs` or `Net` entry hold the address and length of
//! a text the entry owns. A live entry's state never changes once its handle is returned, so an
//! operation reads its words through a snapshot that rechecks the handle once they are read; a
//! text, which revocation frees, is read only under the lock. An outbox's state is its runner's
//! record, its key and whether it is a keyed task's. An executor's state is its cell, the handle
//! of the clock it records, its resolution in nanoseconds, zero standing for one, and its share,
//! packed; its entry also counts the nanoseconds its tasks and those of the executors deriving
//! from it ran, and names the serial executor whose turn its tasks run in, which a serial
//! executor's entry keeps with its waiting tasks, its seed and its picks. A `Clock`
//! entry's state is the nanosecond of the clock that delays count at which its root was minted,
//! then its resolution in nanoseconds, zero standing for one; it also heads the deadlines in
//! flight that `task::within` calls armed through it, linked through their timers. A
//! `WallClock` entry's state is how many nanoseconds the wall clock reads ahead of the clock
//! that delays count, which a host's wall clock reads from the platform instead, then its
//! resolution. An entry of a capability kind holds its minter's state record, then the two words
//! of its record.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, condcodes::IntCC, types};

use super::super::body::Held;
use super::super::call::{Called, Invoked};
use super::super::fx::{Fx, PTR};
use super::super::supplied::tasks::components;
use super::super::{Instance, Lowering};
use super::{Routine, memory, state, tasks};
use crate::check::db::ItemId;
use crate::workspace::Kind;

/// The capability table's capacity.
pub const ENTRIES: u64 = 65_536;
pub const ENTRY_RECORD: u64 = 96;

pub mod entry {
    pub const GENERATION: u64 = 0;
    pub const LIVE: u64 = 4;
    pub const KIND: u64 = 8;
    pub const SOURCE: u64 = 12;
    pub const STATE: u64 = 16;
    pub const DEADLINES: u64 = 48;
    pub const USED: u64 = 48;
    pub const SERIAL: u64 = 56;
    pub const TURN: u64 = 64;
    pub const WAITING: u64 = 72;
    pub const SEED: u64 = 80;
    pub const PICKS: u64 = 88;
}

/// The links of a deadline's timer in its clock entry's deadlines and the clock's handle, after
/// its timeout's time.
pub mod deadline {
    use super::super::tasks::operation;
    pub const NEXT: u64 = operation::SIZE + 16;
    pub const PREVIOUS: u64 = operation::SIZE + 24;
    pub const CLOCK: u64 = operation::SIZE + 32;
    pub const SIZE: u64 = operation::SIZE + 40;
    /// The previous link of a timer its clock's revocation took out of the deadlines.
    pub const DETACHED: i64 = -1;
}

/// The kinds of capability entries.
pub mod kind {
    pub const FS: i64 = 1;
    pub const NET: i64 = 2;
    pub const CLOCK: i64 = 3;
    pub const RANDOM: i64 = 4;
    pub const LOG: i64 = 5;
    pub const STDIO: i64 = 6;
    pub const EXECUTOR: i64 = 7;
    pub const OUTBOX: i64 = 8;
    pub const WALL: i64 = 9;
    pub const KIND: i64 = 10;
}

/// The state word of a `Clock` or `WallClock` entry holding its resolution, and that of an
/// executor's entry.
pub const RESOLUTION: u64 = entry::STATE + 8;
pub const EXECUTOR_RESOLUTION: u64 = entry::STATE + 16;

/// The state word of an executor's entry holding its share, and the share of a root grant's
/// executor: a weight of 1 in class 0 reserving nothing.
pub const EXECUTOR_SHARE: u64 = entry::STATE + 24;
const ROOT_SHARE: u64 = 1;

/// The working directory's descriptor for the `*at` system calls.
const WORKING_DIRECTORY: i64 = -100;

pub fn define(lo: &mut Lowering, fx: &mut Fx, routine: Routine) {
    let params = fx.entry();
    match routine {
        Routine::Derive => {
            let state_address = lo.state(fx);
            let lock = fx.offset(state_address, state::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let source = live_entry(lo, fx, params[0]);
            let live = fx.b.ins().icmp_imm_s(IntCC::NotEqual, source, 0);
            let handle = fx.choose(
                live,
                &[PTR],
                |fx| {
                    let kind = fx.load(types::I32, source, entry::KIND);
                    let kind = fx.b.ins().uextend(PTR, kind);
                    let handle = new_entry(lo, fx, kind);
                    let record = entry_at(lo, fx, handle);
                    for word in 0..4 {
                        let value = fx.load(PTR, source, entry::STATE + 8 * word);
                        fx.store(value, record, entry::STATE + 8 * word);
                    }
                    let serial = fx.load(PTR, source, entry::SERIAL);
                    fx.store(serial, record, entry::SERIAL);
                    let index = fx.b.ins().band_imm_s(params[0], 0xFFFF_FFFF);
                    let index = fx.b.ins().iadd_imm_s(index, 1);
                    let index = fx.b.ins().ireduce(types::I32, index);
                    fx.store(index, record, entry::SOURCE);
                    let raised = fx.b.ins().iadd(record, params[1]);
                    let value = fx.load(PTR, raised, 0);
                    let value = fx.b.ins().umax(value, params[2]);
                    fx.store(value, raised, 0);
                    let owns = owns_text(fx, record);
                    fx.when(owns, |fx| derive_text(lo, fx, record, params[3], params[4]));
                    vec![handle]
                },
                |fx| vec![fx.int(PTR, -1)],
            )[0];
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            fx.b.ins().return_(&[handle]);
        }
        Routine::Revoke => {
            let state_address = lo.state(fx);
            let lock = fx.offset(state_address, state::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            revoke(lo, fx, params[0]);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            fx.b.ins().return_(&[]);
        }
        Routine::KindMint => {
            let state_address = lo.state(fx);
            let lock = fx.offset(state_address, state::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let kind = fx.ptr(kind::KIND as u64);
            let handle = new_entry(lo, fx, kind);
            let record = entry_at(lo, fx, handle);
            for (word, value) in params.iter().enumerate() {
                fx.store(*value, record, entry::STATE + 8 * word as u64);
            }
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            fx.b.ins().return_(&[handle]);
        }
        Routine::KindPin => {
            let state_address = lo.state(fx);
            let lock = fx.offset(state_address, state::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let record = live_entry(lo, fx, params[0]);
            let live = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
            let minter = fx.choose(
                live,
                &[PTR],
                |fx| {
                    let minter = fx.load(PTR, record, entry::STATE);
                    let one = fx.ptr(1);
                    super::fetch_add(fx, minter, one);
                    for word in [1, 2] {
                        let value = fx.load(PTR, record, entry::STATE + 8 * word);
                        fx.store(value, params[1], 8 * (word - 1));
                    }
                    vec![minter]
                },
                |fx| vec![fx.ptr(0)],
            )[0];
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            fx.b.ins().return_(&[minter]);
        }
        Routine::KindDerive => {
            let state_address = lo.state(fx);
            let lock = fx.offset(state_address, state::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let source = live_entry(lo, fx, params[0]);
            let live = fx.b.ins().icmp_imm_s(IntCC::NotEqual, source, 0);
            let handle = fx.choose(
                live,
                &[PTR],
                |fx| {
                    let kind = fx.ptr(kind::KIND as u64);
                    let handle = new_entry(lo, fx, kind);
                    let record = entry_at(lo, fx, handle);
                    let minter = fx.load(PTR, source, entry::STATE);
                    fx.store(minter, record, entry::STATE);
                    fx.store(params[1], record, entry::STATE + 8);
                    fx.store(params[2], record, entry::STATE + 16);
                    let index = fx.b.ins().band_imm_s(params[0], 0xFFFF_FFFF);
                    let index = fx.b.ins().iadd_imm_s(index, 1);
                    let index = fx.b.ins().ireduce(types::I32, index);
                    fx.store(index, record, entry::SOURCE);
                    vec![handle]
                },
                |fx| vec![fx.int(PTR, -1)],
            )[0];
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            fx.b.ins().return_(&[handle]);
        }
        Routine::KindRevoke => {
            let state_address = lo.state(fx);
            let lock = fx.offset(state_address, state::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let record = live_entry(lo, fx, params[0]);
            let live = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
            fx.when(live, |fx| {
                let minter = fx.load(PTR, record, entry::STATE);
                let belongs = fx.b.ins().icmp(IntCC::Equal, minter, params[1]);
                fx.when(belongs, |fx| revoke(lo, fx, params[0]));
            });
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            fx.b.ins().return_(&[]);
        }
        Routine::KindClose => {
            let state_address = lo.state(fx);
            let lock = fx.offset(state_address, state::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let count = fx.load(PTR, state_address, state::ENTRY_COUNT);
            let zero = fx.ptr(0);
            fx.each(zero, count, |fx, index| {
                let record = entry_at(lo, fx, index);
                let kind = fx.load(types::I32, record, entry::KIND);
                let of_kind = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, kind::KIND);
                let live = fx.load(types::I32, record, entry::LIVE);
                let live = fx.b.ins().icmp_imm_s(IntCC::Equal, live, 1);
                let minter = fx.load(PTR, record, entry::STATE);
                let belongs = fx.b.ins().icmp(IntCC::Equal, minter, params[0]);
                let revoked = fx.b.ins().band(of_kind, live);
                let revoked = fx.b.ins().band(revoked, belongs);
                fx.when(revoked, |fx| {
                    let revoking = fx.int(types::I32, 2);
                    fx.store(revoking, record, entry::LIVE);
                });
            });
            sweep(lo, fx);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            fx.b.ins().return_(&[]);
        }
        Routine::MintRoot => mint_root(lo, fx, params[0]),
        Routine::OutboxMint => {
            let state_address = lo.state(fx);
            let lock = fx.offset(state_address, state::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let outbox = fx.ptr(kind::OUTBOX as u64);
            let handle = new_entry(lo, fx, outbox);
            let record = entry_at(lo, fx, handle);
            for (word, value) in params.iter().enumerate() {
                fx.store(*value, record, entry::STATE + 8 * word as u64);
            }
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            fx.b.ins().return_(&[handle]);
        }
        Routine::OutboxRetire => {
            let state_address = lo.state(fx);
            let lock = fx.offset(state_address, state::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let record = live_entry(lo, fx, params[0]);
            let live = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
            fx.when(live, |fx| {
                let index = fx.b.ins().band_imm_s(params[0], 0xFFFF_FFFF);
                free_entry(lo, fx, record, index);
            });
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            fx.b.ins().return_(&[]);
        }
        Routine::OutboxClaim => {
            let state_address = lo.state(fx);
            let lock = fx.offset(state_address, state::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let record = live_entry(lo, fx, params[0]);
            let live = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
            let runner = fx.choose(
                live,
                &[PTR],
                |fx| {
                    let runner = fx.load(PTR, record, entry::STATE);
                    let refs = fx.offset(runner, components::runner::REFS);
                    let one = fx.ptr(1);
                    super::fetch_add(fx, refs, one);
                    vec![runner]
                },
                |fx| vec![fx.ptr(0)],
            )[0];
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            fx.b.ins().return_(&[runner]);
        }
        _ => unreachable!("a capability table routine"),
    }
}

/// Whether an entry's state words hold a text it owns: those of `Fs` and `Net` entries.
fn owns_text(fx: &mut Fx, record: Value) -> Value {
    let kind = fx.load(types::I32, record, entry::KIND);
    let fs = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, kind::FS);
    let net = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, kind::NET);
    fx.b.ins().bor(fs, net)
}

/// Gives a derived `Fs` or `Net` entry a text of its own in runtime memory: its source's, which
/// its state words hold, extended by a given text as a path for `Fs` and replaced by it for
/// `Net`, or kept when the given text's address is zero.
fn derive_text(lo: &mut Lowering, fx: &mut Fx, record: Value, text: Value, len: Value) {
    let source = fx.load(PTR, record, entry::STATE + 8);
    let source_len = fx.load(PTR, record, entry::STATE + 16);
    let given = fx.b.ins().icmp_imm_s(IntCC::NotEqual, text, 0);
    let kind = fx.load(types::I32, record, entry::KIND);
    let fs = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, kind::FS);
    let net = fx.b.ins().bxor_imm_s(fs, 1);
    let replaced = fx.b.ins().band(given, net);
    let zero = fx.ptr(0);
    let head_len = fx.b.ins().select(replaced, zero, source_len);
    let joined = fx.b.ins().band(given, fs);
    let has_head = fx.b.ins().icmp_imm_s(IntCC::NotEqual, source_len, 0);
    let separated = fx.b.ins().band(joined, has_head);
    let separator = fx.b.ins().uextend(PTR, separated);
    let tail_len = fx.b.ins().select(given, len, zero);
    let total = fx.b.ins().iadd(head_len, separator);
    let total = fx.b.ins().iadd(total, tail_len);
    let size = fx.b.ins().iadd_imm_s(total, 1);
    let copy = lo.record_alloc(fx, size);
    let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, copy, 0);
    fx.when(failed, |fx| {
        super::report::raise_kind(lo, fx, "trap.exhausted")
    });
    lo.call_routine(fx, Routine::Copy, &[copy, source, head_len]);
    fx.when(separated, |fx| {
        let at = fx.b.ins().iadd(copy, head_len);
        let slash = fx.int(types::I8, i64::from(b'/'));
        fx.store(slash, at, 0);
    });
    let tail = fx.b.ins().iadd(copy, head_len);
    let tail = fx.b.ins().iadd(tail, separator);
    lo.call_routine(fx, Routine::Copy, &[tail, text, tail_len]);
    fx.store(copy, record, entry::STATE + 8);
    fx.store(total, record, entry::STATE + 16);
}

/// Revokes the live entry a handle names and every entry deriving from it, transitively: each
/// becomes free, its generation advanced and its text freed. The caller holds the lock.
fn revoke(lo: &mut Lowering, fx: &mut Fx, handle: Value) {
    let target = live_entry(lo, fx, handle);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, target, 0);
    fx.when(present, |fx| {
        let revoking = fx.int(types::I32, 2);
        fx.store(revoking, target, entry::LIVE);
        sweep(lo, fx);
    });
}

/// Revokes the entries being revoked and every live entry deriving from one, transitively:
/// each becomes free, its generation advanced and its text freed. The caller holds the lock.
fn sweep(lo: &mut Lowering, fx: &mut Fx) {
    let state_address = lo.state(fx);
    let count = fx.load(PTR, state_address, state::ENTRY_COUNT);
    let no = fx.int(types::I8, 0);
    fx.repeat(&[], |fx, _| {
        let zero = fx.ptr(0);
        let changed = fx.var(types::I8, no);
        fx.each(zero, count, |fx, index| {
            let record = entry_at(lo, fx, index);
            let live = fx.load(types::I32, record, entry::LIVE);
            let live = fx.b.ins().icmp_imm_s(IntCC::Equal, live, 1);
            let source = fx.load(types::I32, record, entry::SOURCE);
            let derived = fx.b.ins().icmp_imm_s(IntCC::NotEqual, source, 0);
            let candidate = fx.b.ins().band(live, derived);
            fx.when(candidate, |fx| {
                let source = fx.b.ins().uextend(PTR, source);
                let source = fx.b.ins().iadd_imm_s(source, -1);
                let parent = entry_at(lo, fx, source);
                let parent_live = fx.load(types::I32, parent, entry::LIVE);
                let spreading = fx.b.ins().icmp_imm_s(IntCC::Equal, parent_live, 2);
                fx.when(spreading, |fx| {
                    let revoking = fx.int(types::I32, 2);
                    fx.store(revoking, record, entry::LIVE);
                    let yes = fx.int(types::I8, 1);
                    fx.b.def_var(changed, yes);
                });
            });
        });
        (fx.b.use_var(changed), Vec::new())
    });
    let zero = fx.ptr(0);
    fx.each(zero, count, |fx, index| {
        let record = entry_at(lo, fx, index);
        let live = fx.load(types::I32, record, entry::LIVE);
        let revoked = fx.b.ins().icmp_imm_s(IntCC::Equal, live, 2);
        fx.when(revoked, |fx| {
            expire_deadlines(lo, fx, record);
            super::serial::release_waiting(lo, fx, record);
            let owns = owns_text(fx, record);
            fx.when(owns, |fx| {
                let text = fx.load(PTR, record, entry::STATE + 8);
                lo.record_free(fx, text);
            });
            free_entry(lo, fx, record, index);
        });
    });
}

/// Passes every deadline armed through a revoked entry of a clock: each one's region is cancelled
/// and its timer leaves the entry's deadlines. The caller holds the lock.
fn expire_deadlines(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let first = fx.load(PTR, record, entry::DEADLINES);
    let entry_kind = fx.load(types::I32, record, entry::KIND);
    let clock = fx.b.ins().icmp_imm_s(IntCC::Equal, entry_kind, kind::CLOCK);
    let armed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, first, 0);
    let armed = fx.b.ins().band(clock, armed);
    fx.when(armed, |fx| {
        fx.repeat(&[first], |fx, state| {
            let timer = state[0];
            let next = fx.load(PTR, timer, deadline::NEXT);
            let region = fx.load(PTR, timer, super::tasks::operation::TASK);
            lo.call_routine(fx, Routine::Cancel, &[region]);
            let detached = fx.int(PTR, deadline::DETACHED);
            fx.store(detached, timer, deadline::PREVIOUS);
            let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next, 0);
            (more, vec![next])
        });
        let zero = fx.ptr(0);
        fx.store(zero, record, entry::DEADLINES);
    });
}

/// Links a deadline's timer into the deadlines of its live clock entry: whether the clock is
/// live, and the nanosecond at which its root was minted and its resolution, meaningless when
/// it is stale.
pub fn link_deadline(
    lo: &mut Lowering,
    fx: &mut Fx,
    clock: Value,
    timer: Value,
) -> (Value, [Value; 2]) {
    let state_address = lo.state(fx);
    let lock = fx.offset(state_address, state::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    fx.store(clock, timer, deadline::CLOCK);
    let slot = entry_at(lo, fx, clock);
    let minted = fx.load(PTR, slot, entry::STATE);
    let resolution = fx.load(PTR, slot, RESOLUTION);
    let record = live_entry(lo, fx, clock);
    let live = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
    fx.when(live, |fx| {
        let first = fx.load(PTR, record, entry::DEADLINES);
        fx.store(first, timer, deadline::NEXT);
        let zero = fx.ptr(0);
        fx.store(zero, timer, deadline::PREVIOUS);
        let has_first = fx.b.ins().icmp_imm_s(IntCC::NotEqual, first, 0);
        fx.when(has_first, |fx| fx.store(timer, first, deadline::PREVIOUS));
        fx.store(timer, record, entry::DEADLINES);
    });
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    (live, [minted, resolution])
}

/// Takes a deadline's timer out of its clock entry's deadlines, unless the clock's revocation
/// already did.
pub fn unlink_deadline(lo: &mut Lowering, fx: &mut Fx, timer: Value) {
    let clock = fx.load(PTR, timer, deadline::CLOCK);
    let state_address = lo.state(fx);
    let lock = fx.offset(state_address, state::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let previous = fx.load(PTR, timer, deadline::PREVIOUS);
    let linked =
        fx.b.ins()
            .icmp_imm_s(IntCC::NotEqual, previous, deadline::DETACHED);
    fx.when(linked, |fx| {
        let next = fx.load(PTR, timer, deadline::NEXT);
        let first = fx.b.ins().icmp_imm_s(IntCC::Equal, previous, 0);
        fx.choose(
            first,
            &[],
            |fx| {
                let record = entry_at(lo, fx, clock);
                fx.store(next, record, entry::DEADLINES);
                Vec::new()
            },
            |fx| {
                fx.store(next, previous, deadline::NEXT);
                Vec::new()
            },
        );
        let has_next = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next, 0);
        fx.when(has_next, |fx| fx.store(previous, next, deadline::PREVIOUS));
    });
    lo.call_routine(fx, Routine::Unlock, &[lock]);
}

/// Frees an entry: its generation advances and it heads the free entries, unless that
/// generation is 2^32 − 1, which retires it. The caller holds the lock.
fn free_entry(lo: &mut Lowering, fx: &mut Fx, record: Value, index: Value) {
    let dead = fx.int(types::I32, 0);
    fx.store(dead, record, entry::LIVE);
    let generation = fx.load(types::I32, record, entry::GENERATION);
    let generation = fx.b.ins().iadd_imm_s(generation, 1);
    fx.store(generation, record, entry::GENERATION);
    let reusable = fx.b.ins().icmp_imm_s(IntCC::NotEqual, generation, -1);
    fx.when(reusable, |fx| {
        let state_address = lo.state(fx);
        let free = fx.load(PTR, state_address, state::FREE_ENTRY);
        let free = fx.b.ins().ireduce(types::I32, free);
        fx.store(free, record, entry::SOURCE);
        let head = fx.b.ins().iadd_imm_s(index, 1);
        fx.store(head, state_address, state::FREE_ENTRY);
    });
}

/// Makes every outbox of a runner stale.
pub fn retire_outboxes(lo: &mut Lowering, fx: &mut Fx, runner: Value) {
    let state_address = lo.state(fx);
    let lock = fx.offset(state_address, state::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let count = fx.load(PTR, state_address, state::ENTRY_COUNT);
    let zero = fx.ptr(0);
    fx.each(zero, count, |fx, index| {
        let record = entry_at(lo, fx, index);
        let kind = fx.load(types::I32, record, entry::KIND);
        let outbox = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, kind::OUTBOX);
        let live = fx.load(types::I32, record, entry::LIVE);
        let live = fx.b.ins().icmp_imm_s(IntCC::NotEqual, live, 0);
        let target = fx.load(PTR, record, entry::STATE);
        let same = fx.b.ins().icmp(IntCC::Equal, target, runner);
        let retired = fx.b.ins().band(outbox, live);
        let retired = fx.b.ins().band(retired, same);
        fx.when(retired, |fx| free_entry(lo, fx, record, index));
    });
    lo.call_routine(fx, Routine::Unlock, &[lock]);
}

/// The record of an entry by handle, whatever its generation.
pub fn entry_at(lo: &mut Lowering, fx: &mut Fx, handle: Value) -> Value {
    let index = fx.b.ins().band_imm_s(handle, 0xFFFF_FFFF);
    let state_address = lo.state(fx);
    let entries = fx.load(PTR, state_address, state::ENTRIES);
    let offset = fx.b.ins().imul_imm_s(index, ENTRY_RECORD as i64);
    fx.b.ins().iadd(entries, offset)
}

/// The record of an entry whose handle is live, zero for a stale one.
pub fn live_entry(lo: &mut Lowering, fx: &mut Fx, handle: Value) -> Value {
    let record = entry_at(lo, fx, handle);
    let generation = fx.load(types::I32, record, entry::GENERATION);
    let wanted = fx.b.ins().ushr_imm_s(handle, 32);
    let wanted = fx.b.ins().ireduce(types::I32, wanted);
    let same = fx.b.ins().icmp(IntCC::Equal, generation, wanted);
    let live = fx.load(types::I32, record, entry::LIVE);
    let live = fx.b.ins().icmp_imm_s(IntCC::NotEqual, live, 0);
    let ok = fx.b.ins().band(same, live);
    let zero = fx.ptr(0);
    fx.b.ins().select(ok, record, zero)
}

/// The state words of the entry a handle names, read without the lock, and whether the handle
/// was still live once they were read, which makes them that entry's.
pub fn snapshot(lo: &mut Lowering, fx: &mut Fx, handle: Value) -> (Value, [Value; 4]) {
    let record = entry_at(lo, fx, handle);
    let flags = MemFlagsData::trusted();
    let words = [0, 1, 2, 3].map(|word| {
        let at = fx.offset(record, entry::STATE + 8 * word);
        fx.b.ins().atomic_load(PTR, flags, at)
    });
    let generation = fx.b.ins().atomic_load(types::I32, flags, record);
    let live_at = fx.offset(record, entry::LIVE);
    let live = fx.b.ins().atomic_load(types::I32, flags, live_at);
    let wanted = fx.b.ins().ushr_imm_s(handle, 32);
    let wanted = fx.b.ins().ireduce(types::I32, wanted);
    let same = fx.b.ins().icmp(IntCC::Equal, generation, wanted);
    let live = fx.b.ins().icmp_imm_s(IntCC::NotEqual, live, 0);
    (fx.b.ins().band(same, live), words)
}

/// The share and the serial executor's handle of the executor's entry a handle names, read
/// without the lock; a root grant's share and none for a stale handle.
pub fn executor_terms(lo: &mut Lowering, fx: &mut Fx, handle: Value) -> (Value, Value) {
    let record = entry_at(lo, fx, handle);
    let flags = MemFlagsData::trusted();
    let [share, serial] = [EXECUTOR_SHARE, entry::SERIAL].map(|word| {
        let at = fx.offset(record, word);
        fx.b.ins().atomic_load(PTR, flags, at)
    });
    let generation = fx.b.ins().atomic_load(types::I32, flags, record);
    let live_at = fx.offset(record, entry::LIVE);
    let live = fx.b.ins().atomic_load(types::I32, flags, live_at);
    let wanted = fx.b.ins().ushr_imm_s(handle, 32);
    let wanted = fx.b.ins().ireduce(types::I32, wanted);
    let same = fx.b.ins().icmp(IntCC::Equal, generation, wanted);
    let live = fx.b.ins().icmp_imm_s(IntCC::NotEqual, live, 0);
    let live = fx.b.ins().band(same, live);
    let root = fx.ptr(ROOT_SHARE);
    let none = fx.ptr(0);
    (
        fx.b.ins().select(live, share, root),
        fx.b.ins().select(live, serial, none),
    )
}

/// A copy in runtime memory of the text of the live entry a handle names, taken under the lock,
/// and its length; zero for a stale handle. The caller frees the copy.
pub fn copy_text(lo: &mut Lowering, fx: &mut Fx, handle: Value) -> (Value, Value) {
    let state_address = lo.state(fx);
    let lock = fx.offset(state_address, state::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let record = live_entry(lo, fx, handle);
    let live = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
    let copy = fx.choose_in(
        lo,
        live,
        &[PTR, PTR],
        |lo, fx| {
            let text = fx.load(PTR, record, entry::STATE + 8);
            let len = fx.load(PTR, record, entry::STATE + 16);
            let size = fx.b.ins().iadd_imm_s(len, 1);
            let copy = lo.record_alloc(fx, size);
            let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, copy, 0);
            fx.when(failed, |fx| {
                super::report::raise_kind(lo, fx, "trap.exhausted")
            });
            lo.call_routine(fx, Routine::Copy, &[copy, text, len]);
            vec![copy, len]
        },
        |_, fx| vec![fx.ptr(0), fx.ptr(0)],
    );
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    (copy[0], copy[1])
}

/// A new live entry of a kind, a free one when any is, by handle; traps when the table is
/// full. The caller holds the lock.
fn new_entry(lo: &mut Lowering, fx: &mut Fx, kind: Value) -> Value {
    let state_address = lo.state(fx);
    let free = fx.load(PTR, state_address, state::FREE_ENTRY);
    let reused = fx.b.ins().icmp_imm_s(IntCC::NotEqual, free, 0);
    let index = fx.choose_in(
        lo,
        reused,
        &[PTR],
        |lo, fx| {
            let index = fx.b.ins().iadd_imm_s(free, -1);
            let record = entry_at(lo, fx, index);
            let next = fx.load(types::I32, record, entry::SOURCE);
            let next = fx.b.ins().uextend(PTR, next);
            fx.store(next, state_address, state::FREE_ENTRY);
            vec![index]
        },
        |lo, fx| {
            let count = fx.load(PTR, state_address, state::ENTRY_COUNT);
            let full =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, count, ENTRIES as i64);
            fx.when(full, |fx| {
                super::report::raise_kind(lo, fx, "trap.exhausted")
            });
            let next = fx.b.ins().iadd_imm_s(count, 1);
            fx.store(next, state_address, state::ENTRY_COUNT);
            vec![count]
        },
    )[0];
    let record = entry_at(lo, fx, index);
    let one = fx.int(types::I32, 1);
    fx.store(one, record, entry::LIVE);
    let kind = fx.b.ins().ireduce(types::I32, kind);
    fx.store(kind, record, entry::KIND);
    let zero32 = fx.int(types::I32, 0);
    fx.store(zero32, record, entry::SOURCE);
    let zero = fx.ptr(0);
    for word in 0..10 {
        fx.store(zero, record, entry::STATE + 8 * word);
    }
    let generation = fx.load(types::I32, record, entry::GENERATION);
    let generation = fx.b.ins().uextend(PTR, generation);
    let generation = fx.b.ins().ishl_imm_s(generation, 32);
    fx.b.ins().bor(index, generation)
}

/// `MintRoot(grant)`: the root cell and the entries of the root grant's capabilities, written
/// into a `host::World` with empty arguments and environment, or into a machine target's grant.
fn mint_root(lo: &mut Lowering, fx: &mut Fx, world: Value) {
    if lo.machine() {
        mint_machine_root(lo, fx, world);
        return;
    }
    let world_ty = lo.std_ty("host", "World", Vec::new());
    let limit = fx.int(PTR, -1);
    let parent = fx.int(PTR, -1);
    let cell = memory::new_cell(lo, fx, limit, parent);
    let now = lo.now_ns(fx);
    let mut handles = Vec::new();
    for (field, entry_kind) in [
        ("fs", kind::FS),
        ("net", kind::NET),
        ("clock", kind::CLOCK),
        ("random", kind::RANDOM),
        ("log", kind::LOG),
        ("stdio", kind::STDIO),
        ("exec", kind::EXECUTOR),
        ("wall", kind::WALL),
    ] {
        let kind_value = fx.ptr(entry_kind as u64);
        let handle = new_entry(lo, fx, kind_value);
        let record = entry_at(lo, fx, handle);
        handles.push((field, handle, record));
    }
    let directory = fx.int(PTR, WORKING_DIRECTORY);
    fx.store(directory, handles[0].2, entry::STATE);
    fx.store(now, handles[2].2, entry::STATE);
    fx.store(cell, handles[6].2, entry::STATE);
    fx.store(handles[2].1, handles[6].2, entry::STATE + 8);
    let share = fx.ptr(ROOT_SHARE);
    fx.store(share, handles[6].2, EXECUTOR_SHARE);
    for (field, handle, _) in &handles {
        let at = lo.layouts.field(lo.ck, &world_ty, None, field).offset;
        fx.store(*handle, world, at);
    }
    grant_memory(lo, fx, world, cell);
    fx.b.ins().return_(&[]);
}

/// A machine target's root grant: a root cell without a limit, whose supply is the boot heap,
/// made the process domain's; an executor drawing through it that records the kernel's clock, a
/// `Clock` entry minted now; a `WallClock` entry when the target's wall clock source gives a
/// time, its field all ones otherwise; its regions and `hw::Ring0` are not yet taken.
fn mint_machine_root(lo: &mut Lowering, fx: &mut Fx, grant: Value) {
    let grant_ty = lo.root_grant();
    let unlimited = fx.int(PTR, -1);
    let cell = memory::new_cell(lo, fx, unlimited, unlimited);
    let clock_kind = fx.ptr(kind::CLOCK as u64);
    let clock = new_entry(lo, fx, clock_kind);
    let now = lo.now_ns(fx);
    let record = entry_at(lo, fx, clock);
    fx.store(now, record, entry::STATE);
    let executor = fx.ptr(kind::EXECUTOR as u64);
    let exec = new_entry(lo, fx, executor);
    let record = entry_at(lo, fx, exec);
    fx.store(cell, record, entry::STATE);
    fx.store(clock, record, entry::STATE + 8);
    let share = fx.ptr(ROOT_SHARE);
    fx.store(share, record, EXECUTOR_SHARE);
    let (valid, wall_now) = lo.wall_source(fx);
    let wall = fx.choose_in(
        lo,
        valid,
        &[PTR],
        |lo, fx| {
            let wall_kind = fx.ptr(kind::WALL as u64);
            let wall = new_entry(lo, fx, wall_kind);
            let record = entry_at(lo, fx, wall);
            let ahead = fx.b.ins().isub(wall_now, now);
            fx.store(ahead, record, entry::STATE);
            vec![wall]
        },
        |_, fx| vec![fx.int(PTR, -1)],
    )[0];
    for (field, value) in [
        ("alloc", cell),
        ("exec", exec),
        ("clock", clock),
        ("wall", wall),
    ] {
        let at = lo.layouts.field(lo.ck, &grant_ty, None, field).offset;
        fx.store(value, grant, at);
    }
    let untaken = fx.int(types::I8, 0);
    for field in ["regions", "ring0", "modules", "iommu", "functions"] {
        let at = lo.layouts.field(lo.ck, &grant_ty, None, field).offset;
        fx.store(untaken, grant, at);
    }
    process_cell(lo, fx, cell);
    fx.b.ins().return_(&[]);
}

/// Makes a root grant's cell the process domain's.
fn process_cell(lo: &mut Lowering, fx: &mut Fx, cell: Value) {
    let state_address = lo.state(fx);
    let process = fx.load(PTR, state_address, state::PROCESS);
    fx.store(cell, process, tasks::domain::CELL);
}

/// Writes a `host::World`'s allocator, naming a root cell, which becomes the process domain's,
/// and its empty arguments and environment, which store through it.
pub fn grant_memory(lo: &mut Lowering, fx: &mut Fx, world: Value, cell: Value) {
    let world_ty = lo.std_ty("host", "World", Vec::new());
    let alloc = lo.layouts.field(lo.ck, &world_ty, None, "alloc").offset;
    fx.store(cell, world, alloc);
    for field in ["args", "environment"] {
        let at = lo.layouts.field(lo.ck, &world_ty, None, field).offset;
        let zero = fx.ptr(0);
        let dangling = fx.ptr(8);
        fx.store(dangling, world, at);
        for word in 1..3 {
            fx.store(zero, world, at + 8 * word);
        }
        fx.store(cell, world, at + 24);
    }
    process_cell(lo, fx, cell);
}

/// A new live entry of a kind, by handle.
pub fn mint(lo: &mut Lowering, fx: &mut Fx, kind: Value) -> Value {
    let state_address = lo.state(fx);
    let lock = fx.offset(state_address, state::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let handle = new_entry(lo, fx, kind);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    handle
}

impl Lowering<'_, '_> {
    /// Calls the function of the target's platform package named `name` with values held as it
    /// takes them.
    pub fn platform_call(&mut self, fx: &mut Fx, name: &str, args: &[Held]) -> Called {
        let item = self.platform_function(name);
        self.call_function(fx, item, args)
    }

    /// Calls the function of the core package named `name` with values held as it takes them.
    pub fn core_call(&mut self, fx: &mut Fx, name: &str, args: &[Held]) -> Called {
        let item = self
            .ck
            .db
            .core_item(name)
            .unwrap_or_else(|| panic!("the core function core::{name}"));
        self.call_function(fx, item, args)
    }

    fn call_function(&mut self, fx: &mut Fx, item: ItemId, args: &[Held]) -> Called {
        let instance = Instance::Nir {
            item,
            role: crate::nir::Role::Body,
            index: 0,
            args: crate::check::ty::no_args(),
        };
        let abi = self.abi(&instance);
        let id = self.instance(instance);
        self.invoke(fx, Invoked::Direct(id), &abi, args)
    }

    /// The function of the target's platform package named `name`.
    fn platform_function(&self, name: &str) -> ItemId {
        let platform = match self.ck.db.target.kind() {
            Some(Kind::Uefi) => "uefi_platform",
            Some(Kind::Bare) => "bare_platform",
            _ => "host_platform",
        };
        self.ck
            .db
            .packages
            .iter()
            .position(|package| package.name == platform)
            .and_then(|index| {
                let package = &self.ck.db.packages[index];
                let root = package.root?;
                self.ck.db.lookup(root, name)
            })
            .unwrap_or_else(|| panic!("the platform function {platform}::{name}"))
    }
}
