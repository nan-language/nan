# Suspension, I/O and cleanup

This chapter defines stackful tasks, suspension points and waiting primitives; task stacks, their sizes, their static bounds and overflow; completion I/O, with views lent to ordinary operations and buffers returned in `task::Completion`; acknowledged cancellation and cancellation points; which values may be live at a suspension point; deferred calls with `defer` and `undefer`; and abandon methods.

## Stackful tasks

Every [task](16-concurrency.md#task-scopes) runs on its own stack, which holds the frames of every call the task is executing. A **suspension point** is an [awaited call](10-effects-digests.md#awaited-calls) or an [`await when`](16-concurrency.md#await-when).

A **waiting primitive** is a std function whose head states `uses wait`. A task suspends only inside a waiting primitive, or while an `await when` acquires its cowns:

- Suspending saves the task's execution state on its own stack, and the stack-switching [support routine](27-program-model.md#runtime-support) runs other ready tasks of its [executor](16-concurrency.md#executors). The task resumes at the same point once what it waits for has happened, possibly on another processor.
- A frame never moves while its task lives. While the task is suspended, its frames change only through the views it passed to the waiting primitive, through places it [lent](16-concurrency.md#lending) and through values with [interior mutability](12-memory.md#interior-mutability).
- An awaited call that reaches no waiting primitive returns or fails without suspending.

[Kernel execution model](24-kernel.md#kernel-suspension) states how suspension is lowered on the machine targets.

```nan
pub fn pause_steps(clock: sys::Clock, steps: u32) fails task::Cancelled uses wait {
    for _ in 0..<steps {
        try await clock.sleep(ms: 10)
    }
} pause_steps

pub fn pause_twice(clock: sys::Clock) fails task::Cancelled uses wait {
    try await pause_steps(clock, steps: 2)
    try await pause_steps(clock, steps: 2)
} pause_twice

test sleeps_in_nested_frames for pause_twice {
    let world = testing::world()
    let start = try world.clock.elapsed_ms()
    try await pause_twice(world.clock)
    let finish = try world.clock.elapsed_ms()
    expect finish - start >= 40
} sleeps_in_nested_frames
```

## Task stacks

The **entry** of a task is the code it starts with: the [task closure](16-concurrency.md#task-closures) or named function whose `task fn` value starts it, the body of a [test](22-testing.md#test-items) for the task of that test, or, on the host targets, the [entry function](27-program-model.md#entry) for the first task of a program.

- When the entry's own head states `limits stack N`, the task's stack holds at least N bytes, and every final build lowering the entry checks that its stack use is at most N ([stack limits](10-effects-digests.md#stack-limits)).
- The stack of the first task of a domain that `run_sized` of `sys::Domain` or `sys::Abandoner` runs holds at least the `stack` bytes the call gives ([domain run](11-errors-domains.md#domain-run)), and so does the stack of a child that `task::Scope.spawn_sized` starts ([joins](16-concurrency.md#joins)).
- Otherwise, on the host targets, the stack of a program's [entry task](27-program-model.md#host-runs) holds the entry task stack of that target's [descriptor](27-program-model.md#descriptors), and every other stack its default task stack.
- On the host targets, an inaccessible guard region of at least 4 KiB lies beyond the end of every task stack toward which the stack grows.

```nan
pub closed enum JobError derives Render {
    TooLarge
    Cancelled(task::Cancelled) from
} JobError

pub fn triangle(n: u64) -> u64 fails JobError {
    if n > 1_000_000 {
        fail JobError.TooLarge
    }
    return n * (n + 1) / 2
} triangle

pub fn contained_triangle(n: u64, exec: task::Executor, alloc: mem::Alloc) -> Option[u64] fails JobError uses wait {
    let outcome = try await sys::Domain.run(fn() -> u64 fails JobError limits stack 4_096 {
        return try triangle(n)
    }, exec, alloc)
    let Ok(value) = outcome else {
        return None
    }
    return Some(value)
} contained_triangle
```

### Static bounds

On the machine targets, every stack has a static bound. The code of each context in this table starts at an entry whose head states `limits stack N`, every final build checks the entry's stack use against N ([stack limits](10-effects-digests.md#stack-limits)), and the section in the Stack column fixes how much the context's stack holds.

| Context | Entry | Stack |
| --- | --- | --- |
| boot | the entry function | the boot stack ([machine runs](27-program-model.md#machine-runs)) |
| task | the entry of each task | the task's own stack ([kernel suspension](24-kernel.md#kernel-suspension)) |
| interrupt | each [interrupt fn](24-kernel.md#interrupt-functions) | the processor's interrupt stack ([interrupt functions](24-kernel.md#interrupt-functions)) |
| landing | each [landing fn](23-low-level.md#landing-functions) | the stack its seam switches to ([user exceptions and syscalls](24-kernel.md#user-exceptions-and-syscalls), [machine runs](27-program-model.md#machine-runs)) |

- An entry of these contexts other than a test body whose own head states no `limits stack N`, in code checked for a machine target or within the reach of a final build for one ([environments](27-program-model.md#environments)), is an error (`suspend.stack-missing`), reported on the line of its head.
- A test body states no limits. On these targets, the build of a test image ([emulator runs](22-testing.md#emulator-runs)) computes the body's stack use as [stack limits](10-effects-digests.md#stack-limits) does for a function demanding `stack N`, reporting each unbounded call site of the body (`limit.stack-unbounded`), and the test's task runs on a stack holding that stack use.

```nan
// file: idle/package.nan
//! A UEFI program that spins.

package idle {
    language 1
    targets x86_64-uefi
    entry main
} idle

// file: idle/idle.nan
//! Entry of the spinning program.

fn main(take m: boot::Machine) -> Never limits stack 16_384 {
    var spins: u64 = 0
    loop {
        spins = spins.wrapping_add(1)
    }
} main
```

```nan
// file: idle/package.nan
//! A UEFI program that spins.

package idle {
    language 1
    targets x86_64-uefi
    entry main
} idle

// file: idle/idle.nan
//! Entry of the spinning program.

fn main(take m: boot::Machine) -> Never { // error: suspend.stack-missing
    var spins: u64 = 0
    loop {
        spins = spins.wrapping_add(1)
    }
} main
```

### Stack overflow

On the host targets, an access to a task's guard region traps (`trap.stack-overflow`). Before using a frame larger than 4 KiB, lowered code accesses that frame once per 4 KiB, starting at the end nearest the caller's frame, so a task whose stack use exceeds its stack accesses the guard region before writing outside the stack.

On the machine targets, each stack holds at least the stack use of the code running on it, counting a [foreign fn](23-low-level.md#foreign-functions) at the `stack N` it declares, so `trap.stack-overflow` never occurs there.

## Completion I/O

An operation is **in flight** from the moment it is submitted to the kernel or a device until the kernel or device has completed it or has acknowledged its cancellation ([acknowledged cancellation](#acknowledged-cancellation)). A waiting primitive returns or fails only once none of its operations is in flight. [Host I/O backend](27-program-model.md#host-io-backend) fixes how the host performs operations.

An **ordinary I/O operation** is a waiting primitive that receives its buffer as a view: a read writes into an `Untrusted[SliceMut[u8]]` passed by `mut`, and a write reads a `Slice[u8]` ([std capabilities](14-authority.md#std-capabilities)).

- The kernel or device accesses the buffer only while the operation is in flight.
- For the whole call, [exclusivity](07-ownership.md#exclusivity) keeps other code from writing the buffer's source, and from reading it when the operation writes the buffer. [Settlement](11-errors-domains.md#settlement) frees no memory that an operation in flight may access.

A **completion operation** is a function stating `uses wait` whose result type is a `task::Completion[B, T, E]` and that receives its **buffer**, a value of type B, by `take`. It returns a **completion**: a `task::Completion` holding that buffer in `buf` and the operation's result in `outcome`.

```nan-api
// module: task

pub closed struct Completion[B: Any, T: Any, E] {
    pub buf: B
    pub outcome: Result[T, E]
} Completion
```

- A completion operation owns the buffer from its call until it returns, and the kernel or device accesses the buffer only while the operation is in flight.
- For a completion operation written outside std, that guarantee rests on the device operations of the code that gives the device the buffer's bus address ([safe code](23-low-level.md#safe-code)): when its wait fails with cancellation, the operation stops the device's access to the buffer through volatile calls or [DMA access calls](23-low-level.md#dma) and polls them, without waiting, until the device acknowledges, and only then returns the buffer in `buf` with the cancellation in `outcome`.
- A std completion operation never fails. Its head states a cancellable failure type, as [cancellation](11-errors-domains.md#cancellation) requires of every head stating `uses wait`, and its E is cancellable too: every failure, cancellation included, arrives as `Err` in `outcome`, with the buffer in `buf`.
- A completion is [linear](05-properties.md#linear-types) when B or T is, so a completion operation on a linear buffer, such as an [`Untrusted[hw::Dma[T]]`](23-low-level.md#dma), returns it on every path that does not trap.
- A std completion operation whose buffer a device or the kernel writes has an `Untrusted` type as B ([sources](15-untrusted.md#sources)). [DMA](23-low-level.md#dma) requires it of every completion operation on an `hw::Dma` buffer.

```nan
pub closed linear struct Block {
    pub bytes: Array[u8, 64]
} Block

pub closed enum FillError derives Eq, Render {
    ReadOnly
} FillError

pub fn fill_block(take block: Block, value: u8, writable: bool) -> task::Completion[Block, usize, FillError] {
    if not writable {
        return task::Completion(buf: take block, outcome: Err(FillError.ReadOnly))
    }
    for byte in mut block.bytes {
        byte = value
    }
    return task::Completion(buf: take block, outcome: Ok(64))
} fill_block

pub fn refill(take block: Block) -> Block {
    let task::Completion(buf: returned, outcome: _) = fill_block(take block, value: 0xFF, writable: true)
    return returned
} refill
```

```nan
pub closed linear struct Block {
    pub bytes: Array[u8, 64]
} Block

pub closed enum FillError derives Eq, Render {
    ReadOnly
} FillError

pub fn fill_block(take block: Block, value: u8, writable: bool) -> task::Completion[Block, usize, FillError] {
    if not writable {
        return task::Completion(buf: take block, outcome: Err(FillError.ReadOnly))
    }
    for byte in mut block.bytes {
        byte = value
    }
    return task::Completion(buf: take block, outcome: Ok(64))
} fill_block

pub fn fill_succeeded(take block: Block) -> bool {
    let done = fill_block(take block, value: 0, writable: true)
    return done.outcome == Ok(64) // error: own.linear-dropped
} fill_succeeded
```

## Acknowledged cancellation

A call **is cancelled** when it lies in the region of a cancelling construct that has cancelled ([origin and absorption](11-errors-domains.md#origin-and-absorption)). The **exempt primitives** are `task::scope`, `task::Join.join`, `task::within`, `task::race`, `task::Receiver.drain`, `task::Sender.send` of a linear T, the [domain runs](11-errors-domains.md#domains) and `host::Runner.run`, whose own sections state how cancellation reaches them. A **cancellation point** is a call of a waiting primitive other than an exempt primitive; `await when` is never one ([await when](16-concurrency.md#await-when)). **Acknowledged cancellation** is the behaviour of cancellation points:

1. A cancellation point whose call is cancelled when it starts begins no operation and fails at once with the cancellation of its failure type. A std completion operation instead returns at once, with that cancellation as `Err` in `outcome` and the buffer it took in `buf`.
2. When the call of a cancellation point becomes cancelled after it has started, the runtime asks the kernel or device to cancel each of its operations in flight, and resumes the task only after each **acknowledgement**, by which the kernel or device guarantees that the operation no longer accesses its buffers and reports whether it completed first. What the call waits for without the kernel or a device, such as a channel value or a signal post, is acknowledged as soon as the call is cancelled.
3. When what the call waits for completed first, the call yields its outcome exactly as without cancellation. Otherwise it fails with the cancellation, or, for a std completion operation, returns the cancellation in `outcome`.
4. No deadline bounds the wait for an acknowledgement.
5. Cancellation interrupts no other code: a task observes it only at cancellation points, at calls of exempt primitives as their sections state, and through `task::Scope.is_cancelled` ([task scopes](16-concurrency.md#task-scopes)).
6. A waiting primitive that fails, by cancellation or otherwise, has consumed every value it received by `take`. It releases each without waiting: a linear [resource](14-authority.md#resources) as its [abandon method](#abandoning-resources) does, and any other value as its [end of scope](07-ownership.md#ownership) does.

In the next example, when the deadline passes during the first read, that read returns only once its cancellation is acknowledged, so the second read is the only operation writing `buf`.

```nan
pub closed enum PollError derives Render {
    Net(sys::net::NetError) from
    Cancelled(task::Cancelled) from
} PollError

pub fn poll_once(mut conn: sys::net::Conn, clock: sys::Clock, alloc: mem::Alloc) -> usize fails PollError uses wait {
    var buf = Untrusted.of(mem::Bytes.zeroed(512, alloc))
    let Some(count) = try await task::within(clock, ms: 100, body: fn[mut conn, mut buf]() -> usize fails PollError uses wait {
        return try await mut conn.read(mut buf.view_mut())
    }) else {
        return try await mut conn.read(mut buf.view_mut())
    }
    return count
} poll_once
```

## Views across suspension points

A value is live at a suspension point when a path from that point reaches a use of it or of a value derived from it, as [holds](07-ownership.md#holds) defines liveness for projection results. Every value, views included, may be live at a suspension point, except one whose type is or contains a [state token](05-properties.md#state-tokens).

- A value whose type is or contains a state token, live at a suspension point, is an error (`suspend.token-across-await`). It is reported once per value, on the line of the first such suspension point in source order, with a `bound` reason for the line defining the value and a `used` reason for the use keeping it live.
- A value derived from a cown's value is never live at a suspension point: an `await when` body contains no suspension point (`task.when-wait`), and no such value outlives the body (`task.when-escape`).

```nan
pub closed enum ReadError derives Render {
    Net(sys::net::NetError) from
    Cancelled(task::Cancelled) from
} ReadError

pub fn read_two(mut conn: sys::net::Conn, alloc: mem::Alloc) -> usize fails ReadError uses wait {
    var buf = Untrusted.of(mem::Bytes.zeroed(512, alloc))
    var window = mut buf.view_mut()
    let earlier = try await mut conn.read(mut window)
    let later = try await mut conn.read(mut window)
    return earlier + later
} read_two
```

```nan
pub closed view struct Muted {
} Muted

pub fn Muted.is_held(self) -> bool = true

pub closed struct Speaker {
    pub volume: u8
} Speaker

pub proj fn Speaker.muted(self) -> Muted {
    return Muted()
} Speaker.muted

pub fn pause_muted(speaker: Speaker, clock: sys::Clock) -> bool fails task::Cancelled uses wait {
    let quiet = speaker.muted()
    try await clock.sleep(ms: 10) // error: suspend.token-across-await
    return quiet.is_held()
} pause_muted
```

## Defer

A **deferred call** is a call that a `defer` statement registers with its block and that runs when the block exits. `undefer` cancels deferred calls, so that a place they use may move.

```ebnf
defer_stmt = "defer" call_expr NL ;
undefer_stmt = "undefer" place NL ;
```

The checker judges the expression after `defer` by these rules, in this order, before any rule of another chapter judges it, whatever markers it carries. It reports only the first defect, which fails the expression ([root-cause reporting](34-diagnostics.md#root-cause-reporting)); a failed deferred call still counts as registered for every other rule.

1. Its postfix expression ends in an argument list, and its callee is a function item, an associated function or a method (`defer.not-call`).
2. Its receiver and arguments are each a place without index steps, a literal, or a path naming a const item, a unit variant or a function item (`defer.operand-form`).
3. It is not a call that [may wait](10-effects-digests.md#awaited-calls) (`defer.waits`). When the call passes its receiver p by `take` and the receiver's type has an [abandon method](#abandoning-resources), the exact fix replaces the call with `take p.abandon()`.
4. Its callee's head states no `fails` (`defer.fails`).
5. Its result type is `Unit` (`defer.has-result`).

A call passing these checks is checked for its markers, modes and types as a call written at the `defer` statement, with the codes of those rules. The `defer` statement itself accesses no place. The call's accesses happen at each exit where it runs, and are checked there by [exclusivity](07-ownership.md#exclusivity) and [holds](07-ownership.md#holds), except that a move of a place it names is judged only by [deferred places](#deferred-places).

### Registration and exits

- Executing a `defer` statement registers its call with the innermost block holding the statement. The call is **pending** from then until that block exits or an `undefer` cancels it. A `defer` in a loop body registers once per iteration that executes it.
- An **exit** of a block is exactly reaching its end, or leaving it by `break`, `continue`, `return`, `fail` or a `try` that propagates a failure. At an exit, the value returned or the failure raised is evaluated first. Then the pending deferred calls of every block being left run, innermost block first and, within a block, in reverse order of registration. Only then are the values of those blocks released ([ownership](07-ownership.md#ownership)).
- A deferred call evaluates its receiver and arguments when it runs, reading each place's value at that moment.
- A trap runs no deferred call, including those remaining in an exit already under way ([abandoning a domain](11-errors-domains.md#abandoning-a-domain)).
- A deferred call passing a place by `take` consumes its value at each exit where it runs, which counts as consumption for [linear consumption](07-ownership.md#linear-consumption).

### Deferred places

A deferred call **names** each place it passes as its receiver or as an argument, in any mode.

- A place named by a pending deferred call stays usable until the exit: it may be read, assigned and accessed by `mut`.
- A move of a place path overlapping a place p named by a deferred call pending on some path to the move is an error (`defer.deferred-place-moved`). When `undefer p` would be valid before the moving statement, the exact fix inserts it there.
- `undefer p` cancels every pending deferred call naming exactly the place path p and registered in the function, closure or test body holding the `undefer` statement. It is an error (`defer.undefer-unmatched`) when p has an index step or when such a call is not pending on every path reaching the statement; the exact fix deletes the statement.

```nan
pub closed enum LinkError derives Render {
    Net(sys::net::NetError) from
    Cancelled(task::Cancelled) from
} LinkError

pub fn greet(take conn: sys::net::Conn) -> sys::net::Conn fails LinkError uses wait {
    defer take conn.abandon()
    discard try await mut conn.write(b"hello\n")
    undefer conn
    return conn
} greet

pub fn send_pings(take conn: sys::net::Conn, log: sys::Log) fails LinkError uses wait {
    var sent: usize = 0
    defer log.info("bytes sent", value: sent)
    defer take conn.abandon()
    sent += try await mut conn.write(b"ping\n")
    sent += try await mut conn.write(b"ping\n")
    undefer conn
    try await take conn.close()
} send_pings
```

```nan
pub closed enum LinkError derives Render {
    Net(sys::net::NetError) from
    Cancelled(task::Cancelled) from
} LinkError

pub fn greet(take conn: sys::net::Conn) -> sys::net::Conn fails LinkError uses wait {
    defer take conn.abandon()
    discard try await mut conn.write(b"hello\n")
    return conn // error: defer.deferred-place-moved
} greet
```

```nan
pub closed enum LinkError derives Render {
    Net(sys::net::NetError) from
    Cancelled(task::Cancelled) from
} LinkError

pub fn ping(take conn: sys::net::Conn) fails LinkError uses wait {
    defer take conn.close() // error: defer.waits
    discard try await mut conn.write(b"ping\n")
} ping
```

## Abandoning resources

An **abandon method** is a method whose head is `fn T.abandon(take self)`, with any visibility, limits and clause region, and no modifier, generic parameter of its own, other parameter, result, `fails` or `uses wait`. Its call releases its receiver without waiting, so a deferred call may release a [resource](14-authority.md#resources) whose graceful release waits.

- A function item named `T.abandon`, for any type T, whose head is not that of an abandon method is an error (`defer.abandon-head`).
- Every std resource whose graceful release waits has a `pub` abandon method. It ends the resource at once: the runtime closes the platform object without waiting, reports nothing of that release to the program, and discards what the graceful release would have delivered, such as unsent output.

```nan
pub closed enum LinkError derives Render {
    Net(sys::net::NetError) from
    Cancelled(task::Cancelled) from
} LinkError

pub closed linear struct Session {
    pub conn: sys::net::Conn
    pub requests: u32
} Session

pub fn Session.abandon(take self) {
    let Session(conn: conn, requests: _) = self
    take conn.abandon()
} Session.abandon

pub fn Session.finish(take self) fails LinkError uses wait {
    let Session(conn: conn, requests: _) = self
    try await take conn.close()
} Session.finish

pub fn serve(take session: Session) fails LinkError uses wait {
    defer take session.abandon()
    discard try await mut session.conn.write(b"ready\n")
    session.requests += 1
    undefer session
    try await take session.finish()
} serve
```

```nan
pub closed linear struct Session {
    pub conn: sys::net::Conn
} Session

pub fn Session.abandon(take self) fails sys::net::NetError uses wait { // error: defer.abandon-head
    let Session(conn: conn) = self
    try await take conn.close()
} Session.abandon
```

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `defer.abandon-head` | error | [Abandoning resources](#abandoning-resources) |
| `defer.deferred-place-moved` | error | [Deferred places](#deferred-places) |
| `defer.fails` | error | [Defer](#defer) |
| `defer.has-result` | error | [Defer](#defer) |
| `defer.not-call` | error | [Defer](#defer) |
| `defer.operand-form` | error | [Defer](#defer) |
| `defer.undefer-unmatched` | error | [Deferred places](#deferred-places) |
| `defer.waits` | error | [Defer](#defer) |
| `suspend.stack-missing` | error | [Static bounds](#static-bounds) |
| `suspend.token-across-await` | error | [Views across suspension points](#views-across-suspension-points) |
