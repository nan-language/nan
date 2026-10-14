# 0009. Low level and kernel without assembly

Status: accepted

## Context

NanOS is written in nan from the bootloader up: UEFI firmware calls, page tables, descriptor tables, interrupt entry, device registers and DMA rings. This code needs operations no type system can check, and in mainstream systems languages it is written with an undifferentiated `unsafe` and inline assembly strings. Neither states what it relies on, neither is visible to analysis, and agents invent instructions and register constraints inside assembly text that nothing checks. Kernel state usually lives in mutable globals, and the rules of interrupt context (never sleep, never allocate, hold the right lock with interrupts masked) are conventions that a reader of one window cannot verify.

nan has no ambient authority ([0008](0008-authority-as-values.md)), and the bootstrap generates code with Cranelift, which cannot be relied on to keep memory-mapped accesses in order or to leave floating-point state untouched. The trusted base must be small, listed and typed, and kernel rules must be compiler errors.

## Decision

- **Trusted code.** A [`trusted "premise" { ... }`](../spec/23-low-level.md#trusted-code) block is legal only in modules granted `trusted`. It runs exactly as the same code outside it would and permits only two things: calls of a closed set of trusted primitives, and `assume`. Each primitive declares structured requires that the prover proves, a check enforces or an `assume` takes on; the premise states the rest of its soundness condition. Every trusted block is a ledger entry, and its item a trust escape that needs a seal.
- **Typed hardware.** `hw::Ptr` has no dereference; its accesses are trusted primitives governed by stated Ptr rules. Address types are distinct, and `UserAddr.copy_in` reads user memory in one fetch and returns it Untrusted. [Layouts](../spec/23-low-level.md#layouts) cover every bit and state endianness. [Register blocks](../spec/23-low-level.md#registers) are reached only through `hw::Mmio` or `hw::Ports` windows by volatile calls that are never merged or reordered, which removes most trusted code from drivers. [DMA](../spec/23-low-level.md#dma) buffers are linear, taken by submission and returned in a completion with their contents Untrusted; descriptor rings use `hw::DmaShared` with ordered accessors and `hw::dma_fence`.
- **Privileged intrinsics.** Privileged instructions are a [closed per-target set](../spec/23-low-level.md#privileged-intrinsics) of typed methods of `hw::Ring0`. An intrinsic whose argument the hardware keeps referencing takes it by `take` and returns a linear install token, as `load_idt` does, or the replaced root, as `write_cr3` does, so a live descriptor table or page table cannot be freed or rewritten from safe code.
- **Compiler stubs and machine seams.** Every privileged intrinsic, volatile access, DMA access and fence is a call of an out-of-line [compiler stub](../spec/23-low-level.md#compiler-stubs), and no memory access moves across it, so ordering never depends on the code generator. The machine seams (program entry, interrupt vectors, the system call entry) are stubs too: they save the interrupted state, extended CPU state included, and call an interrupt fn or a landing fn bound through a [platform registry](../spec/25-registries.md#platform-registries). A [landing fn](../spec/23-low-level.md#landing-functions) receives raw integers, pointers, layouts and Untrusted integers, alone mints root grants, and is sealed. A [foreign fn](../spec/23-low-level.md#foreign-functions) declares an external symbol with its calling convention and AnyBits signature, for firmware and the host C library. Each list is closed per descriptor and changes only with the language version.
- **Memory model.** Safe code is free of data races and sequentially consistent. Trusted code follows the Ptr rules and the C++20 atomics model without consume, a model the code generator can implement and authors already know.
- **The anchor.** A seam enters with no parameter to carry kernel state, so boot installs one value per kernel with `take boot::Kernel.anchor(take state, run: f)`, which starts the scheduler and runs `f` as the first kernel task. [Only entry contexts](../spec/24-kernel.md#anchor), the bodies of interrupt and landing fns, reach it; everything else receives state as parameters or captures. Its mutable parts sit behind spin locks, per-CPU values or atomics.
- **Interrupt context as types.** An [interrupt fn](../spec/24-kernel.md#interrupt-functions) is one function kind for IRQs and kernel-origin exceptions: it receives a context or a frame typed by its vector's class, implies notrap, terminates and noalloc, cannot wait, needs a static stack bound, and returns whether to resume or reschedule. Interrupt state is the `hw::IrqsDisabled` token. `hw::SpinLock` bodies are notrap, so locks never poison. A device shared with its interrupt fn lives in `hw::IrqShared`, which takes the IRQ-safe lock on both paths. An interrupt fn wakes a task through `hw::Signal`, a one-slot mailbox. A kernel trap halts every CPU with a report on the serial port.

Rejected alternatives:

- **`unsafe`:** it unlocks everything at once, states no premise and leaves its obligations invisible.
- **Inline assembly:** a second, unchecked language inside nan, opaque to digests, contracts and authority checks.
- **Unions:** reinterpretation forges tags and predicates; trusted byte views cover the rare need.
- **Mutable statics:** they are ambient, unsynchronised and cannot be replaced in tests.
- **Masking interrupts as exclusion:** it excludes only the local CPU.

## Consequences

- The trusted base is enumerable: the ledger lists every trusted block, `assume`, foreign fn, landing fn, device or priv grant and declassifying validator, and `nan verify` requires a current seal on each. Kernel code may need more trusted blocks than hoped; ledger counts at milestone M4 decide whether primitives are added.
- Each volatile access and intrinsic costs a call; in exchange its ordering holds by construction, on every code generator.
- A new instruction or seam requires a new language version, decided by ADR.
- Interrupt fns and spin-lock bodies depend on the prover to discharge their obligations ([0007](0007-total-contracts-and-a-normative-prover.md)).
- A driver can still point a device at memory it does not own until an IOMMU is modelled; that is undefined behaviour of the device operations, which is why device grants are sealed.
- No operation of language 1 enters user mode: the user-exception and system call seams and their registry fields are checked and reserved, never entered.
- Floating point is rejected in reachable code on `x86_64-uefi` and `x86_64-bare`, and seams save extended CPU state on every entry, a latency cost the milestones measure.
- Interrupt logic is tested through the ordinary functions an interrupt fn calls, with simulated register windows; whole images run under QEMU.
