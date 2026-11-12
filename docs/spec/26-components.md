# Components

This chapter defines components, their handlers and quiescence, component state and its state schema, Env and messages, the component context with its `send` and `after` operations, keyed tasks and outboxes, the runner `host::Runner`, steppers with their checkpoints and system interfaces, protocols with their servers, operation calls, hashes and wire encoding, and the runner capabilities a stepper's holder attaches, with their ports and the resources they hold.

## Components

A **component** is a type implementing the std [interface](08-generics-interfaces.md#interfaces) `sys::Component[Env, M]`. That type is the component's **component state**, Env is its environment type, and a **message** is a value of M.

```nan-api
// module: sys

pub interface Component[Env: Any and Send, M: Send] {
    fn start(mut env: Env) -> Self
    fn on(mut self, take message: M, mut env: Env, mut cx: Cx[M])
} Component
```

- A component is declared by an [impl line](08-generics-interfaces.md#impl-lines) `impl S: sys::Component[Env, M]` together with the functions `S.start` and `S.on` that [conformance](08-generics-interfaces.md#conformance) requires. These two functions are the component's **handlers**.
- Env is Send and may be linear, the runner releasing a linear Env through its abandon method ([Env and messages](#env-and-messages)). M is Send and, being unbounded by `Any`, [droppable](05-properties.md#generic-parameters): a linear argument for it is an error (`prop.linear-argument`), reported on the impl line. `sys::Cx` and `sys::Outbox` bound their M, `host::Runner.run` its Env and M, and `sys::Stepper` and `sys::Stopped` their Env and M, by `Any`, so the handlers and the uses of the runner and of steppers accept a linear one.
- A running component has an **intake**, a first-in, first-out queue of messages. Its runner, a [run](#runner) of `host::Runner` or a [stepper](#steppers), calls `S.start` once, then removes messages from the intake one at a time, oldest first, calling `S.on` for each with the state, the message, the Env and a [component context](#component-context). It removes the next message only after `on` has returned.
- Neither interface method states `uses wait`, so an implementing function stating it is an error by conformance (`interface.head-mismatch`). A handler therefore contains no [suspension point](17-suspension-io.md#stackful-tasks): it never suspends.
- A component is **quiescent** whenever none of its handlers runs. The runner gives the state and the Env to handlers alone, and a stepper gives the state to its holder only between handler calls, so every handler starts on a quiescent component, and the state `run` returns, or a stepper gives, is that of a quiescent component.

```nan
pub closed struct Env {
    pub log: sys::Log
} Env

pub closed struct Meter derives Encode {
    pub frames: u64
    pub bytes: u64
} Meter

impl Meter: sys::Component[Env, Untrusted[mem::Bytes]]

pub fn Meter.start(mut env: Env) -> Meter = Meter(frames: 0, bytes: 0)

pub fn Meter.on(mut self, take message: Untrusted[mem::Bytes], mut env: Env, mut cx: sys::Cx[Untrusted[mem::Bytes]]) {
    self.frames += 1
    self.bytes += u64.widen(message.byte_len())
    env.log.info("bytes", value: self.bytes)
} Meter.on
```

```nan
pub closed struct Env {
    pub clock: sys::Clock
} Env

pub closed struct Meter derives Encode {
    pub bytes: u64
} Meter

impl Meter: sys::Component[Env, Untrusted[mem::Bytes]]

pub fn Meter.start(mut env: Env) -> Meter = Meter(bytes: 0)

pub fn Meter.on(mut self, take message: Untrusted[mem::Bytes], mut env: Env, mut cx: sys::Cx[Untrusted[mem::Bytes]]) fails task::Cancelled uses wait { // error: interface.head-mismatch
    try await env.clock.sleep(10)
    self.bytes += u64.widen(message.byte_len())
} Meter.on
```

```nan
pub closed struct Env {
    pub log: sys::Log
} Env

pub closed linear struct Lease {
    pub slot: u32
} Lease

pub closed struct Keeper derives Encode {
    pub kept: u64
} Keeper

impl Keeper: sys::Component[Env, Lease] // error: prop.linear-argument

pub fn Keeper.start(mut env: Env) -> Keeper = Keeper(kept: 0)

pub fn Keeper.on(mut self, take message: Lease, mut env: Env, mut cx: sys::Cx[Lease]) {
    let Lease(slot: _) = message
    self.kept += 1
} Keeper.on

pub fn keep(world: host::World) -> Keeper fails task::SendError uses wait {
    return try await host::Runner[Keeper].run(Env(log: world.log), exec: world.exec, alloc: world.alloc, body: fn(inbox: sys::Outbox[Lease]) fails task::SendError uses wait {
        try await inbox.post(Lease(slot: 1))
    })
} keep
```

### Component state

An impl line naming `sys::Component` is checked in this order, and only the first failing check is reported, on the impl line:

1. A target with generic parameters is an error (`component.state-generic`).
2. A target that is not [Inert](05-properties.md#inert) is an error (`component.state-not-inert`).
3. A target that does not implement [Encode](13-text-encode.md#encode) is an error (`component.state-not-encode`). Its exact fix adds `Encode` to the target's `derives` clause, writing `derives Encode` when the target has none.
4. A [linear](05-properties.md#linear) Env whose type has no [abandon method](17-suspension-io.md#abandoning-resources) visible at the impl line, or whose abandon method has a requires clause that is not [disputed](18-contracts.md#disputed-clauses), is an error (`component.env-not-abandonable`).

The **state schema** of a component is the [schema hash](13-text-encode.md#schema-hashes) of its component state. The [code root](30-artefacts.md#code-roots) of a package records the schema tree of every component state the package declares.

```nan
pub closed struct Env {
    pub log: sys::Log
} Env

pub closed struct Relay {
    pub log: sys::Log
    pub relayed: u64
} Relay

impl Relay: sys::Component[Env, u32] // error: component.state-not-inert

pub fn Relay.start(mut env: Env) -> Relay = Relay(log: env.log, relayed: 0)

pub fn Relay.on(mut self, take message: u32, mut env: Env, mut cx: sys::Cx[u32]) {
    self.relayed += u64.widen(message)
    self.log.info("relayed", value: self.relayed)
} Relay.on
```

```nan
pub closed struct Env {
    pub log: sys::Log
} Env

pub closed struct Counter {
    pub total: u64
} Counter

impl Counter: sys::Component[Env, u32] // error: component.state-not-encode

pub fn Counter.start(mut env: Env) -> Counter = Counter(total: 0)

pub fn Counter.on(mut self, take message: u32, mut env: Env, mut cx: sys::Cx[u32]) {
    self.total += u64.widen(message)
} Counter.on
```

```nan
pub closed struct Env {
    pub log: sys::Log
} Env

pub closed struct Peak[T: Ord and Copy and Send] {
    pub highest: Option[T]
} Peak

impl Peak: sys::Component[Env, T] // error: component.state-generic

pub fn Peak.start(mut env: Env) -> Peak[T] = Peak(highest: None)

pub fn Peak.on(mut self, take message: T, mut env: Env, mut cx: sys::Cx[T]) {
    let Some(highest) = self.highest else {
        self.highest = Some(message)
        return
    }
    if message > highest {
        self.highest = Some(message)
    }
} Peak.on
```

```nan
pub closed linear struct Env {
    pub conn: sys::net::Conn
} Env

pub closed struct Relay derives Encode {
    pub relayed: u64
} Relay

impl Relay: sys::Component[Env, u32] // error: component.env-not-abandonable

pub fn Relay.start(mut env: Env) -> Relay = Relay(relayed: 0)

pub fn Relay.on(mut self, take message: u32, mut env: Env, mut cx: sys::Cx[u32]) {
    self.relayed += u64.widen(message)
} Relay.on
```

```nan
pub closed linear struct Env {
    pub budget: mem::Budget
    pub drained: bool
} Env

pub fn Env.abandon(take self)
    requires self.drained
{
    let Env(budget: budget, drained: _) = self
    take budget.close()
} Env.abandon

pub closed struct Drain derives Encode {
    pub drained: u64
} Drain

impl Drain: sys::Component[Env, u32] // error: component.env-not-abandonable

pub fn Drain.start(mut env: Env) -> Drain = Drain(drained: 0)

pub fn Drain.on(mut self, take message: u32, mut env: Env, mut cx: sys::Cx[u32]) {
    self.drained += u64.widen(message)
    env.drained = true
} Drain.on
```

### Env and messages

- The runner owns the Env from the start of `run` and releases it when the component task ends ([runner](#runner)), and a stepper from its start until it ends ([steppers](#steppers)): a linear Env by calling its type's [abandon method](17-suspension-io.md#abandoning-resources), any other as its [end of scope](07-ownership.md#ownership) releases it. The Env holds the component's [capabilities](14-authority.md#capabilities) and [resources](14-authority.md#resources), and only the handlers reach it, by `mut`.
- A handler moves a resource out of the Env by holding it in an `Option` field and calling [`Option.extract`](28-stdlib.md#option-and-result-members); a keyed task can return an affine one inside a message.
- A message enters the intake only through a post on an [outbox](#keyed-tasks), a `send` or an `after` ([component context](#component-context)). The intake holds messages in the order they entered it.
- `on` receives each message by `take`. A message that never reaches `on` is released as its [end of scope](07-ownership.md#ownership) releases it.

```nan
pub closed linear struct Env {
    pub budget: mem::Budget
} Env

pub fn Env.abandon(take self) {
    let Env(budget: budget) = self
    take budget.close()
} Env.abandon

pub closed struct Framer derives Encode {
    pub bytes: u64
} Framer

impl Framer: sys::Component[Env, u32]

pub fn Framer.start(mut env: Env) -> Framer = Framer(bytes: 0)

pub fn Framer.on(mut self, take message: u32, mut env: Env, mut cx: sys::Cx[u32]) {
    let buffer = mem::Bytes.zeroed(usize.widen(message), alloc: env.budget.alloc())
    self.bytes += u64.widen(buffer.len())
} Framer.on

pub fn frame(world: host::World) -> Framer fails task::SendError uses wait {
    let budget = world.alloc.split(65_536)
    return try await host::Runner[Framer].run(Env(budget: take budget), exec: world.exec, alloc: world.alloc, body: fn(inbox: sys::Outbox[u32]) fails task::SendError uses wait {
        try await inbox.post(512)
    })
} frame
```

## Component context

A **component context** is the `sys::Cx[M]` that `on` and the methods of the [system interfaces](#system-interfaces) receive. Its operations act on the component whose message is being handled.

```nan-api
// module: sys

pub open view struct Cx[M: Any and Send] {
} Cx

pub fn Cx.send(mut self, take message: M)

pub fn Cx.after(mut self, ms: u64, take send: M)

pub fn Cx.after_ns(mut self, ns: u64, take send: M)
```

- `sys::Cx[M]` is a base type: a [view](05-properties.md#views), and never linear, Copy, Send, Inert or AnyBits. Only a runner creates one, for one call of `on` or of a system interface method; `start` receives none.
- `mut cx.send(m)` appends `m` to the intake.
- `mut cx.after_ns(ns: n, send: m)` appends `m` to the intake as soon as at least `n` nanoseconds have elapsed since the call, so at once when `n` is 0, and `mut cx.after(ms: n, send: m)` does so after `n` milliseconds. The nanoseconds are those that `sys::Clock.elapsed_ns` counts for the clock that the runner's executor records, and the delay ends only once they are a multiple of the executor's resolution ([executors](16-concurrency.md#executors)) ([capability tables](14-authority.md#capability-tables)); on the [seeded scheduler](22-testing.md#seeded-scheduler) the end of the delay is a pending deadline of the virtual clock. Messages whose delays end at the same time enter in the order of their `after` and `after_ns` calls.
- The operations of a component context never wait. `send`, `after` and `after_ns` draw the message's intake entry through the runner's allocator, and trap when it is exhausted (`trap.exhausted`); the entry is released when the message leaves the intake.
- When the intake stops, every message still delayed by `after` or `after_ns` is released, and a later `send`, `after` or `after_ns` releases its message.

### Headers

A **header** is a `sys::Header` that a task carries, so that the work a message starts carries what its deliverer gave it, such as its cause and sender.

```nan-api
// module: sys

pub closed struct Header derives Copy, Eq, Hash {
    pub bytes: Array[u8, 32]
} Header

pub fn current_header() -> Option[Header]

pub fn run_headed[T, E](header: Header, body: fn() -> T fails E uses wait) -> T fails E uses wait
```

- A task starts with the header of the task that started it, none for the first task of a program, of a test or of an interpreter run.
- While a handler runs for a message, the task running it has the message's header, then its own again. A message delivered by [`deliver_headed`](#steppers) has the header that call gives and one delivered by `deliver` none; a message of the intake has the header of the task that queued it by `send`, `after`, `after_ns` or a [post](#keyed-tasks).
- `sys::current_header()` returns the running task's header.
- `sys::run_headed(h, body: b)` calls `b` on the calling task with `h` as the task's header, then returns `b`'s result or fails with its failure, the task having its own header again. So a runner gives a header to the calls it serves through [`Wire[P].serve`](#wire-encoding), and to a stepper's `start`, `resume` and `resumed` and the tasks they start.

```nan
pub closed struct Env {
    pub log: sys::Log
} Env

pub closed enum Message {
    Count(u32)
    Done
} Message

pub closed struct Countdown derives Encode {
    pub steps: u32
} Countdown

impl Countdown: sys::Component[Env, Message]

pub fn Countdown.start(mut env: Env) -> Countdown = Countdown(steps: 0)

pub fn Countdown.on(mut self, take message: Message, mut env: Env, mut cx: sys::Cx[Message]) {
    match message {
        Message.Count(left) => {
            self.steps += 1
            if left == 0 {
                mut cx.send(Message.Done)
            } else {
                mut cx.after(ms: 1_000, send: Message.Count(left - 1))
            }
        }
        Message.Done => env.log.info("steps", value: self.steps)
    }
} Countdown.on
```

```nan
pub fn requeue(mut cx: sys::Cx[u32], message: u32) limits noalloc {
    mut cx.send(message) // error: limit.allocates
} requeue
```

```nan
pub fn retry(mut cx: sys::Cx[u32], message: u32) limits notrap {
    mut cx.after(ms: 100, send: message) // error: limit.may-trap
} retry
```

## Keyed tasks

A **keyed task** is a task that `mut cx.spawn(key: k, run: f)` starts on the runner's executor, identified by its key `k`. An **outbox** is a `sys::Outbox[M]`, the handle through which tasks post messages to a component's intake.

```nan-api
// module: sys

pub fn Cx.spawn(mut self, key: u64, take run: task fn(out: Outbox[M]) fails task::Cancelled uses wait)

pub fn Cx.cancel(mut self, key: u64)

pub open struct Outbox[M: Any] {
} Outbox

pub fn Outbox.post(self, take message: M) fails task::SendError uses wait
```

- `run` has a `task fn` type, so a closure written for it is a [task closure](16-concurrency.md#task-closures) capturing by `take` and Copy only ([lending](16-concurrency.md#lending)).
- `spawn` first **supersedes** key `k`, in one step: every earlier outbox of key `k` becomes stale, and every keyed task of key `k` still running is cancelled. It then starts a new task running `run` with a new outbox of key `k`, drawing the task's stack and record as `task::Scope.spawn` does ([joins](16-concurrency.md#joins)).
- `mut cx.cancel(key: k)` supersedes key `k` and starts nothing.
- Cancelling a keyed task by superseding its key is a runtime action that is a cancelling construct whose region is that task ([origin and absorption](11-errors-domains.md#origin-and-absorption)). Its cancellation points then behave as [acknowledged cancellation](17-suspension-io.md#acknowledged-cancellation) states; the handler never waits for them.
- The failure of a keyed task, always a cancellation, ends that task alone: the runner releases it.
- `sys::Outbox[M]` is a base type: Copy and Send, and never linear, a view, Inert or AnyBits. It is a [generational handle](12-memory.md#generational-handles) naming a slot of the outbox table, which the capability-tables [support routine](27-program-model.md#runtime-support) keeps: its slots are memory of the support routines charged to no cell, a slot takes a new occupant only under a new generation, and a `spawn`, `run`, `start` or `resume` needing a slot when that memory is exhausted traps (`trap.exhausted`). Each keyed task has its own outbox, the runner passes one more to its body, and a stepper has one more.
- A keyed task that has ended keeps its outbox, and the runner its record of the task, until its key is superseded, as `run` and a stepper's `end` and `abandon` supersede every key: a component holds an outbox slot for each key it spawned under and has not superseded since.

`out.post(m)` is a [cancellation point](17-suspension-io.md#acknowledged-cancellation) and allocates nothing. It runs these steps:

1. When the call is cancelled at its start, it fails with the cancellation. Otherwise, when `out` is stale or the intake has stopped, it fails with `task::SendError.Closed`.
2. Otherwise `m` enters the intake, and the call suspends until the runner removes `m` to pass it to `on`, then returns.
3. When the call is cancelled while `m` waits in the intake, `m` leaves the intake and the call fails with the cancellation, acknowledged at once.
4. When `out` becomes stale while `m` waits in the intake and the call is not cancelled, `m` leaves the intake and the call fails with `task::SendError.Closed`.

```nan
pub closed struct Env {
    pub listeners: mem::List[sys::net::Listener]
} Env

pub closed enum Message {
    Open
    Accepted
} Message

pub closed struct Gate derives Encode {
    pub accepted: u64
} Gate

impl Gate: sys::Component[Env, Message]

pub fn Gate.start(mut env: Env) -> Gate = Gate(accepted: 0)

pub fn Gate.on(mut self, take message: Message, mut env: Env, mut cx: sys::Cx[Message]) {
    match message {
        Message.Open => {
            let Some(listener) = mut env.listeners.pop() else {
                return
            }
            mut cx.spawn(key: 1, run: fn[take listener](out: sys::Outbox[Message]) fails task::Cancelled uses wait {
                loop {
                    let Ok(conn) = attempt await mut listener.accept() else {
                        return
                    }
                    take conn.abandon()
                    let Ok(_) = attempt await out.post(Message.Accepted) else {
                        return
                    }
                }
            })
        }
        Message.Accepted => {
            self.accepted += 1
        }
    }
} Gate.on
```

```nan
// file: pulse/package.nan
//! Heartbeat counter on the host.

package pulse {
    language 1
    targets x86_64-host
    entry main
} pulse

// file: pulse/pulse.nan
//! A component counting the beats of a keyed ticker task.

const TICKER: u64 = 1

pub closed struct Env {
    pub clock: sys::Clock
} Env

pub closed enum Message {
    Start
    Tick
    Stop
} Message

pub closed struct Pulse derives Encode {
    pub ticks: u64
} Pulse

impl Pulse: sys::Component[Env, Message]

pub fn Pulse.start(mut env: Env) -> Pulse = Pulse(ticks: 0)

pub fn Pulse.on(mut self, take message: Message, mut env: Env, mut cx: sys::Cx[Message]) {
    match message {
        Message.Start => {
            let clock = env.clock
            mut cx.spawn(key: TICKER, run: fn(out: sys::Outbox[Message]) fails task::Cancelled uses wait {
                loop {
                    try await clock.sleep(100)
                    let Ok(_) = attempt await out.post(Message.Tick) else {
                        return
                    }
                }
            })
            mut cx.after(ms: 1_000, send: Message.Stop)
        }
        Message.Tick => {
            self.ticks += 1
        }
        Message.Stop => mut cx.cancel(key: TICKER)
    }
} Pulse.on

fn main(take world: host::World) fails task::SendError uses wait {
    let exec = world.exec
    let alloc = world.alloc
    let pause = world.clock
    let last = try await host::Runner[Pulse].run(Env(clock: world.clock), exec, alloc, body: fn(inbox: sys::Outbox[Message]) fails task::SendError uses wait {
        try await inbox.post(Message.Start)
        try await pause.sleep(2_000)
    })
    world.log.info("ticks", value: last.ticks)
} main
```

## Runner

The **runner** `host::Runner[S]` runs a component of state S for the duration of a body.

```nan-api
// module: host

pub open struct Runner[S] {
} Runner

pub fn Runner.run[Env: Any, M: Any, E](take env: Env, exec: task::Executor, alloc: mem::Alloc, body: fn(inbox: sys::Outbox[M]) fails E uses wait) -> S fails E uses wait
```

- `host::Runner[S]` has no values; it names `run`. The runner's executor and allocator are the `exec` and `alloc` of the call.
- A use of `host::Runner[S].run` where S does not implement `sys::Component[Env, M]` for the Env and M fixed at that use ([inference](08-generics-interfaces.md#inference)) is an error (`component.not-component`).
- A use of `host::Runner.run` in code checked for a machine or nanos target, or within the reach of a final build for one ([environments](27-program-model.md#environments)), is an error (`component.unavailable`).

A call `host::Runner[S].run(env, exec, alloc, body: b)` runs these steps:

1. It draws the runner record, which holds the intake, through `alloc`, and traps when `alloc` is exhausted (`trap.exhausted`).
2. It calls `S.start(mut env)` on the calling task; the result is the state.
3. It starts the **component task** on `exec`, drawing its stack and record as `task::Scope.spawn` does. The component task owns the state and the Env and handles the intake's messages as [components](#components) states.
4. The calling task runs `b`, passing it a new outbox.
5. When `b` returns or fails, the intake stops: the messages in it stay, and no message enters it afterwards.
6. The calling task waits until the component task has handled every message left in the intake. The component task then ends, releasing the Env ([Env and messages](#env-and-messages)) and passing the state to the calling task.
7. It supersedes the key of every keyed task still running, and waits until every keyed task has ended.
8. It makes every outbox of the call stale and releases the runner record. It returns the state when `b` returned, and otherwise releases the state and fails with the failure of `b`.

- `run` is an [exempt primitive](17-suspension-io.md#acknowledged-cancellation): it is no cancellation point, and it fails only with the failure of `b`, or, when `exec` is stale, at once with the cancellation of E, without running these steps ([revocation](14-authority.md#revocation)) but releasing the Env as the component task does. Cancellation reaches the cancellation points of `b` and of the keyed tasks; the component task has none. The call waits through steps 6 and 7 whatever cancellation reaches the calling task.
- The component task and every keyed task belong to the [domain](11-errors-domains.md#domains) of the calling task, so a trap in a handler or a keyed task abandons that domain. They lie in the region of every cancelling construct whose region holds the call.

```nan
// file: meter/package.nan
//! Frame meter on the host.

package meter {
    language 1
    targets x86_64-host
    entry main
} meter

// file: meter/meter.nan
//! Totals the sizes of frames posted to a component.

pub closed struct Env {
    pub log: sys::Log
} Env

pub closed struct Meter derives Encode {
    pub frames: u64
    pub bytes: u64
} Meter

impl Meter: sys::Component[Env, Untrusted[mem::Bytes]]

pub fn Meter.start(mut env: Env) -> Meter = Meter(frames: 0, bytes: 0)

pub fn Meter.on(mut self, take message: Untrusted[mem::Bytes], mut env: Env, mut cx: sys::Cx[Untrusted[mem::Bytes]]) {
    self.frames += 1
    self.bytes += u64.widen(message.byte_len())
} Meter.on

pub fn measure(world: host::World, frames: u32, size: usize) -> Meter fails task::SendError uses wait {
    let exec = world.exec
    let alloc = world.alloc
    return try await host::Runner[Meter].run(Env(log: world.log), exec, alloc, body: fn(inbox: sys::Outbox[Untrusted[mem::Bytes]]) fails task::SendError uses wait {
        for _ in 0..<frames {
            try await inbox.post(Untrusted.of(mem::Bytes.zeroed(size, alloc)))
        }
    })
} measure

fn main(take world: host::World) fails task::SendError uses wait {
    let last = try await measure(world, frames: 3, size: 512)
    world.log.info("bytes", value: last.bytes)
} main

test measure_totals_every_frame for measure {
    let world = testing::world()
    let last = try await measure(world, frames: 4, size: 10)
    expect last.frames == 4
    expect last.bytes == 40
} measure_totals_every_frame
```

```nan
// file: gauge/package.nan
//! Gauge reader on the host.

package gauge {
    language 1
    targets x86_64-host
    entry main
} gauge

// file: gauge/gauge.nan
//! Runs a type that is not a component.

pub closed struct Env {
    pub log: sys::Log
} Env

pub closed struct Gauge derives Encode {
    pub level: u32
} Gauge

fn main(take world: host::World) fails task::SendError uses wait {
    let exec = world.exec
    let alloc = world.alloc
    let last = try await host::Runner[Gauge].run(Env(log: world.log), exec, alloc, body: fn(inbox: sys::Outbox[u32]) fails task::SendError uses wait { // error: component.not-component
        try await inbox.post(7)
    })
    world.log.info("level", value: last.level)
} main
```

```nan
// file: probe/package.nan
//! Probe tally for a bare machine.

package probe {
    language 1
    targets x86_64-bare
} probe

// file: probe/probe.nan
//! Totals probe readings in a component.

pub closed struct Env {
} Env

pub closed struct Tally derives Encode {
    pub total: u64
} Tally

impl Tally: sys::Component[Env, u32]

pub fn Tally.start(mut env: Env) -> Tally = Tally(total: 0)

pub fn Tally.on(mut self, take message: u32, mut env: Env, mut cx: sys::Cx[u32]) {
    self.total += u64.widen(message)
} Tally.on

pub fn tally_readings(exec: task::Executor, alloc: mem::Alloc) -> Tally fails task::SendError uses wait {
    return try await host::Runner[Tally].run(Env(), exec, alloc, body: fn(inbox: sys::Outbox[u32]) fails task::SendError uses wait { // error: component.unavailable
        try await inbox.post(1)
    })
} tally_readings
```

## Steppers

A **stepper** `sys::Stepper[S, Env, M]` runs a component of state S on the task holding it, its **holder**, which calls each handler and decides when keyed tasks start. An operating system runs its components so, keeping their states as checkpoints.

```nan-api
// module: sys

pub open linear struct Stepper[S, Env: Any, M: Any] {
} Stepper

pub closed struct Stopped[S, Env: Any] {
    pub state: S
    pub env: Env
} Stopped

pub closed struct Handled derives Copy, Eq {
    pub header: Option[Header]
    pub operation: u16
} Handled

pub closed struct Sent[M: Any] {
    pub message: M
    pub header: Option[Header]
    pub delay_ns: u64
} Sent

pub fn Stepper.start(take env: Env, exec: task::Executor, alloc: mem::Alloc) -> Stepper[S, Env, M] fails task::Cancelled

pub fn Stepper.resume(take state: S, take env: Env, exec: task::Executor, alloc: mem::Alloc) -> Stepper[S, Env, M] fails task::Cancelled

pub fn Stepper.outbox(self) -> Outbox[M]

pub fn Stepper.deliver(mut self, take message: M)

pub fn Stepper.deliver_headed(mut self, take message: M, header: Header)

pub fn Stepper.step(mut self) -> Option[Handled]

pub fn Stepper.take_sent(mut self) -> Option[Sent[M]]

pub fn Stepper.take_held(mut self) -> Option[u64]

pub fn Stepper.incoming(self) fails task::Cancelled uses wait

pub fn Stepper.ready(self) -> bool fails task::Cancelled uses wait

pub fn Stepper.start_keyed(mut self) -> usize

pub fn Stepper.idle(self) -> bool

pub proj fn Stepper.state(self) -> S

pub fn Stepper.end(take self) -> Stopped[S, Env] fails task::Cancelled uses wait

pub fn Stepper.abandon(take self)
```

- `sys::Stepper[S, Env, M]` is a base type: linear, and never a view, Copy, Send, Inert or AnyBits. A use of `sys::Stepper.start` or `sys::Stepper.resume` where S does not implement `sys::Component[Env, M]` for the Env and M fixed at that use is an error (`component.not-component`). Steppers are available on every target.
- `start(env, exec, alloc)` draws the stepper's record, which holds the intake, through `alloc`, trapping when `alloc` is exhausted (`trap.exhausted`), then calls `S.start(mut env)` on the calling task and returns the stepper holding the state and the Env. `resume(state, env, exec, alloc)` does the same with `state` as the state, calling no handler. The stepper's executor and allocator are `exec` and `alloc`. When `exec` is stale, both fail at once with the cancellation, releasing what they took as `run` releases its Env ([revocation](14-authority.md#revocation)).
- `s.outbox()` returns the stepper's outbox, the same on every call.
- `mut s.deliver(m)` calls `on` with `m`, or its operation's handler for a protocol, on the calling task, and `mut s.deliver_headed(m, header: h)` does so with `h` as the message's [header](#headers). `mut s.step()` removes the oldest message of the intake, handles it so, completing its post or giving a request's handler's result or failure to its poster as the runner does, and returns what it handled: a `sys::Handled` holding the message's header, `None` for a message without one, and its operation's number, the index of its variant for an enum M, of its operation for a protocol, and 0 for any other M. It returns `None` when the intake is empty. Both publish the summary as the runner does ([servers](#servers)).
- A keyed task that `spawn` starts on a stepper is **held**: its stack and record are drawn, but it starts only when `mut s.start_keyed()` starts every held keyed task, in the order of their spawns, returning how many it started. Superseding the key of a held task cancels it before it starts.
- `mut s.take_sent()` removes the oldest message that the component's handlers sent through `send`, `after` or `after_ns` and gives it, calling no handler, as a `sys::Sent` holding the message, its header and the nanoseconds left before it would enter the intake, 0 for a message of the intake; those of the intake come first, then the delayed ones in the order they would enter it. It returns `None` when no such message waits; posts stay in the intake.
- `mut s.take_held()` supersedes the key of the oldest held keyed task, cancelling it before it starts, and gives that key; it returns `None` when no keyed task is held.
- A stepper is **idle** when its intake is empty, no message waits for its delay to end, and none of its keyed tasks is held or running. `s.idle()` returns whether it is.
- `s.incoming()` returns once a message waits in the intake. `s.ready()` returns `true` once a message waits in the intake and `false` once the stepper is idle. Both are cancellation points, failing with the cancellation when it reaches them while they wait.
- `s.state()` gives the state. Handlers run only within `start`, `deliver`, `step` and the operations of the [system interfaces](#system-interfaces), so the holder reads the state of a quiescent component, and the [encoding](13-text-encode.md#encode) of it is a **checkpoint** of the component, which `resume` takes back once decoded. A checkpoint holds the state alone: the intake, the delayed messages and the keyed tasks are the holder's to drain first, until the stepper is idle.
- `take s.end()` stops the intake: the posts waiting in it fail with `task::SendError.Closed`, its other messages and every delayed message are released, and no message enters it afterwards. It then supersedes the key of every keyed task, starts every held one, and waits until every keyed task has ended. It makes the stepper's outbox stale and returns the state and the Env in a `sys::Stopped`. It is an [exempt primitive](17-suspension-io.md#acknowledged-cancellation): no cancellation point, and it never fails.
- `take s.abandon()` is the stepper's [abandon method](17-suspension-io.md#abandoning-resources): it stops the intake and supersedes and starts keyed tasks as `end` does, without waiting for them, makes the outbox stale, and releases the state and the Env, a linear Env through its type's abandon method.
- `start`, `deliver`, `step` and the operations of the [system interfaces](#system-interfaces) call handlers on the calling task: the digest of each holds the [declared digest](10-effects-digests.md#indirect-calls) of a call through the methods of `sys::Component` or of a system interface, and a trap in a handler abandons the domain of the calling task. A keyed task belongs to the domain of the task whose handler spawned it.
- A stepper belongs to the domain of the task that started it. When that domain is abandoned, the stepper ends with it: its intake stops, the posts waiting in it fail with `task::SendError.Closed`, a request whose handler was running fails as closed, its delayed messages are released, and its outboxes become stale.

```nan
// file: tally/package.nan
//! A tally stepped on the host.

package tally {
    language 1
    targets x86_64-host
} tally

// file: tally/tally.nan
//! Steps a tally until it is idle, checkpoints it and resumes it from the checkpoint.

pub closed enum RunError derives Render {
    Cancelled(task::Cancelled) from
    Decode(core::DecodeError) from
} RunError

pub closed struct Env {
} Env

pub closed enum Count {
    Add(u32)
    Twice(u32)
} Count

pub closed struct Tally derives Encode {
    pub total: u64
} Tally

impl Tally: sys::Component[Env, Count]

pub fn Tally.start(mut env: Env) -> Tally = Tally(total: 0)

pub fn Tally.on(mut self, take message: Count, mut env: Env, mut cx: sys::Cx[Count]) {
    match message {
        Count.Add(value) => {
            self.total += u64.widen(value)
        }
        Count.Twice(value) => {
            mut cx.send(Count.Add(value))
            mut cx.after(ms: 10, send: Count.Add(value))
        }
    }
} Tally.on

pub fn drain(mut stepper: sys::Stepper[Tally, Env, Count]) fails task::Cancelled uses wait {
    while try await stepper.ready() {
        discard mut stepper.step()
    }
} drain

pub fn hibernate(world: host::World, value: u32) -> Tally fails RunError uses wait {
    var stepper = try sys::Stepper[Tally, Env, Count].start(Env(), exec: world.exec, alloc: world.alloc)
    defer take stepper.abandon()
    mut stepper.deliver(Count.Twice(value))
    try await drain(mut stepper)
    var checkpoint = mem::Bytes.empty(world.alloc)
    mem::wire::Codec[Tally].encode(stepper.state(), out: mut checkpoint)
    undefer stepper
    let sys::Stopped(state: _, env: env) = try await take stepper.end()
    let restored = try mem::wire::Codec[Tally].decode(Untrusted.of(checkpoint.view()), alloc: world.alloc)
    var resumed = try sys::Stepper[Tally, Env, Count].resume(take restored, take env, exec: world.exec, alloc: world.alloc)
    mut resumed.deliver(Count.Add(1))
    let last = try await take resumed.end()
    return last.state
} hibernate

test resumed_tally_keeps_its_total for hibernate {
    let world = testing::world()
    let tally = try await hibernate(world, value: 4)
    expect tally.total == 9
} resumed_tally_keeps_its_total
```

On a machine target, a kernel steps its components itself:

```nan
// file: probe/package.nan
//! Probe tally for a bare machine.

package probe {
    language 1
    targets x86_64-bare
} probe

// file: probe/probe.nan
//! Totals probe readings in a component that the kernel steps.

pub closed struct Env {
} Env

pub closed struct Tally derives Encode {
    pub total: u64
} Tally

impl Tally: sys::Component[Env, u32]

pub fn Tally.start(mut env: Env) -> Tally = Tally(total: 0)

pub fn Tally.on(mut self, take message: u32, mut env: Env, mut cx: sys::Cx[u32]) {
    self.total += u64.widen(message)
} Tally.on

pub fn tally_readings(exec: task::Executor, alloc: mem::Alloc, readings: Slice[u32]) -> Tally fails task::Cancelled uses wait {
    var stepper = try sys::Stepper[Tally, Env, u32].start(Env(), exec, alloc)
    for reading in readings {
        mut stepper.deliver(reading)
    }
    let last = try await take stepper.end()
    return last.state
} tally_readings
```

```nan
pub closed struct Env {
} Env

pub closed struct Gauge derives Encode {
    pub level: u32
} Gauge

pub fn level(exec: task::Executor, alloc: mem::Alloc) -> u32 fails task::Cancelled uses wait {
    var stepper = try sys::Stepper[Gauge, Env, u32].start(Env(), exec, alloc) // error: component.not-component
    mut stepper.deliver(7)
    let last = try await take stepper.end()
    return last.state.level
} level
```

### System interfaces

A **system interface** is one of the std interfaces below, which a component state may implement beside `sys::Component` for the same Env and M. Their methods are handlers too: a holder calls them through its stepper as the events of the component's life that they name occur.

```nan-api
// module: sys

pub interface Resume[Env: Any and Send, M: Send] {
    fn resume(mut self, reason: ResumeReason, mut env: Env, mut cx: Cx[M])
} Resume

pub closed enum ResumeReason derives Copy, Eq, Render, Encode {
    Started
    Restored
    Revived
    Updated
    Rewired
} ResumeReason

pub interface Pressure[Env: Any and Send, M: Send] {
    fn pressure(mut self, level: u8, mut env: Env, mut cx: Cx[M])
} Pressure

pub interface Health[Env: Any and Send, M: Send] {
    fn health(mut self, mut env: Env, mut cx: Cx[M]) -> HealthReply
} Health

pub closed enum HealthReply derives Copy {
    Healthy
    Unhealthy(text::Fixed[256])
} HealthReply

pub interface Suspend[Env: Any and Send, M: Send] {
    fn suspend(mut self, mut env: Env, mut cx: Cx[M])
    fn wake(mut self, mut env: Env, mut cx: Cx[M])
} Suspend

pub fn Stepper.resumed(mut self, reason: ResumeReason)

pub fn Stepper.pressure(mut self, level: u8)

pub fn Stepper.health(mut self) -> HealthReply

pub fn Stepper.suspend(mut self)

pub fn Stepper.wake(mut self)
```

- An impl line naming a system interface whose target does not implement `sys::Component` for the same Env and M is an error (`component.system-unmatched`), reported on the impl line.
- `mut s.resumed(reason)` calls `S.resume`, `mut s.pressure(level)` `S.pressure`, `mut s.health()` `S.health`, `mut s.suspend()` `S.suspend` and `mut s.wake()` `S.wake`, each on the calling task as `deliver` calls `on`: with the state and the Env by `mut` and a component context of the stepper, after the operation's argument, publishing the summary afterwards. When S does not implement the interface for the stepper's Env and M, the operation calls nothing, and `health` returns `HealthReply.Healthy`.
- The holder chooses when to call each and what it passes. `resume` re-arms what a component needs after `start` (`Started`) or after resuming from a checkpoint, a checkpoint holding no keyed task and no delayed message: of a restart (`Restored`), of a hibernation (`Revived`), under new code (`Updated`) or with a new Env (`Rewired`). `pressure` reports memory pressure, a higher level meaning scarcer memory, so that the component releases what it can rebuild; `health` gives a liveness reply; `suspend` comes before the device the component drives leaves its working power state, and `wake` after it returns to it.

A reader that its holder restores from a checkpoint re-arms its poll, which the checkpoint does not hold:

```nan
// file: reader/package.nan
//! A polling reader stepped on the host.

package reader {
    language 1
    targets x86_64-host
} reader

// file: reader/reader.nan
//! Polls once resumed, and reports itself unhealthy while it polls nothing.

pub closed struct Env {
} Env

pub closed enum Poll {
    Tick
} Poll

pub closed struct Reader derives Encode {
    pub ticks: u32
    pub polling: bool
} Reader

impl Reader: sys::Component[Env, Poll]

impl Reader: sys::Resume[Env, Poll]

impl Reader: sys::Health[Env, Poll]

pub fn Reader.start(mut env: Env) -> Reader = Reader(ticks: 0, polling: false)

pub fn Reader.on(mut self, take message: Poll, mut env: Env, mut cx: sys::Cx[Poll]) {
    self.ticks += 1
} Reader.on

pub fn Reader.resume(mut self, reason: sys::ResumeReason, mut env: Env, mut cx: sys::Cx[Poll]) {
    self.polling = true
    mut cx.send(Poll.Tick)
} Reader.resume

pub fn Reader.health(mut self, mut env: Env, mut cx: sys::Cx[Poll]) -> sys::HealthReply {
    if self.polling {
        return sys::HealthReply.Healthy
    }
    return sys::HealthReply.Unhealthy(text::Fixed[256].of("not polling"))
} Reader.health

pub fn restore(world: host::World, take checkpoint: Reader) -> Reader fails task::Cancelled uses wait {
    var stepper = try sys::Stepper[Reader, Env, Poll].resume(take checkpoint, env: Env(), exec: world.exec, alloc: world.alloc)
    mut stepper.resumed(sys::ResumeReason.Restored)
    discard mut stepper.step()
    let last = try await take stepper.end()
    return last.state
} restore

test restored_reader_polls_again for restore {
    let world = testing::world()
    let reader = try await restore(world, checkpoint: Reader(ticks: 4, polling: false))
    expect reader.ticks == 5 and reader.polling
} restored_reader_polls_again
```

```nan
pub closed struct Env {
} Env

pub closed struct Gauge derives Encode {
    pub level: u32
} Gauge

impl Gauge: sys::Component[Env, u32]

impl Gauge: sys::Pressure[Env, u64] // error: component.system-unmatched

pub fn Gauge.start(mut env: Env) -> Gauge = Gauge(level: 0)

pub fn Gauge.on(mut self, take message: u32, mut env: Env, mut cx: sys::Cx[u32]) {
    self.level = message
} Gauge.on

pub fn Gauge.pressure(mut self, level: u8, mut env: Env, mut cx: sys::Cx[u64]) {
    self.level = 0
} Gauge.pressure
```

## Protocols

A **protocol** item declares the typed **operations** of the components serving it. Its name is a type, the message type of those components.

```ebnf
protocol_item = 'protocol' IDENT ( 'summary' type_expr )? "{" protocol_operation+ COL0 "}" item_label NL ;
protocol_operation = ( DOC NL )* visibility? "fn" IDENT "(" operation_params? ")" fn_result? fails_clause? limits_clause? NL clause_region? ;
operation_params = operation_param ( "," operation_params? )? ;
operation_param = IDENT ":" type_expr ;
```

- An operation with a result or a `fails` clause is a **request**; any other is a **one-way** operation. Its parameter types, result and failure type are its **types**.
- Operation names are distinct within their protocol (`item.duplicate`). An operation named `post`, `start`, `summary`, `hash` or `serve` is an error (`component.operation-name`).
- An operation is visible where both it and its protocol are.
- Each operation's `limits` clause holds `pure` or a [class](10-effects-digests.md#reversibility), the **class** of the operation, `traceless` for `pure`. An operation without one is an error (`component.operation-class`).
- Each type of an operation is Send and not linear, and each type of a `pub` operation implements [Encode](13-text-encode.md#encode), is a **handle type**: a type implementing [`sys::Capability`](14-authority.md#revocation), capability kinds included, or `sys::Outbox[Q]`, or is a **handle record**: a struct outside std that does not implement Encode and holds no kept fields, whose fields each implement Encode or are of a handle type, at least one of a handle type. Any other type is an error (`component.operation-type`), reported on the operation's line.
- The clause region of an operation holds `requires`, `ensures`, `undo` and `todo` clauses, over the operation's parameters and, in `ensures`, `result`. Any other clause, or a `todo` clause holding an example clause, is an error (`component.operation-clause`).
- The **summary type**, written after `summary`, implements Encode and is Copy; any other is an error (`component.summary-type`).

A protocol P is a type with no generic parameters. It is Send, never linear, Copy, a view or AnyBits, and Inert when the parameter types of its operations are. A value of P is a message of one of its one-way operations: `P.op(p: v, ...)` builds the message of the one-way operation `op` holding its arguments, labelled by the operation's parameter names and marked as construction values are ([construction](04-types.md#construction)), where the operation is visible. Naming a request so is an error (`component.request-value`). No pattern matches a value of P.

```nan
pub closed enum Missing derives Render, Encode {
    Empty
} Missing

pub closed struct Usage derives Copy, Encode {
    pub writes: u64
} Usage

pub protocol Cell summary Usage {
    /// The value held, failing when none is.
    pub fn read() -> u32 fails Missing limits pure
    /// Holds `value` from now on.
    pub fn write(value: u32) limits pure
        requires value > 0
    fn expire() limits pure
} Cell

pub fn expiry() -> Cell = Cell.expire()
```

The API view of `Cell` leaves out `expire`, which consumers do not see, and ends with its [protocol hash](#protocol-hashes):

```text
$ nan show example::Cell --api
ok show example::Cell --api: records 1
api example::Cell example/example.nan:10
    pub protocol Cell summary Usage {
        pub fn read() -> u32 fails Missing limits pure
        pub fn write(value: u32) limits pure
            requires value > 0
    } Cell
    properties: Send, Inert
    protocol: ahkvrid3vsd7ojspd5
ok show example::Cell --api: records 1
```

```nan
pub protocol Gate {
    pub fn open(at: u64) // error: component.operation-class
} Gate
```

```nan
pub protocol Relay {
    pub fn forward(frame: mem::Bytes) limits recorded
    pub fn adopt(conn: sys::net::Conn) limits irreversible(emit) // error: component.operation-type
} Relay
```

A [capability kind](14-authority.md#capability-kinds) is a handle type:

```nan
// file: ports/package.nan
//! Ports a router hands to the components it serves.

package ports {
    language 1
    targets any
    allow capability in ports
} ports

// file: ports/ports.nan
//! Ports handed on through the Router protocol.

pub open struct Port {
} Port

impl Port: sys::Kind[u32, u64]

pub protocol Router {
    pub fn attach(port: Port, slot: u32) limits recorded
} Router
```

```nan
pub closed enum Missing derives Render, Encode {
    Empty
} Missing

pub protocol Cell {
    pub fn read() -> u32 fails Missing limits pure
} Cell

pub fn reading() -> Cell = Cell.read() // error: component.request-value
```

### Servers

An impl line `impl S: sys::Component[Env, P]`, P a protocol, declares a component **serving** P. In place of `S.on`, its [conformance](08-generics-interfaces.md#conformance) requires the **handler** of each operation `op` of P, read as an interface method with this head, and, when P declares a summary type Y, the **summary function**, read as an interface method `fn summary(self) -> Y`:

```text
fn op(mut self, take p: T, ..., mut env: Env, mut cx: sys::Cx[P]) -> R fails E
```

The parameters `p: T` are the operation's in order, R is its result and E its failure type, `-> R` and `fails E` being left out where the operation has none.

- A missing handler or summary function is an error (`interface.missing-method`) and a mismatched head one (`interface.head-mismatch`), as conformance states. The visibility of a handler is the less visible of S and the operation, and that of the summary function the less visible of S and P (`interface.impl-visibility`).
- The operation's limits apply to its handler as an interface method's apply to its implementing function, and the code of the summary function meets `limits pure`.
- The operation's `requires` and `ensures` clauses bind its handler as an interface method's bind its implementing function ([interface contracts](18-contracts.md#interface-contracts)), over the parameters they share.
- An operation's `undo` clause names another operation of P by its name; anything else is an error (`effect.undo-target`), and so is an undoer whose class is not at most `restorable` (`effect.undo-irreversible`). The handler's digest is compensated as if the handler had the clause ([compensation](10-effects-digests.md#compensation)), and the ledger records an `undo` entry on P.
- The runner of a component serving P passes each message it removes from the intake to its operation's handler with the message's arguments, in place of `on`. When a request's handler returns or fails, its result or failure goes to the task that posted the request ([operation calls](#operation-calls)), unless the handler [postponed](#runner-capabilities) the request.
- When P declares a summary type, the runner **publishes** the component's summary: after `start` returns and after each handler returns or fails, it calls the summary function on the state and keeps the value it gives, in place of the one kept before.

```nan
pub closed enum Missing derives Render, Encode {
    Empty
} Missing

pub closed struct Usage derives Copy, Encode {
    pub writes: u64
} Usage

pub protocol Cell summary Usage {
    pub fn read() -> u32 fails Missing limits pure
    pub fn write(value: u32) limits pure
} Cell

pub closed struct Env {
} Env

pub closed struct Slot derives Encode {
    pub value: u32
    pub writes: u64
} Slot

impl Slot: sys::Component[Env, Cell]

pub fn Slot.start(mut env: Env) -> Slot = Slot(value: 0, writes: 0)

pub fn Slot.read(mut self, mut env: Env, mut cx: sys::Cx[Cell]) -> u32 fails Missing {
    if self.value == 0 {
        fail Missing.Empty
    }
    return self.value
} Slot.read

pub fn Slot.write(mut self, take value: u32, mut env: Env, mut cx: sys::Cx[Cell]) {
    self.value = value
    self.writes = self.writes.wrapping_add(1)
} Slot.write

pub fn Slot.summary(self) -> Usage = Usage(writes: self.writes)
```

```nan
pub protocol Cell {
    pub fn write(value: u32) limits pure
} Cell

pub closed struct Env {
} Env

pub closed struct Slot derives Encode {
    pub value: u32
} Slot

impl Slot: sys::Component[Env, Cell] // error: interface.missing-method

pub fn Slot.start(mut env: Env) -> Slot = Slot(value: 0)

pub fn Slot.on(mut self, take message: Cell, mut env: Env, mut cx: sys::Cx[Cell]) {
} Slot.on
```

The handler of `split` meets its operation's `notrap` through the operation's `requires`:

```nan
pub protocol Share {
    pub fn split(parts: u32) -> u32 limits pure, notrap
        requires parts > 0
} Share

pub closed struct Env {
} Env

pub closed struct Pie derives Encode {
    pub size: u32
} Pie

impl Pie: sys::Component[Env, Share]

pub fn Pie.start(mut env: Env) -> Pie = Pie(size: 100)

pub fn Pie.split(mut self, take parts: u32, mut env: Env, mut cx: sys::Cx[Share]) -> u32 {
    return self.size / parts
} Pie.split
```

```nan
pub protocol Lamp {
    pub fn light() limits restorable
        undo dim "dim turns off the lamp light turned on"
    pub fn dim() limits restorable
        undo light "light turns on the lamp dim turned off"
} Lamp
```

```nan
pub protocol Lamp {
    pub fn light() limits restorable
        undo smash "smash puts the room dark again" // error: effect.undo-irreversible
    pub fn smash() limits irreversible(actuate)
} Lamp
```

```nan
pub protocol Journal {
    pub fn note(entry: u64) limits traceless
} Journal

pub closed struct Env {
    pub log: sys::Log
} Env

pub closed struct Notes derives Encode {
    pub count: u64
} Notes

impl Notes: sys::Component[Env, Journal]

pub fn Notes.start(mut env: Env) -> Notes = Notes(count: 0)

pub fn Notes.note(mut self, take entry: u64, mut env: Env, mut cx: sys::Cx[Journal]) {
    env.log.info("entry", value: entry) // error: limit.reversibility-exceeded
} Notes.note
```

### Operation calls

On a value of `sys::Outbox[P]`, P a protocol, the candidates of [method resolution](08-generics-interfaces.md#method-resolution) for a name also hold P's operation of that name where it is visible, and `summary` when P declares a summary type Y. As methods of the outbox, they have these heads:

| Candidate | Head |
| --- | --- |
| an operation without a `fails` clause | `fn op(self, take p: T, ...) -> R fails task::SendError uses wait`, without `-> R` for a one-way operation |
| an operation with `fails E` | `fn op(self, take p: T, ...) -> R fails sys::CallError[E] uses wait` |
| `summary` | `fn summary(self) -> Y fails task::SendError` |

```nan-api
// module: sys

pub closed enum CallError[E: Send] derives Render {
    Failed(E)
    Malformed(core::DecodeError)
    Closed(task::SendError) from
    Cancelled(task::Cancelled) from
} CallError
```

- A one-way call `out.op(...)` posts the message `P.op(...)` as [`out.post`](#keyed-tasks) does.
- A request call `out.op(...)` runs the steps of a post of its message. Once the runner removes the message, the call waits, whatever cancellation reaches it, until the handler returns or fails, then returns the handler's result or fails with `CallError.Failed` holding its failure; it fails as closed when the task running the handler stops first. When the posting task has stopped by then, the result or failure is released.
- A request call fails as closed with `task::SendError.Closed`, or `CallError.Closed` holding it when its operation has a `fails` clause.
- `out.summary()` gives the summary the component published last, and fails with `task::SendError.Closed` when `out` is stale. It never waits.
- At an operation call, the operation's `requires`, over the arguments after the outbox, is an obligation, and its `ensures` a fact once the call returns.
- The digest of an operation call states waiting and the operation's class, compensated when the operation has an `undo` clause; that of building a message states the operation's class alone, and that of `summary` nothing. The handler's digest never enters the caller's.

```nan
// file: cell/package.nan
//! A cell on the host.

package cell {
    language 1
    targets x86_64-host
} cell

// file: cell/cell.nan
//! A cell holding one value, served through the Cell protocol.

pub closed enum Missing derives Render, Encode {
    Empty
} Missing

pub closed struct Usage derives Copy, Encode {
    pub writes: u64
} Usage

pub protocol Cell summary Usage {
    pub fn read() -> u32 fails Missing limits pure
    pub fn write(value: u32) limits pure
} Cell

pub closed struct Env {
} Env

pub closed struct Slot derives Encode {
    pub value: u32
    pub writes: u64
} Slot

impl Slot: sys::Component[Env, Cell]

pub fn Slot.start(mut env: Env) -> Slot = Slot(value: 0, writes: 0)

pub fn Slot.read(mut self, mut env: Env, mut cx: sys::Cx[Cell]) -> u32 fails Missing {
    if self.value == 0 {
        fail Missing.Empty
    }
    return self.value
} Slot.read

pub fn Slot.write(mut self, take value: u32, mut env: Env, mut cx: sys::Cx[Cell]) {
    self.value = value
    self.writes = self.writes.wrapping_add(1)
} Slot.write

pub fn Slot.summary(self) -> Usage = Usage(writes: self.writes)

pub fn exchange(world: host::World, value: u32) -> Slot fails sys::CallError[Missing] uses wait {
    let exec = world.exec
    let alloc = world.alloc
    return try await host::Runner[Slot].run(Env(), exec, alloc, body: fn(inbox: sys::Outbox[Cell]) fails sys::CallError[Missing] uses wait {
        try await inbox.write(value: value)
        discard try await inbox.read()
        discard try inbox.summary()
    })
} exchange

test exchange_writes_once for exchange {
    let world = testing::world()
    let slot = try await exchange(world, value: 7)
    expect slot.value == 7
    expect slot.writes == 1
} exchange_writes_once
```

```nan
pub protocol Mailer {
    pub fn send(to: u64) limits irreversible(emit)
} Mailer

pub fn notify(outbox: sys::Outbox[Mailer]) fails task::SendError uses wait limits restorable {
    try await outbox.send(to: 1) // error: limit.reversibility-exceeded
} notify
```

### Protocol hashes

The **operation hash** of an operation is the hash of the UTF-8 bytes `operation`, LF, each of the operation's lines in its protocol's [normalised form](29-identity-versioning.md#normalised-form) followed by LF, then, for each of its types in order, parameters first, the full form of the type's [schema hash](13-text-encode.md#schema-hashes), for a handle type its **handle line**, `handle ` and the type's normalised text, or for a handle record `record ` and the full form of the hash of `record`, LF, then, in byte order of name, each field's name, a space and the schema hash or handle line of its type followed by LF; each followed by LF. The **protocol hash** of a protocol is the hash of `protocol`, LF, then, when it declares a summary type, `summary `, the full form of that type's schema hash and LF, then, for each `pub` operation in byte order of name, its name, a space, the full form of its operation hash and LF.

- The code root of a package records the hashes of each `pub` protocol it declares ([code roots](30-artefacts.md#code-roots)), and `nan show --api` prints a protocol's ([commands](33-toolchain.md#commands)).
- Closures exchange a protocol's operations through their [wire encoding](#wire-encoding): all of them when they agree on its protocol hash, and otherwise each operation that the server's [protocol schema](#protocol-schemas) serves for the client's.

### Protocol schemas

The **protocol schema** of a protocol describes what its `pub` operations exchange, so that closures declaring a protocol differently judge, operation by operation, which calls they can still exchange.

```nan-api
// module: sys

pub closed struct ProtocolSchema derives Encode {
    pub hash: ProtocolHash
    pub operations: mem::List[OperationSchema]
} ProtocolSchema

pub closed struct OperationSchema derives Encode {
    pub name: text::String
    pub class: Reversibility
    pub parameters: mem::List[ParameterSchema]
    pub result: Option[TypeSchema]
    pub failure: Option[TypeSchema]
} OperationSchema

pub closed enum Reversibility derives Copy, Eq, Encode {
    Traceless
    Recorded
    Restorable
    Irreversible(emit: bool, actuate: bool, erase: bool, spend: bool)
} Reversibility

pub closed struct ParameterSchema derives Encode {
    pub name: text::String
    pub position: u32
    pub value: TypeSchema
} ParameterSchema

pub closed enum TypeSchema derives Encode {
    Encoded(mem::wire::NamedSchema)
    Handle(Array[u8, 33])
    Record(mem::List[FieldSchema])
} TypeSchema

pub closed struct FieldSchema derives Encode {
    pub name: text::String
    pub value: PartSchema
} FieldSchema

pub closed enum PartSchema derives Encode {
    Encoded(mem::wire::NamedSchema)
    Handle(Array[u8, 33])
} PartSchema

pub fn Wire.schema(alloc: mem::Alloc) -> ProtocolSchema

pub fn ProtocolSchema.serves(self, client: ProtocolSchema, operation: Str, alloc: mem::Alloc) -> bool

pub fn ProtocolSchema.serves_call(self, hash: ProtocolHash, peers: Slice[ProtocolSchema], operation: Str, alloc: mem::Alloc) -> bool
```

- `Wire[P].schema(alloc)` gives P's protocol schema drawn through `alloc`, trapping when memory is exhausted (`trap.exhausted`): P's protocol hash and, in byte order of name, each `pub` operation of P with the [class](10-effects-digests.md#reversibility) its `limits` clause states, its parameters in byte order of name, each with its position among the operation's parameters as declared, from 0, the type schema of its result when it is a request, and that of its failure when it has a `fails` clause. A class is the `Reversibility` variant of its name, `Irreversible` holding whether it has each kind. The **type schema** of a handle type is `Handle` holding its [handle hash](#wire-encoding); that of a handle record is `Record` holding, in byte order of name, each field's name and the part schema of its type, which is `Handle` or `Encoded` as a type schema is; that of another type is `Encoded` holding its [schema tree](13-text-encode.md#schema-hashes) with its names.
- A type or part schema **accepts** another when both are `Encoded` and its tree [accepts](29-identity-versioning.md#encode-class) the other's, both are `Handle` holding one hash, or both are `Record` with fields of the same names, each field's part schema accepting the other's of its name.
- A server's protocol schema S **serves** an operation for a client's protocol schema C when S has an operation of that name and, unless S and C hold one protocol hash, C has one too, such that: they have parameters of the same names, each of S's accepting C's of its name; C's has a result exactly when S's has one, accepting it; and C's has a failure when S's has one, accepting it. S's decoders then read every call of it C encodes, and C's every reply S gives. `server.serves(client, operation, alloc)` decides it, comparing trees as `Schema.accepts` does through `alloc`.
- S serves a call of an operation made under a protocol hash `hash`, knowing the schemas `peers`, when it serves the operation for itself if `hash` is its own, and otherwise for the first schema of `peers` holding `hash`, none serving it when no schema does. `server.serves_call(hash, peers, operation, alloc)` decides it.

### Wire encoding

Closures exchange the calls of a protocol's operations and their replies as `sys::Encoded` values: the `mem::wire` bytes of a call or a reply, and beside them the **handles** through which its values of handle types travel. `sys::Wire[P]` encodes the calls of P's `pub` operations and serves the calls it decodes to a component serving P; `sys::Reply` decodes their replies.

```nan-api
// module: sys

pub interface Protocol {
} Protocol

pub closed struct ProtocolHash derives Copy, Eq, Ord, Hash, Encode {
    pub bytes: Array[u8, 33]
} ProtocolHash

pub open struct Handle {
} Handle

pub fn Handle.of[C: Capability and Copy](capability: C) -> Handle

pub closed struct Encoded {
    pub bytes: mem::Bytes
    pub handles: mem::List[Handle]
} Encoded

pub fn Encoded.empty(alloc: mem::Alloc) -> Encoded

pub open struct Wire[P: Protocol] {
} Wire

pub fn Wire.hash() -> ProtocolHash limits pure, notrap, terminates

pub fn Wire.serve(out: Outbox[P], request: u64, hash: ProtocolHash, peers: Slice[ProtocolSchema], raw: Untrusted[Slice[u8]], handles: Slice[Handle], alloc: mem::Alloc) -> Option[Encoded] fails CallError[Never] uses wait

pub open struct Reply[R: Send, E: Send] {
} Reply

pub fn Reply.decode(self, raw: Untrusted[Slice[u8]], handles: Slice[Handle], alloc: mem::Alloc) -> R fails CallError[E]
```

- Every protocol implements `sys::Protocol`, and nothing else does: an impl line naming it is an error (`component.protocol-impl`).
- A `sys::Handle` holds a value of a handle type and that type's **handle hash**, the hash of its handle line ([protocol hashes](#protocol-hashes)). It is a base type and a primitive capability, Copy and Send. `sys::Reply[R, E]` is a base type holding nothing, Copy and Send.
- `Wire.hash()` gives P's protocol hash, `Encoded.empty(alloc)` an encoded call without bytes or handles, and `sys::Handle.of(capability)` the handle holding `capability` with C's handle hash, as the handles beside an encoding hold it.

On `sys::Wire[P]`, the candidates of method resolution for a name also hold P's `pub` operation of that name where it is visible. As members of `sys::Wire[P]`, they have these heads, E being `Never` for an operation without a `fails` clause:

| Candidate | Head |
| --- | --- |
| a one-way operation | `fn op(mut out: sys::Encoded, take p: T, ...)` |
| a request | `fn op(mut out: sys::Encoded, take p: T, ...) -> sys::Reply[R, E]` |

The **encoding** of a call of `op` is a `mem::wire` variant keyed `op`, without payload when the operation has no parameters, and otherwise with the record of its arguments keyed by their parameters' names, as an enum variant with labelled payloads [encodes](13-text-encode.md#derived-encode-and-decode). The encoding of the **reply** to a request is the variant `Ok` holding its handler's result or `Err` holding its failure, as `Result[R, E]` encodes. A value of a handle type encodes as an unsigned integer: the index of its handle among the handles beside the encoding, which hold them in the order the encoding does. A handle record encodes as a struct deriving Encode would. Decoding an encoding against its handles reads each value of a handle type T as `mem::wire::Codec.decode` reads one ([wire format](13-text-encode.md#wire-format)), and also fails with `Malformed` when an index it reads is not the count of handles read before it, with `Truncated` when no handle has that index, with `Invalid` when that handle's handle hash is not T's, and with `Trailing` when it leaves handles unread.

- `Wire[P].op(mut out, ...)` appends the encoding of its call to `out.bytes` and the handles of its values of handle types to `out.handles`, each index counting the handles `out` held before, trapping when memory is exhausted (`trap.exhausted`). The operation's `requires` is no obligation there, and its class enters no digest.
- `Wire[P].serve(out, request, hash, peers, raw, handles, alloc)` serves a call made under the protocol hash `hash`, which the holder serving it numbers `request`, `peers` holding the protocol schemas of the clients the server knows. It decodes the encoding of a call of a `pub` operation of P from `raw` against `handles`, an encoding keyed by another name failing with `UnknownVariant`. When `hash` is not P's protocol hash, it then fails with `UnknownSchema` unless P's protocol schema [serves the call](#protocol-schemas) under `hash` knowing `peers`, the schema drawn and the call judged through `alloc`. It then evaluates the operation's `requires` clauses over the arguments, a false one failing with `Invalid`, each such failure being `CallError.Malformed` holding it. It then calls the operation through `out` with the arguments, a request carrying its number, and gives `Some` of a `sys::Encoded` drawn through `alloc`, trapping when memory is exhausted: without bytes or handles for a one-way operation, and holding the reply for a request, `Err` when its handler failed. It gives `None` when the handler [postponed](#runner-capabilities) the request. The call failing as closed or cancelled, `serve` fails with `CallError.Closed` or `CallError.Cancelled` holding that failure.
- `reply.decode(raw, handles, alloc)` decodes the reply of its request from `raw` against `handles` and gives its result, fails with `CallError.Failed` holding its failure, or, when it does not decode, with `CallError.Malformed` holding the decoding's failure. A call through an outbox never fails with `CallError.Malformed`.
- The digest of `serve` states waiting and the class of each `pub` operation of P ([operation calls](#operation-calls)).
- The [codec uses](15-untrusted.md#validate-grants) of `serve` mark P `decode` and `encode`, a use of the encoder of a protocol being a use of the encoders of the results and failures of its `pub` requests, and those of `decode` mark R and E `decode`. A call of `Wire[P].op` uses the encoder of each type of its operation's parameters.

A probe and a tally in separate closures each declare the `Counter` protocol over the types of `refusals`, with one protocol hash, so one encodes calls the other serves. The probe's `Adder`, declaring refusals of its own, has another protocol hash, yet the tally serves its `add`, whose schemas are compatible. The tally refuses the one-way `add` of the probe's `Gauge`, and calls breaking `requires`:

```nan
// file: refusals/package.nan
//! The refusals of tallies.

package refusals {
    language 1
    targets any
} refusals

// file: refusals/refusals.nan
//! Why a tally refuses an amount.

pub closed enum Refused derives Render, Encode {
    TooLarge
} Refused

// file: counter/package.nan
//! A tally other closures call.

package counter {
    language 1
    targets x86_64-host
    dep refusals "../refusals"
    allow validate in counter
} counter

// file: counter/counter.nan
//! A tally served through the Counter protocol.

pub protocol Counter {
    pub fn add(amount: u32) -> u64 fails refusals::Refused limits pure
        requires amount > 0
    pub fn reset() limits pure
    pub fn echo(clock: sys::Clock) -> sys::Clock limits pure
} Counter

pub closed struct Env {
} Env

pub closed struct Tally derives Encode {
    pub total: u64
} Tally

impl Tally: sys::Component[Env, Counter]

pub fn Tally.start(mut env: Env) -> Tally = Tally(total: 0)

pub fn Tally.add(mut self, take amount: u32, mut env: Env, mut cx: sys::Cx[Counter]) -> u64 fails refusals::Refused {
    if amount > 1_000 {
        fail refusals::Refused.TooLarge
    }
    self.total = self.total.wrapping_add(u64.widen(amount))
    return self.total
} Tally.add

pub fn Tally.reset(mut self, mut env: Env, mut cx: sys::Cx[Counter]) {
    self.total = 0
} Tally.reset

pub fn Tally.echo(mut self, take clock: sys::Clock, mut env: Env, mut cx: sys::Cx[Counter]) -> sys::Clock = clock

/// The reply of the tally behind `inbox` to a call made under the protocol hash `hash`, knowing
/// the clients whose protocol schemas `peers` holds; the tally postpones no request.
pub fn serve(
    inbox: sys::Outbox[Counter],
    hash: sys::ProtocolHash,
    peers: Slice[sys::ProtocolSchema],
    call: sys::Encoded,
    alloc: mem::Alloc,
    ) -> sys::Encoded fails sys::CallError[Never] uses wait {
    let Some(reply) = try await sys::Wire[Counter].serve(inbox, request: 0, hash, peers, raw: Untrusted.of(call.bytes.view()), handles: call.handles.view(), alloc) else {
        fail sys::CallError.Closed(task::SendError.Closed)
    }
    return reply
} serve

// file: probe/package.nan
//! A client of the tallies other closures run.

package probe {
    language 1
    targets x86_64-host
    dep refusals "../refusals"
} probe

// file: probe/probe.nan
//! Its own declarations of the protocols of the tallies it calls.

pub protocol Counter {
    pub fn add(amount: u32) -> u64 fails refusals::Refused limits pure
        requires amount > 0
    pub fn reset() limits pure
    pub fn echo(clock: sys::Clock) -> sys::Clock limits pure
} Counter

pub protocol Gauge {
    pub fn add(amount: u64) limits pure
} Gauge

pub closed enum Refusal derives Render, Encode {
    Busy
    TooLarge
} Refusal

pub protocol Adder {
    pub fn add(amount: u32) -> u64 fails Refusal limits pure
} Adder

// file: link/package.nan
//! Calls carried from the closure of probe to that of counter.

package link {
    language 1
    targets x86_64-host
    dep counter "../counter"
    dep probe "../probe"
    dep refusals "../refusals"
    allow validate in counter, link
} link

// file: link/link.nan
//! Calls of a probe served by a tally, each carried encoded.

/// The reply to `call`, made under the protocol hash `hash`, of the tally behind `inbox`, which
/// knows the probe's adders and gauges.
fn carry(inbox: sys::Outbox[counter::Counter], hash: sys::ProtocolHash, call: sys::Encoded, alloc: mem::Alloc) -> sys::Encoded fails sys::CallError[refusals::Refused] uses wait {
    var peers = mem::List[sys::ProtocolSchema].empty(alloc)
    mut peers.push(sys::Wire[probe::Adder].schema(alloc))
    mut peers.push(sys::Wire[probe::Gauge].schema(alloc))
    return match attempt await counter::serve(inbox, hash, peers: peers.view(), call, alloc) {
        Ok(reply) => reply
        Err(sys::CallError.Malformed(error)) => fail sys::CallError.Malformed(error)
        Err(sys::CallError.Cancelled(cancelled)) => fail sys::CallError.Cancelled(take cancelled)
        else => fail sys::CallError.Closed(task::SendError.Closed)
    }
} carry

/// Whether a tally serves the additions of an adder.
pub fn serves_adder(alloc: mem::Alloc) -> bool
    example serves_adder(testing::world().alloc)
{
    let tally = sys::Wire[counter::Counter].schema(alloc)
    return tally.serves(sys::Wire[probe::Adder].schema(alloc), operation: "add", alloc) and not tally.serves(sys::Wire[probe::Gauge].schema(alloc), operation: "add", alloc)
} serves_adder

/// The sum an adder reads from a tally's reply `reply` to its addition, zero when it reads none.
pub fn read_sum(reply: Untrusted[Slice[u8]], alloc: mem::Alloc) -> u64
    example read_sum(Untrusted.of(b"\x02Ok\x01\x01\x05"), alloc: testing::world().alloc) == 5
    example read_sum(Untrusted.of(b"\x03Err\x01\x0a\x08TooLarge\x00"), alloc: testing::world().alloc) == 0
{
    var call = sys::Encoded.empty(alloc)
    let added = sys::Wire[probe::Adder].add(mut call, amount: 5)
    let Ok(sum) = attempt added.decode(reply, handles: call.handles.view(), alloc) else {
        return 0
    }
    return sum
} read_sum

test probe_calls_tally for carry {
    let world = testing::world()
    let alloc = world.alloc
    let clock = world.clock
    let last = try await host::Runner[counter::Tally].run(counter::Env(), exec: world.exec, alloc, body: fn(inbox: sys::Outbox[counter::Counter]) fails sys::CallError[refusals::Refused] uses wait {
        let hash = sys::Wire[probe::Counter].hash()
        var call = sys::Encoded.empty(alloc)
        let added = sys::Wire[probe::Counter].add(mut call, amount: 5)
        expect call.bytes.view() == b"\x03add\x01\x0a\x01\x06amount\x01\x05"
        let reply = try await carry(inbox, hash, call, alloc)
        let total = try added.decode(Untrusted.of(reply.bytes.view()), handles: reply.handles.view(), alloc)
        expect total == 5
        var large = sys::Encoded.empty(alloc)
        let refused = sys::Wire[probe::Counter].add(mut large, amount: 5_000)
        let answer = try await carry(inbox, hash, call: large, alloc)
        let outcome = attempt refused.decode(Untrusted.of(answer.bytes.view()), handles: answer.handles.view(), alloc)
        let Err(sys::CallError.Failed(refusals::Refused.TooLarge)) = outcome else {
            expect false
            return
        }
        var echo = sys::Encoded.empty(alloc)
        let echoed = sys::Wire[probe::Counter].echo(mut echo, clock)
        expect echo.handles.len() == 1
        let echoed_reply = try await carry(inbox, hash, call: echo, alloc)
        let returned = attempt echoed.decode(Untrusted.of(echoed_reply.bytes.view()), handles: echoed_reply.handles.view(), alloc)
        let Ok(returned_clock) = returned else {
            expect false
            return
        }
        let elapsed = attempt returned_clock.elapsed_ns()
        expect elapsed.is_ok()
        var adding = sys::Encoded.empty(alloc)
        let summed = sys::Wire[probe::Adder].add(mut adding, amount: 2)
        let summed_reply = try await carry(inbox, hash: sys::Wire[probe::Adder].hash(), call: adding, alloc)
        let sum = attempt summed.decode(Untrusted.of(summed_reply.bytes.view()), handles: summed_reply.handles.view(), alloc)
        let Ok(7) = sum else {
            expect false
            return
        }
        var other = sys::Encoded.empty(alloc)
        sys::Wire[probe::Gauge].add(mut other, amount: 1)
        let mismatched = attempt await carry(inbox, hash: sys::Wire[probe::Gauge].hash(), call: other, alloc)
        let Err(sys::CallError.Malformed(core::DecodeError.UnknownSchema)) = mismatched else {
            expect false
            return
        }
        var zero = sys::Encoded.empty(alloc)
        discard sys::Wire[probe::Counter].add(mut zero, amount: 0)
        let broken = attempt await carry(inbox, hash, call: zero, alloc)
        let Err(sys::CallError.Malformed(core::DecodeError.Invalid)) = broken else {
            expect false
            return
        }
        var reset = sys::Encoded.empty(alloc)
        sys::Wire[probe::Counter].reset(mut reset)
        let reset_reply = try await carry(inbox, hash, call: reset, alloc)
        expect reset_reply.bytes.len() == 0
    })
    expect last.total == 0
} probe_calls_tally
```

A handle record carries values and handles together, here a reading with the clock it was taken on:

```nan
// file: stamps/package.nan
//! Readings handed out with their clock.

package stamps {
    language 1
    targets x86_64-host
    allow validate in stamps
} stamps

// file: stamps/stamps.nan
//! Readings returned beside the clock they were taken on.

pub closed struct Stamped {
    pub at: u64
    pub clock: sys::Clock
} Stamped

pub protocol Stamps {
    pub fn stamp(clock: sys::Clock) -> Stamped limits pure
    pub fn keep(stamped: Stamped) limits pure
} Stamps

pub protocol Gauges {
    pub fn stamp(clock: sys::Clock) -> Stamped limits pure
    pub fn reset() limits pure
} Gauges

pub closed struct Env {
} Env

pub closed struct Stamper derives Encode {
    pub stamps: u64
    pub kept: u64
} Stamper

impl Stamper: sys::Component[Env, Stamps]

pub fn Stamper.start(mut env: Env) -> Stamper = Stamper(stamps: 0, kept: 0)

pub fn Stamper.stamp(mut self, take clock: sys::Clock, mut env: Env, mut cx: sys::Cx[Stamps]) -> Stamped {
    self.stamps = self.stamps.wrapping_add(1)
    return Stamped(at: self.stamps, clock: clock)
} Stamper.stamp

pub fn Stamper.keep(mut self, take stamped: Stamped, mut env: Env, mut cx: sys::Cx[Stamps]) {
    self.kept = stamped.at
} Stamper.keep

/// The handles beside a call keeping a stamp taken on `clock`.
pub fn kept_handles(clock: sys::Clock, alloc: mem::Alloc) -> usize
    example kept_handles(testing::world().clock, alloc: testing::world().alloc) == 1
{
    var call = sys::Encoded.empty(alloc)
    sys::Wire[Stamps].keep(mut call, stamped: Stamped(at: 3, clock: clock))
    return call.handles.len()
} kept_handles

test stamps_travel_with_their_clock for Stamper.stamp {
    let world = testing::world()
    let alloc = world.alloc
    let clock = world.clock
    let last = try await host::Runner[Stamper].run(Env(), exec: world.exec, alloc, body: fn(inbox: sys::Outbox[Stamps]) fails sys::CallError[Never] uses wait {
        var call = sys::Encoded.empty(alloc)
        let stamped = sys::Wire[Stamps].stamp(mut call, clock)
        let peers = mem::List[sys::ProtocolSchema].empty(alloc)
        let Some(reply) = try await sys::Wire[Stamps].serve(inbox, request: 0, hash: sys::Wire[Stamps].hash(), peers: peers.view(), raw: Untrusted.of(call.bytes.view()), handles: call.handles.view(), alloc) else {
            expect false
            return
        }
        let value = try stamped.decode(Untrusted.of(reply.bytes.view()), handles: reply.handles.view(), alloc)
        let elapsed = attempt value.clock.elapsed_ns()
        expect reply.handles.len() == 1 and value.at == 1
        expect elapsed.is_ok()
        var keeping = sys::Encoded.empty(alloc)
        sys::Wire[Stamps].keep(mut keeping, stamped: Stamped(at: 7, clock: clock))
        discard try await sys::Wire[Stamps].serve(inbox, request: 1, hash: sys::Wire[Stamps].hash(), peers: peers.view(), raw: Untrusted.of(keeping.bytes.view()), handles: keeping.handles.view(), alloc)
    })
    let served = sys::Wire[Stamps].schema(alloc).serves(sys::Wire[Gauges].schema(alloc), operation: "stamp", alloc)
    expect last.stamps == 1 and last.kept == 7
    expect served
} stamps_travel_with_their_clock
```

An impl line naming `sys::Protocol` is an error:

```nan
pub closed struct Fake {
} Fake

impl Fake: sys::Protocol // error: component.protocol-impl
```

## Runner capabilities

A component reaches what lies beyond its own intake through the **runner capability** its stepper's holder attaches, a `sys::Runner` that a kernel serves through a capability kind of its own ([served capabilities](14-authority.md#served-capabilities)): the components of other closures through numbered **ports**, the requests it postpones, which the holder keeps pending for it, its published summary, its checkpoints and the capabilities and resources the holder keeps for it by number. Calls, replies and summaries reach the runner capability as wire encodings, so the holder and the component may be separate closures.

```nan-api
// module: sys

pub fn Stepper.attach(mut self, runner: Runner)

pub fn Cx.send_to[Q: Protocol](mut self, port: Port[Q], call: Encoded) fails RunnerError

pub fn Cx.postpone(mut self) -> Option[u64]

pub fn Cx.reply(mut self, request: u64, reply: Encoded) fails RunnerError

pub fn Cx.publish(mut self, summary: Slice[u8]) fails RunnerError

pub fn Cx.sync(mut self) fails RunnerError

pub fn Cx.capability[C: Capability and Copy](mut self, id: CapabilityId[C]) -> C fails RunnerError

pub fn Cx.drop_capability[C: Capability and Copy](mut self, id: CapabilityId[C]) fails RunnerError

pub closed struct CapabilityId[C: Capability and Copy] derives Copy, Eq, Encode {
    pub id: u64
} CapabilityId

pub fn CapabilityId.of_handle(self, handle: Handle) -> Option[C]

pub open struct Port[Q: Protocol] derives Copy, Send {
} Port

pub fn Port.of[M: Any and Send](cx: Cx[M], id: u64) -> Port[Q]

pub fn Port.call[R: Send, E: Send](self, reply: Reply[R, E], call: Encoded, alloc: mem::Alloc) -> R fails CallError[E] uses wait

pub interface Resource {
    fn held(take self) -> Held limits pure, notrap, terminates
    fn of_held(take held: Held) -> Result[Self, Held] limits pure, notrap, terminates
} Resource

pub closed enum Held {
    File(fs::File)
    Listener(net::Listener)
} Held

impl fs::File: Resource

impl net::Listener: Resource

pub closed struct ResourceId[R: Resource] derives Copy, Eq, Encode {
    pub id: u64
} ResourceId

pub open struct Resources derives Copy, Send {
} Resources

pub fn Resources.of[M: Any and Send](cx: Cx[M]) -> Resources

pub fn Resources.resource[R: Resource](self, id: ResourceId[R]) -> R fails RunnerError uses wait

pub fn Resources.take_resource[R: Resource](self, id: ResourceId[R]) -> R fails RunnerError uses wait

pub fn Resources.put_resource[R: Resource](self, id: ResourceId[R], take resource: R) fails RunnerError uses wait
```

- A stepper starts without a runner capability, and a run of `host::Runner` never has one. `mut s.attach(runner)` attaches `runner` to the stepper in place of the one attached before. The operations of this section act through the runner capability attached to the stepper whose component context they receive, or through one whose handle is stale when none is.
- `sys::Port[Q].of(cx, id)` gives the port numbered `id` of the runner capability of `cx`, through which a component calls Q's operations on whatever the holder wires to that number. A port holds its runner capability, so a keyed task's closure captures it.
- `mut cx.send_to(port, call)` hands the holder a call of one of Q's operations, encoded as `sys::Wire[Q]` encodes it ([wire encoding](#wire-encoding)): it calls the `send_to` of the port's runner capability with the port's number, Q's protocol hash and the bytes and handles of `call`.
- `mut cx.postpone()` **postpones** the request whose handler receives `cx`, and gives the number its holder gave it through [`sys::Wire.serve`](#wire-encoding): when the handler returns or fails, its result or failure is released, and `serve` gives `None` in place of the reply. A later call within the same handler gives the same number. It gives `None` and postpones nothing in a handler of a one-way operation, of a request that no `serve` numbered or of a system interface, and when no runner capability is attached to the stepper.
- `mut cx.reply(request, reply)` answers the request the holder numbers `request`, calling `reply` with the bytes and handles of `reply`, the encoding of the request's reply ([wire encoding](#wire-encoding)).
- A holder keeps a postponed request pending under its number from the call of `serve` on, as `reply` may reach it before `serve` returns, until `reply` answers it or the holder ends it. The component's state holds the numbers of the requests it postponed, so a later handler answers them, after a checkpoint is resumed too. Meanwhile the client's call waits, a call through a port as a cancellation point.
- `mut cx.publish(summary)` gives the holder the encoding of the component's summary, calling `publish`, and `mut cx.sync()` asks the holder to keep a checkpoint of the component once it is quiescent, calling `sync`.
- A holder keeps capabilities for its component by number, so the component's state, which a checkpoint holds, names each by a `sys::CapabilityId[C]`. `id.of_handle(handle)` gives the capability `handle` holds, none when its handle hash is not C's ([wire encoding](#wire-encoding)), whatever `id.id` is. `mut cx.capability(id)` calls `capability` with `id.id` and gives the capability `id.of_handle` gives of the `sys::Handle` it returns, failing with `RunnerError.Unknown` when that gives none. `mut cx.drop_capability(id)` calls `drop_capability` with `id.id`, telling the holder the component no longer needs that capability.
- Each of these operations of a component context but `postpone` calls its method on the calling task, the handler's, and fails with the method's failure.
- `port.call(reply, call, alloc)` calls the `call` of the port's runner capability with the port's number, Q's protocol hash, the bytes and handles of `call` and `alloc`, then decodes the encoding it gives as `reply.decode` does ([wire encoding](#wire-encoding)), drawing through `alloc`: it gives the request's result, or fails with `CallError.Failed` holding its failure or `CallError.Malformed` when the encoding does not decode. When the method fails, it fails with `CallError.Cancelled` holding a cancellation and with `CallError.Closed` holding `task::SendError.Closed` otherwise. It is a cancellation point, so a component calls it from a keyed task, posting what it gives to its intake.
- A holder keeps files and listeners for its component by number too, each in a `sys::Held` of its type, and the component's state names each by a `sys::ResourceId[R]`. `sys::Resource` is implemented by `sys::fs::File` and `sys::net::Listener` alone: `take r.held()` gives the `Held` holding `r`, and `R.of_held(held)` gives `Ok` of what `held` holds when it is an R and `Err` of `held` otherwise. A connection is held by no number, as an id of a linear type would be linear.
- `sys::Resources.of(cx)` gives the resources of the runner capability of `cx`, which hold that capability as a port does, so a keyed task's closure captures them. Their operations wait, as those of the resources they give do, so a component calls them from a keyed task.
- `resources.resource(id)` lends the component the resource the holder keeps under `id.id`, calling `resource`, and `resources.take_resource(id)` gives it the resource, calling `take_resource`, after which the holder keeps nothing under that number. Each gives what the `Held` the method returns holds when it is an R; otherwise it hands that `Held` back through `put_resource` under `id.id` and fails with `RunnerError.Unknown`. `resources.put_resource(id, resource)` hands the holder `resource` under `id.id`, a lent one back, calling `put_resource` with `take resource.held()`.
- A resource `resource` or `take_resource` gives belongs to the calling task's [domain](11-errors-domains.md#domain-memory), so settlement releases it with that domain and never with the holder's, and one `put_resource` hands over belongs to whatever the holder's kind moves it into.

A mirror hands a store the values it is sent and fetches the store's value from a keyed task, both through port 1 of its runner capability:

```nan
pub protocol Store {
    pub fn put(value: u32) limits pure
    pub fn get() -> u32 limits pure
} Store

pub protocol Front {
    pub fn copy(value: u32) limits traceless
    pub fn fetch() limits traceless
    fn fetched(value: u32) limits traceless
} Front

pub closed struct Env {
    pub alloc: mem::Alloc
} Env

pub closed struct Mirror derives Encode {
    pub last: u32
} Mirror

impl Mirror: sys::Component[Env, Front]

pub fn Mirror.start(mut env: Env) -> Mirror = Mirror(last: 0)

pub fn Mirror.copy(mut self, take value: u32, mut env: Env, mut cx: sys::Cx[Front]) {
    var call = sys::Encoded.empty(env.alloc)
    sys::Wire[Store].put(mut call, value)
    let store = sys::Port[Store].of(cx, id: 1)
    discard attempt mut cx.send_to(store, call)
} Mirror.copy

pub fn Mirror.fetch(mut self, mut env: Env, mut cx: sys::Cx[Front]) {
    let store = sys::Port[Store].of(cx, id: 1)
    let alloc = env.alloc
    mut cx.spawn(key: 1, run: fn(out: sys::Outbox[Front]) fails task::Cancelled uses wait {
        var call = sys::Encoded.empty(alloc)
        let got = sys::Wire[Store].get(mut call)
        let Ok(value) = attempt await store.call(got, call, alloc) else {
            return
        }
        discard attempt await out.fetched(value)
    })
} Mirror.fetch

fn Mirror.fetched(mut self, take value: u32, mut env: Env, mut cx: sys::Cx[Front]) {
    self.last = value
} Mirror.fetched
```

A console postpones a read until a byte arrives, then answers it with that byte, and answers 0 at once when a read waits already or its holder numbered none:

```nan
pub protocol Console {
    pub fn read() -> u8 limits traceless
    pub fn arrived(byte: u8) limits traceless
} Console

pub closed struct Env {
    pub alloc: mem::Alloc
} Env

pub closed struct Uart derives Encode {
    pub waiting: bool
    pub request: u64
} Uart

impl Uart: sys::Component[Env, Console]

pub fn Uart.start(mut env: Env) -> Uart = Uart(waiting: false, request: 0)

pub fn Uart.read(mut self, mut env: Env, mut cx: sys::Cx[Console]) -> u8 {
    if self.waiting {
        return 0
    }
    let Some(request) = mut cx.postpone() else {
        return 0
    }
    self.waiting = true
    self.request = request
    return 0
} Uart.read

pub fn Uart.arrived(mut self, take byte: u8, mut env: Env, mut cx: sys::Cx[Console]) {
    if not self.waiting {
        return
    }
    var reply = sys::Encoded.empty(env.alloc)
    let answer: Result[u8, Never] = Ok(byte)
    mem::wire::Codec[Result[u8, Never]].encode(answer, out: mut reply.bytes)
    discard attempt mut cx.reply(self.request, reply)
    self.waiting = false
} Uart.arrived
```

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `component.env-not-abandonable` | error | [Component state](#component-state) |
| `component.not-component` | error | [Runner](#runner), [Steppers](#steppers) |
| `component.operation-class` | error | [Protocols](#protocols) |
| `component.operation-clause` | error | [Protocols](#protocols) |
| `component.operation-name` | error | [Protocols](#protocols) |
| `component.operation-type` | error | [Protocols](#protocols) |
| `component.protocol-impl` | error | [Wire encoding](#wire-encoding) |
| `component.request-value` | error | [Protocols](#protocols) |
| `component.state-generic` | error | [Component state](#component-state) |
| `component.state-not-encode` | error | [Component state](#component-state) |
| `component.state-not-inert` | error | [Component state](#component-state) |
| `component.summary-type` | error | [Protocols](#protocols) |
| `component.system-unmatched` | error | [System interfaces](#system-interfaces) |
| `component.unavailable` | error | [Runner](#runner) |
