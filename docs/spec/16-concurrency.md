# Structured concurrency

This chapter defines tasks and task scopes, spawning with join handles and reaping, task closures, the lexical lending rule, deadlines and races, cowns with `await when` and poisoned cowns, channels, atomics and executors.

## Task scopes

A **task** is a stackful unit of concurrent execution that an [executor](#executors) runs ([stackful tasks](17-suspension-io.md#stackful-tasks)). The runtime starts the task of the entry function on `x86_64-host` ([entry](27-program-model.md#entry)) and the task of each test run ([test runs](22-testing.md#test-runs)). Every other task is a child of a task scope, a task of a race, the first task of a domain body ([domain run](11-errors-domains.md#domain-run)), the component task of a [runner](26-components.md#runner), a [keyed task](26-components.md#keyed-tasks) or the first kernel task ([anchor](24-kernel.md#anchor)). A **task scope** is the region that a call of `task::scope` opens; the call ends only after every task spawned on the scope has ended.

```nan-api
// module: task

pub open view struct Scope {
} Scope

pub fn scope[E](exec: Executor, body: fn(s: Scope) fails E uses wait) fails E uses wait

pub fn Scope.cancel(self)

pub fn Scope.is_cancelled(self) -> bool
```

The **body closure** of a scope is the closure written as the `body` argument of its `task::scope` call, when that argument is a closure. A call `task::scope(exec, body: b)` opens a new scope S and runs these steps:

1. The calling task runs `b`, passing a `task::Scope` value of S. Children spawned on S run on `exec`.
2. A **scope failure** is a failure with which `b` or a child of S ends, other than a [cancellation](11-errors-domains.md#cancellation) whose origin is S. A child's failure reaches S by [failure conversion](11-errors-domains.md#failure-conversion) to E; [joins](#joins) rejects a child whose failure type does not convert. The first scope failure that the runtime records is kept, and recording it cancels S. Each later scope failure is released.
3. Once `b` and every child of S have ended, the call fails with the kept failure, or returns when none is kept. The call waits for the children whatever cancellation reaches the calling task.

- Cancelling S makes it a cancelling construct that has cancelled ([origin and absorption](11-errors-domains.md#origin-and-absorption)). Its region is the calling task while it runs `b`, and the children of S. The calls in that region are then cancelled ([acknowledged cancellation](17-suspension-io.md#acknowledged-cancellation)).
- `s.cancel()` cancels S without recording a failure, so a scope ended by `cancel` alone returns. Cancelling a cancelled scope has no effect.
- `s.is_cancelled()` is true exactly when S, or a cancelling construct whose region holds the children of S, has cancelled.
- Scopes nest: the children of a scope opened by `b` or by a child of S lie in the region of S.
- `task::Scope` is a base type: a [view](05-properties.md#views), Send, and never linear, Copy, Inert or AnyBits.

```nan
pub fn take_samples(wanted: u32, exec: task::Executor, alloc: mem::Alloc) -> u64 fails task::SendError uses wait {
    var total: u64 = 0
    try await task::scope(exec, body: fn[mut total](s: task::Scope) fails task::SendError uses wait {
        let channel = task::Channel[u32].bounded(8, alloc)
        let sender = channel.sender
        var receiver = channel.receiver
        discard s.spawn(fn[take sender]() fails task::SendError uses wait {
            var next: u32 = 0
            loop {
                try await sender.send(next)
                next = next.wrapping_add(1)
            }
        })
        for _ in 0..<wanted {
            let Some(value) = try await mut receiver.receive() else {
                return
            }
            total += u64.widen(value)
        }
        s.cancel()
    })
    return total
} take_samples
```

The producer's next `send` after `s.cancel()` fails with a cancellation whose origin is the scope, which the scope absorbs, and the call returns.

## Joins

A **join handle** is the `task::Join[T, E]` that `s.spawn(c)` returns for the child task it starts with the `task fn` value `c`, such as a [task closure](#task-closures).

```nan-api
// module: task

pub open struct Join[T: Any and Send, E: Send] {
} Join

pub fn Scope.spawn[T: Any and Send, E: Send](self, take child: task fn() -> T fails E uses wait) -> Join[T, E]

pub fn Scope.spawn_sized[T: Any and Send, E: Send](self, take child: task fn() -> T fails E uses wait, stack: usize) -> Join[T, E]

pub fn Join.join(take self) -> T fails E uses wait
```

- `spawn` never waits. It draws the child's stack and [task record](#task-closures) through the allocator of the executor that S runs its children on ([executors](#executors)), and traps when that allocator is exhausted (`trap.exhausted`). The child is ready at once, even when S has cancelled; its calls are then cancelled from its start.
- `s.spawn_sized(c, stack)` spawns as `s.spawn(c)` does, except that the child's stack holds at least `stack` bytes ([task stacks](17-suspension-io.md#task-stacks)), so that a child whose depth no [stack limit](10-effects-digests.md#stack-limits) bounds, such as a recursive one, can run beside its scope's body. Every rule of this chapter on `spawn` holds for `spawn_sized`.
- A **lexical spawn** is a call of `spawn` or `spawn_sized` whose receiver is the parameter of a body closure B and that is written in B, or in a closure nested in B that is not a task closure. At a lexical spawn, a child failure type E that does not convert to the failure type of B's `task::scope` call is an error (`error.unconvertible`). At any other spawn, an E other than `task::Cancelled` is an error (`task.unscoped-failure`).
- A child's failure always reaches S as step 2 of [task scopes](#task-scopes) states; a join handle never yields it.
- `take h.join()` waits until the child has ended. When the child returned t, `join` yields t, even when the joining call is cancelled. Otherwise S has cancelled, so the joining call is cancelled, and `join` fails with the cancellation of E ([cancellation](11-errors-domains.md#cancellation)). `join` is no cancellation point.
- A `join` whose handle's `task::scope` call has returned traps (`trap.stale-handle`).
- `task::Join[T, E]` is a base type: linear exactly when T is, never a view, Copy, Send, Inert or AnyBits.
- A join handle released at its owner's [end of scope](07-ownership.md#ownership), including one that `discard` receives, **reaps** its child: the child's result is released when the child ends, or at once when it has ended. A join handle of a linear T is never reaped ([linear consumption](07-ownership.md#linear-consumption)).

```nan
pub closed struct Counter {
    pub hits: u64
} Counter

pub fn record_hits(counter: task::Cown[Counter], amount: u64) -> u64 fails task::Cancelled uses wait {
    await when mut counter {
        counter.hits = counter.hits.saturating_add(amount)
    }
    return await when counter => counter.hits
} record_hits

pub fn larger_count(counter: task::Cown[Counter], exec: task::Executor) -> u64 fails task::Cancelled uses wait {
    var larger: u64 = 0
    try await task::scope(exec, body: fn[mut larger](s: task::Scope) fails task::Cancelled uses wait {
        let small_job = s.spawn(fn() -> u64 fails task::Cancelled uses wait {
            return try await record_hits(counter, amount: 2)
        })
        let large_job = s.spawn(fn() -> u64 fails task::Cancelled uses wait {
            return try await record_hits(counter, amount: 3)
        })
        let small = try await take small_job.join()
        let large = try await take large_job.join()
        if small > large {
            larger = small
        } else {
            larger = large
        }
    })
    return larger
} larger_count
```

```nan
pub closed struct Counter {
    pub hits: u64
} Counter

pub closed enum FetchError derives Render {
    Empty
    Cancelled(task::Cancelled) from
} FetchError

pub fn fetch_hits(counter: task::Cown[Counter]) -> u64 fails FetchError uses wait {
    let hits = await when counter => counter.hits
    if hits == 0 {
        fail FetchError.Empty
    }
    return hits
} fetch_hits

pub fn start_fetch(s: task::Scope, counter: task::Cown[Counter]) {
    let mine = counter.share()
    discard s.spawn(fn[take mine]() fails FetchError uses wait { // error: task.unscoped-failure
        discard try await fetch_hits(mine)
    })
} start_fetch
```

## Task closures

A **task closure** is a closure written where a `task fn` [function type](10-effects-digests.md#function-types) is expected ([closures](07-ownership.md#closures)).

- A value of a `task fn` type is linear and Send, and never a view, Copy, Inert or AnyBits.
- Exactly these functions start a task with a `task fn` value: `task::Scope.spawn`, `task::Scope.spawn_sized`, `task::race`, `sys::Domain.run`, `sys::Domain.run_sized`, `sys::Cx.spawn` and `boot::Kernel.anchor`. Each receives the value by `take` and runs it once, as the [entry](17-suspension-io.md#task-stacks) of a new task. Calling a `task fn` value is an error (`task.task-fn-call`).
- The value moves into the new task's **task record**, the storage the runtime keeps for the task. The entry owns each `take` capture as a `take` parameter owns its argument, and releases what remains of it when the task ends. A record keeps the task's result once the task ends, and lives until the task has ended and, for a child of a scope, until its join handle has been joined or has reaped it, or, for a task of a race, until the race call returns.
- Every capture of a task closure has a Send type, and a view is captured only by lending; any other capture is an error (`task.capture-not-send`), reported at the capture's entry in the capture list, or at its first use in the body when it is unlisted.
- A Copy read capture is copied when the closure is created and a `take` capture moves into it. Any other capture is lending, legal only as [lending](#lending) states.

```nan
pub closed struct Counter {
    pub hits: u64
} Counter

pub fn record_hits(counter: task::Cown[Counter], amount: u64) -> u64 fails task::Cancelled uses wait {
    await when mut counter {
        counter.hits = counter.hits.saturating_add(amount)
    }
    return await when counter => counter.hits
} record_hits

pub fn start_job(s: task::Scope, take job: task fn() -> u64 fails task::Cancelled uses wait) -> task::Join[u64, task::Cancelled] {
    return s.spawn(take job)
} start_job

pub fn count_once(counter: task::Cown[Counter], exec: task::Executor) -> u64 fails task::Cancelled uses wait {
    var hits: u64 = 0
    try await task::scope(exec, body: fn[mut hits](s: task::Scope) fails task::Cancelled uses wait {
        let mine = counter.share()
        let job = start_job(s, job: fn[take mine]() -> u64 fails task::Cancelled uses wait {
            return try await record_hits(mine, amount: 1)
        })
        hits = try await take job.join()
    })
    return hits
} count_once
```

```nan
pub closed struct Counter {
    pub hits: u64
} Counter

pub fn record_hits(counter: task::Cown[Counter], amount: u64) -> u64 fails task::Cancelled uses wait {
    await when mut counter {
        counter.hits = counter.hits.saturating_add(amount)
    }
    return await when counter => counter.hits
} record_hits

pub fn relayed_count(counter: task::Cown[Counter], exec: task::Executor) -> u64 fails task::Cancelled uses wait {
    var hits: u64 = 0
    try await task::scope(exec, body: fn[mut hits](s: task::Scope) fails task::Cancelled uses wait {
        let job = s.spawn(fn() -> u64 fails task::Cancelled uses wait {
            return try await record_hits(counter, amount: 1)
        })
        let relay = s.spawn(fn[take job]() -> u64 fails task::Cancelled uses wait { // error: task.capture-not-send
            return try await take job.join()
        })
        hits = try await take relay.join()
    })
    return hits
} relayed_count
```

## Lending

**Lending** is a task closure's capture of a place by `mut`, or by read when the place is not Copy. A task closure lends only when both hold:

1. It is written as the argument of a [lexical spawn](#joins); otherwise lending is an error (`task.lend-outside-scope`). When every lent place has a root that [modes](07-ownership.md#modes) allow to be moved, the exact fix captures each of them by `take`.
2. The root of each place it lends is the parameter of the spawn's body closure B or a capture of B. Lending any other binding, parameter or loop element of B or of a closure nested in B, or the value of a cown inside an [`await when`](#await-when) body, is an error (`task.lend-local`).

Both codes are reported where `task.capture-not-send` is. Inside the child, a lent read is used as a read parameter's place and a lent `mut` as a `mut` parameter's place.

A lent read is a read access, and a lent `mut` a `mut` access, to its place. It is active in B from the statement of B holding the spawn, directly or within a nested closure, to every exit of B; [exclusivity](07-ownership.md#exclusivity) judges it (`own.conflict`). B's own access to a capture stays active until the `task::scope` call returns ([captures](07-ownership.md#captures)), after every child has ended.

```nan
pub closed struct Counter {
    pub hits: u64
} Counter

pub fn record_hits(counter: task::Cown[Counter], amount: u64) -> u64 fails task::Cancelled uses wait {
    await when mut counter {
        counter.hits = counter.hits.saturating_add(amount)
    }
    return await when counter => counter.hits
} record_hits

pub fn record_all(amounts: Slice[u64], counter: task::Cown[Counter], mut seen: mem::List[u64], exec: task::Executor) fails task::Cancelled uses wait {
    try await task::scope(exec, body: fn[mut seen](s: task::Scope) fails task::Cancelled uses wait {
        let job = s.spawn(fn[mut seen]() fails task::Cancelled uses wait {
            for amount in amounts {
                mut seen.push(try await record_hits(counter, amount))
            }
        })
        try await take job.join()
    })
} record_all
```

```nan
pub fn sum_squares(count: u64, exec: task::Executor) -> u64 fails task::Cancelled uses wait {
    var total: u64 = 0
    try await task::scope(exec, body: fn[mut total](s: task::Scope) fails task::Cancelled uses wait {
        let job = s.spawn(fn[mut total]() {
            for value in 0..<count {
                if s.is_cancelled() {
                    return
                }
                total = total.saturating_add(value.saturating_mul(value))
            }
        })
        try await take job.join()
    })
    return total
} sum_squares
```

```nan
pub closed struct Counter {
    pub hits: u64
} Counter

pub fn record_hits(counter: task::Cown[Counter], amount: u64) -> u64 fails task::Cancelled uses wait {
    await when mut counter {
        counter.hits = counter.hits.saturating_add(amount)
    }
    return await when counter => counter.hits
} record_hits

pub fn start_recorder(s: task::Scope, counter: task::Cown[Counter]) {
    discard s.spawn(fn() fails task::Cancelled uses wait {
        discard try await record_hits(counter, amount: 1) // error: task.lend-outside-scope
    })
} start_recorder
```

```nan
pub closed struct Counter {
    pub hits: u64
} Counter

pub fn record_hits(counter: task::Cown[Counter], amount: u64) -> u64 fails task::Cancelled uses wait {
    await when mut counter {
        counter.hits = counter.hits.saturating_add(amount)
    }
    return await when counter => counter.hits
} record_hits

pub fn record_twice(counter: task::Cown[Counter], exec: task::Executor) fails task::Cancelled uses wait {
    try await task::scope(exec, body: fn(s: task::Scope) fails task::Cancelled uses wait {
        var seen: u64 = 0
        let job = s.spawn(fn[mut seen]() fails task::Cancelled uses wait { // error: task.lend-local
            seen += try await record_hits(counter, amount: 2)
        })
        try await take job.join()
    })
} record_twice
```

## Deadlines and races

```nan-api
// module: task

pub fn within[T, E](clock: sys::Clock, ms: u64, body: fn() -> T fails E uses wait) -> Option[T] fails E uses wait

pub fn race[T: Send, E: Send](exec: Executor, take left: task fn() -> T fails E uses wait, take right: task fn() -> T fails E uses wait) -> T fails E uses wait
```

`task::within(clock, ms, body: b)` runs `b` on the calling task with a deadline `ms` milliseconds after the call starts, as `sys::Clock.elapsed_ms` measures time for `clock`. When `clock` is stale at the call or becomes stale while `b` runs ([capability tables](14-authority.md#capability-tables)), the deadline passes at that moment.

- When the deadline passes before `b` ends, the call cancels. Its region is the calling task while it runs `b`.
- When `b` returns t, the call returns `Some(t)`, whether or not the deadline has passed.
- When `b` fails with a cancellation whose origin is this call, the call returns `None`. When `b` fails otherwise, the call fails with that failure.

`task::race(exec, left: l, right: r)` runs these steps:

1. It starts `l` and `r` as two tasks on `exec`, drawing their stacks and records as `spawn` does. Its region is these two tasks.
2. The first of them whose end the runtime records decides the race, and the call then cancels.
3. Once both have ended, the call returns the deciding task's result or fails with its failure. The other task's result or failure is released.

`within` and `race` are cancelling constructs ([origin and absorption](11-errors-domains.md#origin-and-absorption)).

```nan
pub closed enum PollError derives Render {
    Net(sys::net::NetError) from
    Cancelled(task::Cancelled) from
} PollError

pub fn read_or_timeout(mut conn: sys::net::Conn, clock: sys::Clock, alloc: mem::Alloc) -> Option[Untrusted[mem::Bytes]] fails PollError uses wait {
    var buf = Untrusted.of(mem::Bytes.zeroed(512, alloc))
    let Some(count) = try await task::within(clock, ms: 2_000, body: fn[mut conn, mut buf]() -> usize fails PollError uses wait {
        return try await mut conn.read(mut buf.view_mut())
    }) else {
        return None
    }
    return Some(buf.copy_prefix(count, alloc))
} read_or_timeout

pub fn first_alert(take urgent: task::Receiver[u32], take routine: task::Receiver[u32], exec: task::Executor) -> Option[u32] fails task::Cancelled uses wait {
    return try await task::race(exec, left: fn[take urgent]() -> Option[u32] fails task::Cancelled uses wait {
        return try await mut urgent.receive()
    }, right: fn[take routine]() -> Option[u32] fails task::Cancelled uses wait {
        return try await mut routine.receive()
    })
} first_alert
```

## Cowns

A **cown** is a value of a type T shared through `task::Cown[T]` handles, which code reaches only in the body of an `await when` naming one of them.

```nan-api
// module: task

pub open struct Cown[T: Send] {
} Cown

pub fn Cown.of(take value: T, alloc: mem::Alloc) -> Cown[T]

pub fn Cown.try_of(take value: T, alloc: mem::Alloc) -> Cown[T] fails mem::OutOfMemory

pub fn Cown.share(self) -> Cown[T]
```

- `Cown.of` moves `value` into memory drawn through `alloc` as a new cown and returns its first handle; it traps when `alloc` is exhausted (`trap.exhausted`), and `Cown.try_of` fails with `mem::OutOfMemory` instead.
- `c.share()` returns another handle to the cown of `c`. The handles of a cown are counted atomically, and releasing the last one releases its value, unless the cown is poisoned ([domain memory](11-errors-domains.md#domain-memory)). Each release of a handle synchronises with the release that later observes the count it left, so the last release follows every access made through the other handles ([memory model](23-low-level.md#memory-model)).
- When a cown is created, the runtime gives it a rank in one total order over every cown of the program.
- `task::Cown[T]` is a base type: Send, and never linear, a view, Copy, Inert or AnyBits. Its value is reached only through `await when`, which gives it [interior mutability](12-memory.md#interior-mutability).

### Await when

```ebnf
when_expr = "when" when_access ( "," when_access )* when_body ;
when_access = "mut" when_place | "take" when_place {!task.when-take} | when_place ;
when_place = place_root ( "." IDENT )* ;
when_body = block | "=>" expr ;
```

A `when_expr` follows the `await` marker ([markers](06-statements-expressions.md#markers)). Each access names a place holding a cown handle.

- A place whose type is not a `task::Cown` is an error (`task.when-not-cown`). `take` before a place is an error (`task.when-take`), whose exact fix writes `mut`.
- Two accesses with the same place path are an error (`task.when-duplicate`), whose exact fix deletes the second.
- The mode applies to the cown's value; the handle place itself is read for the whole expression. Inside the body, each place path names the cown's value instead of its handle: after `mut` it is used as a `mut` parameter's place, otherwise as a read parameter's place ([modes](07-ownership.md#modes)).
- The body, a block or the expression after `=>`, holds no awaited call and no `await when` outside the bodies of the closures it contains; either is an error (`task.when-wait`).
- Inside the body, each cown's value counts as a projection result for [holds](07-ownership.md#holds). A value derived from it that is live after the body ends is an error (`task.when-escape`), reported on the line holding `when`.
- The block form has type `Unit`; in the form `=> e`, `e` is a whole `expr` that extends as far as an expression continues ([one-token lookahead](41-grammar.md#one-token-lookahead)), and the form has the type of `e` and yields its value.
- `attempt await when ...` has type `Result[R, task::Poisoned]`, where R is the type of the plain form.

Evaluating `await when` runs these steps:

1. It evaluates the places, left to right. When two of them hold handles of one cown, it traps (`trap.duplicate-access`).
2. It requests its cowns one at a time, in increasing rank, suspending until each is granted. A cown is granted to one `await when` at a time, whatever the access's mode, and grants requests in the order they reach it.
3. Holding every cown, it checks for [poisoning](11-errors-domains.md#poisoning). When a cown is poisoned, it releases them all, then traps (`trap.poisoned`), or under `attempt` yields `Err` of a `task::Poisoned` without running the body.
4. It runs the body, then releases every cown at each exit of the body. Under `attempt` it yields `Ok` of the body's value.

A task running an `await when` body never suspends before the body ends, and every `await when` requests its cowns in increasing rank, so acquisition never deadlocks. `await when` is not a cancellation point: a cancelled task acquires its cowns, runs the body and continues to its next cancellation point ([acknowledged cancellation](17-suspension-io.md#acknowledged-cancellation)).

```nan
pub closed struct Account {
    pub balance: u64
} Account

pub fn transfer(source: task::Cown[Account], target: task::Cown[Account], amount: u64) -> bool fails task::Cancelled uses wait {
    var moved = false
    await when mut source, mut target {
        if source.balance >= amount {
            source.balance -= amount
            target.balance += amount
            moved = true
        }
    }
    return moved
} transfer

pub fn balance_of(account: task::Cown[Account]) -> u64 fails task::Cancelled uses wait {
    return await when account => account.balance
} balance_of

test transfer_moves_funds for transfer {
    let world = testing::world()
    let savings = task::Cown.of(Account(balance: 10), alloc: world.alloc)
    let checking = task::Cown.of(Account(balance: 0), alloc: world.alloc)
    let moved = try await transfer(savings, target: checking, amount: 4)
    expect moved
    let left = try await balance_of(savings)
    expect left == 6
} transfer_moves_funds
```

```nan
pub closed struct Account {
    pub balance: u64
} Account

pub fn audit(account: task::Cown[Account], report: fn(balance: u64) fails task::Cancelled uses wait) fails task::Cancelled uses wait {
    await when account {
        try await report(account.balance) // error: task.when-wait
    }
} audit
```

```nan
pub closed struct Journal {
    pub entries: mem::List[u32]
} Journal

pub fn recent_count(journal: task::Cown[Journal]) -> usize fails task::Cancelled uses wait {
    let recent = await when journal => journal.entries.view() // error: task.when-escape
    return recent.len()
} recent_count
```

```nan
pub closed struct Account {
    pub balance: u64
} Account

pub fn transfer(source: task::Cown[Account], target: task::Cown[Account], amount: u64) -> bool fails task::Cancelled uses wait {
    var moved = false
    await when mut source, mut target { // traps: trap.duplicate-access
        if source.balance >= amount {
            source.balance -= amount
            target.balance += amount
            moved = true
        }
    }
    return moved
} transfer

test self_transfer_traps for transfer {
    let world = testing::world()
    let savings = task::Cown.of(Account(balance: 10), alloc: world.alloc)
    discard try await transfer(savings, target: savings.share(), amount: 1)
} self_transfer_traps
```

### Poisoned cowns

[Abandoning a domain](11-errors-domains.md#abandoning-a-domain) fixes which cowns become poisoned. The poison holds for every handle of the cown, whichever domain owns it.

```nan-api
// module: task

pub open struct Poisoned derives Send {
} Poisoned

impl Poisoned: Render
```

- Only the runtime creates values of `task::Poisoned`, and rendering one writes `poisoned`.
- `task::Poisoned` is a [parsing type](15-untrusted.md#validator-classes).

```nan
pub closed struct Gauge {
    pub level: u32
} Gauge

pub fn divide_level(gauge: task::Cown[Gauge], divisor: u32) fails task::Cancelled uses wait {
    await when mut gauge {
        gauge.level = gauge.level / divisor
    }
} divide_level

test zero_divisor_poisons for divide_level {
    let world = testing::world()
    let exec = world.exec
    let alloc = world.alloc
    let gauge = task::Cown.of(Gauge(level: 8), alloc)
    let held = gauge.share()
    let outcome = try await sys::Domain.run(fn[take held]() fails task::Cancelled uses wait {
        try await divide_level(held, divisor: 0)
    }, exec, alloc)
    let trapped = match outcome {
        Ok(_) => false
        Err(_) => true
    }
    expect trapped
    let level = attempt await when gauge => gauge.level
    let poisoned = match level {
        Ok(_) => false
        Err(_) => true
    }
    expect poisoned
} zero_divisor_poisons
```

## Channels

A **channel** is a bounded first-in, first-out queue of values passing between tasks, written to through `task::Sender[T]` handles and read through one `task::Receiver[T]`.

```nan-api
// module: task

pub closed struct Channel[T: Any and Send] {
    pub sender: Sender[T]
    pub receiver: Receiver[T]
} Channel

pub fn Channel.bounded(capacity: usize, alloc: mem::Alloc) -> Channel[T]
    requires capacity > 0

pub fn Channel.try_bounded(capacity: usize, alloc: mem::Alloc) -> Channel[T] fails mem::OutOfMemory
    requires capacity > 0

pub open struct Sender[T: Any and Send] {
} Sender

pub fn Sender.split(self) -> Sender[T]

pub fn Sender.send(self, take value: T) fails SendError uses wait

pub open struct Receiver[T: Any and Send] {
} Receiver

pub fn Receiver.receive(mut self) -> Option[T] fails Cancelled uses wait

pub fn Receiver.drain(take self, visit: fn(take value: T)) fails Cancelled uses wait

pub closed enum SendError derives Render {
    Closed
    Cancelled(Cancelled) from
} SendError
```

- `Channel.bounded` draws a buffer of `capacity` values through `alloc` and traps when it is exhausted (`trap.exhausted`); `Channel.try_bounded` fails with `mem::OutOfMemory` instead.
- `task::Sender[T]` is affine: a base type, Send, and never linear, a view, Copy, Inert or AnyBits. `sender.split()` returns another sender of the same channel.
- `task::Receiver[T]` is a base type: linear exactly when T is, Send, and never a view, Copy, Inert or AnyBits.
- `sender.send(v)` appends `v` to the buffer, waiting while it is full. Values reach the receiver in the order their sends completed.
- When T is droppable, `send` is a cancellation point. A cancelled `send` fails with cancellation, and otherwise a `send` whose receiver has been released fails with `SendError.Closed`; either failure releases `v`.
- When T is linear, `send` is no cancellation point and never fails. Once the receiver has been released, which only [settlement](11-errors-domains.md#settlement) does to a receiver of a linear T, `send` completes at once and `v` stays in the buffer, never released.
- `receiver.receive()` removes and returns the oldest value, waiting while the buffer is empty and a sender of the channel is unreleased. It returns `None` once the buffer is empty and every sender has been released. A `receive` that fails with cancellation removes no value.
- `take receiver.drain(visit: f)` passes every value, oldest first, to `f`, waiting while the buffer is empty and a sender is unreleased, and releases the receiver once the buffer is empty and every sender has been released. It is no cancellation point and never fails.
- A receiver of a droppable T that reaches its owner's [end of scope](07-ownership.md#ownership) releases the values in its buffer. The buffer's memory returns to its store once the receiver and every sender have been released, by end of scope, by `drain` or by settlement.

```nan
pub fn sum_readings(readings: Slice[u32], exec: task::Executor, alloc: mem::Alloc) -> u64 fails task::SendError uses wait {
    var total: u64 = 0
    try await task::scope(exec, body: fn[mut total](s: task::Scope) fails task::SendError uses wait {
        let channel = task::Channel[u32].bounded(4, alloc)
        let sender = channel.sender
        let receiver = channel.receiver
        discard s.spawn(fn[take sender]() fails task::SendError uses wait {
            for reading in readings {
                try await sender.send(reading)
            }
        })
        try await take receiver.drain(visit: fn[mut total](take value: u32) {
            total += u64.widen(value)
        })
    })
    return total
} sum_readings
```

## Atomics

An **atomic** is a value of a `core::atomic` type, holding one scalar that tasks read and change through a read receiver.

```nan-api
// module: core::atomic

pub open struct AtomicU32 {
} AtomicU32

pub fn AtomicU32.of(value: u32) -> AtomicU32 limits notrap, terminates, noalloc

pub fn AtomicU32.load(self) -> u32 limits notrap, terminates, noalloc

pub fn AtomicU32.store(self, value: u32) limits notrap, terminates, noalloc

pub fn AtomicU32.swap(self, value: u32) -> u32 limits notrap, terminates, noalloc

pub fn AtomicU32.compare_swap(self, expected: u32, replacement: u32) -> Result[u32, u32] limits notrap, terminates, noalloc

pub fn AtomicU32.fetch_add(self, amount: u32) -> Option[u32] limits notrap, terminates, noalloc

pub fn AtomicU32.fetch_sub(self, amount: u32) -> Option[u32] limits notrap, terminates, noalloc

pub fn AtomicU32.fetch_and(self, mask: u32) -> u32 limits notrap, terminates, noalloc

pub fn AtomicU32.fetch_or(self, mask: u32) -> u32 limits notrap, terminates, noalloc

pub fn AtomicU32.fetch_xor(self, mask: u32) -> u32 limits notrap, terminates, noalloc

pub open struct AtomicBool {
} AtomicBool

pub fn AtomicBool.of(value: bool) -> AtomicBool limits notrap, terminates, noalloc

pub fn AtomicBool.load(self) -> bool limits notrap, terminates, noalloc

pub fn AtomicBool.store(self, value: bool) limits notrap, terminates, noalloc

pub fn AtomicBool.swap(self, value: bool) -> bool limits notrap, terminates, noalloc

pub fn AtomicBool.compare_swap(self, expected: bool, replacement: bool) -> Result[bool, bool] limits notrap, terminates, noalloc
```

`core::atomic` also declares `AtomicU8`, `AtomicU16`, `AtomicU64` and `AtomicUsize`, each with the items declared for `AtomicU32`, writing its own name for `AtomicU32` and `u8`, `u16`, `u64` or `usize` respectively for `u32`.

- `load` returns the value, `store` replaces it, and `swap` replaces it and returns the previous value.
- `compare_swap` replaces the value with `replacement` and returns `Ok` of the previous value when the value equals `expected`, and otherwise returns `Err` of the value, unchanged.
- `fetch_add` stores the sum and returns `Some` of the previous value when the sum fits the type, and otherwise leaves the value unchanged and returns `None`. `fetch_sub` does the same with the difference.
- `fetch_and`, `fetch_or` and `fetch_xor` store the bitwise result and return the previous value.
- Every operation is sequentially consistent: all atomic operations of a program take effect one at a time in one total order that agrees with each task's [evaluation order](06-statements-expressions.md#evaluation-order) ([memory model](23-low-level.md#memory-model)).
- The atomic types are base types: Send, and never linear, a view, Copy, Inert or AnyBits. Tasks share one by lending it or by holding it in shared storage such as `mem::Shared`.

```nan
pub fn claim_ticket(next: core::atomic::AtomicU32) -> Option[u32] {
    return next.fetch_add(1)
} claim_ticket

test tickets_stop_at_the_top for claim_ticket {
    let next = core::atomic::AtomicU32.of(4_294_967_295)
    let ticket = claim_ticket(next)
    expect ticket == None
} tickets_stop_at_the_top
```

## Executors

An **executor** is a `task::Executor`, the [capability](14-authority.md#capabilities) that runs tasks.

```nan-api
// module: task

pub open struct Executor {
} Executor
```

- `task::Executor` is a primitive capability: Copy and Send, and never linear, a view, Inert or AnyBits.
- An executor is revoked as [revocation](14-authority.md#revocation) states.
- A root grant mints its executor together with its allocator ([root grants](14-authority.md#root-grants)). `spawn` and `task::race` draw the stacks and task records of the tasks they start on an executor through that allocator.
- An executor runs its ready tasks concurrently, on one or more processors. A task is ready when it has not ended and is not suspended at a [suspension point](17-suspension-io.md#stackful-tasks). The runtime chooses which ready task runs, on which processor and for how long; on the seeded scheduler of tests the seed fixes that choice ([replay](22-testing.md#replay)).

```nan
// file: counter/package.nan
//! Counts hits from two tasks on the host.

package counter {
    language 1
    targets x86_64-host
    entry main
} counter

// file: counter/counter.nan
//! Entry of the hit counter.

pub closed struct Counter {
    pub hits: u64
} Counter

fn main(take world: host::World) fails task::Cancelled uses wait {
    let exec = world.exec
    let counter = task::Cown.of(Counter(hits: 0), alloc: world.alloc)
    try await task::scope(exec, body: fn(s: task::Scope) {
        for _ in 0..<2u32 {
            discard s.spawn(fn() fails task::Cancelled uses wait {
                await when mut counter {
                    counter.hits += 1
                }
            })
        }
    })
    let hits = await when counter => counter.hits
    world.log.info("hits", value: hits)
} main
```

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `task.capture-not-send` | error | [Task closures](#task-closures) |
| `task.lend-local` | error | [Lending](#lending) |
| `task.lend-outside-scope` | error | [Lending](#lending) |
| `task.task-fn-call` | error | [Task closures](#task-closures) |
| `task.unscoped-failure` | error | [Joins](#joins) |
| `task.when-duplicate` | error | [Await when](#await-when) |
| `task.when-escape` | error | [Await when](#await-when), [Views across suspension points](17-suspension-io.md#views-across-suspension-points) |
| `task.when-not-cown` | error | [Await when](#await-when) |
| `task.when-take` | error | [Await when](#await-when) |
| `task.when-wait` | error | [Await when](#await-when), [Views across suspension points](17-suspension-io.md#views-across-suspension-points) |
