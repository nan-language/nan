# 0022. Std in NaN with supplied operations

Status: accepted

## Context

Std must be written in NaN, every pub function carrying its contracts and evidence ([28](../spec/28-stdlib.md#std-evidence)), and the specification's `nan-api` blocks are its interface. The bootstrap carried those blocks as body-less declarations with natives in the interpreter (0021). Most of std is library code that NaN expresses, but some operations need the representation of a base type, the runtime or the machine: allocation and growth, element storage, interior mutability, atomics, scheduling, I/O, hardware access. The layer rule keeps `hw::Ptr` out of `mem`, so not even trusted std code can write storage.

## Decision

- **Sources in `std/`.** Each std package is a directory of NaN sources, checked like any workspace. `build.rs` embeds them in the binary; a workspace holding a package of a std name checks it instead of the embedded copy, whose diagnostics stay muted.
- **Supplied operations.** A std function may have no body; the toolchain implements it. Its digest follows rules over its head and its owning chapter instead of the worst digest, so that NaN bodies calling it can meet `pure`, `notrap` and `noalloc`.
- **NaN wherever it is expressible.** A function has a body whenever NaN over its own and lower layers states it at the same cost. Interpreter natives exist only for supplied operations.
- **The specification stays the interface.** The conformance harness checks that std declares each `nan-api` item exactly, head and visible members; std adds clauses, bodies and private items and tests.
- **Evidence where a host run reaches it.** Every pub function has an example, which `nan check` runs, or a test of its package; chapter 28 lists the functions no host run can call.

Alternatives rejected:

- **Declarations extracted from the specification**: std would stay natives the self-hosted toolchain must rewrite, and nothing would check its library code.
- **The worst digest for supplied operations**: no body calling one could keep a limit.
- **Every std type written in NaN over one raw memory primitive**: the primitive would itself be supplied, and interior mutability and scheduling still need the runtime.

## Consequences

- A std interface changes in the specification first; the harness rejects a std head that differs.
- Each supplied operation needs an interpreter native for examples, and later a lowering or support routine; a NaN body needs neither.
- A rule exempting std, such as module cycles, must be stated in the specification.
