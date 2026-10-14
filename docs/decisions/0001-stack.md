# 0001. Stack

Status: superseded by 0002

## Context

nan is a language for agentic coding that will compile itself and build NanOS, an operating system for AI, on x86_64. Until then, a bootstrap compiler written in an existing language compiles nan for Linux on x86_64; it is discarded once nan compiles itself, and nothing it depends on may shape the language.

The bootstrap compiler generates machine code through an existing code-generation library: a hand-written backend would be discarded with it, and the lasting backend will be written in nan.

Tools must be mature and widely adopted, and checks must be fast, runnable for one scope and quiet enough for agents to read.

## Decision

- **Language:** Rust, edition 2024, toolchain pinned in `rust-toolchain.toml` with clippy and rustfmt. Rust is memory-safe without a garbage collector and is Cranelift's own language, so no foreign-function layer sits between them. Compiler code is safe Rust (`unsafe_code` forbidden) and depends only on the standard library and Cranelift.
- **Build:** a Cargo workspace with one crate per directory under `bootstrap/`; `Cargo.lock` is committed.
- **Code generation:** Cranelift on its long-term support line, 0.135 (Wasmtime 48): `cranelift-codegen`, `cranelift-frontend`, `cranelift-module` and `cranelift-object`. The target and its settings are fixed in the compiler, never detected from the host CPU.
- **Linking:** `cranelift-object` writes ELF relocatable objects, which the compiler links into position-independent executables against glibc by running the system `cc` with `-pie`. Everything Linux-specific lives in one target module.
- **Lint and format:** clippy with warnings denied; rustfmt in its default style.
- **Tests:** cargo-nextest. A module's unit tests live in `<module>/tests.rs` beside it. Language end-to-end tests are plain files at the repository root, `tests/<feature>/<case>.nan` with its expected standard output and exit code, run by a `datatest-stable` harness and compared byte for byte.
- No database and no infrastructure.

Cranelift and `datatest-stable` enter the manifest with the first change that uses them.

## Consequences

- Rewriting the compiler in nan is a translation, not a port of Rust libraries; argument parsing, errors and serialisation are written by hand.
- Cranelift needs no system dependency but optimises less than LLVM, so programs built by the bootstrap compiler are slower. Performance belongs to the nan-written backend.
- Cranelift's API changes between releases. The compiler follows one long-term support line at a time (security fixes for 24 months, until about August 2028 for 0.135) and keeps every Cranelift call in one backend unit, so each upgrade, and the switch to the nan backend, replaces that unit. Cranelift fixes are published as Wasmtime security advisories, not under Cranelift's crate names.
- Cranelift adds about 30 crates; a clean build takes about 30 seconds, an incremental one under a second.
- Programs built during the bootstrap depend on glibc. Linking without it (an own entry point and system-call stubs, linked with `ld -static -nostdlib`) works with the same tools and is left to the decision on the program runtime.
- Fixed target settings make output identical on every machine, which comparing the stages of a self-hosting compiler requires, at the cost of newer CPU extensions until the baseline is raised deliberately.
- cargo-nextest is installed separately, at a pinned version that `.config/nextest.toml` enforces. It does not run doctests, so behaviour is tested in unit and end-to-end tests, not in doc comments.
- The end-to-end tests belong to the language, not to the Rust compiler, so any nan compiler, including the self-hosted one, can be checked against them.
