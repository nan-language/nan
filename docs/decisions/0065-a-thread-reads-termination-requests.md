# 0065. A thread reads termination requests

Status: accepted

## Context

A later termination request must end a host process at once (spec 27). The runtime read the requests from a `signalfd` through the first executor thread's ring, so they were read only when that thread waited in its ring. While it ran a task that never waits, nothing read them, and because standard signals do not queue, a second `SIGTERM` merged into the first one still pending: the process ran on, and only `SIGKILL` ended it. Both toolchains generate this runtime.

## Decision

- **A thread of its own reads termination requests**, blocking on the `signalfd`. It passes the first to the first executor thread's ring with `IORING_OP_MSG_RING`, as the completion of the operation recording the request, so the executor handles it exactly as before: the entry task is cancelled by acknowledged cancellation. At a later request the thread itself ends the process with status 128 + n.
- **Both lowerings change together**, since this is a defect both toolchains share; the lowered IR stays identical between them.

Rejected alternatives:

- **A signal handler counting requests:** the first request's cancellation would run inside a handler, where taking the runtime's locks is unsafe.
- **Polling the ring from running tasks:** host tasks are not preempted, so a task that never waits would still hold the thread.

## Consequences

- A host process has one more thread, which runs no task and holds a ring for the one message it sends.
- The first request still takes effect at the entry task's next cancellation point.
