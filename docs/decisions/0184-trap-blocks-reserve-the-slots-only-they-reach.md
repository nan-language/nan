# 0184. Trap blocks reserve the slots only they reach

Status: accepted

## Context

A checked operation that fails writes its trap report's records into a stack slot in its cold block, then calls the report routine and traps. A function whose only stack slots are such records still reserved them in its prologue and released them in each epilogue, so every call of a small function with checked arithmetic paid two instructions for space only a trap uses. The doubly recursive Fibonacci of the comparison ([0170](0170-run-times-compared-with-rust-and-cpp.md)) made both on each of its calls, half of which return at once.

## Decision

- **When no value spills and only blocks ending in a trap address the stack**, the prologue reserves only the padding that keeps calls aligned, and each such block subtracts the slots' bytes from the stack pointer at its start. A block ending in a trap never returns, so nothing restores the stack pointer.
- The frame size the function reports keeps the slots, as the stack still holds them while a trap runs.
- AArch64 frames keep their slots in the prologue.

## Consequences

- Fibonacci runs 10% fewer instructions, and 1.54 times Rust's time instead of 1.77.
- A function spilling a value, passing arguments on the stack or using a slot outside its trap blocks reserves its whole frame in its prologue as before.
