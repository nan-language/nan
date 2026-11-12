# 0183. Speculated quick products and arm checks branching alone

Status: accepted

## Context

Collatz's step in the comparison of [0170](0170-run-times-compared-with-rust-and-cpp.md) branches on the parity of its value, a branch the processor often mispredicts, where Rust's step selects its next value. Speculation ([0175](0175-live-segments-and-loop-rewrites-before-selection.md)) refused the step: its arms multiply by 3 and divide by 2, and speculated arms held no multiplication or division, for their latency, though selection makes these a `lea` and a shift. Speculating it anyway guarded each of the odd arm's two overflow checks by the arm's condition, both set in registers and combined: 27 instructions per step, slower than the branch. And selection set an overflow flag in a register rather than branch on it when a dead instruction lay between the two.

## Decision

- **A speculated arm may divide or take a remainder by a power of two, and on x86-64 multiply a 64-bit value by 3, 5 or 9 with an overflow check**, each one instruction once selected.
- **A check of a speculated arm whose operation reads no operand of the comparison the branch tests branches on its own flag** to a cold block, which goes on to the trap when the branch's condition picks the arm and back to the speculated code otherwise. A check reading such an operand, as `a - b` in the arm that `a >= b` picks, keeps its conjunction with the arm's condition, since the other arm's condition makes it fail.
- Selection branches on an overflow flag that only dead instructions separate from the branch.

## Consequences

- Collatz's step picks its next value without a branch in 16 instructions; the program runs in 1.20 times Rust's time instead of 1.43.
- A check that shares no operand with the branch's comparison yet often fails while the other arm is taken sends the code through its cold block each time.
