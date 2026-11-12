# 0179. Sunk loop instructions, bounded products and trims in layout order

Status: accepted

## Context

After [0177](0177-vector-copies-of-versioned-loops.md), loops of the comparison's programs ([0170](0170-run-times-compared-with-rust-and-cpp.md)) still did work their source does not ask for:

- A byte loop computed its next pointer before reading the current byte, so the pointer lived past its increment, and the increment took another register and a copy on every iteration.
- An unsigned multiplication by a constant checked for overflow, as Collatz's `3 * value + 1`, selected the widening `mul`, fixed to two registers and slower than `lea` or `imul`.
- Trimming made a loop test's parameter stand for the counter's next value, defined by the loop's body, which the test is laid out before. The copy, made in layout order, met that value before copying it and was abandoned with every trim and fold of the function, so the loops of n-body, fannkuch and the interpreter kept all their parameters and tested a range's `inclusive` byte on every iteration.
- Known bits recorded a parameter no argument of which was found yet as knowing nothing, so a loop's test reached only through a back edge laid out after it kept that ignorance around the loop.

## Decision

- **Sinking.** Before hoisting, an instruction of the kinds hoisting moves, in a loop, that its block reads only in its terminator and whose operands are each read past it, sinks to the end of the last block of the run of jumps from its block that only it reaches and that read it only in their terminators, never past a block branching on an overflow flag. An operand then ends where the instruction computes from it, whose result can take its register.
- **Bounded products** on x86-64: an unsigned multiplication by a constant from 2 below 2^31 checked for overflow selects `imul` by the immediate, or `lea` for 3, 5 and 9 at 64 bits, and compares the operand with the largest one whose product the type holds; the overflow flag is that comparison's "above".
- **Trimming keeps a parameter that would stand for a value an instruction laid out in or after the parameter's block defines.**
- **Known bits are found optimistically**: a value is found once its operands are, and a parameter once an argument passed to it is, so a loop's parameters start from the values entering the loop.

## Consequences

- The comparison's programs run fewer instructions: 12% fewer for n-body, 4% for fannkuch and 3% for the hash map and strings; Collatz runs 4% more, `lea` and a comparison in place of `mul`, yet takes less time. N-body runs in 1.58 times Rust's time instead of 1.64, fannkuch 1.38 instead of 1.43 and Collatz 1.43 instead of 1.46.
- A parameter standing for a value its loop's body defines stays a parameter when its block comes first in layout.
- AArch64 code takes the sinking, trimming and known bits; its checked products keep their high-half test.
