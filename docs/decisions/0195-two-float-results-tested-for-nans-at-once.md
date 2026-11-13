# 0195. Two float results tested for NaNs at once

Status: accepted

## Context

x86-64 code makes a float result canonical where its bits can show ([0172](0172-constant-divisions-overflow-branches-and-nans-made-canonical-where-seen.md)), testing it for a NaN with a comparison and a branch that compete with the float arithmetic for the same execution port. The n-body program of the comparison ([0170](0170-run-times-compared-with-rust-and-cpp.md)) stores each velocity and position it computes, nine tests in each step's pairs and bodies; removing three of them made it run 9 to 15% faster. Each store followed its result's test before the next result was computed, so the tests could not be joined as the lane tests of float vectors are ([0192](0192-vector-copies-of-four-counts-with-one-nan-test-for-two-vectors.md)).

## Decision

- **Selection tests two floats of one size for NaNs at once** where a block tests one and later the other: one unordered comparison of the two finds a NaN in either, and only then is each made canonical.
- **The stores of the first float move past the joined test** when no instruction between reads that float otherwise, has an effect, writes a register the stores read, or loads bytes they may write; a load may write no stored byte when it reads from the same base and index at a displacement whose bytes do not meet the store's.

## Consequences

- The n-body program runs 5% fewer instructions and 5 to 9% faster; its velocities of a body pair their tests, those of two bodies do not, as their bases differ.
- A float the joined test reads stays live until it, a register longer than before.
