# 0060. Jump threading, left-out calls and a lock per pool

Status: accepted

## Context

The self-built toolchain still ran slower than the bootstrap on small commands. Its profiles showed costs the code generator leaves in place: struct copies written word by word kept every `Range` and `Option` slot in memory, through a store-forwarding stall at each loop entry; iterator loops tested the same condition two or three times, once in `SliceCursor.next` taken in place and again on its returned tag; every call of a function with `requires` still called the requires predicate and dropped its result; and spilled operands were reloaded at each use even when the scratch register still held them. The IR lowering builds must stay the bootstrap's ([0046](0046-the-nan-code-generator.md)), so these belong to code generation, as in [0051](0051-leaves-in-place-and-slots-in-values.md).

Parallel checking runs many tasks of one domain through cells of their own, but every allocation and free of every executor thread took the runtime's one lock of allocations, spinning on `sched_yield`; an eight-thread check spent most of its system time there. A pool per domain and cell ([0049](0049-carved-allocations-are-reused.md)) already decides where each allocation lands, so only the locking serialised what placement keeps apart.

## Decision

- **A slot holds a wider access over narrower ones in one value.** Accesses of a stack slot that meet must lie within one widest access of an integer of at most 64 bits; the narrower ones read and write its bits.
- **Edges are threaded.** An edge into a block of few instructions, through its jumps to blocks only it reaches, goes straight to the target of the block's conditional branch when constants and the outcomes of dominating branches and comparisons decide it; the block's instructions are copied onto the edge and its values read elsewhere merge through variables. Facts on values a block defines anew are dropped on entry to it.
- **Unread calls of functions that cannot be seen are left out.** A function is noted, as it compiles, when from its entry no path meets a block twice and no instruction it reaches traps, writes memory other than its stack slots or calls a function not noted; callees compile first, so a recursive or not yet compiled callee is never noted. Its calls whose results nothing reads are dropped. A load of bytes a dominating load read since the last write takes that load's value.
- **Calls of `Copy` into a stack slot nothing else reaches are left out**, the lowering naming the routine to code generation.
- **Emission skips a reload into a scratch register that holds the slot**, indexes scale through `imul` and `ishl` by up to 8, a multiplication by a power of two shifts, a `bor` of opposite shifts rotates, and a stack slot's address is computed again at each read.
- **Spills weigh accesses by loop depth**: the range spilled is the one whose accesses weigh least, each weighing eight times more per loop around it, and a value living across calls stays in a register the calls clobber, saved around each, when that weighs less than a callee-saved register.
- **On `x86_64-host` each pool has a lock.** `Alloc`, `AllocIn` and `Free` carve and free a small allocation under its pool's lock, found without a lock as pools are only ever added; the lock of allocations is taken as well to add a pool, to draw or return chunks, which charges and checks the cell chains, for runs no pool carves and in `Lift`. A routine taking both takes the pool's lock first. Those slow paths, and waiting on a busy lock, which reads it between pauses before the thread yields, are routines of their own, so the fast paths keep few registers. Machine targets keep the bootstrap's routines.
- **The self-check compares host lowerings without `Lift` and the new routines as well**, now among the routines the NaN lowering writes its own way ([0050](0050-host-support-routines-of-the-nan-lowering.md)); programs and test runs compare what they do.

Rejected alternatives:

- **Per-thread or per-task pools:** which chunk an allocation lands in, and so charges and points of exhaustion, would depend on scheduling or differ from the bootstrap's.
- **Proving predicates free of traps from the checker's proofs:** a requires proven at a call says its value, not that evaluating it cannot overflow.

## Consequences

- The self-built toolchain runs about 14% fewer instructions on a cold `nan test` of a std package without tests.
- A left-out call or copy shrinks its caller's frame; stack limits still count the call and the slot, so judgements only grow more conservative than the frames.
- Tasks allocating through different cells, or in different domains, no longer wait for each other except to draw or return chunks; a single thread pays about 1% more instructions for the second lock.
