# 0026. Releases checked from stored sources

Status: accepted

## Context

The [checker](../spec/29-identity-versioning.md#lock) reads a locked release that is not the current state of its path source from its artefact in the local store. The artefact's code root holds the release's NIR, API snapshots, requires-set and ledger, but not every fact checking a consumer reads: signatures with their generic bounds, structural properties, the bodies of transparent functions, const values, impl lines and registry contributions. Reading those from the code root would take a second front end, turning NIR and snapshot texts back into the checker's facts.

## Decision

A store entry holds, beside the artefact and publish summary, the release's source tree: the source and machine files its attestation hashes. The bootstrap reads a release used as a dependency by loading those files as a quiet package of the consumer's closure: checked like a workspace package, none of its diagnostics reported. Workspace packages whose closures assign the same members share one checker per target.

Alternatives rejected:

- **Facts decoded from the code root:** a second reader of every fact kind, which each change to the checker would have to follow.

## Consequences

- A release and the path package it was published from give consumers identical facts, from one checker.
- The local store grows by each release's sources.
- Checking a consumer re-checks the releases of its closure.
- The artefact keeps identifying the release by its code root and carrying its attestation.
