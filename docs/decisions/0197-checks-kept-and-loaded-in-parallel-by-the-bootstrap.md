# 0197. Checks kept, and sources loaded in parallel, by the bootstrap

Status: accepted

## Context

The [latency](../spec/33-toolchain.md#query-engine) requirement binds every implementation, the bootstrap included. A `nan check .` of the NaN toolchain's workspace, 264,632 lines, took the bootstrap 70 to 80 s whether or not anything had changed since its last check: it kept nothing of a check, computed the last-green entries on fresh checkers, which checked the workspace a second time, and parsed and lowered every source file on one thread. The first stage of the NaN toolchain took 4 to 5 s for the same check: its image held no `.nan.tree` section ([0052](0052-the-source-tree-hash-recorded-in-the-image.md)), so it hashed its 30 MB of code roots and parsed every source file without the build cache's syntax ([0054](0054-source-syntax-in-the-build-cache.md)) at each command.

## Decision

- **The bootstrap keeps snapshots of checks as the NaN toolchain does** ([0055](0055-snapshots-of-checks-in-the-build-cache.md)): the same key, restoration and acceptance rules, written by `nan check` and read by every check of a scope. The work of an item, the reads it traces and the owner of each diagnostic follow `Checker::about`, as `Checker.about` does.
- **The bootstrap's `nan check` takes the last-green entries from the checkers it checked with** under the conditions of [0053](0053-order-free-queries-and-greens-from-the-check.md), and finds the members of a function's cycle from the components of the dependencies, as the NaN toolchain does, unless reads are recorded.
- **The bootstrap parses a package's source files and lowers the files of a check's database on up to eight threads**, each file's work independent of the others'.
- **Every `x86_64-host` image holds the `.nan.tree` section**, the bootstrap's included, so that the first stage reads its toolchain hash and uses the syntax the build cache holds.
- **`nanbench measure` times a check of M5 after a function-body edit**, so that the latency is published with the other measurements.

## Consequences

- On 4 threads at 2.1 GHz, a check of the NaN toolchain's workspace after a function-body edit takes the bootstrap 1.7 s instead of 70 s, and the first stage 2.7 s instead of 4.4 s; a check of a 7-line package takes the first stage 0.16 s instead of 1 s.
- Both still parse or decode every source file, lower every item and run the rules relating items at each check, and rewrite the snapshot after an edit: a check's cost grows with its workspace.
- An edit to a body whose names, unit or typed body many items' own work read, such as a helper every test reaches, checks all of those again: editing such a helper of the toolchain's workspace costs the bootstrap 18 s and the first stage 8 s.
- The bootstrap's checks use the machine's threads; its outputs do not depend on their number.
