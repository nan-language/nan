# 0213. Approvals over every target of a dependency

Status: accepted

## Context

A package's authority summary covers its items' digests under every target it declares, and a final-link root's closure summary holds each entry of each dependency's. The checker of a root holds only the variants its own target selects, so a lock approved only the escapes of those variants: a copy of a root declaring another target needed `nan dep approve` before it checked, as the NaN toolchain's macOS, Windows and nanos copies did ([0205](0205-the-nan-toolchain-on-windows.md)), and the approvals of every other target lay outside the committed lock.

## Decision

The closure summary takes each dependency's authority summary under the root's checker for the root's target and, for each other target the dependency declares, under a checker of the part of the root's closure the dependency reaches, built over that target. `nan dep sync`, `nan dep approve` and the approval rules of `nan check`, `nan test`, `nan build` and `nan run` all read it.

## Consequences

- A root's lock approves the escapes of every variant of its dependencies, whichever target the root declares, so a copy declaring another target checks without `nan dep approve`.
- Each check of a root builds one more database per other target a dependency declares, over the part of the closure that dependency reaches.
