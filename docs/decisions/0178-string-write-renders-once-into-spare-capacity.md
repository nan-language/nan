# 0178. String.write renders once into spare capacity

Status: accepted

## Context

`text::String.write` rendered every value twice: once to measure the output, then into memory drawn for exactly that output. Formatting a number into a string that already had room thus ran its rendering, budget checks and bookkeeping twice, and the strings program of [0170](0170-run-times-compared-with-rust-and-cpp.md), which writes ten million numbers, spent most of its time there. The specification stated the two calls, though `Render.render` is pure and terminates, so no program can tell how often it runs.

## Decision

- **When the string's spare capacity holds the render budget, `write` renders straight into it**, eliding past the budget as any rendering does, and grows the length by what the rendering kept. No memory is drawn, so neither `write` traps nor `try_write` fails there.
- Otherwise `write` measures, then writes into memory drawn for exactly the output, as before.
- The specification states both cases.

## Consequences

- The strings program runs in about three quarters of its former time.
- A string written repeatedly within its capacity, such as one cleared and reused, renders each value once.
- The lowering of `write` holds both paths.
