# 0176. Versioned loops and rotated loop tests

Status: accepted

## Context

After the rewrites of [0175](0175-live-segments-and-loop-rewrites-before-selection.md), the inner loop of the matrix product still ran 25 instructions per element: two overflow checks of the sums indexing it, a check of a value set outside the loop, two bounds checks, and the loads of both lengths and both data pointers. Each check holds for every count once it holds for the last, since the indices grow with the counter, yet each iteration tested it again; the lengths could not be hoisted, as a store of the loop might write them. Every loop also ran a jump back to a test at its top, and a checked addition such as `count += 1` copied its operand to another register on every iteration, because the cold block reporting an overflow reads the operand after the result has replaced it.

## Decision

- **Versioning** runs after hoisting. An innermost loop qualifies when its header tests a 64-bit parameter below a limit set outside the loop, its latch passes that parameter plus one, it leaves only from its header or for cold blocks that trap, no block outside it reads its values, and each of its branches to such a block is a check that holds for every count left once it holds for the last: a condition set outside the loop on every path to the latch, no overflow of the counter plus a value set outside the loop, or that sum, or the counter, below a limit set outside the loop or loaded from an address set outside it.
- **The test runs at the end of each iteration of the original loop**, which goes on in a copy without the checks from the next count when every check holds for the last count and no store of the counts left reaches the bytes of a load the copy pins: the copy reads the value that iteration loaded instead of loading it again. A store's bytes are bounded when its address is set outside the loop or adds a constant multiple of such a sum to a value that stays the same. Testing after an iteration rather than before the loop runs no load the loop would not have run.
- **Each loop's header that tests whether the loop goes on is laid out after the block jumping back to it**, so that block falls through into the test, which branches back to the body.
- **A cold block that traps, reached only when a 32- or 64-bit addition or subtraction overflows, rebuilds the operand it reads from the result**, the first operand or the second when the first is a constant, so the operand need not stay live past the operation.
- Selection reads a loaded integer as zero-extended, as the load writes it, selects an overflowing operation whose flag nothing reads as the plain one, and writes a 64-bit constant no 32-bit immediate gives, or a float other than zero, into its register once where it is defined.

## Consequences

- The matrix product's inner loop runs 17 instructions per element instead of 25. The comparison's programs run fewer instructions: 39% fewer for the matrix product, 24% for Collatz steps, 15% for the sieve and 8% for the hash map.
- A versioned loop's first iteration, and every one after a failed test, runs the original code; a loop of few iterations pays the test without gaining from it.
- Loops counting otherwise, calling, storing through addresses of other forms, or leaving other than through their header keep their checks.
- The versioned matrix loop stays scalar: selection has no vector instructions on 64-bit float lanes.
