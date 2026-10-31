//! Serial executors: their tasks run one at a time. The task holding a serial executor's turn
//! runs; a task made ready while another holds it waits among the executor's waiting tasks, the
//! last to wait first. When the holder parks, ends or stops, the turn passes to the waiting task
//! at the position the next value of the scheduling stream of the executor's seed gives, so a run
//! that makes the same tasks ready in the same order passes the turn alike. A task records the
//! entry of its serial executor; once that entry is revoked its tasks run without a turn.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::Lowering;
use super::super::fx::{Fx, PTR};
use super::tables::{self, entry};
use super::tasks::task;
use super::{Routine, seeded, state};

/// Whether a task made ready may run now: it records no serial executor or a stale one, or it
/// takes or holds the turn; otherwise it joins the waiting tasks.
pub fn admit(lo: &mut Lowering, fx: &mut Fx, record: Value) -> Value {
    let serial = fx.load(PTR, record, task::SERIAL);
    let gated = fx.b.ins().icmp_imm_s(IntCC::NotEqual, serial, 0);
    fx.choose_in(
        lo,
        gated,
        &[types::I8],
        |lo, fx| {
            let state_address = lo.state(fx);
            let lock = fx.offset(state_address, state::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let owner = tables::live_entry(lo, fx, serial);
            let live = fx.b.ins().icmp_imm_s(IntCC::NotEqual, owner, 0);
            let admitted = fx.choose(
                live,
                &[types::I8],
                |fx| {
                    let turn = fx.load(PTR, owner, entry::TURN);
                    let free = fx.b.ins().icmp_imm_s(IntCC::Equal, turn, 0);
                    let held = fx.b.ins().icmp(IntCC::Equal, turn, record);
                    let takes = fx.b.ins().bor(free, held);
                    fx.choose(
                        takes,
                        &[types::I8],
                        |fx| {
                            fx.store(record, owner, entry::TURN);
                            vec![fx.int(types::I8, 1)]
                        },
                        |fx| {
                            let waiting = fx.load(PTR, owner, entry::WAITING);
                            fx.store(waiting, record, task::NEXT);
                            fx.store(record, owner, entry::WAITING);
                            vec![fx.int(types::I8, 0)]
                        },
                    )
                },
                |fx| vec![fx.int(types::I8, 1)],
            )[0];
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            vec![admitted]
        },
        |_, fx| vec![fx.int(types::I8, 1)],
    )[0]
}

/// `Turn(task)`: when the task holds its serial executor's turn, passes it to the waiting task the
/// executor's seed chooses, which joins a run queue, or frees it when none waits.
pub fn define_turn(lo: &mut Lowering, fx: &mut Fx) {
    let record = fx.entry()[0];
    let serial = fx.load(PTR, record, task::SERIAL);
    let gated = fx.b.ins().icmp_imm_s(IntCC::NotEqual, serial, 0);
    fx.when(gated, |fx| {
        let state_address = lo.state(fx);
        let lock = fx.offset(state_address, state::LOCK);
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let owner = tables::live_entry(lo, fx, serial);
        let live = fx.b.ins().icmp_imm_s(IntCC::NotEqual, owner, 0);
        let next = fx.choose_in(
            lo,
            live,
            &[PTR],
            |lo, fx| {
                let turn = fx.load(PTR, owner, entry::TURN);
                let holds = fx.b.ins().icmp(IntCC::Equal, turn, record);
                fx.choose_in(
                    lo,
                    holds,
                    &[PTR],
                    |lo, fx| {
                        let next = pick(lo, fx, owner);
                        fx.store(next, owner, entry::TURN);
                        vec![next]
                    },
                    |_, fx| vec![fx.ptr(0)],
                )
            },
            |_, fx| vec![fx.ptr(0)],
        )[0];
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        let passed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next, 0);
        fx.when(passed, |fx| {
            lo.call_routine(fx, Routine::Enqueue, &[next]);
        });
    });
    fx.b.ins().return_(&[]);
}

/// Takes the waiting task the serial executor's seed chooses out of its waiting tasks, zero when
/// none waits. The caller holds the lock.
fn pick(lo: &mut Lowering, fx: &mut Fx, owner: Value) -> Value {
    let first = fx.load(PTR, owner, entry::WAITING);
    let zero = fx.ptr(0);
    let count = fx.repeat(&[first, zero], |fx, state| {
        let (current, count) = (state[0], state[1]);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
        let next = fx.choose(
            present,
            &[PTR, PTR],
            |fx| {
                let next = fx.load(PTR, current, task::NEXT);
                vec![next, fx.b.ins().iadd_imm_s(count, 1)]
            },
            |_| vec![current, count],
        );
        (present, next)
    })[1];
    let any = fx.b.ins().icmp_imm_s(IntCC::NotEqual, count, 0);
    fx.choose_in(
        lo,
        any,
        &[PTR],
        |lo, fx| {
            let seed = fx.load(PTR, owner, entry::SEED);
            let picks = fx.load(PTR, owner, entry::PICKS);
            let next = fx.b.ins().iadd_imm_s(picks, 1);
            fx.store(next, owner, entry::PICKS);
            let stream = fx.ptr(seeded::SCHEDULING);
            let value = lo.call_routine(fx, Routine::Stream, &[seed, stream, picks])[0];
            let position = fx.b.ins().urem(value, count);
            let first = fx.load(PTR, owner, entry::WAITING);
            let zero = fx.ptr(0);
            let found = fx.repeat(&[first, zero, position], |fx, state| {
                let (current, previous, left) = (state[0], state[1], state[2]);
                let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, left, 0);
                let next = fx.choose(
                    more,
                    &[PTR, PTR, PTR],
                    |fx| {
                        let next = fx.load(PTR, current, task::NEXT);
                        vec![next, current, fx.b.ins().iadd_imm_s(left, -1)]
                    },
                    |_| vec![current, previous, left],
                );
                (more, next)
            });
            unlink(fx, owner, found[0], found[1]);
            vec![found[0]]
        },
        |_, fx| vec![fx.ptr(0)],
    )[0]
}

/// Takes a waiting task, after `previous` or first when it is zero, out of the waiting tasks.
fn unlink(fx: &mut Fx, owner: Value, record: Value, previous: Value) {
    let next = fx.load(PTR, record, task::NEXT);
    let first = fx.b.ins().icmp_imm_s(IntCC::Equal, previous, 0);
    fx.choose(
        first,
        &[],
        |fx| {
            fx.store(next, owner, entry::WAITING);
            Vec::new()
        },
        |fx| {
            fx.store(next, previous, task::NEXT);
            Vec::new()
        },
    );
}

/// Takes a task out of its serial executor's waiting tasks; gives whether it waited there.
pub fn withdraw(lo: &mut Lowering, fx: &mut Fx, record: Value) -> Value {
    let serial = fx.load(PTR, record, task::SERIAL);
    let gated = fx.b.ins().icmp_imm_s(IntCC::NotEqual, serial, 0);
    fx.choose_in(
        lo,
        gated,
        &[types::I8],
        |lo, fx| {
            let state_address = lo.state(fx);
            let lock = fx.offset(state_address, state::LOCK);
            lo.call_routine(fx, Routine::Lock, &[lock]);
            let owner = tables::live_entry(lo, fx, serial);
            let live = fx.b.ins().icmp_imm_s(IntCC::NotEqual, owner, 0);
            let found = fx.choose(
                live,
                &[types::I8],
                |fx| {
                    let first = fx.load(PTR, owner, entry::WAITING);
                    let zero = fx.ptr(0);
                    let found = fx.repeat(&[first, zero], |fx, state| {
                        let (current, previous) = (state[0], state[1]);
                        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
                        let other = fx.b.ins().icmp(IntCC::NotEqual, current, record);
                        let more = fx.b.ins().band(present, other);
                        let next = fx.choose(
                            more,
                            &[PTR, PTR],
                            |fx| vec![fx.load(PTR, current, task::NEXT), current],
                            |_| vec![current, previous],
                        );
                        (more, next)
                    });
                    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, found[0], 0);
                    fx.when(present, |fx| unlink(fx, owner, found[0], found[1]));
                    vec![present]
                },
                |fx| vec![fx.int(types::I8, 0)],
            )[0];
            lo.call_routine(fx, Routine::Unlock, &[lock]);
            vec![found]
        },
        |_, fx| vec![fx.int(types::I8, 0)],
    )[0]
}

/// Puts every task waiting for the turn of a serial executor's entry being revoked in a run queue.
/// The caller holds the lock.
pub fn release_waiting(lo: &mut Lowering, fx: &mut Fx, owner: Value) {
    let first = fx.load(PTR, owner, entry::WAITING);
    fx.repeat(&[first], |fx, state| {
        let current = state[0];
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
        let next = fx.choose_in(
            lo,
            present,
            &[PTR],
            |lo, fx| {
                let next = fx.load(PTR, current, task::NEXT);
                lo.call_routine(fx, Routine::Enqueue, &[current]);
                vec![next]
            },
            |_, fx| vec![fx.ptr(0)],
        )[0];
        (present, vec![next])
    });
    let zero = fx.ptr(0);
    fx.store(zero, owner, entry::WAITING);
}
