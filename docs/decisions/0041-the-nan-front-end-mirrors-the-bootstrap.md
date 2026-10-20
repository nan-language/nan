# 0041. The NaN front end mirrors the bootstrap

Status: accepted

## Context

M5 requires the NaN front end to print, byte for byte, what the bootstrap's `nan check` prints for every workspace and scope, and to give NIR whose canonical encoding is identical ([NaN front end](../spec/38-self-hosting.md#nan-front-end)). Output that equal depends on more than the rules: the order of reports and of fixes, the order of queries, which context packs print as the order of first read, interning and numbering orders, budget charges, and the exact texts. A front end designed afresh would differ from the bootstrap in many of these details, and each difference would have to be found by search across two unrelated designs.

The front end implements `nan check` alone. The comparison needs more: the harnesses' home directory and local store instead of the user's, and a way to read the NIR the front end gives.

## Decision

- `toolchain/nan` ports the bootstrap's front end module by module and function by function: syntax, workspace, check with elaboration, the interpreter and the prover, fixes and output, each with the algorithm, order of work and texts of its Rust original. Where NaN forbids the Rust form, the port restructures locally and keeps the observable order. A bug found in either is fixed in both.
- The std sources and the grammar text the front end carries are NaN files generated from `std/` and the grammar chapter; a milestone test fails when they are not current and rewrites them under `NAN_BLESS`.
- The self-check harness builds the front end with a root file of its own in place of `nan/nan.nan`. That root reads the home directory, the local store and a NIR request from the environment, and dumps NIR through `Session.nir_streams`, the front end's one item that serves only the comparison. The front end itself reads no environment.
- The harness runs the front end on copies of the workspace and local store the bootstrap has just used, under an address-space cap and a time limit, so that neither run's cache writes reach the other and a runaway run cannot exhaust the machine.

## Consequences

- A difference between the two outputs points at one pair of functions, and a reviewer reads a port against its original side by side.
- The front end inherits the bootstrap's structure, including what a design for NaN alone would do otherwise; restructuring it is work for after the freeze, when the bootstrap no longer moves.
- Every change to the bootstrap's front end needs its port in `toolchain/nan`, or the self-check fails.
