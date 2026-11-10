# NaN

NaN is a programming language made for agentic coding: performant, fast and safe, with strong versioning so that compiled NaN artefacts can reuse each other. Its toolchain is written in NaN and compiles itself. It targets Linux on x86_64 and AArch64, UEFI and bare metal.

The language is specified in [docs/spec](docs/spec/00-charter.md); [the primer](docs/spec/39-primer.md) is the short introduction.

## Installation

On Linux, x86_64 or AArch64:

```bash
curl -fsSL https://raw.githubusercontent.com/nan-language/nan/main/install.sh | sh
```

This installs `~/.local/bin/nan`; `NAN_INSTALL_DIR` sets another directory, `NAN_VERSION` another release (`v0.1.0`).

Fedora (Copr):

```bash
sudo dnf copr enable @nanlang/nan
sudo dnf install nan
```

Ubuntu 22.04 or later (PPA):

```bash
sudo add-apt-repository ppa:nanlang/nan
sudo apt install nan
```

Requirements:

- Host programs: a C compiler as `cc`; for the other architecture, `x86_64-linux-gnu-gcc` or `aarch64-linux-gnu-gcc`.
- `uefi` and `bare` programs: QEMU 10.2 or later and `swtpm`; for `uefi`, the OVMF or AAVMF firmware.

## Usage

Run `nan` from a directory of NaN packages. Most commands take a scope: a package, module or item path, a file, or `file:line`.

| Command | Does |
| --- | --- |
| `nan check` | Report diagnostics |
| `nan fmt` | Rewrite files into canonical format |
| `nan build` | Write a program's image under `.nan/images/<target>/` |
| `nan run` | Build and run a program; UEFI and bare images boot under QEMU, state in `.nan/machine/<target>/` |
| `nan test` | Run tests; `--affected` only those changed since they passed, `--replay <token>` a failing run |
| `nan show <path> --api` | Print an item's head, contracts and digest, std included |
| `nan primer` | Print the primer |

Packages and releases:

| Command | Does |
| --- | --- |
| `nan dep sync` | Lock dependencies, named by `dep` directives by path or local store name |
| `nan api update` | Record API snapshots |
| `nan seal <item> --as <principal>` | Seal an item holding a trust escape |
| `nan verify` | Release gate: accept a package ready to publish |
| `nan publish --local <dir> --name <name>` | Append a release to the local store in `~/.nan/store`, signed with `--as <principal>` |
| `nan upgrade` | Move a package to newer releases |
| `nan key new <principal>` | Make a secret key in `~/.nan/keys` and print its public key |
| `nan key trust <principal> <key>` | Trust a principal's key in the current package |
| `nan diff <old> <new>` | Compare two versions of a workspace |
| `nan merge <base> <ours> <theirs>` | Merge three versions; a git merge driver with `--path` |

A kernel declaring loaded registries also gets an interface file, against which `nan build <package> --against <file> --as <principal>` writes a signed bundle it loads.

## Development

`toolchain/` holds the NaN toolchain; `nan build nan` there builds it into `toolchain/.nan/images/x86_64-host/nan` (or for AArch64 with `targets aarch64-host` in its manifest).

`bootstrap/` holds the bootstrap compiler, in Rust with Cranelift. It built the first NaN toolchain and checks every toolchain change against its own output; it freezes when the NaN toolchain 1.0 is released. It requires [rustup](https://rustup.rs) and `cc`. From `bootstrap/`:

```bash
rustup toolchain install
cargo install --locked cargo-nextest@0.9.148
```

| Command | Does |
| --- | --- |
| `cargo build` | Build to `bootstrap/target/debug/nan` |
| `cargo fmt --all` | Format |
| `cargo clippy --workspace --all-targets -- -D warnings` | Lint |
| `cargo nextest run` | Run all tests |
| `cargo nextest run -p nan --test <harness> -E 'test(/<name>/)'` | Run one test of a harness: `conformance` (chapter), `programs`, `milestones`, `self_check`, `measurements` |
| `cargo run -p nanbench -- <command>` | Nanbench: `tokens`, `measure`, `corpus`, `run <model>` (`ANTHROPIC_API_KEY`), `report` |

## License

Copyright The NaN Authors. Licensed under the [Apache License 2.0 with LLVM Exceptions](LICENSE): programs compiled with NaN may embed its runtime and standard library without carrying its license.
