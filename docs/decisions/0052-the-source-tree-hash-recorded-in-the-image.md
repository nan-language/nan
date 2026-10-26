# 0052. The source tree hash recorded in the image

Status: accepted

## Context

The NaN toolchain's source tree hash is the hash of the list of the code roots its image's `.nan` section holds ([0047](0047-diverse-double-compiling-of-the-nan-toolchain.md)). Every command that reads or writes the build cache needs it, since the toolchain hash keys every cache entry. Computing it reads the whole image, 55 MB, and hashes the 30 MB of code roots its section holds: 1 s of the 1.8 s the self-built toolchain took for a warm `nan check` of a one-package workspace, which the bootstrap runs in 0.11 s. The hash depends only on what the final build that wrote the image placed in its `.nan` section.

## Decision

- **The final build records the hash.** Each `x86_64-host` image the NaN toolchain writes holds, beside `.nan`, a section `.nan.tree` holding the hash of the encoding of the list of the code roots of its `.nan` artefacts, in order. The toolchain reads the section headers and that section of its running image, a few kilobytes, instead of the whole image.
- **The definition is unchanged.** The recorded hash is the one the toolchain computed before; a toolchain whose image holds no `.nan.tree` section computes it from `.nan` as before.
- **The bootstrap's images hold no such section.** The bootstrap is frozen, so the toolchain it builds, the first stage, computes the hash from `.nan`; every self-built toolchain reads it.

Rejected alternatives:

- **A faster hash computation:** hashing 30 MB takes longer than the whole of the bootstrap's command.
- **Caching the hash in the user directory:** a cache entry's key holds the toolchain hash, and an image has no identity cheaper than its contents.
- **Changing the bootstrap's image writer:** the bootstrap is frozen.

## Consequences

- A self-built toolchain finds its toolchain hash in microseconds; the first stage still pays a second per command.
- Every `x86_64-host` final build hashes the code roots of its closure once more, for the section.
- Both ways of finding the hash stay in use: the self-check runs the first stage, users the self-built toolchain.
- Stage 2 equals stage 3 as before, since the section is a function of the code roots, and stage comparison reads only `.nan`.
