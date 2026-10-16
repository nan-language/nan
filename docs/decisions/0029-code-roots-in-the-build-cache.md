# 0029. Code roots in the build cache

Status: accepted

## Context

[Host symbol names](../spec/30-artefacts.md#host-symbol-names) carry each package's code root prefix, and the image of a final build holds the artefact of every package of its final closure, std included. Computing a package's code root checks it whole and encodes its snapshots, NIR stream and requires-set: about 4.5 seconds for std in a debug build, which every `nan test` and `nan build` would pay.

## Decision

`check::root::build` keeps a package state's code root and attestation seals as an entry of the build cache, under the toolchain hash, the package's name and the state of each member of its closure: a workspace package's files but its lock, and a release's code root. It assembles the attestation, which hashes the lock too, for each state it builds.

## Consequences

- The first test image or build after a toolchain change pays std's code roots; later ones read them.
- A resolution's candidate locks and the lock it writes share entries, the lock being outside the key.
- The build cache holds a third kind of entry beside proof results and query results.
