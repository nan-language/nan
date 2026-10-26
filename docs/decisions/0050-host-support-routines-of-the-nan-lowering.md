# 0050. Host support routines of the NaN lowering

Status: accepted

## Context

The NaN lowering mirrors the bootstrap's, and the self-check compares the text of every function it lowers with the bootstrap's ([0046](0046-the-nan-code-generator.md)). The bootstrap's support routines for allocation pay costs the frozen bootstrap cannot shed: `Unlock` frees its lock word with an atomic store, which x86-64 code generators emit as a store and `mfence`; `Lock` and `Unlock` each call the `Thread` routine; `Alloc`, `AllocIn` and `Free` call `Lock` and `Unlock`; and a reused allocation is zeroed through a call of the C library's `memset`. Every allocation and release of a NaN program pays them: in the self-built toolchain, `Unlock` alone took 6% of a check and 44% of the nanbench host benchmark, whose allocation ran twice as slow as built by the bootstrap. On the machine used, taking a lock with `lock cmpxchg` and freeing it costs 30 ns with `mfence`, 17 ns with `xchg` and 9 ns with a plain store.

## Decision

- **On `x86_64-host`, the NaN lowering writes `Alloc`, `AllocIn`, `Free`, `Lock` and `Unlock` its own way.** `Unlock` frees the word by a plain store, which x86-64 orders after every access before it, as a release needs. `Lock` and `Unlock` read the thread word in place, and `Alloc`, `AllocIn` and `Free` take and release their lock in place. A reused allocation of up to 128 bytes is zeroed word by word. Allocations, charges, points of exhaustion and the locks a trap releases stay those of the bootstrap's routines.
- **Machine targets keep the bootstrap's routines.** No harness runs a machine image the NaN toolchain builds, so the comparison of their text is the only check they have.
- **The self-check compares host lowerings without those five routines,** each other function by name, with every external name numbered by its first mention, since which functions and C library functions lowering declares first moves with what the five call. What the five do is compared through the programs and test runs both toolchains build and run.

Rejected alternatives:

- **Freeing the lock with `xchg`:** a full barrier the release does not need, at twice the cost of a store.
- **Per-thread allocation pools without the lock:** which chunk an allocation lies in, and so a cell's charge, would depend on which thread runs a task, and points of exhaustion would no longer reproduce.
- **Changing the bootstrap's routines:** the bootstrap is frozen.

## Consequences

- Images the NaN toolchain builds for `x86_64-host` allocate faster than the bootstrap's: the nanbench host benchmark draws a box in 48 ns instead of 80, and the self-built toolchain checks its workspace in 285 s instead of 350.
- The text of those five routines no longer guards the port; a change to them is checked by what programs and test runs do.
- A code generator for the NaN toolchain must keep the order of memory accesses around a lock's release.
