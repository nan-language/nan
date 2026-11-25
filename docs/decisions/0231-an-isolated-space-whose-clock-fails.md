# 0231. An isolated space whose clock fails

Status: accepted; every clock of a world deriving from the runtime's root clock superseded by 0232

## Context

An isolated space's runtime keeps its timers by the kernel's clock, which it reads through a system call, and the clock of every world it builds is its own root clock ([0118](0118-nanos-jobs-in-isolated-spaces.md)). The isolation answers that call, and waits for the runtime's first timer, on the clock `isolate` takes. Once that clock was stale, the runtime took the call's failure answer as the time, the end of time, so every timer was due at once: the space never waited, and the kernel task driving it kept its CPU. A world's clock read that time as live, where a stale clock fails its readings with `sys::Revoked.Stale`. The runtime learns that the clock is stale only from an answer, and its timers are armed under the timer lock, where revoking a clock, which passes the deadlines armed on it, cannot run.

## Decision

- **The isolation answers a stale clock's reading as stale, and waits for the doorbell and orders alone** when the runtime waits for a timer on it.
- **A failed reading leaves the runtime's time where its last reading did,** and marks the failure.
- **The scheduler then revokes the runtime's root clock and passes every timer armed,** before it next runs a task. Every clock of a world derives from that root or is it, so each fails and passes as a stale clock does; a sleep or deadline armed between the reading and the revocation passes with the other timers, and later timers of the runtime's own wait out a time that no longer passes.
- **A bridge's pump waits for its nudge alone once the runtime's clock is stale,** since a deadline on a stale clock passes at once.

Alternatives rejected:

- **Each clock operation of a space checking the failure mark:** every operation, deadlines and derivations included, would need its own check, and a deadline already armed would never pass.
- **Revoking the root clock where the reading fails:** that reading is made under the timer lock, which passing the deadlines takes again.
- **The kernel ordering the runtime to revoke the clock its worlds hold:** the kernel too learns of the stale clock only at a reading, and a world would read the time between that reading and the order.

## Consequences

- A space whose clock is revoked runs on: its jobs and instances find their clocks stale, and the space waits until its kernel rings or orders.
- A space checks one word of its data each time its scheduler loops.
- A runtime no longer looks at its rings every second once its clock is stale, waiting for its doorbell instead.
