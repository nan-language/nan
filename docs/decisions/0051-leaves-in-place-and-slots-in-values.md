# 0051. Small leaves taken in place and slots held in values

Status: accepted

## Context

Lowering calls tiny functions everywhere: `Slice.len`, `List.at`, `String.as_str`, `u32.wrapping_add`, `SliceCursor.next`. Neither backend inlines, so each costs a call, a frame and the moves around it, and together they take a large share of the toolchain's own run time. Their results and receivers mostly pass through stack slots, an `Option` written through an out pointer or a cursor advanced in memory, so even code without calls loads and stores them on every iteration. The IR lowering builds must stay the bootstrap's ([0046](0046-the-nan-code-generator.md)), so these optimisations belong to the code generator. Lowering defines a function before the functions it calls, so a callee's body is usually unknown when its caller is built. Trap reports and the stopping of tasks read frames and line tables, so a body taken in place must not change what either sees.

## Decision

- **Before instruction selection, code generation rewrites the function into a copy** built with the function builder, which constructs SSA form from variables.
- **A direct call of a small leaf takes the leaf's body in place.** A leaf qualifies when it has at most 32 instructions and no stack slot, and none of its instructions is a call, a trap, a frame pointer read, a stack slot address, a thread-local access, a stack switch or a branch to its entry. Such code cannot trap, call or read its frame, so no trap report and no frame walk can tell it from its caller's code; it carries the call's source location. Critical functions are never taken in place, so that a stopping task still finds their frames.
- **A stack slot that only loads and stores at fixed offsets reach, bodies taken in place included, is held in values**: each distinct access becomes a variable, and slots whose accesses overlap without matching stay in memory.
- **Compilation waits for callees.** The object module holds each defined function until every colocated function it calls is defined, then compiles it; leaves compile at once and offer their bodies. When the waiting functions exceed a bound on their IR values, the oldest compiles without waiting further. Lowering takes frame sizes and line table records as functions compile.

Rejected alternatives:

- **Inlining in lowering:** it changes the IR the self-check compares with the bootstrap's.
- **Inlining during instruction selection:** the slots a callee writes through its parameters stay in memory, since no IR holds caller and callee together.
- **Compiling every function at the end:** it holds the whole closure's IR at once; waiting holds about a quarter of it at its peak on the toolchain.
- **Inlining callees that trap or call:** their frames and locations would vanish from trap reports, which must be the bootstrap's.

## Consequences

- Images the NaN toolchain builds run fewer calls and memory accesses than the bootstrap's; their frames, and so `limit.stack-exceeded` near a bound, differ more from Cranelift's.
- The text section holds functions in the order they compile, not the order lowering defines them.
- Code generation holds the IR of waiting functions and of every qualifying leaf until the build ends.
