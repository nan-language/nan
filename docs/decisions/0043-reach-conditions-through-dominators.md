# 0043. Reach conditions through dominators

Status: accepted

## Context

Language 1 freezes only when the prover proves at least 90% of the bounds and overflow obligations of M4 and, separately, of M5 ([measurements](../spec/38-self-hosting.md#measurements)); below that, [0007](0007-total-contracts-and-a-normative-prover.md) adds prover rules by decision. Measured on the two scenarios, it proved about half of each.

Most of the rest depends on facts that no rule can derive within one function: the length of a list another function built to a known shape, an index read from another table, a counter a loop carries. Code states those, as 0007 expects of it: an `expect` where a shape is established, a loop `invariant`, an `ensures` on a helper, or a slice cut to the bytes it reads.

The remainder came from the procedure. A block's reach condition was the disjunction of its incoming edges' conditions, each the conjunction of its source's reach condition and its own. Past a join, every earlier fact sat under one disjunct per incoming path, and a few joins in, the branch conditions a later site needs were buried under definitional symbols that only case splits uncover, deeper than depth 2 reaches. A `while` scanning a slice after an earlier `if` left its own index unproven though its condition was on the path.

## Decision

The reach condition of a block is the conjunction of its immediate dominator's reach condition and its local condition: the disjunction of its incoming edges' conditions relative to that dominator, `true` when one of them is another's negation, as at the end of an `if`. An edge's condition relative to a dominator is the conjunction of the local conditions of the blocks from its source up to the dominator and of its own condition. A join merges block parameters and contents by these relative conditions ([weakest preconditions](../spec/20-prover.md#weakest-preconditions)).

Every path to a block passes its dominators, so a reach condition holds on the same executions as before; only its form changes, and each dominating branch's condition becomes a premise of its own.

Alternatives rejected:

- **Deeper case splits**: once reach conditions factor through dominators, depth 3 and 4 prove nothing more on the scenarios, and every query pays for them in fuel.
- **A bound on lengths**, so that `i + k` past `i < len` is proven: sound only with a new limit on array lengths, for a few dozen sites that cutting a slice reaches as well.
- **Truncation read as a remainder**: the code it would serve reads better with `fit`, which the fragment already interprets.
- **Combining atoms of three or more terms**: it changes the procedure's cost for facts a cut slice states directly.

## Consequences

- Query encodings, and so obligation hashes, change; the toolchain hash keys the proof cache, so no cached result carries over.
- Path facts survive joins: sequential checks and scans prove without restating their conditions.
- The scenarios carry their facts in their code, and the measurements harness checks the bound on M4 and M5 at every run.
