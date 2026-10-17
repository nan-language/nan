# 0027. Forwarding functions

Status: superseded by 0030

## Context

History tombstones [forward, match and take old forms](../spec/29-identity-versioning.md#history) as tombstones in source do, and a use of a `preserves` tombstone [elaborates](../spec/19-nir.md#elaboration) as its rewrite. A tombstone's rewrite is typed in its own module, as the body of a function with its old head, with its own locals and spans, while the elaborator works over the one typed body of the item it elaborates.

## Decision

- The workspace reads a package's valid history as one file per module holding its tombstones, each tombstone on its line of the history and every other line blank. The checker adds those tombstones to their modules beside the source's, but never to the modules' files, nor to the units or tombstone forms of the code root that stand for source.
- A tombstone with an old head has a NIR unit holding its forwarding function: the old head with the typed rewrite as its body. A forwarded use calls it.

Alternatives rejected:

- **The rewrite inlined at each use:** elaborating one typed body inside another needs local and span mapping across items throughout the elaborator.

## Consequences

- Forwarded uses run in the interpreter and in images, and their digests come from the rewrite.
- A forwarded use is a call in NIR: the prover reads the forwarding function's facts at it, not the rewrite's.
- The code root of a package holding a head tombstone in source holds its forwarding function.
- Publishing checks that a head tombstone preserves its item through the same forwarding function, given the parent's contracts.
