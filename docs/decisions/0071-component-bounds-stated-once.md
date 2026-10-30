# 0071. Component bounds stated once

Status: accepted

## Context

`sys::Component[Env, M]` requires Env and M to be Send and droppable. `sys::Cx`, `sys::Outbox` and `host::Runner.run` restated these bounds on their own M and Env. Once the arguments of every bound are checked, as the instantiation rules require, one linear message type would be reported on the impl line, on the `on` head writing `sys::Cx[M]`, on each `sys::Outbox[M]` written for the runner's body and at each `run` use: several diagnostics for one defect, against root-cause reporting.

## Decision

The requirements on Env and M are stated by `sys::Component`. `sys::Outbox` and `host::Runner.run` bound them by `Any` alone. `sys::Cx` bounds its M by `Any and Send`: Cx is a view type, so without `Send` its parameter would accept views, which the outboxes of its keyed tasks cannot hold.

A checker rule silencing the restated bounds where a component's impl line already fails was rejected: it would tie diagnostics on other items to that line.

## Consequences

- A component whose Env or M is linear gets one diagnostic, on its impl line. A message type that is not Send is reported on the impl line and on the `on` head.
- A `run` use of a state with no impl line for the Env and M it fixes reports `component.not-component`, whatever those types are.
- Soundness holds: the runner creates every context and outbox, and only for a component, whose impl line enforces the requirements.
