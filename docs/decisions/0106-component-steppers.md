# 0106. Component steppers

Status: accepted

## Context

Components ran only under `host::Runner`, which owns a task of its own for the handlers and is unavailable on machine targets. An operating system written in NaN runs its services and drivers as components inside its kernel: it must hold their state and Env, call their handlers on tasks it schedules, decide when their keyed tasks start, and checkpoint and restore their states without calling `start`. Only std can create a `sys::Cx`, and quiescence is guaranteed only where std calls the handlers.

The operating system also wants operations of its own around handlers: sends to other components' intakes, deferred replies, synchronous checkpoints, capability and resource tables. Std items cannot gain methods in a later version.

## Decision

Std provides `sys::Stepper[S, Env, M]`, a linear base type on every target whose holder steps a component on its own task: `start` and `resume` (from a decoded state, calling no handler), `deliver` and `step` (one handler call each, protocol dispatch and replies as the runner does), `incoming` and `ready` (waits for a message, or for the stepper to turn idle), `start_keyed` (keyed spawns are held until then), `idle`, the projection `state`, `end`, which drains keyed tasks and gives back the state and the Env in `sys::Stopped`, and the abandon method `abandon`.

- Std still mints every `sys::Cx`: the seam is a stepping interface, not a raw one, so handler quiescence, keyed-task and outbox semantics stay those of the runner, and both share one record and one implementation.
- A checkpoint is the wire encoding of the state of an idle stepper. Intake entries and keyed tasks are not checkpointed: messages and task closures are not `Encode`.
- The operating system's own operations live in the Env, whose types it defines and which handlers reach by `mut`: outboxes of other components, pending-reply tables keyed by Inert ids, checkpoint requests acted on between steps, capability and resource tables. `Cx` gains nothing.
- A stepper's record is a holding of the domain that started it, so settlement withdraws it as it withdraws a run; keyed tasks and armed timers reference the record, so `abandon` ends a stepper without waiting.
- `start`, `deliver` and `step` carry the declared digest of a call through `sys::Component`, since they call handlers on the caller's stack.
- A stale executor makes `start` and `resume` fail with the cancellation, as it makes `host::Runner.run` fail.

## Consequences

- Kernels and the reserved nanos targets run components; `host::Runner` stays host-only.
- Holders write the event loop: drain with `ready` and `step`, start keyed tasks, checkpoint when idle. A component holding a long-lived keyed task never turns idle and so is never checkpointed.
- The runner's record grows by the holding word, and keyed nodes by the held flag.
