# 0072. Linear Env abandoned by the runner

Status: accepted

## Context

A component's Env had to be droppable, so a component could not keep a linear resource across messages. NaN OS drivers are components that hold linear hardware capabilities (MMIO windows, shared interrupts, DMA rings, budgets) for their whole life. Moving a resource out of the Env and back in a message does not help: messages stay droppable, since the intake releases those that never reach a handler.

## Decision

`sys::Component` bounds its Env by `Any and Send`. The runner releases a linear Env by calling its type's abandon method (17 §Abandoning resources) when the component task ends, and on the calling task when the executor is stale. An impl line whose linear Env has no abandon method visible there is an error, `component.env-not-abandonable`, and so is one whose abandon method has a requires clause that is not disputed: the runner calls it on whatever Env the last handler left, where no obligation can be stated or discharged, so the precondition would silently go unchecked. Messages stay droppable.

Rejected:

- Runner-held resources reached through Inert ids stored in the state: a new id type and table in the runtime, and ids in the state that outlive their resources across state schemas.
- `run` returning the Env with the state: every caller would have to consume the Env, and a failing body or a stale executor would still need a release the runner performs.

## Consequences

- A linear Env's resources end with the component at once, without a graceful release that would wait.
- The abandon method of an Env is reachable from the impl line, so it is never unused when only the runner calls it.
- A trap in a handler abandons the domain as before, and the Env is not abandoned through its method then.
