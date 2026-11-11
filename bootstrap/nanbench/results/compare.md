# NaN against Rust and C++

Taken by `nanbench compare`: each program of `bootstrap/nanbench/bench/compare/`, written alike in the three languages, runs on its size, built for the base x86-64 instruction set by the bootstrap for `x86_64-host`, by `rustc 1.99.0 (b940084d7 2026-09-28)` with `--edition 2024 -C opt-level=3` and by `c++ (Ubuntu 13.3.0-6ubuntu2~24.04.1) 13.3.0` with `-std=c++20 -O3`. Each time is the median wall time of 5 runs, on a machine with Intel(R) Xeon(R) Processor @ 2.10GHz, 4 hardware threads, 15 GiB of memory.

| Program | Measures | Size | NaN | Rust | C++ | NaN / Rust | NaN / C++ |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| fib | function calls | 42 | 1.711 s | 0.851 s | 0.412 s | 2.01 | 4.15 |
| collatz | integer arithmetic | 3000000 | 1.571 s | 0.597 s | 0.803 s | 2.63 | 1.96 |
| sieve | byte stores | 50000000 | 0.654 s | 0.315 s | 0.284 s | 2.07 | 2.30 |
| fannkuch | small inline arrays | 10 | 0.815 s | 0.186 s | 0.203 s | 4.38 | 4.02 |
| matmul | nested loops over float arrays | 800 | 3.248 s | 0.205 s | 0.172 s | 15.85 | 18.87 |
| nbody | float arithmetic over structs | 5000000 | 2.775 s | 0.298 s | 0.271 s | 9.32 | 10.25 |
| interpreter | dispatch over enums | 20000000 | 1.033 s | 0.455 s | 0.450 s | 2.27 | 2.30 |
| binary_trees | allocation | 16 | 0.676 s | 0.300 s | 0.342 s | 2.25 | 1.97 |
| hash_map | hash maps | 4000000 | 1.813 s | 0.535 s | 1.775 s | 3.39 | 1.02 |
| strings | text formatting | 10000000 | 0.723 s | 0.290 s | 0.621 s | 2.50 | 1.16 |
| Geometric mean | | | | | | 3.54 | 3.09 |
