# 0064. The Rust code under bootstrap/

Status: accepted

## Context

The Cargo workspace, its lock, the pinned Rust toolchain and the nextest configuration sat at the repository's root beside `nanbench/`, though Rust builds only the bootstrap and nanbench, which reads the bootstrap through its library. The root mixed the language (its specification, std, the NaN toolchain) with the frozen implementation that verifies it.

## Decision

- **`bootstrap/` holds all the Rust code**: the Cargo workspace with `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `rustfmt.toml` and `.config/`, the bootstrap compiler in `bootstrap/nan/` and nanbench, with its task corpus, benchmark programs and results, in `bootstrap/nanbench/`.
- Cargo, rustup and nextest run from `bootstrap/`, which rustup needs to find the pinned toolchain; the workflows set it as their working directory.

## Consequences

- Retiring the bootstrap removes one directory.
- The bootstrap's build target is `bootstrap/target/`.
- The bootstrap's toolchain source tree holds the moved files under their new paths, so its toolchain hash changes once.
