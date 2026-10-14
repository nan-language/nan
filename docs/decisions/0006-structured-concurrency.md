# 0006. Structured concurrency

Status: accepted

## Context

nan has no garbage collector and no references: values are reached through access modes and projections held by liveness ([0004](0004-value-semantics-without-references.md)), and waiting is the one effect code writes ([0005](0005-only-waiting-is-written.md)). On the host, reads and writes go through io_uring; in the kernel, through DMA. In both, the kernel or a device writes into a task's memory while the task waits.

The bugs concurrency usually brings are invisible from the window an agent reads: a task that outlives the data it uses, a lock order that deadlocks two functions apart, a cancelled read whose buffer the device still fills, a data race. An agent also cannot debug a failure that happens one run in a hundred. NanOS services must be updatable while they run, which needs a moment when a service holds no half-finished work.

The concurrency model therefore has to make these bugs impossible by construction, judged inside one item, and make concurrent tests reproducible.

## Decision

- **Task scopes.** Every task belongs to a construct that outlives it, in practice a [task scope](../spec/16-concurrency.md#task-scopes): `task::scope` returns only after every child has ended, and the first child failure cancels the others and fails the scope. There is one scope form; a child that must not fail its siblings handles its own failures with `attempt`. A [join handle](../spec/16-concurrency.md#joins) is linear exactly when its result is, and a discarded child is reaped, so no result or failure is lost silently.
- **Lexical lending.** A [task closure](../spec/16-concurrency.md#lending) lends places only when it is written in the scope's body closure, is spawned on that body's own scope, and lends places the body captured from outside. Those places provably outlive every child, and exclusivity checks this within one item, with no lifetime annotations. Every other task closure captures by `take` and Copy only.
- **Stackful tasks.** [Each task runs on its own stack](../spec/17-suspension-io.md#stackful-tasks), and its frames never move, so views stay valid across `await` and an ordinary loop may wait in its body.
- **Cowns.** Shared mutable state lives in a [`task::Cown`](../spec/16-concurrency.md#cowns). `await when` acquires every named cown at once, in one global order, and its body cannot wait, so no program deadlocks on cowns. `await when` is not a cancellation point: acquisition always completes, needs no `try`, and lets a cancelled task still repair shared state. Channels carry messages; a `Sender` is affine and a `Receiver` linear when its value is.
- **Acknowledged cancellation.** [A cancelled call that is in flight returns only after the kernel or device acknowledges](../spec/17-suspension-io.md#acknowledged-cancellation) that it no longer touches the buffers, with no deadline on that wait. This makes I/O into views sound and closes the io_uring use-after-free class. Cancellation reaches a task only at cancellation points, so code between them never stops halfway.
- **Data-race freedom.** Safe code is free of data races and sequentially consistent; its atomics are SeqCst.
- **Replay.** Tests run on a [seeded scheduler](../spec/22-testing.md#replay) that explores interleavings at scheduling points, with a virtual clock, and every failure prints a replay token that reproduces it exactly. Interleavings are explored under sequential consistency (coverage class `sc`); other class names are reserved.
- **Components.** A [component](../spec/26-components.md) is a type implementing `sys::Component[Env, M]`. Its handlers have no `uses wait`, so they never suspend and a component is quiescent between messages by its type. Background work runs in keyed tasks through `sys::Cx`: a new task under a key, or `cancel`, cancels the earlier one with acknowledgement, and tasks post results back through an `Outbox`. The runtime owns the Env, which holds capabilities and resources; the state is Inert data. `host::Runner.run` is bound to a scope: it handles every message posted before its body returns, then stops intake and cancels keyed tasks. The loader, live update, placement and UI keep their names reserved.

Rejected alternatives:

- **Unstructured spawn:** a detached task can outlive every value it uses and fail with nobody told.
- **Lending through a `Scope` passed as a parameter:** the callee could lend its own locals, which end before the scope does.
- **Stackless futures:** they need pinning and self-referential types that nan lacks, and they forbid views across suspension.
- **Locks in safe code:** their deadlocks depend on the order of acquisitions in distant functions.
- **Cancellation by dropping frames, or owned buffers for every operation:** the first frees memory a device still writes; the second gives up views in I/O.
- **A component item kind:** an interface and an impl line express it, with no new item kind for the parser, the formatter and the merge driver.
- **Handlers that wait:** a message handled while another handler waits would observe half-updated state, and quiescence, the precondition of live update, would no longer hold.

## Consequences

- No task outlives its scope, cowns never deadlock and safe code never races, all checked inside one item.
- Tasks spawned away from a scope body copy or move what they use instead of lending it; how often this costs copies is measured at milestones M2 and M6.
- A `when` body cannot wait, so work that waits while holding shared state is split: read or move the value out, wait, then write back.
- Cancellation takes as long as the slowest acknowledgement. A driver that writes its own completion operation stops the device and polls it until it acknowledges before returning the buffer.
- Every task needs a stack: 64 KiB guard-paged on the host, a static bound in kernel contexts. Host switching relies on Cranelift's `stack_switch`, which the Cranelift spike must confirm before M2.
- Replay reproduces the interleavings safe code can observe, not orders that trusted atomics may observe beyond sequential consistency, nor device and interrupt timing.
- Before NanOS, components run only under `host::Runner`. A handler needing I/O delegates it to a keyed task, and a resource leaves the Env through an `Option` field and returns inside a message.
