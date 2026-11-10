# 0167. Headers carried by tasks

Status: accepted

## Context

A runner hands a stepper's handler only the message's value. Capability code that the handler or its keyed tasks run cannot tell which call it serves, and an operating system that traces causes would otherwise have to pass the cause through every protocol operation and trust each component to forward it.

## Decision

- **A task carries a header**, an opaque 32-byte `sys::Header` or none, which `sys::current_header()` reads wherever the task's code runs.
- **A task starts with the header of the task that started it**, so keyed tasks, and any task their work starts, carry the header of the call that spawned them without a rule of their own.
- **Each intake entry carries the header of the task that queued it**, through `send`, `after`, `after_ns` or a post, and a handler runs with its message's header, the one `Stepper.deliver_headed` gives, or none for `deliver`; the task's own header returns once the handler ends.

Rejected alternatives:

- **A header parameter of `Stepper.deliver`:** it would change every holder's calls.
- **A header the component context alone holds:** capability code that a handler's keyed tasks run reaches no context.

## Consequences

- A task record and an intake entry each take 40 bytes more.
- The header's bytes mean what the runner encodes in them; NaN neither reads nor checks them.
