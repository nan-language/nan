# 0046. The NaN code generator

Status: accepted

## Context

The NaN toolchain must build itself ([bootstrap](../spec/38-self-hosting.md#bootstrap)), so it needs a code generator written in NaN for `x86_64-host`, `x86_64-uefi` and `x86_64-bare`. Lowered code must behave exactly as its NIR does, with the bootstrap's trap reports, line tables and support routines: the task runtime and io_uring executor, settlement, capability tables, codecs, the seeded scheduler and simulated capabilities of test images, the kernel runtime, seams and stubs. The bootstrap's lowering encodes all of this in 39,000 lines that build Cranelift IR; Cranelift itself, a Rust library, compiles that IR. A lowering designed afresh would re-derive every routine and representation choice, and its errors would show only as programs misbehaving.

## Decision

- **`lower` mirrors the bootstrap's lowering.** It ports `bootstrap/nan/src/lower/` module by module and function by function, as the front end ports the checker ([0041](0041-the-nan-front-end-mirrors-the-bootstrap.md)): the same instances, glue, layouts, ABI, support routines and order of work.
- **`clif` mirrors the Cranelift IR the lowering builds.** It holds the subset of Cranelift's IR, its instruction builder and cranelift-frontend's function builder with its construction of SSA form, numbering values, instructions and blocks as Cranelift numbers them and writing the text `Function::display` writes. The self-check harness compares the text of every function the NaN toolchain lowers with the bootstrap's, through `nan::lowered_functions`, so a difference names the first instruction where the port departs.
- **`object` mirrors cranelift-module and cranelift-object**: declarations, data objects and the ELF relocatable object. `cc` links host images as the bootstrap links them, and `image` ports the bootstrap's image writer.
- **`codegen` and `x64` replace Cranelift's backend.** Instruction selection maps each IR instruction to x86-64 instructions, folding immediates, base and offset addressing and compares into branches; liveness and linear-scan register allocation keep values live across calls in callee-saved registers or spill them; every function keeps the frame pointer chain trap reports walk. Every NaN function and the C library use the System V convention, firmware calls win64; stack switching, thread-local access and NaN canonicalisation follow Cranelift's forms. Code uses the x86-64 base set with SSE2, and code for `x86_64-uefi` and `x86_64-bare` uses no SSE register.
- **Frame sizes are the NaN backend's.** The NaN toolchain's [stack limits](../spec/10-effects-digests.md#stack-limits) check reads the frame sizes its own code generator reports.

Rejected alternatives:

- **A lowering designed for NaN from the start:** it would differ from the bootstrap's in every routine and could be checked only by running programs.
- **Comparing machine code with Cranelift's:** two backends allocate registers and lay out frames differently; the IR before compilation is what both share.
- **Graph-colouring or SSA-based register allocation:** more code for a first self-hosted backend, whose output only needs to be correct and deterministic.

## Consequences

- A change to the bootstrap's lowering needs its port in `toolchain/nan/lower`, or the self-check fails; after the freeze only the NaN toolchain changes.
- The NaN backend optimises less than Cranelift: images it builds run slower than the bootstrap's.
- `limit.stack-exceeded` can differ between the two toolchains for a function within a few bytes of its bound, since their frames differ.
- Machine-target code holds no floating-point state, which seams still save.
