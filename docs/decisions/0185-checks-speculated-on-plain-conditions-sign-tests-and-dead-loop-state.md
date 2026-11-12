# 0185. Checks speculated on plain conditions, sign tests and dead loop state

Status: accepted

## Context

The fannkuch program of the comparison ([0170](0170-run-times-compared-with-rust-and-cpp.md)) ran 2.8 times the instructions of Rust's. Five costs of the code generator stood out:

- Its checksum's branch on a `bool` was speculated with each check of its arms conjoined with the condition, as speculation ([0183](0183-speculated-quick-products-and-arm-checks-branching-alone.md)) let a check branch alone only when a comparison decides the branch.
- Its `i64.fit` check compared with the largest signed integer, a constant x86-64 holds in a register, which register allocation then spilled.
- Its rotation loop, which mostly runs once, paid the whole test of its versioned latch ([0175](0175-live-segments-and-loop-rewrites-before-selection.md)) before leaving.
- `order[0]` put its index 0 in a register to address the stack slot.
- The state of its `for` loops' ranges stayed as loop parameters that only dead instructions read, holding a register the calls of its outer loop made it spill.

## Decision

- **A check of a speculated arm branches alone when its operation or comparison reads no operand of the comparisons the branch's condition combines** through `band`, `bor` and `bxor`. A condition no comparison gives, such as a `bool` variable, shares none.
- **A versioned loop's latch branches to the copy's header when the next count reaches the limit**, and tests whether the copy may go on only otherwise.
- **An unsigned comparison with the largest or the smallest signed number of its width tests the value's sign**: `test` on x86-64, a comparison with 0 on AArch64.
- **A constant index folds into the displacement** of an x86-64 memory operand.
- **Trimming counts a value as read only when a live instruction reads it**: one with an effect, a return, a branch's test, or one giving a value they read. A parameter nothing live reads is dropped with the instructions only it needed.

## Consequences

- Fannkuch runs 7% fewer instructions, n-body 2% and binary trees 1% fewer.
- The matrix product's version test, whose temporaries register allocation already keeps in stack slots, runs 2% more of the program's instructions in spills; its loop is unchanged.
