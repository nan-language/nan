//! Keyed tasks: their spawns, held on a stepper until its holder starts them, and the
//! superseding of their keys.
//!
//! A keyed task's node holds its links, its task, key, outbox and region, whether its key was
//! superseded, whether it has ended and whether it is held.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC};
use cranelift_module::FuncId;

use super::super::super::super::fx::{Fx, PTR};
use super::super::super::super::runtime::regions::region;
use super::super::super::super::runtime::tasks::{self, task};
use super::super::super::super::runtime::{Routine, report};
use super::super::super::super::{Glue, Lowering};
use super::super::super::Supply;
use super::super::super::mem::lists::fn_param;
use super::super::scopes::start_task;
use super::{entry, intake, reference, runner, status, unreference, wake_waiter};

mod node {
    pub const NEXT: u64 = 0;
    pub const PREVIOUS: u64 = 8;
    pub const TASK: u64 = 16;
    pub const KEY: u64 = 24;
    pub const OUTBOX: u64 = 32;
    pub const REGION: u64 = 40;
    pub const SUPERSEDED: u64 = 48;
    pub const ENDED: u64 = 56;
    pub const RUNNER: u64 = 64;
    pub const HELD: u64 = 72;
    pub const SIZE: u64 = 80;
}

/// `mut cx.spawn(key: k, run: f)`: supersedes `k`, then starts a keyed task running `f` with a
/// new outbox of key `k`, in a region of its own, or holds it on a stepper.
pub fn spawn(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let run_ty = fn_param(supply, 2);
    let record = fx.load(PTR, supply.param(0).address(), 0);
    let key = supply.param(1).scalar();
    let lock = fx.offset(record, runner::LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    supersede_key(lo, fx, record, key);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    let size = fx.ptr(region::SIZE);
    let task_region = lo.call_routine(fx, Routine::RegionOpen, &[size])[0];
    let keyed_outbox = fx.ptr(1);
    let outbox = lo.call_routine(fx, Routine::OutboxMint, &[record, key, keyed_outbox])[0];
    let node_size = fx.ptr(node::SIZE);
    let node = lo.record_alloc(fx, node_size);
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, node, 0);
    fx.when(missing, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
    fx.store(key, node, node::KEY);
    fx.store(outbox, node, node::OUTBOX);
    fx.store(task_region, node, node::REGION);
    fx.store(record, node, node::RUNNER);
    reference(fx, record);
    let cell = fx.load(PTR, record, runner::EXECUTOR);
    let exec = fx.load(PTR, record, runner::HANDLE);
    let domain = tasks::current_domain(lo, fx);
    let done = keyed_done(lo);
    let started = start_task(
        lo,
        fx,
        &run_ty,
        supply.param(2).address(),
        exec,
        cell,
        domain,
        task_region,
        node,
        None,
        None,
        done,
    );
    fx.store(outbox, started, task::ARGUMENT);
    fx.store(started, node, node::TASK);
    let holding = fx.load(PTR, record, runner::HOLDING);
    let held = fx.b.ins().icmp_imm_s(IntCC::NotEqual, holding, 0);
    fx.when(held, |fx| {
        let one = fx.ptr(1);
        fx.store(one, node, node::HELD);
    });
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let head = fx.load(PTR, record, runner::KEYED);
    fx.store(head, node, node::NEXT);
    let has_head = fx.b.ins().icmp_imm_s(IntCC::NotEqual, head, 0);
    fx.when(has_head, |fx| fx.store(node, head, node::PREVIOUS));
    fx.store(node, record, runner::KEYED);
    let live = fx.load(PTR, record, runner::LIVE_KEYED);
    let live = fx.b.ins().iadd_imm_s(live, 1);
    fx.store(live, record, runner::LIVE_KEYED);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    let now = fx.b.ins().bxor_imm_s(held, 1);
    fx.when(now, |fx| tasks::spawned(lo, fx, started));
    supply.done(lo, fx);
}

/// Runs `visit` on each keyed task node of a runner, which may free the node. The caller holds
/// the lock.
pub fn each_keyed(
    lo: &mut Lowering,
    fx: &mut Fx,
    record: Value,
    mut visit: impl FnMut(&mut Lowering, &mut Fx, Value),
) {
    let first = fx.load(PTR, record, runner::KEYED);
    fx.repeat(&[first], |fx, state| {
        let node = state[0];
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, node, 0);
        let next = fx.choose_in(
            lo,
            present,
            &[PTR],
            |lo, fx| {
                let next = fx.load(PTR, node, node::NEXT);
                visit(lo, fx, node);
                vec![next]
            },
            |_, fx| vec![fx.ptr(0)],
        );
        (present, next)
    });
}

/// Supersedes a key: every outbox of the key goes stale, the posts waiting through them leave
/// the intake and every keyed task of the key still running is cancelled. The caller holds the
/// lock.
pub fn supersede_key(lo: &mut Lowering, fx: &mut Fx, record: Value, key: Value) {
    each_keyed(lo, fx, record, |lo, fx, node| {
        let node_key = fx.load(PTR, node, node::KEY);
        let same = fx.b.ins().icmp(IntCC::Equal, node_key, key);
        let superseded = fx.load(PTR, node, node::SUPERSEDED);
        let live = fx.b.ins().icmp_imm_s(IntCC::Equal, superseded, 0);
        let target = fx.b.ins().band(same, live);
        fx.when(target, |fx| supersede_node(lo, fx, node));
    });
    let first = fx.load(PTR, record, runner::OLDEST);
    fx.repeat(&[first], |fx, state| {
        let node = state[0];
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, node, 0);
        let next = fx.choose_in(
            lo,
            present,
            &[PTR],
            |lo, fx| {
                let next = fx.load(PTR, node, entry::NEXT);
                let poster = fx.load(PTR, node, entry::POSTER);
                let posted = fx.b.ins().icmp_imm_s(IntCC::NotEqual, poster, 0);
                let keyed_post = fx.load(PTR, node, entry::KEYED);
                let keyed_post = fx.b.ins().icmp_imm_s(IntCC::NotEqual, keyed_post, 0);
                let node_key = fx.load(PTR, node, entry::KEY);
                let same = fx.b.ins().icmp(IntCC::Equal, node_key, key);
                let removed = fx.b.ins().band(posted, keyed_post);
                let removed = fx.b.ins().band(removed, same);
                fx.when(removed, |fx| {
                    intake::unlink(fx, record, node);
                    let status = fx.ptr(status::REMOVED as u64);
                    fx.store(status, node, entry::STATUS);
                    lo.call_routine(fx, Routine::Wake, &[poster]);
                });
                vec![next]
            },
            |_, fx| vec![fx.ptr(0)],
        );
        (present, next)
    });
}

/// Supersedes one keyed task: its outbox goes stale and the task is cancelled unless it has
/// ended, in which case its node is freed. The caller holds the lock.
fn supersede_node(lo: &mut Lowering, fx: &mut Fx, node: Value) {
    let one = fx.ptr(1);
    fx.store(one, node, node::SUPERSEDED);
    let outbox = fx.load(PTR, node, node::OUTBOX);
    lo.call_routine(fx, Routine::OutboxRetire, &[outbox]);
    let ended = fx.load(PTR, node, node::ENDED);
    let ended = fx.b.ins().icmp_imm_s(IntCC::NotEqual, ended, 0);
    fx.choose_in(
        lo,
        ended,
        &[],
        |lo, fx| {
            free_node(lo, fx, node);
            Vec::new()
        },
        |lo, fx| {
            let task_region = fx.load(PTR, node, node::REGION);
            lo.call_routine(fx, Routine::Cancel, &[task_region]);
            Vec::new()
        },
    );
}

/// Supersedes the key of every keyed task of a runner and starts each held one, which that
/// cancellation then reaches. The caller holds the lock.
pub fn supersede_all(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    each_keyed(lo, fx, record, |lo, fx, node| {
        let held = fx.load(PTR, node, node::HELD);
        let held = fx.b.ins().icmp_imm_s(IntCC::NotEqual, held, 0);
        let superseded = fx.load(PTR, node, node::SUPERSEDED);
        let live = fx.b.ins().icmp_imm_s(IntCC::Equal, superseded, 0);
        fx.when(live, |fx| supersede_node(lo, fx, node));
        fx.when(held, |fx| start_node(lo, fx, node));
    });
}

/// Starts the held keyed tasks of a stepper's record in the order of their spawns, giving how
/// many it started. The caller holds the lock.
pub fn start_held(lo: &mut Lowering, fx: &mut Fx, record: Value) -> Value {
    let zero = fx.ptr(0);
    let first = fx.load(PTR, record, runner::KEYED);
    let last = fx.repeat(&[zero, first], |fx, state| {
        let (previous, current) = (state[0], state[1]);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
        let next = fx.choose(
            present,
            &[PTR],
            |fx| vec![fx.load(PTR, current, node::NEXT)],
            |fx| vec![fx.ptr(0)],
        )[0];
        let kept = fx.b.ins().select(present, current, previous);
        (present, vec![kept, next])
    })[0];
    fx.repeat(&[last, zero], |fx, state| {
        let (current, count) = (state[0], state[1]);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
        let next = fx.choose_in(
            lo,
            present,
            &[PTR, PTR],
            |lo, fx| {
                let held = fx.load(PTR, current, node::HELD);
                let held = fx.b.ins().icmp_imm_s(IntCC::NotEqual, held, 0);
                fx.when(held, |fx| start_node(lo, fx, current));
                let started = fx.b.ins().uextend(PTR, held);
                let count = fx.b.ins().iadd(count, started);
                vec![fx.load(PTR, current, node::PREVIOUS), count]
            },
            |_, fx| vec![fx.ptr(0), count],
        );
        (present, next)
    })[1]
}

/// The count of a runner's keyed tasks that have not ended. The caller holds the lock.
pub fn unended(fx: &mut Fx, record: Value) -> Value {
    let first = fx.load(PTR, record, runner::KEYED);
    let zero = fx.ptr(0);
    fx.repeat(&[first, zero], |fx, state| {
        let (current, count) = (state[0], state[1]);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
        let next = fx.choose(
            present,
            &[PTR, PTR],
            |fx| {
                let ended = fx.load(PTR, current, node::ENDED);
                let live = fx.b.ins().icmp_imm_s(IntCC::Equal, ended, 0);
                let live = fx.b.ins().uextend(PTR, live);
                let count = fx.b.ins().iadd(count, live);
                vec![fx.load(PTR, current, node::NEXT), count]
            },
            |fx| vec![fx.ptr(0), count],
        );
        (present, next)
    })[1]
}

/// Starts a held keyed task. The caller holds the lock.
fn start_node(lo: &mut Lowering, fx: &mut Fx, node: Value) {
    let zero = fx.ptr(0);
    fx.store(zero, node, node::HELD);
    let started = fx.load(PTR, node, node::TASK);
    lo.call_routine(fx, Routine::Wake, &[started]);
}

/// Removes a keyed task's node from its runner's list and frees it. The caller holds the lock.
fn free_node(lo: &mut Lowering, fx: &mut Fx, node: Value) {
    let record = fx.load(PTR, node, node::RUNNER);
    let next = fx.load(PTR, node, node::NEXT);
    let previous = fx.load(PTR, node, node::PREVIOUS);
    let has_previous = fx.b.ins().icmp_imm_s(IntCC::NotEqual, previous, 0);
    fx.choose(
        has_previous,
        &[],
        |fx| {
            fx.store(next, previous, node::NEXT);
            Vec::new()
        },
        |fx| {
            fx.store(next, record, runner::KEYED);
            Vec::new()
        },
    );
    let has_next = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next, 0);
    fx.when(has_next, |fx| fx.store(previous, next, node::PREVIOUS));
    lo.record_free(fx, node);
}

/// What the executor thread does once a keyed task has ended: its region is freed, its node too
/// once its key is superseded, the task waiting for the record's tasks wakes, the task's record
/// is freed and its reference to the runner's record dropped. Its failure, a cancellation, needs
/// no release.
fn keyed_done(lo: &mut Lowering) -> FuncId {
    lo.glue_function(
        Glue::KeyedDone,
        super::super::record_signature(),
        |lo, fx| {
            let params = fx.entry();
            let started = params[0];
            let node = fx.load(PTR, started, task::OWNER);
            let record = fx.load(PTR, node, node::RUNNER);
            let lock = fx.offset(record, runner::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let one = fx.ptr(1);
            fx.store(one, node, node::ENDED);
            let live = fx.load(PTR, record, runner::LIVE_KEYED);
            let live = fx.b.ins().iadd_imm_s(live, -1);
            fx.store(live, record, runner::LIVE_KEYED);
            let task_region = fx.load(PTR, node, node::REGION);
            lo.record_free(fx, task_region);
            let superseded = fx.load(PTR, node, node::SUPERSEDED);
            let superseded = fx.b.ins().icmp_imm_s(IntCC::NotEqual, superseded, 0);
            fx.when(superseded, |fx| free_node(lo, fx, node));
            wake_waiter(lo, fx, record);
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            lo.record_free(fx, started);
            unreference(lo, fx, record);
            fx.b.ins().return_(&[]);
        },
    )
}
