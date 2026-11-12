# 0180. Decimal digits in pairs and short appends in place

Status: accepted

## Context

The strings program of [0170](0170-run-times-compared-with-rust-and-cpp.md) spent a third of its instructions rendering numbers. Each decimal digit took its own division by 10, a high multiplication, a shift, a multiplication and a subtraction, and each append to a rendering, the digits and then the comma, called the copy routine, which calls the C library's `memmove`, to copy a few bytes.

## Decision

- **Decimal rendering writes two digits per division by 100** from a table of the hundred digit pairs while two or more digits are left, then the last one or two.
- **An append copies up to 16 bytes in place**: the first and the last bytes of the widest of 8, 4 and 2 bytes the count holds, both loaded before either is stored so that they may overlap, or its one byte. Longer appends call the copy routine.

## Consequences

- The strings program runs 15% fewer instructions, and 1.26 times Rust's time instead of 1.43.
- An image holds the 200-byte table once, and each append the branches of its short copy.
