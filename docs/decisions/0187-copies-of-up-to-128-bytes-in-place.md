# 0187. Copies of up to 128 bytes in place

Status: accepted

## Context

The fannkuch program of the comparison ([0170](0170-run-times-compared-with-rust-and-cpp.md)) passes its 128-byte permutation by value to the function counting its flips, which copies it into the array it reverses: two copies per call. Lowering wrote a copy of up to 64 bytes as loads and stores and called the copy routine, which calls the C library's `memmove`, for a longer one. Each of those calls ran about thirty instructions, two calls and the branches choosing `memmove`'s strategy, and the program spent a tenth of its time in them.

## Decision

- **A copy of a size known to lowering of up to 128 bytes is written in place**, as loads and stores of the widest of 8, 4, 2 and 1 bytes its alignment allows; a longer one calls the copy routine.

## Consequences

- Fannkuch runs 2% fewer instructions and in 1.25 times Rust's time instead of 1.4.
- Each copy of 65 to 128 bytes takes up to 32 instructions in place of a call.
