# 0192. Vector copies of four counts with one NaN test for two vectors

Status: accepted

## Context

The matrix product of the comparison ([0170](0170-run-times-compared-with-rust-and-cpp.md)) spends its time in the vector copy of its inner loop ([0177](0177-vector-copies-of-versioned-loops.md)): 13 instructions for two floats, of which five test the sum's lanes for NaNs before it is stored and three count and branch, where Rust's loop takes 12 for four. The loop ran as many instructions a cycle as the processor issues, so its time followed its instruction count. A float vector operation's NaN test also ran on a temporary that was then copied into the value's register, between the test and any later one.

## Decision

- **A versioned loop's vector copy takes four counts an iteration, two to a vector, while four are left**, and stores once it has computed the floats of all four; the scalar copy takes the up to three counts left.
- **Selection tests the lanes of two float vectors for NaNs at once** when no instruction between the two tests reads the first vector: one unordered comparison of the two vectors finds a NaN in either, and only then is each made canonical.
- A float vector operation's NaN test runs on the value's own register, after the operation's result is copied into it.

## Consequences

- The matrix product's inner loop runs 4.5 instructions per element; the program runs 20% fewer instructions and 8 to 13% faster.
- A loop with fewer than four counts left on reaching the vector copy runs them all in the scalar copy, where two counts sufficed.
