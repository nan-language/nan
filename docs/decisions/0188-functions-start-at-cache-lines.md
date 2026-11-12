# 0188. Functions start at cache lines

Status: accepted

## Context

The run times of the comparison's programs ([0170](0170-run-times-compared-with-rust-and-cpp.md)) moved by up to 8% between builds of the NaN toolchain that ran the same instructions, as their code moved: the doubly recursive Fibonacci function, under 64 bytes of hot code, took one cache line where it started at one and two where it started halfway through one, and the strings program's digit loop took one more as well. x86-64 functions started at 16-byte boundaries, so where a small function's code fell within its lines changed with every function compiled before it.

## Decision

- **An x86-64 function starts at a 64-byte boundary**, so that its code spans the fewest cache lines it can.
- AArch64 functions keep starting at 16-byte boundaries.

## Consequences

- Fibonacci runs 4% faster and the strings program 7%; the other programs change by less than run-to-run noise.
- Each function's padding grows from 8 to 32 bytes on average.
