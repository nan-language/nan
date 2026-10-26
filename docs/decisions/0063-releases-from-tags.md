# 0063. Releases from tags

Status: accepted

## Context

The toolchain needs versions people can name, order and cite, while NaN identifies each release of a package by the hash of its code root and computes its class, so no version number lives in the sources. Releases are rare at 0.x and decided by the maintainer; commits land on `main` directly with free-form subjects, which rules out tools that need a file or a commit prefix per change.

## Decision

- **A version is a git tag `vX.Y.Z` on a commit of `main`**, pushed by the maintainer; the release workflow runs on it.
- **The release asset is the self-built toolchain**: the bootstrap builds the toolchain, which builds it, which builds it again; the workflow publishes the second image only when the third equals it byte for byte and the bootstrap's stage comparison accepts it. Each stage starts from an empty user directory.
- **The notes identify the image** by its SHA-256 and the source tree hash its `.nan.tree` section records, and list the subjects of the commits since the previous tag.

Rejected alternatives:

- **Changesets, release-please or release-plz:** a file per change or Conventional Commit prefixes, and a standing release pull request whose every update reruns the test suite.
- **A manually dispatched workflow:** the version would live in a form field instead of the history.

## Consequences

- No version is recorded in the sources, and `nan` does not print one.
- A tag whose workflow fails stays without a release until it is deleted and pushed again.
- The image links the C library of the runner's system.
