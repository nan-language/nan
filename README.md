# NaN

NaN is a programming language made for agentic coding: performant, fast and safe, with strong versioning so that compiled NaN artefacts can reuse each other. It will compile itself and build NaN OS, an operating system for AI, from bootloader to user interface; its Rust bootstrap compiler, targeting Linux on x86_64, then stays frozen and serves only to verify the self-hosted compiler.

## Specification

The language is specified in [docs/spec](docs/spec/00-charter.md), starting with its charter; [the primer](docs/spec/39-primer.md) is the short introduction.

## Installation

Requires [rustup](https://rustup.rs) and a C compiler available as `cc`; running and testing programs for `x86_64-uefi` and `x86_64-bare` also requires QEMU 10.2 or later (`qemu-system-x86_64`) and OVMF. From the repository root:

```bash
rustup toolchain install
cargo install --locked cargo-nextest@0.9.148
```

The first command installs the Rust toolchain pinned in `rust-toolchain.toml`. The second builds the nextest test runner from source, which takes a few minutes.

## Usage

Build the compiler with `cargo build`, then run it from a directory of NaN packages:

```bash
target/debug/nan check
target/debug/nan fmt
target/debug/nan run
target/debug/nan test
```

`nan check` reports the diagnostics of the package holding the current directory, or of every package from the workspace root; `nan fmt` rewrites their files into canonical format. Each takes a narrower scope: a package, module or item path, a file, or `file:line`. `nan build` writes the image of a program under `.nan/images/<target>/`, and `nan run` builds and runs it, booting an `x86_64-uefi` or `x86_64-bare` image under QEMU with its serial port on the terminal. `nan test` runs the tests of its scope, `--affected` only those changed since they last passed, and `--replay <token>` reruns the run a failing test's report names.

Packages name their dependencies with `dep` directives, by relative path or by a store name of the local store in `~/.nan/store`. `nan dep sync` locks a package's dependencies, `nan api update` records its API snapshots, `nan seal <item> --as <principal>` seals an item holding a trust escape, and `nan verify`, the release gate, accepts a package ready to publish; `nan publish --local <dir> --name <store name>` then appends its release to the local store, and `nan upgrade` moves a package to newer releases. `nan diff <old> <new>` compares two versions of a workspace, and `nan merge <base> <ours> <theirs>` merges three, also usable as a git merge driver with `--path`.

The bootstrap implements the commands of the [toolchain](docs/spec/33-toolchain.md) for `x86_64-host`, `x86_64-uefi` and `x86_64-bare`.

## Commands

| Command | Purpose |
| --- | --- |
| `cargo build` | Build the compiler to `target/debug/nan` |
| `cargo fmt --all` | Format all code |
| `cargo clippy --workspace --all-targets -- -D warnings` | Lint; any warning fails |
| `cargo nextest run` | Run all tests |
| `cargo nextest run -p nan -E 'test(/^<module>::/)'` | Run the tests of one module, given by its path in the crate (`a::b`) |
| `cargo nextest run -p nan --test conformance -E 'test(/<chapter>/)'` | Run the conformance tests of one specification chapter, given by its file name (`01-lexical`) |
| `cargo nextest run -p nan --test programs -E 'test(/<program>/)'` | Build and run one program of `bootstrap/nan/tests/programs`, given by its directory name |
| `cargo nextest run -p nan --test milestones -E 'test(/<milestone>/)'` | Run the scenario of one milestone of `bootstrap/nan/tests/milestones` (`m2`) |
| `cargo nextest run -p nan --test self_check -E 'test(/<scope>/)'` | Compare the NaN front end of `toolchain/` with the bootstrap on one chapter, std package or the toolchain (`toolchain`) |
| `cargo nextest run -p nan --test measurements` | Check the prover's discharge on the M4 scenario and the NaN front end against the bound of the freeze |
| `cargo run -p nanbench -- tokens`, `measure`, `corpus`, `run <model>`, `report` | Nanbench: the primer tiers' token counts, the measurements of the freeze, a check of the task corpus, a model's run of it (`ANTHROPIC_API_KEY`) and the metrics, written under `nanbench/results/` |
