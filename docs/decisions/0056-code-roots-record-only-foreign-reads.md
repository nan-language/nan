# 0056. Code roots record only the reads of other packages

Status: accepted

## Context

A package's code root holds its requires-set, whose entries are the foreign items its items' checking reads ([requires-sets](../spec/29-identity-versioning.md#requires-sets)). Both toolchains find them by checking the package again with every query's reads recorded, by subject and by line, as context packs need them. Under recording the checker takes no shortcut that skips or reorders a query ([0053](0053-order-free-queries-and-greens-from-the-check.md)): whether a function lies in a cycle, for instance, is a depth-first search of what it reaches, reading each function it visits. A final build therefore checked its program's closure three times: for the diagnostics, for the lowering, and recorded, for the code root. In the NaN toolchain, building its own workspace took 1.02e12 instructions and peaked at 2.7 GB. The recorded check took more than half of the instructions, and its hashed maps of reads took 1.4 GB.

## Decision

- **A code root records only the reads of other packages.** The checker keeps one flag per item, set when an item of the package reads an item of another. The requires-set takes the flagged items. A query whose subject is an item of the package computes as under full recording. A query about any other item, or about none, records nothing, so it computes as while nothing is recorded. The flags are therefore exactly the foreign items that full recording would list. Context packs keep full recording.
- **Searches that can read nothing new are skipped.** The flags are one set for the whole package, so a search adds nothing once every function it can reach has been read. The checker keeps the functions whose reach has been read through, along body dependencies and along unit dependencies. For such a function, it answers whether it lies in a cycle from the components of the dependency graph instead of searching. A search does not descend into such a function when it lies outside the searched function's cycle.
- **A final build checks once.** When the program's closure checks only the program package, under the one target that package declares, the check that gives the diagnostics records the code root's reads. That checker then gives the diagnostics, the code root and the lowering.

## Consequences

- The NaN toolchain builds its workspace in about 0.4e12 instructions and 1.15 GB. Its code roots stay the bootstrap's, as stage comparison checks.
- A final build of a closure that checks several workspace packages, or of a package declaring several targets, still checks three times.
- A query the requires-set depends on must read through `Db.note` under its own subject; a shortcut taken under any other subject must give the result the full computation gives.
