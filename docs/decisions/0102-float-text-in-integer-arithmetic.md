# 0102. Float text in integer arithmetic

Status: accepted

## Context

Rendering a float, and the `text::json` and `text::construction` codecs of `f32` and `f64`, called host platform functions built on the C library's `strfromd`, `strtod` and `strtof`. Machine images have no C library and, since [0096](0096-machine-code-without-extended-state.md), hold floats as bit patterns in general registers, so lowering these operations for a machine target failed. Kernel code may move, render and transfer floats: only float operations are `kernel.float-use`, and [environments](../spec/27-program-model.md#environments) require every rendering to behave alike on every target. The host functions also searched the shortest digits by formatting and reading back each candidate length, up to 17 C library calls per float, and reading a JSON or construction float copied its text into an allocation for `strtod`.

## Decision

- **Core writes and reads float text from bit patterns, in integer arithmetic.** `core::f64_text` and `core::f32_text` take a pattern and write what `Render` writes; `core::f64_bits_of` reads a decimal number's text, `_` ignored, into the pattern of the nearest `f64`. Lowering calls them for every target, so host and machine images share one implementation and the host platform package no longer depends on the C library for floats.
- **Writing is Dragon4's free-format algorithm** over fixed naturals, the algorithm Rust's formatting falls back to and the interpreter's reference: it gives the shortest digits between a float's halfway points and the nearer candidate, matching the interpreter on every value.
- **Reading is an exact division.** Up to 768 significant digits are kept, a nonzero digit dropped past them counting as one further digit 1, which preserves the side of every halfway point; the value becomes a quotient of naturals scaled by powers of 5 and 2, divided to 55 or 56 bits with a sticky remainder, then rounded to nearest, ties to even, with subnormals and overflow to infinity.
- **Naturals are views over arrays of the caller's frame.** Lowering copies an array for each access through a value, so the naturals live in local arrays sized for their bound, 40 limbs for writing and 88 for reading, and their operations take views; a conversion stays within a few kilobytes of stack, which kernel tasks of 16 KiB afford.

Alternatives rejected:

- **Reporting float text codecs on machine targets as an environment error:** it rejects programs the specification defines on every target, and leaves rendering floats in a kernel broken.
- **Ryu, Schubfach or Eisel-Lemire:** each needs a table of hundreds of 128-bit powers of ten in std's sources, which the NaN toolchain carries and checks; the exact algorithms need no table and are fast enough for text.

## Consequences

- Images of every target render and read floats as the interpreter does, which a property test checks against Rust's formatting and correctly rounded parsing.
- Reading a JSON or construction float no longer allocates, so it no longer fails with `OutOfMemory`.
- Float text in host images costs integer arithmetic proportional to the size of the naturals, which grows with the decimal exponent and the digits read.
