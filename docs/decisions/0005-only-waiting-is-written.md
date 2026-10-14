# 0005. Only waiting is written

Status: accepted

## Context

Kernel code needs guarantees about what a call may do: an interrupt fn must not wait, allocate or trap, and a spin-lock body must terminate. Effect systems written in heads give such guarantees at a high price. When a leaf starts allocating, every head above it changes, and higher-order functions must be polymorphic over effects or split by colour. Checked exceptions showed the same churn for failures.

An agent must still know, from a head, how to write each call and what it may hold across that call. Errors need the same visibility: exceptions hide control flow from a window. Unwinding runs cleanup code during failure, which half-consumed linear values ([0004](0004-value-semantics-without-references.md)) cannot survive.

Completion I/O lends buffers to the kernel or a device ([0002](0002-bootstrap-stack-and-lifetime.md)). Freeing a failed computation's memory while such an operation is in flight lets the kernel or the device write into freed memory.

## Decision

- **Only waiting is written.** `uses wait` is the only effect in heads, and every call that may wait is written `await` ([waiting](../spec/10-effects-digests.md#waiting)). Waiting is the one effect that changes how a call is written and what may be live across it.
- **Everything else is computed.** Each item has a coarse digest: booleans for wait, alloc, device, priv, sync, may-trap, terminates and float use, and the sets of capabilities and validators reached ([digests](../spec/10-effects-digests.md#digests)). Facts propagate through direct calls. Indirect calls use the declared limits of their function type or interface method, or the worst digest. Digests sit beside the interface hash, never in it, and requires-sets record the absences a consumer relied on ([requires-sets](../spec/29-identity-versioning.md#requires-sets)).
- **Limits demand facts where they matter:** `pure`, `notrap` (by the prover), `terminates`, `noalloc` and `stack N` ([limits](../spec/10-effects-digests.md#limits)). On pub items all but `stack N` are promises; `stack N` is a final-build check.
- **Higher-order code.** A function parameter's type is an upper bound on what calls through it may do, and the outer call is written as the actual argument requires. Failures are polymorphic through generic parameters.
- **Errors are values.** A head names one failure type: an enum, a generic parameter or `task::Cancelled` ([failures](../spec/11-errors-domains.md#failures)). `fail` raises, `try` propagates through `from` variants, `attempt` gives a Result, and a non-Unit result is used or `discard`ed. Failure types are never linear, so every failure can be dropped. Cancellation is a failure value carrying its origin, absorbed by the scope or deadline that caused it.
- **Traps abandon a domain.** A bug (bounds, overflow, division by zero, a failed predicate or contract, a reached `todo`, stack overflow) abandons the innermost domain: the process, a `sys::Domain` body, a test, or the kernel, which halts ([traps](../spec/11-errors-domains.md#traps)). Nothing unwinds, no user code runs, and a report of at most 15 lines gives operand values, the call chain and a replay token.
- **Domains settle before freeing.** The runtime cancels every operation in flight in the abandoned domain and waits for each acknowledgement, releases its resources without waiting, closes its budgets and only then frees its memory ([settlement](../spec/11-errors-domains.md#settlement)). A cown it held is poisoned for good ([poisoning](../spec/11-errors-domains.md#poisoning)).
- **Cleanup never waits.** Deferred calls are infallible, never wait, and run at every exit but a trap. A resource whose graceful release waits offers `abandon(take self)` for deferred calls.

Rejected alternatives:

- **Written effect atoms** (alloc, device, priv, raw beside wait): churn in every head above a changed leaf, for facts that capability parameters and the digest already carry.
- **Effect variables and `uses f` / `fails f` clauses:** the most intricate rule of the core for what a parameter's own type already bounds.
- **Effect handlers:** non-local control that a window cannot follow.
- **Digests in the interface hash:** backend-dependent facts would churn versions, and a body edit would change an interface.
- **Exceptions and unwinding:** hidden control flow, landing pads, cleanup running in inconsistent state, and linear values abandoned half-consumed.
- **Freeing an abandoned domain at once:** use-after-free by the kernel or a device.
- **Shielded defers, grace deadlines and cancellable `await when`:** cleanup that waits while cancelled, the shielded execution that settlement and `abandon` replace.
- **A `recover` hook for poisoned cowns:** a second recovery path beside `attempt await when`.

## Consequences

- Heads stay stable when a leaf starts allocating or trapping. Kernel rules become local errors at the offending site, with a because-chain to the operation that carries the fact.
- Coarse digests let a library that starts allocating or trapping break a kernel caller at upgrade. Limits as promises, requires-sets and `nan diff` report it; trap sites and methods appear only in `nan explain` and publish summaries.
- Waiting still propagates through heads. `nan fix --propagate` adds `uses wait`, `fails` and `from` variants up to the first pkg or pub boundary.
- A function without limits accepts any callee, so only code that states limits pays for proving them.
- A trap runs no cleanup. Resources of an abandoned domain return through settlement, and a 1.0 kernel trap halts the machine. `sys::Domain.run` turns a trap into a value where a program chooses, such as a compiler capturing its own internal errors.
- Settlement makes a trapping domain wait for the acknowledgements of its in-flight operations before its memory is reused.
- Failure values carry no stack trace. Trap reports carry the call chain through frame pointers and line tables.
