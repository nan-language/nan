# 0069. The Copr package

Status: accepted

## Context

Releases publish the self-built toolchain for Linux on x86_64 and AArch64 ([0063](0063-releases-from-tags.md), [0067](0067-the-nan-toolchain-on-aarch64.md)), and the AUR carries it for Arch Linux ([0068](0068-the-aur-package.md)). Fedora users install software through dnf, and Copr hosts RPM repositories outside Fedora's own, building each package in its buildroots for every chroot a project enables. A package that builds the toolchain from source would need the Rust bootstrap and three builds of the toolchain in each buildroot.

## Decision

- **A binary package, `nan`, in the group project `@nanlang/nan`**, that installs the released image of the machine's architecture as `/usr/bin/nan`. It is never stripped, so that it stays the released image byte for byte, and RPM derives its glibc requirements from the image.
- **Published by the release workflow.** Once the release is published, a job in a Fedora container fills the template `.github/copr/nan.spec` with the version and the date, writes a source package holding both images the workflow built, and submits it with `copr-cli`, authenticated by the configuration in the `COPR_CONFIG` secret, waiting for every chroot's build.
- **The sources travel in the source package**, so Copr's buildroots need no network access and package exactly the images the workflow built.

Rejected alternative:

- **A Copr webhook building from the tag:** it fires when the tag is pushed, before the release it would package exists.

## Consequences

- The `nanlang` group of Fedora's account system must exist and own the project, which enables the Fedora chroots on x86_64 and AArch64; the token of `COPR_CONFIG` belongs to one of its members and expires every 180 days.
- A tag gets its Copr build only after its release is published; a rerun submits the same version again.
