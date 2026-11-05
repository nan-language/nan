# 0134. Component instances in isolated spaces

Status: accepted; no handle crossing into or out of a space superseded by 0135, and the instance acting through no runner by 0136

## Context

NaN OS places each component instance by admission, in the shared space or in an isolated space, and its runner calls the instance's handlers through the runtime of the space holding it. A kernel holds an instance through channels between its tasks and the instance task ([0119](0119-component-instances-of-nanos-bundles.md)), and runs a job in a space ([0118](0118-nanos-jobs-in-isolated-spaces.md)) whose runtime reaches the kernel through system calls and through the doorbell and channel frames of a link ([0131](0131-links-and-handle-tables-of-isolated-spaces.md)). A std channel lives in one runtime, so the kernel's tasks and a space's cannot share one. A domain run captures by `take` alone, so a kernel running a space inside a domain would lose the space's frames when that domain is abandoned.

## Decision

- **The same instance, held across a link.** `nanos::hold_isolated` gives the holder the `Instance` `hold` gives, its requests and calls on kernel channels as before, so the holder's code is the same whatever the placement. Kernel tasks relay them down the link's first channel and route the answers coming up it to the instance's waits; in the space, the runtime's first task runs the instance task in a domain of its own on space channels and relays between those and the link's channel. Messages are `mem::wire` encodings of private std types after a four-byte length.
- **The space's runtime starts the entry the start record names.** The record says whether it names a job or a component entry, read from `Isolated.component`, and an instance's checkpoint is its input; one runtime image serves both.
- **Waits end by message.** A wait the holder abandons sends its end down the channel; the space delivers each request on a task of its own, so an end never waits behind a request the instance has not taken.
- **The space's frames stay the kernel task's.** The abandoner `Instance.domain` shares runs a domain that only waits; once abandoned, the kernel marks the space halted and rings its doorbell, and the space runs no more from its next exit.
- **A link's channels wake their own waiters.** Each channel holds a signal for its receiver and one for a sender waiting for room, so the kernel's relays of an instance and the caller's tasks on other channels wait at once.

Alternatives rejected:

- **An instance type of its own for spaces:** every holder would be written twice.
- **A system call per request:** the space's thread would block on the kernel and every holder operation would cost a context switch.
- **The kernel's space loop in the abandoner's domain:** abandoning it would lose the space's frames.

## Consequences

- An instance in a space acts through no runner, and no handle crosses into or out of a space: a call or reply carrying handles fails as malformed. Runner capabilities, ports and transferred handles for spaces remain to do; the kernel's `Runner` operations are private to `sys`, and a handle names a value of the runtime that minted it.
- An instance in a space costs the kernel five relay tasks and the first channel of its link, and the space a task per request in flight.
