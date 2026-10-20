# 0042. Read views wrap in place

Status: accepted

## Context

`Some`, `Ok` and `Err` could wrap only parts, the results of projections. A function could not wrap what a read parameter holds: `Some(self.field)` in a projection, or `Some(found)` for a name a pattern binds from a read parameter, was `mode.unmarked`, whose exact fix inserts `take`, and the move that marker asks for is `own.move-not-owned`. Writing the NaN front end, agents followed that fix to unsound code the checker then accepted, and the sound forms needed an identity projection returning its receiver as a part. Returning or passing an optional view of a parameter's field is among the most common things a function does.

A read parameter is a view of its caller's value for the whole body: nothing writes it while the body runs, and a value wrapping it cannot outlive the body except as a projection's result, which derives from the receiver.

## Decision

A read view is a read parameter, `self` received by read included, a field path from one, or a view binding of what a read view holds. In `Some`, `Ok` or `Err`, a read view that is not Copy is unmarked and builds a wrapped part, as a part does. Parameters received by `mut` and `take` stay outside the rule.

## Consequences

- Optional views of parameters are written directly; the identity projections leave the front end.
- A `take` before a read view in `Some`, `Ok` or `Err` is `mode.mismatch`, as before a part.
- A wrapped read view follows the rules of wrapped parts: used within its statement or returned from a projection whose receiver it derives from.
