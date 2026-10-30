# 0075. Capability checks inlined in lowering

Status: accepted

## Context

Every operation through a Copy capability, and every executor reached through its handle, checked the handle's generation by calling a `cap_table` routine that read the entry's generation and live words and gave the entry's record or zero. The routine took no lock and had no slower path: the call, its frame and its return were the only cost beyond two loads and two compares, on paths NaN OS takes on every message send and store access. The table's address is written once by `Init`, before any other thread or CPU runs; generations and live words change only under the runtime lock, and aligned 32-bit loads are single-copy atomic on both architectures, so lowered code reads them as soundly as the routine did.

## Decision

- **Lowering emits the check in place** at each operation of a sys capability and each executor reached through its handle: the same loads, compares and select the routine performed. The routine is gone.
- **No call on a mismatch.** A stale handle stays stale, since a slot's generation only grows, so a mismatch is the answer and nothing remains for a routine to decide.
- **The bootstrap and the NaN lowering both change**, emitting the same IR, so the comparison of lowered text keeps guarding the machine targets, where it is the only check, and the nanbench timings, taken on images the bootstrap builds, measure the check as programs run it.

## Consequences

- An operation through a stale capability fails without leaving its function; each check site grows by the routine's body, a handful of instructions.
- A change to how entries record liveness changes every check site rather than one routine.
