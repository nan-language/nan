# 0177. Vector copies of versioned loops

Status: accepted

## Context

After [0176](0176-versioned-loops-and-rotated-loop-tests.md), the matrix product's inner loop ran 14 instructions per element, one float at a time, where Rust's runs two at once. Its checks were gone, and nothing but the order of its stores and loads stood in the way of taking two counts per iteration. Selection had no instructions for 64-bit float lanes. Canonicalizing a vector's NaN lanes also rebuilt the canonical NaN on every result, even where no instruction shows the bits. Selection counted the reads of dead instructions, so a flag only a dropped check had tested still counted as read. And a float operation whose first operand outlives it copied that operand before computing in place.

## Decision

- **Versioning gives a loop a vector copy on x86-64** when its blocks run one path, past the checks its copy drops. Each value the copy reads must be steady, the counter, a sum of the counter and a steady value, such a sum scaled by a constant, a steady base plus such a scaled sum, or two 64-bit floats. A pair of floats comes from a load at an address advancing 8 bytes a count, or from addition, subtraction, multiplication or division of such floats and steady ones. Its stores write such floats at such addresses.
- **The latch's test, once the checks hold, branches to the vector copy when no store of the counts left reaches the bytes another access of the loop reads or writes**, and to the scalar copy otherwise. The vector copy runs while two counts are left: each address is rebased once to its count-0 address before the loop and then advanced by the counter, and each steady operand is splat once. It leaves the last count to the scalar copy.
- `f64x2` joins the IR's types; selection uses `addpd`, `subpd`, `mulpd` and `divpd`. Float lanes are made canonical only where an instruction can show their bits, by a sequence that skips the canonical NaN when no lane compares unordered.
- Selection skips the instructions whose results no live instruction reads, counting only live reads. A float or vector addition or multiplication takes its operands in the other order when the first outlives it and the second ends there.

## Consequences

- The matrix product's inner loop runs 6.5 instructions per element and the program 1.16 times Rust's time, from 2.1.
- A loop's vector copy adds a header, a body and the test of the accesses' bytes; one store and one load add one comparison of two ranges.
- Loops on other element types, or reading through addresses of other forms, stay scalar. AArch64 code takes no vector copy.
