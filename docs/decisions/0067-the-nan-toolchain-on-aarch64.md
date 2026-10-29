# 0067. The NaN toolchain on AArch64

Status: accepted

## Context

Language 1 has AArch64 targets ([0066](0066-the-aarch64-targets.md)), but the NaN toolchain was built, double-compiled ([0047](0047-diverse-double-compiling-of-the-nan-toolchain.md)) and released ([0063](0063-releases-from-tags.md)) for x86_64 only. Its manifest declares `x86_64-host`, and a program declares exactly one concrete target, so it cannot hold target variants. Its platform code wrote x86_64's facts: the layout of `struct stat`, the values of `O_DIRECTORY` and `O_NOFOLLOW`, which mean `O_DIRECT` and `O_LARGEFILE` on AArch64, so that removing a tree there would follow a symbolic link into the directory it names, and a running machine always x86_64. A host image runs only on its own architecture, and `nan verify` runs a package's tests, so an AArch64 toolchain can be built by itself, verified and double-compiled only on an AArch64 machine.

## Decision

- **One workspace for both architectures.** The toolchain's manifest declares `x86_64-host`; a build for AArch64 declares `aarch64-host` in its place in a copy, as the release workflow and the self-check harness do for the machine they run on.
- **Platform code that holds on both.** The toolchain reads metadata with `statx`, whose layout every architecture shares, and asks `uname` for the machine's architecture, which gives each host target's linker, whether a host image can run, and the open flags whose values differ.
- **Double-compiling on each architecture.** The self-check harness builds, verifies and double-compiles the toolchain for the host target of the machine it runs on. CI runs its toolchain test on an AArch64 runner beside the full suite on x86_64.
- **A release per architecture.** The release workflow builds the toolchain three times on an x86_64 runner and on an AArch64 runner, and publishes both images, `nan-<tag>-x86_64-linux` and `nan-<tag>-aarch64-linux`, each identified by its SHA-256 and source tree hash, once both pass. The install script picks the machine's.

Rejected alternatives:

- **A library of the toolchain workspace holding the differing facts in target variants:** the checker reads a path dependency from source only when the local store holds its locked release, so every fresh user directory, CI's and each harness's included, would need `nan dep sync` before checking the toolchain.
- **Building the AArch64 toolchain on x86_64:** its later stages could not run there, as user-mode emulation implements no io_uring, and a stage linked by the cross compiler differs from one linked by an AArch64 machine's `cc`.

## Consequences

- Each image holds both architectures' open flags and calls `uname` where it needs the architecture.
- On AArch64, CI runs only the toolchain test; the rest of the suite runs on x86_64, where AArch64 host programs are built but never run.
- A tag gets a release only when both architectures' builds pass.
- The AArch64 image links the C library of the AArch64 runner's system.
