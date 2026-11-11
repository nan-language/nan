# 0170. Run times compared with Rust and C++

Status: accepted

## Context

Nothing measured how fast the code NaN builds runs. The measurements of the freeze time runtime mechanisms, an allocation, a generation check, a seam, not the loops, calls and data structures programs spend their time in. Improving code generation and the runtime needs to know which aspects lag and by how much, against compilers whose output is the usual standard: Rust's and C++'s.

## Decision

- **Programs written alike in three languages.** `nanbench/bench/compare/` holds one directory per program: a NaN package with the same algorithm, data layout and functions as its Rust and C++ files beside it. Each program stresses one aspect, such as calls, integer arithmetic, array stores, float loops, enum dispatch or allocation. Where the aspect is a library's, as hash maps and text formatting are, each language uses its standard library in its usual form.
- **Sizes from the command line.** Each program reads its size from its first argument, NaN through a validator as any program reads its arguments, so no compiler computes a result while building.
- **Outputs that agree.** Each program writes a result of its whole computation, floats converted to integers first so that no float formatting differs. Nanbench compares the three outputs before timing, so no language is timed doing less work, and a miscompilation shows.
- **One instruction set, release optimisation.** NaN builds without a codegen level, `rustc` at `-C opt-level=3`, the system `c++` at `-std=c++20 -O3`, all for the base x86-64 instruction set, each with its usual release checks.
- **Medians of whole runs.** Each image runs once to check its output, then 5 times, interleaved with the other languages' images; its time is the median wall time of the process. `nanbench compare` publishes each program's times, NaN's ratio to each language and their geometric means; naming programs times only those and publishes nothing.

Alternatives rejected:

- **The Benchmarks Game's programs**: each is tuned for its language with threads, intrinsics and custom allocators, so it measures its authors' effort as much as the compiler.
- **Timing inside each program**: the clock and timing code would differ between the languages, and NaN reaches a clock only through a capability.
- **The machine's own instruction set**: results would differ with each machine's extensions.
- **A crate of its own**: nanbench already builds through the bootstrap in-process, describes the machine and publishes results.

## Consequences

- `nanbench compare` needs `rustc` and a C++20 compiler named `c++`; `rustc` runs under the toolchain the repository pins.
- The table compares languages within one run; times vary between runs and machines.
- A new program needs its three sources, outputs that agree and a row in nanbench's program table.
