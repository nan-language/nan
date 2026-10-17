# 0031. Entailment through judged functions

Status: accepted

## Context

[Weakening](../spec/31-trust.md#weakening) and the [compatibility](../spec/29-identity-versioning.md#compatibility) of releases ask whether one spec's clauses entail another's, and whether a parent release's examples still hold against a new one. Only the prover decides entailment and only the interpreter runs examples, and both read checked NIR, while the specs compared exist as texts of API snapshots and of the parent's store entry.

## Decision

- A question is asked of a **judged function**: NaN source the toolchain writes beside the item asked about, added in memory to its module as a file `.judged.nan` in the module's directory, and checked with the package under the first target it is checked for. The answer is whether the function checks without error and the statuses of its obligation sites.
- Accepted requires entail the current ones when every requires obligation of a call of the item, from a function stating the accepted requires, is proven; current ensures entail accepted ones when the ensures obligations of that function, stating the accepted ensures, are. A spec that is not the item's current one is stated by a function with that spec and the body `todo "stated"`.
- A parent's examples hold when a function holding them as its example clauses checks against the release.

Alternatives rejected:

- **Judging by the clauses' texts:** a stronger clause written differently counts as weaker, so equivalent rewrites need acceptance.

## Consequences

- Each judgement checks the package again, its obligations reusing the proof cache.
- A judged function is never source: its file name starts with `.`, which no workspace file does, and no command reports its diagnostics.
