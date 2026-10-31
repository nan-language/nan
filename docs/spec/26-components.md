# Components

This chapter defines components, their handlers and quiescence, component state and its state schema, Env and messages, the component context with its `send` and `after` operations, keyed tasks and outboxes, the runner `host::Runner`, and protocols with their servers, operation calls and hashes.

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
- Env is Send and may be linear, the runner releasing a linear Env through its abandon method ([Env and messages](#env-and-messages)). M is Send and, being unbounded by `Any`, [droppable](05-properties.md#generic-parameters): a linear argument for it is an error (`prop.linear-argument`), reported on the impl line. `sys::Cx` and `sys::Outbox` bound their M, and `host::Runner.run` its Env and M, by `Any`, so the handlers and the uses of the runner accept a linear one.
- A running component has an **intake**, a first-in, first-out queue of messages. Its [runner](#runner) calls `S.start` once, then removes messages from the intake one at a time, oldest first, calling `S.on` for each with the state, the message, the Env and a [component context](#component-context). It removes the next message only after `on` has returned.
- Neither interface method states `uses wait`, so an implementing function stating it is an error by conformance (`interface.head-mismatch`). A handler therefore contains no [suspension point](17-suspension-io.md#stackful-tasks): it never suspends.
- A component is **quiescent** whenever none of its handlers runs. The runner gives the state and the Env to handlers alone, so every handler starts on a quiescent component, and the state `run` returns is that of a quiescent component.

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

- The runner owns the Env from the start of `run` and releases it when the component task ends ([runner](#runner)): a linear Env by calling its type's [abandon method](17-suspension-io.md#abandoning-resources), any other as its [end of scope](07-ownership.md#ownership) releases it. The Env holds the component's [capabilities](14-authority.md#capabilities) and [resources](14-authority.md#resources), and only the handlers reach it, by `mut`.
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

A **component context** is the `sys::Cx[M]` that `on` receives. Its operations act on the component whose message is being handled.

```nan-api
// module: sys

pub open view struct Cx[M: Any and Send] {
} Cx

pub fn Cx.send(mut self, take message: M)

pub fn Cx.after(mut self, ms: u64, take send: M)

pub fn Cx.after_ns(mut self, ns: u64, take send: M)
```

- `sys::Cx[M]` is a base type: a [view](05-properties.md#views), and never linear, Copy, Send, Inert or AnyBits. Only the runner creates one, for one call of `on`; `start` receives none.
- `mut cx.send(m)` appends `m` to the intake.
- `mut cx.after_ns(ns: n, send: m)` appends `m` to the intake as soon as at least `n` nanoseconds have elapsed since the call, so at once when `n` is 0, and `mut cx.after(ms: n, send: m)` does so after `n` milliseconds. The nanoseconds are those that `sys::Clock.elapsed_ns` counts for the clock that the runner's executor records, and the delay ends only once they are a multiple of the executor's resolution ([executors](16-concurrency.md#executors)) ([capability tables](14-authority.md#capability-tables)); on the [seeded scheduler](22-testing.md#seeded-scheduler) the end of the delay is a pending deadline of the virtual clock. Messages whose delays end at the same time enter in the order of their `after` and `after_ns` calls.
- The operations of a component context never wait. `send`, `after` and `after_ns` draw the message's intake entry through the runner's allocator, and trap when it is exhausted (`trap.exhausted`); the entry is released when the message leaves the intake.
- When the intake stops, every message still delayed by `after` or `after_ns` is released, and a later `send`, `after` or `after_ns` releases its message.

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
- `sys::Outbox[M]` is a base type: Copy and Send, and never linear, a view, Inert or AnyBits. It is a [generational handle](12-memory.md#generational-handles) naming a slot of the outbox table, which the capability-tables [support routine](27-program-model.md#runtime-support) keeps: its slots are memory of the support routines charged to no cell, a slot takes a new occupant only under a new generation, and a `spawn` or `run` needing a slot when that memory is exhausted traps (`trap.exhausted`). Each keyed task has its own outbox, and the runner passes one more to its body.

`out.post(m)` is a [cancellation point](17-suspension-io.md#acknowledged-cancellation) and allocates nothing. It runs these steps:

1. When the call is cancelled at its start, it fails with the cancellation. Otherwise, when `out` is stale or the intake has stopped, it fails with `task::SendError.Closed`.
2. Otherwise `m` enters the intake, and the call suspends until the component task removes `m` to pass it to `on`, then returns.
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
- A use of `host::Runner.run` in code checked for a machine target, or within the reach of a final build for one ([environments](27-program-model.md#environments)), is an error (`component.unavailable`).

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

## Protocols

A **protocol** item declares the typed **operations** of the components serving it. Its name is a type, the message type of those components.

```ebnf
protocol_item = 'protocol' IDENT ( 'summary' type_expr )? "{" protocol_operation+ COL0 "}" item_label NL ;
protocol_operation = ( DOC NL )* visibility? "fn" IDENT "(" operation_params? ")" fn_result? fails_clause? limits_clause? NL clause_region? ;
operation_params = operation_param ( "," operation_params? )? ;
operation_param = IDENT ":" type_expr ;
```

- An operation with a result or a `fails` clause is a **request**; any other is a **one-way** operation. Its parameter types, result and failure type are its **types**.
- Operation names are distinct within their protocol (`item.duplicate`). An operation named `post`, `start` or `summary` is an error (`component.operation-name`).
- An operation is visible where both it and its protocol are.
- Each operation's `limits` clause holds `pure` or a [class](10-effects-digests.md#reversibility), the **class** of the operation, `traceless` for `pure`. An operation without one is an error (`component.operation-class`).
- Each type of an operation is Send and not linear, and each type of a `pub` operation implements [Encode](13-text-encode.md#encode) or is a **handle type**: a [primitive capability](05-properties.md#capability-types) that is Send and not linear, capability kinds included, or `sys::Outbox[Q]`. Any other type is an error (`component.operation-type`), reported on the operation's line.
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
    protocol: afshkplsj36ab4xop7
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
- The runner of a component serving P passes each message it removes from the intake to its operation's handler with the message's arguments, in place of `on`. When a request's handler returns or fails, its result or failure goes to the task that posted the request ([operation calls](#operation-calls)).
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
    Closed(task::SendError) from
    Cancelled(task::Cancelled) from
} CallError
```

- A one-way call `out.op(...)` posts the message `P.op(...)` as [`out.post`](#keyed-tasks) does.
- A request call `out.op(...)` runs the steps of a post of its message. Once the component task removes the message, the call waits, whatever cancellation reaches it, until the handler returns or fails, then returns the handler's result or fails with `CallError.Failed` holding its failure; it fails as closed when the component task stops first. When the posting task has stopped by then, the result or failure is released.
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

The **operation hash** of an operation is the hash of the UTF-8 bytes `operation`, LF, each of the operation's lines in its protocol's [normalised form](29-identity-versioning.md#normalised-form) followed by LF, then, for each of its types in order, parameters first, the full form of the type's [schema hash](13-text-encode.md#schema-hashes), or `handle ` and the type's normalised text for a handle type, followed by LF. The **protocol hash** of a protocol is the hash of `protocol`, LF, then, when it declares a summary type, `summary `, the full form of that type's schema hash and LF, then, for each `pub` operation in byte order of name, its name, a space, the full form of its operation hash and LF.

- The code root of a package records the hashes of each `pub` protocol it declares ([code roots](30-artefacts.md#code-roots)), and `nan show --api` prints a protocol's ([commands](33-toolchain.md#commands)).
- Operations reach the components of their own closure only: the wire encoding of messages, and of the handle types they hold, between closures is [reserved](37-evolution.md#reservations).

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `component.env-not-abandonable` | error | [Component state](#component-state) |
| `component.not-component` | error | [Runner](#runner) |
| `component.operation-class` | error | [Protocols](#protocols) |
| `component.operation-clause` | error | [Protocols](#protocols) |
| `component.operation-name` | error | [Protocols](#protocols) |
| `component.operation-type` | error | [Protocols](#protocols) |
| `component.request-value` | error | [Protocols](#protocols) |
| `component.state-generic` | error | [Component state](#component-state) |
| `component.state-not-encode` | error | [Component state](#component-state) |
| `component.state-not-inert` | error | [Component state](#component-state) |
| `component.summary-type` | error | [Protocols](#protocols) |
| `component.unavailable` | error | [Runner](#runner) |
