# 0190. Bounds of checked products moved out of loops

Status: accepted

## Context

x86-64 checks an unsigned product by a constant by comparing its operand with the largest one whose product the type holds ([0179](0179-sunk-loop-instructions-bounded-products-and-trims-in-layout-order.md)). At 64 bits that bound needs a 10-byte `movabs`, which selection wrote before each check, inside the loop holding it: one of the 16 instructions of each step of the Collatz program in the comparison of [0170](0170-run-times-compared-with-rust-and-cpp.md), which bounds `3 * value`, and one of the strings program's digit loop. Hoisting moves constants out of loops, but the bound was no instruction of the function.

A branch on a comparison's flag inverted by a `bxor` with 1, as lowering writes for a negated condition, set the flag, inverted it and tested it, where one on an overflow flag inverted that way branched on the processor's flags.

## Decision

- **On x86-64, a function whose loops check unsigned 64-bit products by constants is copied before hoisting with each such check split** into the product and a comparison of its operand with the bound, a constant that hoisting then moves out of the loops as any other.
- **A branch or select on a flag inverted by a `bxor` with 1 tests the comparison deciding that flag**, with the inverse condition.
- **A 64-bit product by 3, 5 or 9 is a `lea`**, checked or not.

## Consequences

- Collatz runs 6% fewer instructions, in 1.16 times Rust's time instead of 1.22; the strings program runs 1.6% fewer, the binary trees program 1.5% fewer through the allocator's negated conditions.
- A function whose loops check such products is copied once more, and the bound holds a register through its loops.
- AArch64 code keeps checking products by their high half, and takes the inverted comparisons.
