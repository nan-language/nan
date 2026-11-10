# 0166. An `await when` charges the moves its release makes

Status: accepted

## Context

Releasing an `await when`'s cowns moves each value whose chunks pass to another domain, and settling an abandoned domain moves the values of the cowns it poisons. Both walks ran inside support routines, whose calls count only the stub call bound, so no final build judged them: a cown holding a type that holds itself, or a `task fn` whose closure captures another, made them recurse as deep as the run's data went, below the routine reserve (0164), on targets that promise no stack overflow.

## Decision

- **A release of an `await when`'s cowns counts the stack use of the moves of their value types** in the code holding the `await when`, beside the stub call bound, so a move without a bound makes the `await when` `limit.stack-unbounded`, as moving such a value anywhere else does.
- **Settlement's poisoning walks are those releases' walks**: the routine reserve counts the most any of them uses, each bounded wherever its `await when` was judged.

Rejected alternatives:

- **Running each walk's nestings on segments, as nested decodings do:** a type holding itself through a box would still recurse within one nesting.
- **Moving captures through a worklist:** values being moved stay live, so it would need memory the move may not find.

## Consequences

- A program holding a cown of a type whose move is unbounded no longer builds where a stack limit applies.
- An image whose `await when`s all build has a bounded routine reserve.
