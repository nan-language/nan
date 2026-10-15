# 0018. Prefix closing from the grammar tables

Status: accepted

## Context

[Prefix checking](../spec/33-toolchain.md#prefix-checking) reports a prefix's first dead token, then checks the prefix closed by the fewest appended tokens after which the file derives, typed holes uncounted, the ties broken by skipping optional parts or taking the alternative written first ([prefix viability](../spec/41-grammar.md#prefix-viability)). The parser runs on tables generated from the published grammar ([0014](0014-parser-from-the-published-grammar.md)).

## Decision

- **Minimal costs from the tables.** The cost of deriving each sequence and choice minimally is computed once, by fixpoint over the generated tables, counting virtual tokens, not typed holes, and nothing through rejected alternatives.
- **Completion from the parser's frames.** The prefix is parsed with the LL(1) driver up to its first dead token; the open frames, innermost first, are completed by deriving their remaining elements minimally, a closing label repeating the item's name.
- **Closed source.** The appended tokens are written as canonical source after the kept prefix, and the result is checked as the workspace's file; diagnostics on appended lines or in constructs holding appended tokens are dropped as the specification states.

Alternatives rejected:

- **Searching appended token sequences**: the search grows with the depth of open constructs, while the costs are a property of the grammar computed once.
- **A closing grammar written by hand**: it would restate the grammar ([0014](0014-parser-from-the-published-grammar.md)).

## Consequences

- A grammar change changes closing with no code change.
- Closing output follows the canonical format, so the closed source checks like written source.
