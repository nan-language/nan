# 0154. Path dependencies read from their working source

Status: accepted

## Context

A lock pinned every dependency, path sources included: once a path package changed, every command but `nan dep sync` read the release the lock named from the local store. In a workspace holding a library and its consumer, `nan check .` reported the library's working source while `nan build` of the consumer built the earlier release without a word, and failed on neither. One directory then stood for two packages, so a diagnostic or trap report of that image quoted lines of the working tree that were not the code built. Each edit of a library also needed `nan dep sync` of every consumer before a build or test showed it.

## Decision

- **Every command reads a package reached through a path source whose directory holds it as that source's current state.** The lock still records the path release that resolution accepted; a current state whose code root differs from it is incompleteness (`dep.sync-pending`), naming `nan dep sync`, so checks, tests and builds go on and the release gate rejects it.
- **Builds and test runs refuse a dependency that does not check.** A final build already checks its whole closure; a test image is built only when the other packages it reads from source check without error.
- **`nan dep sync` reports the package's own errors** when it locks the closure while ignoring them, instead of succeeding.

Alternatives rejected:

- **Keep building the locked release and say so:** the image would still differ from the source the agent edits, and each library edit would still need a sync of every consumer before a run showed it.
- **`dep.sync-pending` as an error:** each body edit of a library would fail every consumer's check until each is synced, though nothing it guards matters before the release gate.

## Consequences

- Checks, tests and images reflect the working tree; a consumer's lock lags its path dependencies until `nan dep sync`, which runs the consumer's reliance and affected tests, and the release gate requires that sync.
- What resolution accepts for a path release holds at the gate, not at each check between syncs.
- `name.newer-release` covers only store names: a path source is never read at an older state.
- Checking a consumer computes the code root of each changed path dependency's current state, cached per state in the build cache.
