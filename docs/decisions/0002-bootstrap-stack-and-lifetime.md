# 0002. Bootstrap stack and lifetime

Status: accepted

## Context

This record supersedes [0001](0001-stack.md). Its tools stand; three of its premises do not.

- Language 1 freezes only once a UEFI loader and an SMP kernel run ([milestones](../spec/38-self-hosting.md#milestones)), and kernel work cannot wait for a compiler written in NaN. The bootstrap therefore builds `x86_64-uefi` (PE32+) and `x86_64-bare` (freestanding ELF64) images as well as Linux programs ([descriptors](../spec/27-program-model.md#descriptors)). No C toolchain or libc exists for those images to link against.
- The language defines stackful tasks, completion I/O and acknowledged cancellation ([suspension, I/O and cleanup](../spec/17-suspension-io.md)). Host programs need a runtime that completes I/O into buffers a task lent and acknowledges every cancellation before the buffer's owner continues.
- A bootstrap discarded once NaN compiles itself leaves no independent compiler to check the self-hosted one against. Diverse double-compiling needs a second implementation of the whole language, from another code base in another language.

## Decision

These decisions of 0001 stay in force:

- **Language:** Rust, edition 2024, toolchain pinned in `rust-toolchain.toml` with clippy and rustfmt. Compiler code is safe Rust (`unsafe_code` forbidden) and depends only on the standard library and Cranelift.
- **Build:** a Cargo workspace with one crate per directory under `bootstrap/`; `Cargo.lock` is committed.
- **Code generation:** Cranelift on its long-term support line, 0.135 (Wasmtime 48): `cranelift-codegen`, `cranelift-frontend`, `cranelift-module` and `cranelift-object`. Target settings are fixed per descriptor, never detected from the host CPU: the x86-64 base set with SSE2, frame pointers in every function.
- **Host linking:** `cranelift-object` writes ELF relocatable objects, which the compiler links into position-independent executables against glibc by running the system `cc` with `-pie`. Everything Linux-specific lives in one target module.
- **Lint, format and tests:** clippy with warnings denied; rustfmt in its default style; cargo-nextest, with a module's unit tests in `<module>/tests.rs`; language end-to-end tests as `tests/<feature>/<case>.nan` with expected output and exit code, run by a `datatest-stable` harness and compared byte for byte.
- No database and no infrastructure. Cranelift and `datatest-stable` enter the manifest with the first change that uses them.

These decisions are new:

- **Image writer.** Cranelift emits freestanding objects for `x86_64-uefi` and `x86_64-bare`, each function in its calling convention (`win64` for code that firmware calls or that calls firmware). The compiler's own image writer lays out those objects and applies their relocations into a PE32+ image or a static ELF64 image entered by the PVH boot protocol ([images](../spec/27-program-model.md#images)). It links only objects the compiler produced: registries become sorted read-only data in codegen, and no foreign object enters a machine image.
- **QEMU.** Tests and `nan run` of machine-target programs boot their image under `qemu-system-x86_64` on a `q35` machine with 4 processors, through OVMF for UEFI and PVH for bare images. The serial port carries the output, and a write to the exit port ends the run with its status ([emulator runs](../spec/22-testing.md#emulator-runs)).
- **Host runtime.** Every task runs on its own guard-paged stack, 64 KiB unless its limits say otherwise, switched by Cranelift's stack switching. Each executor thread owns one io_uring instance through which every waiting primitive performs its operation; cancelling submits `IORING_OP_ASYNC_CANCEL`, and the operation's own completion is the acknowledgement ([host I/O backend](../spec/27-program-model.md#host-io-backend)). Host images link `libc.so.6` alone.
- **Lifetime.** The bootstrap implements all of language 1: every rule on every target, every command, the prover and the interpreter ([bootstrap](../spec/38-self-hosting.md#bootstrap)). Language 1 freezes on it, and the bootstrap freezes with it: it never implements a later language version. Once the NaN toolchain builds itself, the bootstrap checks that first self-built toolchain by diverse double-compiling and [stage comparison](../spec/36-builds.md#stage-comparison). Each later toolchain is built by its checked predecessor, so the chain of trust starts at an independent compiler without the bootstrap following the language.

Rejected alternatives:

- **A general static linker:** a large component that only foreign objects need. Resolving the compiler's own relocations is a bounded job.
- **System `ld -static -nostdlib` with a separate PE32+ writer:** two linking paths for two formats, and a C toolchain on the kernel path. One writer serves both formats.
- **Testing kernel code only through `hw::sim` on the host:** it reaches no seam, interrupt, second processor, page table or firmware call. Real hardware cannot run in an automated, repeatable test.
- **Stackless futures:** they colour functions, need pinning, and move frames, so views into a task's own frames could not live across a suspension. One switch mechanism serves host and kernel.
- **Readiness-based I/O (epoll):** files would need a second mechanism, and nothing would acknowledge a cancelled operation. io_uring gives files, sockets, timeouts, signals and cross-thread wake-ups one completion model, the one the language defines for devices.
- **Discarding the bootstrap:** every later compiler would rest on a chain of binaries leading back to one nobody can rebuild. Rust took that path, and independent verification of rustc later needed an outside reimplementation, mrustc, which lags behind the language.
- **Keeping the bootstrap for a subset:** double-compiling would then cover only that subset, and the self-hosted toolchain could not use the rest of the language.
- **Keeping the bootstrap in step with every language version:** it doubles the cost of every change and keeps Rust in the language's evolution, while checking the first self-built toolchain and chaining from it gives the same assurance.
- **Writing the NaN backend before kernel work:** NaN OS would wait behind the hardest toolchain project.

## Consequences

- Language changes after the freeze land only in the NaN toolchain. The conformance tests and end-to-end tests belong to the language, so both implementations answer to the same tests of language 1.
- Repeating the double-compiling check needs a Linux host and the frozen Rust toolchain of the bootstrap.
- The design relies on Cranelift 0.135 for stack switching on x86_64, reported frame sizes (which the [stack limits](../spec/10-effects-digests.md#stack-limits) check reads), the `win64` convention, freestanding objects, and code that never stores below the stack pointer on machine targets. Each Cranelift upgrade must keep them.
- Cranelift promises neither the ordering of volatile device accesses nor code free of SSE registers. Device, port and privileged operations are byte-function stubs that act as compiler barriers, and every seam saves the full extended CPU state ([floating point](../spec/24-kernel.md#floating-point)).
- The image writer handles only PE32+ base relocations, ELF64 program headers and the PVH note. Images carry frame pointers and line tables, from which trap reports name the call chain without unwinding.
- Machine-target tests need `qemu-system-x86_64` and OVMF installed beside the toolchain; without the emulator every run on that target fails (`test.emulator-unavailable`).
- Host programs require Linux 6.1 or later and glibc 2.35 or later. glibc is a link-time dependency of host images only and shapes no semantics; Linux, ELF and glibc appear only in the host platform package and one compiler module.
- The runtime's support routines are part of the images the compiler writes, not of the compiler, so the compiler stays safe Rust.
- Each host task reserves at least 64 KiB of address space; overflowing it hits the guard page and traps.
- Cranelift optimises less than LLVM, so images are slower than an optimising backend would make them.
- Fixed target settings make output identical on every machine, which stage comparison and double-compiling require.
