# 0247. A trap abandons a closed domain

Status: accepted

## Context

A domain's first task closes the domain as its entry ends, so that its abandoner no longer abandons it. A trap claimed the domain's abandoning by the same compare-and-swap from zero, so a task of the domain that outlived its first task, such as a keyed task of an abandoned stepper, found the domain closed and stopped without a report: the run returned the body's result, and the stopped record stayed on the list of live tasks, naming the domain once its run freed it. The specification abandons a domain at any trap of its tasks; only the abandoner stops once the first task has ended.

## Decision

- **Closing and abandoning are bits of one word.** The first task's end sets `CLOSED` only while the word is zero; a trap sets the abandoned bit whatever the word holds, the trap that finds it clear being the domain's; the abandoner still abandons only a word of zero.
- **Settlement lets a closed domain's first task end.** That task, its entry over, may still be on the list of live tasks; settlement does not ask it to stop and waits for its count-out as for any task, so it always ends rather than stops, and the run of an abandoned domain that had closed frees its record after settlement.

Rejected alternatives:

- **Freeing a task that traps in a closed domain without abandoning it:** the trap would go unreported and the run would return a result the domain's abandonment should have discarded.

## Consequences

- A domain run returns `Err` whenever a task of its domain traps before every task has ended, whether or not the body had returned.
- A first task that closed its domain is never stopped, by its own domain's settlement or an enclosing one's.
