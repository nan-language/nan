# 0171. Run times taken on the NaN toolchain's builds

Status: accepted

## Context

`nanbench compare` built its NaN programs with the bootstrap ([0170](0170-run-times-compared-with-rust-and-cpp.md)). Releases ship the NaN toolchain built by itself, whose code generator stands in for Cranelift ([0046](0046-the-nan-code-generator.md)) and optimises differently ([0051](0051-leaves-in-place-and-slots-in-values.md), [0060](0060-threading-left-out-calls-and-pool-locks.md)), and the bootstrap freezes once the toolchain reaches 1.0. The bootstrap's times describe code no user runs, and work on the NaN code generator showed in no table.

## Decision

- **The comparison builds NaN's programs with the NaN toolchain.** The bootstrap builds the toolchain from a copy of `toolchain/` in-process, and that toolchain builds the programs. A toolchain the bootstrap built generates the same code as one built by itself, so its images are those a release builds.

Alternatives rejected:

- **Timing both toolchains' images:** the bootstrap's would steer work to a code generator that freezes.
- **Timing a released `nan`:** the table would lag the repository and miss every change under review.

## Consequences

- A full comparison first builds the toolchain, a few minutes on top of the runs.
- The toolchain keeps its build cache in the running user's home directory, which it finds through the user database.
