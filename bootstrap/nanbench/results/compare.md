# NaN against Rust and C++

Taken by `nanbench compare`: each program of `bootstrap/nanbench/bench/compare/`, written alike in the three languages, runs on its size, built for the base x86-64 instruction set by the NaN toolchain, which the bootstrap builds from `toolchain/`, for `x86_64-host`, by `rustc 1.99.0 (b940084d7 2026-09-28)` with `--edition 2024 -C opt-level=3` and by `c++ (Ubuntu 13.3.0-6ubuntu2~24.04.1) 13.3.0` with `-std=c++20 -O3`. Each time is the median wall time of 5 runs, on a machine with Intel(R) Xeon(R) Processor @ 2.10GHz, 4 hardware threads, 15 GiB of memory.

| Program | Measures | Size | NaN | Rust | C++ | NaN / Rust | NaN / C++ |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| fib | function calls | 42 | 1.406 s | 0.898 s | 0.398 s | 1.57 | 3.54 |
| collatz | integer arithmetic | 3000000 | 0.941 s | 0.596 s | 0.770 s | 1.58 | 1.22 |
| sieve | byte stores | 50000000 | 0.352 s | 0.367 s | 0.308 s | 0.96 | 1.14 |
| fannkuch | small inline arrays | 10 | 0.294 s | 0.196 s | 0.212 s | 1.50 | 1.39 |
| matmul | nested loops over float arrays | 800 | 1.377 s | 0.208 s | 0.187 s | 6.62 | 7.35 |
| nbody | float arithmetic over structs | 5000000 | 0.573 s | 0.308 s | 0.278 s | 1.86 | 2.06 |
| interpreter | dispatch over enums | 20000000 | 0.699 s | 0.490 s | 0.515 s | 1.43 | 1.36 |
| binary_trees | allocation | 16 | 0.690 s | 0.354 s | 0.408 s | 1.95 | 1.69 |
| hash_map | hash maps | 4000000 | 1.332 s | 0.578 s | 2.039 s | 2.31 | 0.65 |
| strings | text formatting | 10000000 | 0.664 s | 0.338 s | 0.640 s | 1.96 | 1.04 |
| Geometric mean | | | | | | 1.88 | 1.66 |
