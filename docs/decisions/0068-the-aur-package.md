# 0068. The AUR package

Status: accepted

## Context

Releases publish the self-built toolchain for Linux on x86_64 and AArch64 ([0063](0063-releases-from-tags.md), [0067](0067-the-nan-toolchain-on-aarch64.md)), installed by a script into the user's home. Arch Linux users install software through pacman, which tracks files and optional dependencies, and the AUR is where packages outside the official repositories live. A package that builds the toolchain from source would need the Rust bootstrap and three builds of the toolchain on the user's machine.

## Decision

- **A binary package, `nan-bin`**, that installs the released image of the machine's architecture as `/usr/bin/nan`, checked against the SHA-256 of the image the release workflow built. It is never stripped, so that it stays the released image byte for byte.
- **Published by the release workflow.** Once the release is published, a job in an Arch Linux container fills the template `.github/aur/PKGBUILD` with the version and both images' SHA-256, builds the package from the published assets, writes `.SRCINFO` with `makepkg` and pushes both to the package's AUR repository with the SSH key of the `AUR_SSH_PRIVATE_KEY` secret, trusting only the AUR's pinned host key.

Rejected alternative:

- **A third-party action deploying to the AUR:** it would receive the AUR key, for a few lines of shell.

## Consequences

- The AUR account that maintains `nan-bin` must hold the public key of `AUR_SSH_PRIVATE_KEY`.
- A tag gets its AUR update only after its release is published; a failed push can be rerun alone, and a rerun that changes nothing pushes nothing.
