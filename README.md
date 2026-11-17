# NaN

NaN is a programming language made for agentic coding: performant, fast and safe, with strong versioning so that compiled NaN artefacts can reuse each other. Its toolchain is written in NaN and compiles itself. It targets Linux, Windows, macOS and NaN OS on x86_64 and AArch64, UEFI and bare metal.

The language is specified in [docs/spec](docs/spec/00-charter.md); [the primer](docs/spec/39-primer.md) is the short introduction.

## Installation

Linux or macOS:

```bash
curl -fsSL https://raw.githubusercontent.com/nan-language/nan/main/install.sh | sh
```

Windows, in PowerShell:

```powershell
irm https://raw.githubusercontent.com/nan-language/nan/main/install.ps1 | iex
```

Fedora: `sudo dnf copr enable @nanlang/nan && sudo dnf install nan`. Ubuntu 22.04 or later: `sudo add-apt-repository ppa:nanlang/nan && sudo apt install nan`.

Building for Linux needs `cc`, or `x86_64-linux-gnu-gcc` / `aarch64-linux-gnu-gcc` for the other architecture. Running Windows programs on Linux needs Wine; macOS programs need macOS 15 or later; UEFI and bare-metal programs need QEMU 10.2 or later, `swtpm` and, for UEFI, OVMF or AAVMF.

## Usage

Run `nan` from a directory of NaN packages. Most commands take a scope: a package, module or item path, a file, or `file:line`.

| Command | Does |
| --- | --- |
| `nan check` | Report diagnostics |
| `nan fmt` | Format files |
| `nan build` | Build a program into `.nan/images/<target>/` |
| `nan build <package> --against <interface>` | Build a bundle a NaN OS kernel loads |
| `nan run` | Build and run a program |
| `nan test` | Run tests |
| `nan lsp` | Serve the Language Server Protocol |
| `nan dep sync` | Lock dependencies |
| `nan verify` | Check a package is ready to publish |
| `nan publish --local <dir>` | Publish a release to the local store |

## Development

`toolchain/` holds the NaN toolchain: `nan build nan` there builds it. `bootstrap/` holds the Rust bootstrap compiler, which checks every toolchain change against its own output; it needs [rustup](https://rustup.rs) and the system linker. From `bootstrap/`:

```bash
rustup toolchain install
cargo install --locked cargo-nextest@0.9.148
cargo build
cargo nextest run
```

## License

Copyright The NaN Authors. Licensed under the [Apache License 2.0 with LLVM Exceptions](LICENSE): programs compiled with NaN may embed its runtime and standard library without carrying its license.
