# 0030. Forwarded calls inlined

Status: accepted

## Context

History tombstones [forward, match and take old forms](../spec/29-identity-versioning.md#history) as tombstones in source do. A call matches an old head when it checks against that head and not against the live function its path names, so two tombstones, or a tombstone and a live function, may share a path; a use matching a `preserves` one [elaborates](../spec/19-nir.md#elaboration) as its rewrite. A tombstone's rewrite is typed in its own module, as the body of a function with its old head, with its own locals and spans.

## Decision

- The workspace reads a package's valid history as one file per module holding its tombstones, each tombstone on its line of the history and every other line blank. The checker adds those tombstones to their modules beside the source's, but never to the modules' files, nor to the units or tombstone forms of the code root that stand for source.
- Whether a call checks against a head is probed: the call is typed against it with the diagnostics on the probed item withheld and the body's typing state restored, and it checks when the probe finds no error beyond those its arguments raise typed alone.
- A forwarded call is typed as a call against the old head. Elaboration inlines it: the arguments evaluated as the call's are bound to the rewrite's parameters, then the rewrite is elaborated over the tombstone's typed body, its outermost call with the use's markers and its failure converted through the old head's failure type. The inlined code has the use's span, a generic tombstone's parameters take the call's arguments, and the rewrite's closures become functions numbered after the body's. Tombstones have no units.

Alternatives rejected:

- **A forwarding function per tombstone, called by each use:** tombstones sharing an old form share its canonical path, so their functions collide in NIR references and host symbol names, and the NIR differs from the specification's.
- **The rewrite typed again at each use:** its names resolve in the tombstone's module, its tokens lie in the tombstone's file and its types over the old head's generic parameters.

## Consequences

- The prover decides the rewrite's obligations at each use, and traps and diagnostics inside a rewrite report the use's line.
- Matching a call against old heads types its arguments once per head, and once alone.
- Publishing still checks that a head tombstone preserves its item through a forwarding function given the parent's contracts.
