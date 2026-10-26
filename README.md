# NaN

NaN is a programming language made for agentic coding: performant, fast and safe, with strong versioning so that compiled NaN artefacts can reuse each other. It compiles itself and will build NaN OS, an operating system for AI, from bootloader to user interface; its Rust bootstrap compiler, targeting Linux on x86_64, stays frozen and serves only to verify the self-hosted compiler.

## Installation

On Linux x86_64:

```bash
curl -fsSL https://raw.githubusercontent.com/nan-language/nan/main/install.sh | sh
```

The script installs `nan`, the NaN toolchain built by itself, as `~/.local/bin/nan`; `NAN_INSTALL_DIR` sets another directory and `NAN_VERSION` another release, such as `v0.1.0`. Building programs for `x86_64-host` requires a C compiler available as `cc`, which links them; running and testing programs for `x86_64-uefi` and `x86_64-bare` also requires QEMU 10.2 or later (`qemu-system-x86_64`) and OVMF.

## Specification

The language is specified in [docs/spec](docs/spec/00-charter.md), starting with its charter; [the primer](docs/spec/39-primer.md) is the short introduction.

## Usage

Run `nan` from a directory of NaN packages:

```bash
nan check
nan fmt
nan run
nan test
```

`nan check` reports the diagnostics of the package holding the current directory, or of every package from the workspace root; `nan fmt` rewrites their files into canonical format. Each takes a narrower scope: a package, module or item path, a file, or `file:line`. `nan build` writes the image of a program under `.nan/images/<target>/`, and `nan run` builds and runs it, booting an `x86_64-uefi` or `x86_64-bare` image under QEMU with its serial port on the terminal. `nan test` runs the tests of its scope, `--affected` only those changed since they last passed, and `--replay <token>` reruns the run a failing test's report names. `nan show <path> --api` prints the head, contracts and digest of any item, std included, and `nan primer` the primer.

Packages name their dependencies with `dep` directives, by relative path or by a store name of the local store in `~/.nan/store`. `nan dep sync` locks a package's dependencies, `nan api update` records its API snapshots, `nan seal <item> --as <principal>` seals an item holding a trust escape, and `nan verify`, the release gate, accepts a package ready to publish; `nan publish --local <dir> --name <store name>` then appends its release to the local store, and `nan upgrade` moves a package to newer releases. `nan diff <old> <new>` compares two versions of a workspace, and `nan merge <base> <ours> <theirs>` merges three, also usable as a git merge driver with `--path`.

The NaN toolchain is the workspace in `toolchain/`, written in NaN; `nan build nan` run there builds it into `toolchain/.nan/images/x86_64-host/nan`.

## Bootstrap

`bootstrap/` holds the bootstrap compiler, written in Rust with Cranelift. It implements the same commands, built the first NaN toolchain, and checks every change to the NaN toolchain against its own output; it is frozen. Working on it, or running the test suite, requires [rustup](https://rustup.rs) and a C compiler available as `cc`. From `bootstrap/`:

```bash
rustup toolchain install
cargo install --locked cargo-nextest@0.9.148
```

The first command installs the Rust toolchain pinned in `rust-toolchain.toml`. The second builds the nextest test runner from source, which takes a few minutes. These commands then run from `bootstrap/`:

| Command | Purpose |
| --- | --- |
| `cargo build` | Build the bootstrap to `bootstrap/target/debug/nan` |
| `cargo fmt --all` | Format all code |
| `cargo clippy --workspace --all-targets -- -D warnings` | Lint; any warning fails |
| `cargo nextest run` | Run all tests |
| `cargo nextest run -p nan -E 'test(/^<module>::/)'` | Run the tests of one module, given by its path in the crate (`a::b`) |
| `cargo nextest run -p nan --test conformance -E 'test(/<chapter>/)'` | Run the conformance tests of one specification chapter, given by its file name (`01-lexical`) |
| `cargo nextest run -p nan --test programs -E 'test(/<program>/)'` | Build and run one program of `bootstrap/nan/tests/programs`, given by its directory name |
| `cargo nextest run -p nan --test milestones -E 'test(/<milestone>/)'` | Run the scenario of one milestone of `bootstrap/nan/tests/milestones` (`m2`) |
| `cargo nextest run -p nan --test self_check -E 'test(/<scope>/)'` | Compare the NaN toolchain of `toolchain/` with the bootstrap on one chapter, std package, program or the toolchain, which it then builds twice to check itself (`toolchain`) |
| `cargo nextest run -p nan --test measurements` | Check the prover's discharge on the M4 scenario and the NaN toolchain against the bound of the freeze |
| `cargo run -p nanbench -- tokens`, `measure`, `corpus`, `run <model>`, `report` | Nanbench: the primer tiers' token counts, the measurements of the freeze, a check of the task corpus, a model's run of it (`ANTHROPIC_API_KEY`) and the metrics, written under `bootstrap/nanbench/results/` |

## License

Copyright The NaN Authors. NaN is licensed under the [Apache License 2.0 with LLVM Exceptions](LICENSE): programs compiled with NaN may embed its runtime and standard library without carrying its license.
