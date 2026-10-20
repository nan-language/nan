# 0039. Canonical queries share terms

Status: accepted

## Context

A query's weakest precondition shares heavily: the reach condition of a block holds those of its predecessors, and a value merged at a join holds the values merged before it ([queries](../spec/20-prover.md#queries)). Canonicalisation read these terms as trees. A conjunction inside a clause got a fresh symbol at each occurrence, with its defining clauses repeated, and the canonical encoding wrote every argument term out in full, so term order and obligation hashes read encodings as large as the tree a term unfolds to.

Ordinary code made both exponential. One overflow obligation of a 35-line function, two `match`es before a loop, canonicalised to 1.9 million clauses and a 97 MB encoding; checking it took 25 seconds and gigabytes of memory, and a run of `if`s updating one count grew the encoding twofold per `if`. Since every conforming implementation must produce the canonical form ([determinism](../spec/20-prover.md#determinism)), no implementation could be both conforming and usable on such code.

## Decision

- A conjunction that one term gives in one polarity has one fresh symbol: its first occurrence introduces it with its defining clauses, and every later occurrence is that symbol ([canonicalisation](../spec/20-prover.md#canonicalisation)).
- A term is written as its **term key**, the BLAKE3-256 digest of its own encoding with each argument term written as its key, wherever the canonical encoding writes it. The term order compares terms by their keys, and atoms and clauses by their encodings ([determinism](../spec/20-prover.md#determinism)).

Alternatives rejected:

- **Memoising the expansion inside an implementation:** the specified clause set itself is exponential.
- **Comparing tree encodings lazily, keeping the old order:** every implementation would need byte-level comparison of unbounded encodings, and obligation hashes would stay exponential to compute.
- **Simplifying reach conditions in queries:** values merged at joins share as much as the conditions do.

## Consequences

- Canonical queries, their encodings and their hashes grow linearly with the distinct terms of a query.
- The term order of two distinct terms follows their keys, not their structure; it stays deterministic and identical across implementations.
- Obligation hashes changed, so proof caches written before this change no longer match.
- Every conformance example keeps its outcome under the new canonical form.
