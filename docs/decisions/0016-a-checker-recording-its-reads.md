# 0016. A checker recording its reads

Status: accepted

## Context

[Check time](../spec/33-toolchain.md#check-time) applies the rules of language 1 per item and target, and the [query engine](../spec/33-toolchain.md#query-engine) keys each query by what it read. `nan context` must print exactly the closure set of an item, the completeness test checks that every fact checking an item reads is in its pack, and `nan check --radius`, `nan explain` and the last-green record are computed from those packs. The bootstrap reaches language 1 in stages: elaboration to NIR, the interpreter and the prover arrive in a later task, yet `nan show --api` and the digest line already print facts that depend on them.

## Decision

- **One checker per target.** A checker holds a database of the workspace's and std's items for one target, lowered to an abstract syntax, and computes each fact as a query over one item: signatures, generic shapes, structural properties, digests, typed bodies and the impl index, each memoized for the run.
- **Reads recorded by subject and line.** While recording, the database notes each item a query reads, attributed to the item the innermost running query is about and to the line of its text being checked: its head while its signature is computed, each clause and statement while it is typed. A query records reading its item even when its result is memoized. Rules relating several items, such as duplicate impl lines, and searches for fix candidates read under no subject. Reading an item only for its name and kind, as the names of its module show it, records nothing.
- **Closure sets from the record.** An item's closure set is what checking it reads, with the item types of its expressions; a line's is what checking the head, clause or statement starting at the nearest recorded line at or above it reads, with the types of the expressions starting there. Context packs, closure radii, the completeness test, `nan explain` and the closure sets of failing lines in the last-green record all come from these records.
- **Facts of later stages.** Until const evaluation exists, a const has no interface hash and `nan show` prints `-`. Until obligations and the prover exist, an ensures clause shows `checked`, or `open` when disputed, and every digest line shows `may-trap yes`: the statuses and the fact of a toolchain that discharges no obligation.

Alternatives rejected:

- **Closure sets computed from the syntax**: they would drift from what checking reads, and the completeness test would test the copy.
- **Recording every read under the item being checked**: memoized results would be attributed to whichever item computed them first, so packs would depend on checking order.
- **Leaving statuses and may-trap out of the output**: the record layouts are fixed; the values printed are those the rules give a prover that proves nothing.

## Consequences

- Results do not outlive a run: the query keys and the persistent cache meeting the latency bound remain to be built on the recorded reads.
- A new query about an item must note its read and run under that item, or its reads land on its caller.
- When the prover lands, statuses, may-trap and the const interface hash change in place, without changing a record layout.
