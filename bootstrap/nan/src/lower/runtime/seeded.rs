//! The seeded scheduler of test images: one executor thread runs one task at a time. At each
//! scheduling point it takes the ready task at position w mod k among the k ready tasks, in the
//! order they became ready, w being the next value of the run's scheduling stream. Its virtual
//! clock starts at 0 nanoseconds and advances only when no task is ready, to the earliest
//! pending deadline; with none pending the run stalls.
//!
//! Value n of stream s is the first 8 bytes, little-endian, of the BLAKE3 hash of the run's seed,
//! s and n as 8 bytes little-endian each. The pending deadlines are the virtual clock's timers,
//! ordered by when they are due and then by when they were armed. A completion, of a timer or of
//! a wait in a sim, is queued and delivered when the scheduler next reaps, as a ring's would be.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, condcodes::IntCC, types};

use super::super::Lowering;
use super::super::fx::{Fx, PTR};
use super::tasks::thread;
use super::tasks::{self, CANCELED, EXPIRED, operation, run, task};
use super::{Routine, state, timers};

/// The stream serving scheduling choices.
const SCHEDULING: u64 = 1;

/// BLAKE3's initial chaining value and message word permutation.
const IV: [u32; 8] = [
    0x6A09_E667,
    0xBB67_AE85,
    0x3C6E_F372,
    0xA54F_F53A,
    0x510E_527F,
    0x9B05_688C,
    0x1F83_D9AB,
    0x5BE0_CD19,
];
const PERMUTATION: [usize; 16] = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8];

/// The flags of a block that starts and ends the root chunk.
const ROOT_BLOCK: i64 = 1 | 2 | 8;

pub fn define(lo: &mut Lowering, fx: &mut Fx, routine: Routine) {
    let params = fx.entry();
    match routine {
        Routine::Yield => {
            yield_point(lo, fx);
            fx.b.ins().return_(&[]);
        }
        Routine::Stream => {
            let value = stream_value(fx, params[0], params[1], params[2]);
            fx.b.ins().return_(&[value]);
        }
        _ => unreachable!("a seeded scheduler routine"),
    }
}

/// Value `index` of stream `stream` of the run's seed.
pub fn stream(lo: &mut Lowering, fx: &mut Fx, stream: Value, index: Value) -> Value {
    let state_address = lo.state(fx);
    let seed = fx.load(PTR, state_address, state::SEED);
    lo.call_routine(fx, Routine::Stream, &[seed, stream, index])[0]
}

/// The first 8 bytes of the BLAKE3 hash of three 8-byte words: one compression of the one
/// block of the root chunk.
fn stream_value(fx: &mut Fx, seed: Value, stream: Value, index: Value) -> Value {
    let mut message = Vec::with_capacity(16);
    for word in [seed, stream, index] {
        message.push(fx.b.ins().ireduce(types::I32, word));
        let high = fx.b.ins().ushr_imm_s(word, 32);
        message.push(fx.b.ins().ireduce(types::I32, high));
    }
    while message.len() < 16 {
        message.push(fx.int(types::I32, 0));
    }
    let mut v: Vec<Value> = IV
        .iter()
        .chain(&IV[..4])
        .map(|word| fx.int(types::I32, i64::from(*word)))
        .collect();
    v.push(fx.int(types::I32, 0));
    v.push(fx.int(types::I32, 0));
    v.push(fx.int(types::I32, 24));
    v.push(fx.int(types::I32, ROOT_BLOCK));
    for round in 0..7 {
        let m = &message;
        for (column, (a, b, c, d)) in [
            (0, 4, 8, 12),
            (1, 5, 9, 13),
            (2, 6, 10, 14),
            (3, 7, 11, 15),
            (0, 5, 10, 15),
            (1, 6, 11, 12),
            (2, 7, 8, 13),
            (3, 4, 9, 14),
        ]
        .into_iter()
        .enumerate()
        {
            mix(fx, &mut v, (a, b, c, d), m[2 * column], m[2 * column + 1]);
        }
        if round < 6 {
            message = PERMUTATION.iter().map(|from| message[*from]).collect();
        }
    }
    let low = fx.b.ins().bxor(v[0], v[8]);
    let high = fx.b.ins().bxor(v[1], v[9]);
    let low = fx.b.ins().uextend(PTR, low);
    let high = fx.b.ins().uextend(PTR, high);
    let high = fx.b.ins().ishl_imm_s(high, 32);
    fx.b.ins().bor(low, high)
}

/// BLAKE3's mixing function on four words of the state.
fn mix(
    fx: &mut Fx,
    v: &mut [Value],
    (a, b, c, d): (usize, usize, usize, usize),
    x: Value,
    y: Value,
) {
    for (word, shifts) in [(x, (16, 12)), (y, (8, 7))] {
        let sum = fx.b.ins().iadd(v[a], v[b]);
        v[a] = fx.b.ins().iadd(sum, word);
        let crossed = fx.b.ins().bxor(v[d], v[a]);
        v[d] = fx.b.ins().rotr_imm_s(crossed, shifts.0);
        v[c] = fx.b.ins().iadd(v[c], v[d]);
        let crossed = fx.b.ins().bxor(v[b], v[c]);
        v[b] = fx.b.ins().rotr_imm_s(crossed, shifts.1);
    }
}

/// The next order in which a task becomes ready or a timer is armed.
pub fn next_order(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let state_address = lo.state(fx);
    let order = fx.load(PTR, state_address, state::ORDER);
    let order = fx.b.ins().iadd_imm_s(order, 1);
    fx.store(order, state_address, state::ORDER);
    order
}

/// Appends a task that became ready to the run queue, last in order.
pub fn push_ready(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let order = next_order(lo, fx);
    fx.store(order, record, task::ORDER);
    let state_address = lo.state(fx);
    let zero = fx.ptr(0);
    fx.store(zero, record, task::NEXT);
    let tail = fx.load(PTR, state_address, state::READY_TAIL);
    let has_tail = fx.b.ins().icmp_imm_s(IntCC::NotEqual, tail, 0);
    fx.choose(
        has_tail,
        &[],
        |fx| {
            fx.store(record, tail, task::NEXT);
            Vec::new()
        },
        |fx| {
            fx.store(record, state_address, state::READY_HEAD);
            Vec::new()
        },
    );
    fx.store(record, state_address, state::READY_TAIL);
}

/// The number of ready tasks.
fn ready_count(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let state_address = lo.state(fx);
    let head = fx.load(PTR, state_address, state::READY_HEAD);
    let zero = fx.ptr(0);
    let counted = fx.repeat(&[head, zero], |fx, state| {
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
    });
    counted[1]
}

/// The choice of a scheduling point among `count` ready tasks: the position the next value of
/// the scheduling stream gives, which records that the run met two ready tasks at once.
fn choose(lo: &mut Lowering, fx: &mut Fx, count: Value) -> Value {
    let state_address = lo.state(fx);
    let picks = fx.load(PTR, state_address, state::PICKS);
    let next = fx.b.ins().iadd_imm_s(picks, 1);
    fx.store(next, state_address, state::PICKS);
    let several = fx.b.ins().icmp_imm_s(IntCC::UnsignedGreaterThan, count, 1);
    fx.choose_in(
        lo,
        several,
        &[PTR],
        |lo, fx| {
            let one = fx.ptr(1);
            let state_address = lo.state(fx);
            fx.store(one, state_address, state::CONTENDED);
            let scheduling = fx.ptr(SCHEDULING);
            let value = stream(lo, fx, scheduling, picks);
            vec![fx.b.ins().urem(value, count)]
        },
        |_, fx| vec![fx.ptr(0)],
    )[0]
}

/// Takes the ready task a scheduling point chooses out of the run queue, zero when none is
/// ready.
pub fn pick(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let count = ready_count(lo, fx);
    let any = fx.b.ins().icmp_imm_s(IntCC::NotEqual, count, 0);
    fx.choose_in(
        lo,
        any,
        &[PTR],
        |lo, fx| {
            let position = choose(lo, fx, count);
            let state_address = lo.state(fx);
            let head = fx.load(PTR, state_address, state::READY_HEAD);
            let zero = fx.ptr(0);
            let found = fx.repeat(&[head, zero, position], |fx, state| {
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
            let (chosen, previous) = (found[0], found[1]);
            let next = fx.load(PTR, chosen, task::NEXT);
            let first = fx.b.ins().icmp_imm_s(IntCC::Equal, previous, 0);
            fx.choose(
                first,
                &[],
                |fx| {
                    fx.store(next, state_address, state::READY_HEAD);
                    Vec::new()
                },
                |fx| {
                    fx.store(next, previous, task::NEXT);
                    Vec::new()
                },
            );
            let last = fx.b.ins().icmp_imm_s(IntCC::Equal, next, 0);
            fx.when(last, |fx| {
                fx.store(previous, state_address, state::READY_TAIL)
            });
            vec![chosen]
        },
        |_, fx| vec![fx.ptr(0)],
    )[0]
}

/// Puts the running task back among the ready tasks at the place of the order in which it
/// became ready.
fn requeue(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let order = fx.load(PTR, record, task::ORDER);
    let state_address = lo.state(fx);
    let head = fx.load(PTR, state_address, state::READY_HEAD);
    let zero = fx.ptr(0);
    let found = fx.repeat(&[head, zero], |fx, state| {
        let (current, previous) = (state[0], state[1]);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
        let earlier = fx.choose(
            present,
            &[types::I8],
            |fx| {
                let other = fx.load(PTR, current, task::ORDER);
                vec![fx.b.ins().icmp(IntCC::UnsignedLessThan, other, order)]
            },
            |fx| vec![fx.int(types::I8, 0)],
        )[0];
        let next = fx.choose(
            earlier,
            &[PTR, PTR],
            |fx| vec![fx.load(PTR, current, task::NEXT), current],
            |_| vec![current, previous],
        );
        (earlier, next)
    });
    let (after, before) = (found[0], found[1]);
    fx.store(after, record, task::NEXT);
    let first = fx.b.ins().icmp_imm_s(IntCC::Equal, before, 0);
    fx.choose(
        first,
        &[],
        |fx| {
            fx.store(record, state_address, state::READY_HEAD);
            Vec::new()
        },
        |fx| {
            fx.store(record, before, task::NEXT);
            Vec::new()
        },
    );
    let last = fx.b.ins().icmp_imm_s(IntCC::Equal, after, 0);
    fx.when(last, |fx| {
        fx.store(record, state_address, state::READY_TAIL)
    });
}

/// A scheduling point of the running task, which stays ready: the completions due are
/// delivered, then, when another task is ready, the task goes back among the ready ones and the
/// scheduler chooses; otherwise the choice of the one ready task is made in place.
fn yield_point(lo: &mut Lowering, fx: &mut Fx) {
    let record = tasks::current_task(lo, fx);
    let inside = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
    fx.when(inside, |fx| {
        reap(lo, fx);
        let others = ready_count(lo, fx);
        let alone = fx.b.ins().icmp_imm_s(IntCC::Equal, others, 0);
        fx.choose_in(
            lo,
            alone,
            &[],
            |lo, fx| {
                let one = fx.ptr(1);
                choose(lo, fx, one);
                Vec::new()
            },
            |lo, fx| {
                let ready = fx.int(types::I32, run::READY);
                let word = fx.offset(record, task::STATE);
                fx.b.ins()
                    .atomic_store(MemFlagsData::trusted(), ready, word);
                requeue(lo, fx, record);
                let thread = lo.thread(fx);
                let scheduler = fx.offset(thread, thread::CONTEXT);
                let context = fx.offset(record, task::CONTEXT);
                lo.switch_stacks(fx, context, scheduler, record);
                Vec::new()
            },
        );
    });
}

/// Makes a scheduling point of the running task.
pub fn scheduling_point(lo: &mut Lowering, fx: &mut Fx) {
    if lo.seeded() {
        lo.call_routine(fx, Routine::Yield, &[]);
    }
}

/// The virtual clock's nanoseconds.
pub fn now(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let state_address = lo.state(fx);
    fx.load(PTR, state_address, state::NOW)
}

/// Arms a timer due `ns` nanoseconds from now.
pub fn arm(lo: &mut Lowering, fx: &mut Fx, op: Value, ns: Value) {
    let clock = now(lo, fx);
    let due = timers::due_after(fx, clock, ns);
    timers::arm(lo, fx, op, due);
}

/// Completes an operation in flight with a result, delivered when the scheduler next reaps.
pub fn complete(lo: &mut Lowering, fx: &mut Fx, op: Value, result: Value) {
    let done = fx.ptr(operation::COMPLETED as u64);
    fx.store(done, op, operation::DONE);
    let result = match fx.b.func.dfg.value_type(result) {
        types::I32 => result,
        _ => fx.b.ins().ireduce(types::I32, result),
    };
    fx.store(result, op, operation::RESULT);
    let state_address = lo.state(fx);
    let zero = fx.ptr(0);
    fx.store(zero, op, operation::NEXT);
    let tail = fx.load(PTR, state_address, state::COMPLETIONS_TAIL);
    let has_tail = fx.b.ins().icmp_imm_s(IntCC::NotEqual, tail, 0);
    fx.choose(
        has_tail,
        &[],
        |fx| {
            fx.store(op, tail, operation::NEXT);
            Vec::new()
        },
        |fx| {
            fx.store(op, state_address, state::COMPLETIONS);
            Vec::new()
        },
    );
    fx.store(op, state_address, state::COMPLETIONS_TAIL);
}

/// Cancels an operation in flight: a timer leaves the virtual clock and a wait leaves its sim,
/// and it completes cancelled.
pub fn cancel(lo: &mut Lowering, fx: &mut Fx, op: Value) {
    let done = fx.load(PTR, op, operation::DONE);
    let in_flight = fx.b.ins().icmp_imm_s(IntCC::Equal, done, 0);
    fx.when(in_flight, |fx| {
        let withdraw = fx.load(PTR, op, operation::WITHDRAW);
        let waits = fx.b.ins().icmp_imm_s(IntCC::NotEqual, withdraw, 0);
        fx.choose_in(
            lo,
            waits,
            &[],
            |_, fx| {
                let signature =
                    fx.b.import_signature(super::super::supplied::tasks::record_signature());
                fx.b.ins().call_indirect(signature, withdraw, &[op]);
                Vec::new()
            },
            |lo, fx| {
                timers::disarm(lo, fx, op);
                Vec::new()
            },
        );
        let cancelled = fx.int(types::I32, CANCELED);
        complete(lo, fx, op, cancelled);
    });
}

/// Delivers the queued completions, oldest first.
pub fn reap(lo: &mut Lowering, fx: &mut Fx) {
    fx.repeat(&[], |fx, _| {
        let state_address = lo.state(fx);
        let op = fx.load(PTR, state_address, state::COMPLETIONS);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, op, 0);
        fx.when(present, |fx| {
            let next = fx.load(PTR, op, operation::NEXT);
            fx.store(next, state_address, state::COMPLETIONS);
            let last = fx.b.ins().icmp_imm_s(IntCC::Equal, next, 0);
            fx.when(last, |fx| {
                let zero = fx.ptr(0);
                fx.store(zero, state_address, state::COMPLETIONS_TAIL);
            });
            let result = fx.load(types::I32, op, operation::RESULT);
            tasks::deliver(lo, fx, op, result);
        });
        (present, Vec::new())
    });
}

/// What the scheduler does when no task is ready: the virtual clock advances to the earliest
/// pending deadline and every timer then due expires, in the order they were armed; with none
/// pending the run stalls.
pub fn idle(lo: &mut Lowering, fx: &mut Fx) {
    let state_address = lo.state(fx);
    let first = fx.load(PTR, state_address, state::TIMERS);
    let none = fx.b.ins().icmp_imm_s(IntCC::Equal, first, 0);
    fx.when(none, |fx| super::super::test_image::stalled(lo, fx));
    let due = fx.load(PTR, first, operation::DUE);
    fx.store(due, state_address, state::NOW);
    fx.repeat(&[], |fx, _| {
        let head = fx.load(PTR, state_address, state::TIMERS);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, head, 0);
        let expires = fx.choose(
            present,
            &[types::I8],
            |fx| {
                let when = fx.load(PTR, head, operation::DUE);
                vec![fx.b.ins().icmp(IntCC::Equal, when, due)]
            },
            |fx| vec![fx.int(types::I8, 0)],
        )[0];
        fx.when(expires, |fx| {
            timers::disarm(lo, fx, head);
            let expired = fx.int(types::I32, EXPIRED);
            complete(lo, fx, head, expired);
        });
        (expires, Vec::new())
    });
    reap(lo, fx);
}
