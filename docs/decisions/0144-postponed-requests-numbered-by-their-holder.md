# 0144. Postponed requests numbered by their holder

Status: accepted

## Context

A component's handler never waits, so a request whose answer depends on something yet to happen (input arriving, a device interrupting, a commit becoming durable) cannot be answered by the handler's result. `Cx.reply` answers a request its holder keeps pending, by number, but no handler learns its request's number: a holder knows nothing of a protocol's meaning, so it cannot tell which requests to keep pending, nor pass the number through one of the component's operations without re-encoding the client's call ([0132](0132-runner-capabilities-of-components.md)).

## Decision

- **The holder numbers each call it serves.** `Wire.serve` and `nanos::Inbox.call` take the request's number with the call, and the intake entry of the request carries it. A number the stepper chose would reach the holder only once `serve` returned, after a `reply` the same handler may already have given; the holder's own numbering also stays deterministic under replay and outlives the component's checkpoints.
- **The handler postpones its request.** `Cx.postpone()` marks the request whose handler runs and gives its number, `None` for a one-way operation, a system interface, a request no `serve` numbered (a call through an outbox) or a stepper without a runner capability, where no `reply` could reach a holder. The handler's head is unchanged: its result or failure is released at its end, and the poster learns the request was postponed. `postpone` is the name, `defer` being a keyword.
- **`serve` tells its caller which happened** by giving `Option[Encoded]`, `None` for a postponed request, which the holder keeps pending under the number it chose until `reply` answers it or the holder ends it.
- **`serve` posts a request itself** instead of calling the outbox operation, so that the number enters the entry before the component can take it and the wait reads the postponed status.

Alternatives rejected:

- **A handler head giving an optional result:** every request's handler and the conformance of servers would change for a choice most handlers never make.
- **A typed token in place of the number:** the state keeps it in its checkpoint, and nothing would check it against the operation it answers more than `Reply.decode` checks a reply.

## Consequences

- An intake entry grows by two words, and `serve` lowers its requests' posts and waits inline.
- Every holder serving encoded calls handles `None`, and keeps the postponed requests a checkpoint names across its component's restarts; what a client sees of a request never answered is its holder's to decide.
- A postponed request's reply is encoded by the component, through `mem::wire` for its types; a result of a handle type cannot be answered later yet.
