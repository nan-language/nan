# NaN against Rust and C++

Taken by `nanbench compare`: each program of `bootstrap/nanbench/bench/compare/`, written alike in the three languages, runs on its size, built for the base x86-64 instruction set by the NaN toolchain, which the bootstrap builds from `toolchain/`, for `x86_64-host`, by `rustc 1.99.0 (b940084d7 2026-09-28)` with `--edition 2024 -C opt-level=3` and by `c++ (Ubuntu 13.3.0-6ubuntu2~24.04.1) 13.3.0` with `-std=c++20 -O3`. Each time is the median wall time of 5 runs, on a machine with Intel(R) Xeon(R) Processor @ 2.10GHz, 4 hardware threads, 15 GiB of memory.

| Program | Measures | Size | NaN | Rust | C++ | NaN / Rust | NaN / C++ |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| fib | function calls | 42 | 1.042 s | 0.719 s | 0.351 s | 1.45 | 2.97 |
| collatz | integer arithmetic | 3000000 | 0.574 s | 0.493 s | 0.666 s | 1.16 | 0.86 |
| sieve | byte stores | 50000000 | 0.396 s | 0.363 s | 0.347 s | 1.09 | 1.14 |
| fannkuch | small inline arrays | 10 | 0.194 s | 0.161 s | 0.172 s | 1.21 | 1.13 |
| matmul | nested loops over float arrays | 800 | 0.269 s | 0.239 s | 0.233 s | 1.13 | 1.15 |
| nbody | float arithmetic over structs | 5000000 | 0.317 s | 0.226 s | 0.245 s | 1.40 | 1.30 |
| interpreter | dispatch over enums | 20000000 | 0.482 s | 0.396 s | 0.356 s | 1.22 | 1.35 |
| binary_trees | allocation | 16 | 0.388 s | 0.283 s | 0.295 s | 1.37 | 1.31 |
| hash_map | hash maps | 4000000 | 0.641 s | 0.544 s | 1.708 s | 1.18 | 0.38 |
| strings | text formatting | 10000000 | 0.307 s | 0.263 s | 0.579 s | 1.17 | 0.53 |
| Geometric mean | | | | | | 1.23 | 1.06 |
