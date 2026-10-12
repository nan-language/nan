# nan

nan is a programming language made for agentic coding: performant, fast and safe, with strong versioning so that compiled nan artefacts can reuse each other. Its bootstrap compiler, written in Rust and targeting Linux on x86_64, is temporary: nan will compile itself and build NanOS, an operating system for AI, from bootloader to user interface.

## Installation

Requires [rustup](https://rustup.rs) and a C compiler available as `cc`. From the repository root:

```bash
rustup toolchain install
cargo install --locked cargo-nextest@0.9.148
```

The first command installs the Rust toolchain pinned in `rust-toolchain.toml`. The second builds the nextest test runner from source, which takes a few minutes.

## Commands

| Command | Purpose |
| --- | --- |
| `cargo build` | Build the compiler to `target/debug/nan` |
| `cargo fmt --all` | Format all code |
| `cargo clippy --workspace --all-targets -- -D warnings` | Lint; any warning fails |
| `cargo nextest run` | Run all tests |
| `cargo nextest run -p nan -E 'test(/^<module>::/)'` | Run the tests of one module, given by its path in the crate (`a::b`) |
