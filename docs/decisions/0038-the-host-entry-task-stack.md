# 0038. The host entry task stack

Status: accepted

## Context

Every host task got a 64 KiB stack unless its entry states `limits stack N` ([0002](0002-bootstrap-stack-and-lifetime.md), [0006](0006-structured-concurrency.md)). That size suits servers spawning many tasks, but a program's first task does the work a process's main thread does elsewhere: the NaN front end recurses over syntax, types and terms, and a trivial recursion fills 64 KiB at about 800 frames. `limits stack N` cannot help such a program, since a final build rejects every unbounded call site under it ([stack limits](../spec/10-effects-digests.md#stack-limits)), recursion included.

## Decision

- The `x86_64-host` descriptor gains an entry task stack of 64 MiB, which the entry task of a program gets unless its entry states `limits stack N` ([task stacks](../spec/17-suspension-io.md#task-stacks)). Every other host task keeps the 64 KiB default.
- The stack is mapped without reserving memory, so only the depth a run reaches occupies memory.

Alternatives rejected:

- **Explicit work stacks in every recursive algorithm:** the front end would no longer mirror the bootstrap it must match byte for byte, and every recursive walk would grow a loop and a list.
- **A larger default for every task:** a server's thousands of tasks would each reserve it.
- **Allowing recursion under `limits stack N`:** the limit would stop being a proven bound.

## Consequences

- A host program recursing without end now traps with `trap.stack-overflow` only after filling 64 MiB.
- The entry task's stack use is measured against 64 MiB, its other tasks' against 64 KiB.
