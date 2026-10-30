# 0070. The PPA package

Status: accepted

## Context

Releases publish the self-built toolchain for Linux on x86_64 and AArch64 ([0063](0063-releases-from-tags.md), [0067](0067-the-nan-toolchain-on-aarch64.md)), packaged for Arch Linux and Fedora ([0068](0068-the-aur-package.md), [0069](0069-the-copr-package.md)). Ubuntu users install software through apt, and a Launchpad PPA is where packages outside Ubuntu's archive live. Launchpad accepts only signed source packages, one upload per Ubuntu series, each with a version of its own, and builds them without network access. The images need glibc 2.34, which Ubuntu ships from 22.04.

## Decision

- **A binary package, `nan`, in the PPA `ppa:nanlang/nan`**, that installs the released image of the machine's architecture as `/usr/bin/nan`, unstripped, with its libc dependency derived from the image and the cross compiler of the other host target suggested.
- **The images travel in the source package**: its original tarball holds both, written deterministically, and `debian/rules` installs the one of the host architecture.
- **One upload per supported series from 22.04 on**, read from Launchpad's list of Ubuntu series when the release runs, each versioned `<version>-1~<series>1`.
- **Published by the release workflow.** Once the release is published, a job fills the templates of `.github/ppa/debian` with the version, the series, the date and the identity of the key in the `PPA_GPG_PRIVATE_KEY` secret, appends the LLVM exception to the copyright file, signs each source package with that key and uploads them with `dput`.

Rejected alternative:

- **A source package that downloads the images while building:** Launchpad's builders have no network access.

## Consequences

- The `nanlang` team of Launchpad must own the PPA, with amd64 and arm64 builds enabled; the key of `PPA_GPG_PRIVATE_KEY` has no passphrase and is registered to a member of the team, who receives Launchpad's acceptance and build mails.
- The workflow ends at the upload; Launchpad builds afterwards, and a failed build shows only in the PPA and in those mails.
- A rerun's uploads are rejected, since their versions already exist.
- A new Ubuntu series gets packages from the next release.
