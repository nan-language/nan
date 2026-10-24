# 0047. Diverse double-compiling of the NaN toolchain

Status: accepted

## Context

The bootstrap checks the first toolchain that NaN builds itself ([0002](0002-bootstrap-stack-and-lifetime.md)) by diverse double-compiling and [stage comparison](../spec/36-builds.md#stage-comparison). Diverse double-compiling compiles the toolchain's source with a compiler from another code base, compiles the source again with the result, and compares that with what the toolchain built by itself gives: when the bytes are equal, the self-built toolchain corresponds to its source. Equal bytes need every output of a final build to depend on the build inputs alone. The toolchain hash breaks this: each artefact's attestation file records the hash of the toolchain that wrote it, and the NaN toolchain took the hash of its own executable, which differs between a toolchain built by the bootstrap and one built by itself.

## Decision

- **The check.** The self-check harness builds the toolchain workspace three times: with the bootstrap (the first stage), with the first stage (the second), and with the second (the third). The second and third images must be equal byte for byte, and the bootstrap's `nan build --compare-with` against the second must report no error. Each stage builds with a user directory of its own, so no stage reuses another's proofs.
- **The NaN toolchain's toolchain hash comes from its code roots.** Its source tree hash is the hash of the list of the code roots its image's `.nan` section holds. Code roots are reproducible, so every final build of one source tree, whichever toolchain made it, carries one toolchain hash.
- **One root file for all stages.** Every stage is built from the harness's root file, which gives the toolchain the harness's user directory; the toolchain reads no environment of its own.

Rejected alternatives:

- **Comparing the images without their `.nan` sections:** the sections are part of what the toolchain built; leaving them out would accept a toolchain whose artefacts differ.
- **A source tree hash generated into the toolchain's sources:** every change to the toolchain would also regenerate a file, and a stale one would let two toolchains share cache entries.
- **Building the stages in the user's directory:** the second stage would reuse the first stage's proofs and never run its own prover.

## Consequences

- The double-compiling check takes three final builds of the toolchain, the last by a toolchain the NaN backend compiled; it is the slowest test of the suite, and each self-build peaks at about 17 GB of memory.
- A toolchain built by the bootstrap and one built by itself share cache entries, which the check shows behave alike.
- Repeating the check needs the frozen bootstrap, Linux and the `cc` of the machine.
