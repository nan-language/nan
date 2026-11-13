# 0193. Blocks laid out by loop and in chains

Status: accepted

## Context

Emission laid out a function's blocks in the order lowering created them, each loop's header that tests whether the loop goes on moved after the latch jumping back to it ([0176](0176-versioned-loops-and-rotated-loop-tests.md)), and the cold blocks last. A block jumping to a block created after others jumped over them: the step of a range loop jumped over the blocks of an inclusive range's last count to the loop's body, and the inner loop of the n-body program in the comparison of [0170](0170-run-times-compared-with-rust-and-cpp.md) jumped over its exit on every iteration. A block leaving a loop that lowering created among the loop's blocks, as a hash map's probe loop leaves on finding its key, lay between the loop's first block and its jump back, which register allocation weighs as the loop's own, so its values took registers from the loop's.

## Decision

- **The blocks of each loop are laid out together**, where its first block lies, in the order lowering created them, with those of each loop it holds together alike; a block outside the loop that lowering created among them follows them.
- **Blocks then form chains within one innermost loop**: a block that the jump ending one block alone lands in follows that block, and a block follows the one before it when that one branches to it. The chains are laid out in the order of their first blocks, the cold blocks last.

## Consequences

- The matrix product runs 7.4% fewer instructions, n-body 5.5%, the sieve 2.6%, the hash map program 2.3%, fannkuch 1.6% and binary trees 1.3%; n-body and the interpreter run about 4% faster, the other programs change by less than run-to-run noise.
- AArch64 code takes the same layout.
