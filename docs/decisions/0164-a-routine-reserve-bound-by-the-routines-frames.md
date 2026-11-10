# 0164. A routine reserve bound by the routines' frames

Status: accepted

## Context

A task's stack holds its usable size, which the final build judges counting each call of a support routine at the stub call bound, and below it a routine reserve for the rest of what those routines use. The reserve was a fixed 32 KiB, 8 KiB more on a kernel, under a 4 KiB guard region, and a machine target drew the stack aligned to a 64 KiB chunk, so a task bounded to 4 KiB took a whole chunk of the 16 MiB boot heap and a kernel ran fewer than 300 such tasks. No access to a guard region traps on a machine target, and nothing bounded the fixed reserve.

## Decision

- **A machine image's final build computes its reserve** from the frames and call sites of every function it lowered, following calls of the support routines it defines: the most a call of one of them uses, the most a task's start uses beside its usable part, and on a kernel taking interrupts a seam frame and the most its preemption glue or an exception's trap uses. The image holds the reserve as data that `Spawn` and grown segments read.
- **A support routine's call through an address names the family of glues it reaches**, such as entry, end, withdrawal or policy glues, and the analysis takes the most any glue of that family uses. A policy hook is never called while one runs on the CPU, which the analysis relies on.
- **A loaded bundle's line tables give the most its own glues use**, which a kernel's routines reach once it is loaded, so the reserve adds it with a call of a support routine while bundles are loaded.
- **Machine targets have no guard region**, and their stacks are drawn with a carved allocation's alignment, so they share chunks.
- **Where a support routine's use has no bound**, as when walks of task closures nested in captures recurse over run-time data, the image keeps the fixed reserve.

Rejected alternatives:

- **An allocator given to an account at its split:** it would charge the same 64 KiB per task to another supply, and change the executor's interface.
- **A boot heap a kernel sizes:** a target's descriptor fixes it.
- **A smaller fixed reserve:** nothing would prove it suffices.

## Consequences

- A task bounded to 4 KiB takes about 14 KiB of a test image's heap and about 32 KiB of a kernel's, most of it the trap report a support routine writes on the stack.
- Every support routine's call through an address states its family, or it leaves the reserve unbounded.
- The host targets keep the fixed reserve and the guard region, the C library's use being unknown.
