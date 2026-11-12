# 0181. Scalar map keys folded in place

Status: accepted

## Context

The hash map program of [0170](0170-run-times-compared-with-rust-and-cpp.md) spent a third of its instructions hashing its `u64` keys, two or three times per insertion. Each key's `hash`, the scalar conformance the toolchain supplies, stored the key and called `mem::KeyHasher.write`, which multiplied the state by FNV's prime once per byte: eight dependent multiplications and two calls per key. A map places its keys by a hash that only its seed affects, never its iteration order ([maps and sets](../spec/12-memory.md#maps-and-sets)), so how a key's fragment is computed shows only in the layout.

## Decision

- **A scalar key's fragment takes no call**: the key's bits are flipped into the start state, the FNV basis flipped by the map's seed, and the result multiplied by the 64-bit golden ratio, the high half of the product folded into the low half, then folded to 32 bits as every fragment is.
- Other keys still write to `mem::KeyHasher` through their `hash`.

## Consequences

- The hash map program runs 32% fewer instructions, and 1.66 times Rust's time instead of 2.29.
- A map of scalar keys lays them out differently than before; nothing a program reads changes.
