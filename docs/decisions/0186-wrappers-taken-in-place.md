# 0186. Wrappers taken in place

Status: accepted

## Context

The hash map program of the comparison ([0170](0170-run-times-compared-with-rust-and-cpp.md)) called its generator step, `fn next(state: u64) -> u64 = state.wrapping_mul(K).wrapping_add(C)`, twice per iteration: a call, a frame and a return around two instructions. Its body qualified as a leaf once its two calls took their bodies in place ([0051](0051-leaves-in-place-and-slots-in-values.md)), but a function offered its body as lowering built it, calls included. And a function waiting for its callees compiled without offering its body at all, while the functions waiting for it compiled as soon as it was defined, before it was compiled.

## Decision

- **A function offers its body with the bodies its own calls take in place**, so that a wrapper around leaves qualifies as one when the result still does.
- **A defined function waits for the colocated functions it calls to compile**, not only to be defined, and the functions waiting for it compile once it has. A function that waited offers its body as one compiling at once does.

## Consequences

- The hash map program runs 1.3% fewer instructions; the comparison's other programs start with fewer calls.
- Mutually recursive functions wait for each other until the end of the build, or until the waiting functions reach their bound.
