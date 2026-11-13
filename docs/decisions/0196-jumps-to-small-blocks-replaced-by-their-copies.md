# 0196. Jumps to small blocks replaced by their copies

Status: accepted

## Context

Emission lays out each loop's blocks together and in chains ([0193](0193-blocks-laid-out-by-loop-and-in-chains.md)), but a loop with two paths back to its test, as the digit and comma paths of the strings program's parsing loop in the comparison of [0170](0170-run-times-compared-with-rust-and-cpp.md), still jumped from one of them to the test, which then jumped back to the body: two taken jumps an iteration. A loop entered by a jump to its test, and a block reached through another holding only a jump, ran a jump too.

## Decision

- **On x86-64, a jump to a block of at most four instructions that only compute registers and flags before its jumps is replaced by a copy of that block**, after the jump's moves of block arguments; a jump ending such a copy is replaced by a copy of its target in turn while the copies hold at most four instructions.
- In a copy, when neither target of a conditional jump follows, the jump taken is the one to the earlier block, the other target being reached by an unconditional jump.

## Consequences

- The strings program runs 1.9% fewer instructions and 3 to 6% faster, the sieve 2.7% fewer, fannkuch 2.8% fewer, binary trees 1.8% fewer; a loop entered by a jump to its test tests its first count where it is entered.
- A block's code is repeated at each jump replaced by its copy, at most four instructions each.
- AArch64 code keeps its jumps.
