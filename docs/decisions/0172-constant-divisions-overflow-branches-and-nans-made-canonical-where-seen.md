# 0172. Constant divisions, overflow branches and NaNs made canonical where seen

Status: accepted

## Context

Profiles of the comparison's programs as the NaN toolchain builds them ([0171](0171-run-times-taken-on-the-nan-toolchains-builds.md)) showed costs no language rule asks for. Each division or remainder by a constant ran `div`, two per step of a Collatz sequence and per decimal digit rendered. Each overflow check wrote its flag to a register, inverted it and tested it again before branching. Each float operation tested its result for a NaN, 36 times per pair of bodies in an n-body step, though only the values stored or returned can show a NaN's bits. Each `Range.next` combined `inclusive`, held in the word a slot promotion merges with its padding, into every test of a loop.

## Decision

- **Unsigned division and remainder by a constant select no division.** A power of two shifts or masks the dividend; any other divisor multiplies it by the divisor's magic number, as Hacker's Delight derives it and Cranelift selects it, and a later division of the same dividend by the same divisor in the block takes the quotient again.
- **Overflow flags stay in the processor's flags for their branch.** An overflowing operation whose flag only the conditional branch ending its block reads, directly or through one `bxor` with 1, with only constants between them, writes no flag; the branch tests carry or overflow.
- **A float result is made canonical only where its bits can show.** `fadd`, `fsub`, `fmul`, `fdiv` and `sqrt` give a NaN whenever an operand is one, and comparisons and conversions to integers read no NaN's bits, so a NaN passing through them only ever shows through a result made canonical itself. Every other reader, a store, a call, a return, a block argument, a sign operation or a bit conversion, shows the bits it reads.
- **`Range.next` tests `low < high` alone first**, in both toolchains' lowering; only the last value of an inclusive range reads and clears `inclusive`.

These are x86-64 selection rules: AArch64 selection keeps dividing and making each result canonical.

Alternatives rejected:

- **Canonicalising where bits are read, as by `to_bits`:** a NaN from `from_bits` keeps its payload, so a reader cannot tell which NaNs to rewrite.
- **Lowering `for` over a range without `Range.next`:** NIR's `for` is specified over cursors, and a loop form of its own would change the IR of every loop in both toolchains for what a cheaper `next` gives.
- **A general constant folding and value numbering pass:** more code than these patterns, which were most of the measured cost.

## Consequences

- On x86-64, Collatz steps ran about 3.7 times faster and n-body steps about 1.9 times faster.
- The IR of every range loop changed in both toolchains.
