# 0061. Compiled code in the build cache

Status: accepted

## Context

A final build or a test image of a one-package workspace lowers and compiles its whole closure, std included, every time. In the self-built toolchain, code generation took about 40% of a warm `nan build` once it threads edges and leaves calls out ([0060](0060-threading-left-out-calls-and-pool-locks.md)), and such commands ran slower than the bootstrap's. Most rebuilds compile functions whose IR is unchanged.

## Decision

- **The build cache keeps the machine code of each lowering.** An entry holds, for every function a final build or a test image compiled, its code, relocations, frame size, location runs and whether its calls can be left out, under the key of everything compiling it read: the target's code generation flags, the function's IR text, and for each colocated callee the hash of the body its calls take in place, if any, and whether its calls can be left out. Its own key holds its kind, the toolchain hash and the hash of those keys.
- **A record per program or tested package of a workspace under a target names the latest entry.** A lowering reads it, takes each function's code held under the same key instead of compiling it, and when it compiled any anew writes a new entry, replaces the record and drops the entry the record named before.
- **One entry per lowering**, so a build reads one file of code, however many functions it holds.

Rejected alternatives:

- **An entry per function:** thousands of files to read and check per build.
- **Keys independent of the numbering of functions:** IR names callees by numbers that shift when the program changes; keeping code reusable across such changes needs renumbering code and relocations, beyond what rebuilding an unchanged closure needs.

## Consequences

- A rebuild of an unchanged closure compiles nothing; the self-built toolchain's warm `nan build` and `nan test` on a one-package workspace run faster than the bootstrap's. A changed program recompiles the functions whose numbering or IR moved.
- The cache holds a code entry per program and tested package, as large as the code it holds.
- Images are byte for byte the same with, without and after deleting the cache, as the key holds every input of compiling.
