# 0055. Snapshots of checks in the build cache

Status: accepted

## Context

The NaN toolchain's build cache kept only prover results and code roots, so a `nan check` after changing one function body cost almost as much as a cold one, where the [query engine](../spec/33-toolchain.md#query-engine) requires recomputing only what depends on the change. The checker computes facts lazily, with memos and shortcuts shared across items; its recorded reads exist only in the slow mode context packs use, which turns those shortcuts off, so they cannot serve as the dependency record of an ordinary check.

## Decision

- The work of an item is what runs while it is the subject of a query (`Db.about`), its judgement, elaborated rules and `where` checks included. Each check traces, by item, what that work reads of another item's body (typed, lowered, elaborated, its obligations) or names, and the fields it reads and labels, the authorities it performs and the consts it needs. Every diagnostic found carries the item whose work reported it; those of rules relating items are recomputed by every check.
- A **snapshot** is a cache entry keyed by the hash of every input of a check of a closure under a target. It holds, per item checked, its outcome (diagnostics and those of unused code, encoded whole; whether judging it alone, and the check, rejected it; its contributions; the items it names; a const's interface hash) and green entry; for every database item, the digests, dependencies and may-trap facts computed; the traced reads; and per file its contents' hash, the hash of its text outside function and test bodies, and each body's hash and position. Items are named by their database index, which the snapshot's inputs fix.
- A cache record per workspace, closure and target names the latest snapshot. A check of a state without a snapshot takes the latest one when the two states differ only inside function and test bodies, and checks again the items whose bodies changed, the items whose traced reads reach those, and the members of the dependency cycles holding any of them. It restores every other item's outcome, its positions moved past the changed bodies, and its facts, and reruns the rules relating items. It keeps that result only when nothing it reports depends on the order of work (no `name.case` fix, no `unused.field` fix, no error in a quiet package, no two items' diagnostics that report alike), the items checked again give the rejections, digests, may-trap facts and const values the snapshot holds of them, and the cycles of the dependencies are unchanged. Otherwise, and for any other difference, it checks the state whole. Every fact a restored outcome rests on is thus an input compared exactly or a fact recomputed and compared.
- `nan check` writes the snapshot of its state after its last-green entries, makes it the latest and deletes the one it restored from; the other check-time commands only read snapshots.

## Consequences

- A check after a body edit costs loading and hashing the workspace, reading one snapshot and checking the items the edit reaches.
- Edits outside function bodies, and body edits changing a checked item's rejection, digest, may-trap fact or a cycle, cost a full check.
- One snapshot of every item's outcome is kept per workspace, closure and target.
