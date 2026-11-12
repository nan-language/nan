# 0189. Shifts of no elements skipped

Status: accepted

## Context

A hash map stores its entries in insertion order, so inserting a new key opens the gap for its entry after the last one. Opening a gap in a vector's elements called the copy routine to move the elements after it, which there are none of, and so the C library's `memmove` for zero bytes: about thirty instructions and two calls per insertion, a twentieth of the hash map program's instructions in the comparison of [0170](0170-run-times-compared-with-rust-and-cpp.md). Removing a vector's last element closed a gap the same way.

## Decision

- **Opening or closing a gap in a vector's elements calls the copy routine only when elements move.**

## Consequences

- The hash map program runs 4.5% fewer instructions.
- A shift that moves elements tests their count first.
