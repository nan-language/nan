# 0057. Compact check data, and callee-first judging

Status: accepted

## Context

A cold `nan check` of the toolchain workspace with the NaN toolchain peaked above the bootstrap. Most of its peak is data the checker keeps until the end: every item's typed body and NIR unit. A runtime list holds room for at least four elements, and an enum is as large as its largest variant, so a few rare, large parts sized every typed statement and argument and every NIR operand and instruction, and a NIR block of one instruction held room for four.

Each body was also typed twice: without reporting, when the digest of an item judged earlier read it, then again by its own judgment.

## Decision

- A NIR function holds the instructions of all its blocks in one list, and a block names its range in it.
- NIR literal and place operands are boxed, as are typed arguments, the patterns of `let` and `for`, the value tests of patterns and match guards.
- A package's items are judged each after the items of the package it names, depth first, so that the digests a judgment computes read bodies their own judgments typed. While reads are recorded, items are judged in declaration order.

## Consequences

- The NaN toolchain checks its workspace in less memory than the bootstrap.
- Code reading a block's instructions reads them through its function.
- Judgments are order-free ([0053](0053-order-free-queries-and-greens-from-the-check.md)), so the judging order changes no diagnostic or record; it only spares typing most bodies twice.
