# 0048. Sized domain runs for internal errors

Status: accepted

## Context

A trap in an implementation's own code must report an internal error with a reproducer ([internal errors](../spec/34-diagnostics.md#internal-errors)). The bootstrap catches its panics; the NaN toolchain, written in NaN, can observe a trap only through `sys::Domain.run`. A domain body's first task got the 64 KiB default stack ([0038](0038-the-host-entry-task-stack.md)), and `limits stack N` proves a bound no recursive body has, so a compiler, an interpreter or any program that recurses could not contain its own traps: the NaN toolchain ended with the process trap report and status 70.

## Decision

- **`sys::Domain.run_sized(body, stack, exec, alloc)`** runs as `run` does, its first task's stack holding at least `stack` bytes. The size needs no proof: an overflow abandons only that domain, which the call turns into `Err`.
- **The NaN toolchain runs each command in a sized domain**, on the entry task's 64 MiB, and turns a trap into `internal.invariant`, or `internal.resource-exhausted` for exhausted memory or stack, with its reproducer. What the command learns for the reproducer, its root, scope and the files it read, goes into arenas its caller's domain owns, which an abandoned domain leaves whole.

Rejected alternatives:

- **A domain body inheriting its caller's stack:** a body run from the entry task under a budget would draw 64 MiB through it, which programs bounding domains by budgets cannot afford.
- **A supervisor process starting the toolchain again as a worker:** two processes per command, a channel for the trace, and the launches of `nan run` moved out of the worker, for what the language should express.
- **Accepting status 70:** the toolchain would not conform, and a trap would leave no reproducer.

## Consequences

- A trap of the NaN toolchain ends with an internal error and a reproducer. The bootstrap reports none for exhausted memory or stack, which abort it.
- Every command of the NaN toolchain costs one domain run.
