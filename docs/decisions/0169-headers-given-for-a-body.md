# 0169. Headers given for a body

Status: accepted

## Context

A runner gives a message's header through `Stepper.deliver_headed`, but it serves protocol calls through `Wire[P].serve`, which posts the decoded call itself, and starts and resumes components through `Stepper.start`, `resume` and `resumed`. Each of those took the header of the runner's own task, so neither a served call nor a component's start, resume and their keyed tasks could carry a cause.

## Decision

- **`sys::run_headed(header, body)` runs a body with the calling task's header set to the given one**, then restores the task's own; whatever the body queues or starts carries the header through the rules that already give each message and task the header of the task queuing or starting it.

Rejected alternatives:

- **A header parameter on `Wire.serve`, `start`, `resume` and `resumed`:** four signatures change for one rule, and the next operation that queues or starts work would need its own.
- **A setter of the running task's header:** nothing would restore the task's own header when the work ends or fails.

## Consequences

- Any code that waits may run a body under a header of its choosing; a header carries what its runner encodes and is not an authority.
