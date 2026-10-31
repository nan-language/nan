# 0096. Machine code without extended state

Status: accepted

## Context

Every vector seam and thread switch of the kernel runtime saved and restored the complete extended CPU state: the x87, SSE and AVX registers on x86_64, the floating-point and Advanced SIMD registers with FPCR and FPSR on AArch64. The freeze measured 52.9 ns to save it and 95.8 ns to restore it, against a 346 ns thread switch ([0044](0044-nanbench-and-the-measurements-of-the-freeze.md)). Kernel code already performs no float operation (`kernel.float-use`), and no language 1 thread runs in user mode, so no thread of a kernel needs that state; the save existed because lowering still put values in those registers: Cranelift holds f32 and f64 values there even when code only moves them, and computes an AArch64 `popcnt` in the vector unit. ADR [0013](0013-language-cost-and-scope.md) planned nofloat code generation for after the freeze, and the NaN code generator already kept machine-target floats in general registers ([0046](0046-the-nan-code-generator.md)). Lazy switching, which saves the state only when a thread touches it, leaks it across threads through speculation (LazyFP).

## Decision

- **Machine code holds no extended state.** Lowering for a machine target gives a float the integer type of its width, so its loads, stores, constants and calls use general registers; it computes the f32 and f64 conversions of codecs in integer arithmetic, and counts set bits on AArch64 by bit arithmetic. Both toolchains give the same IR, so the NaN code generator no longer distinguishes machine targets by whether floats may use the vector registers.
- **Nothing saves it.** The seams save the general registers alone and the AArch64 thread switch of a machine target the callee-saved general registers alone; the host switch keeps d8 to d15, which host code preserves across calls.
- **Executor CPUs run with it disabled.** The anchor sets `CR0.TS` on x86_64 and clears `CPACR_EL1.FPEN` on AArch64 on each CPU as its executor starts, so an instruction that slips through faults and halts the kernel instead of corrupting another thread's registers. The boot context keeps it enabled, since UEFI firmware runs there.
- **`write_xcr0` accepts what `xsetbv` accepts.** No saved state depends on `XCR0`, so the intrinsic refuses only the values the instruction faults on. Each x86_64 CPU still enables XSAVE with x87, SSE and AVX state where it has AVX, so that `XCR0` can be read and written on every CPU.
- **Owner-only saves come with user mode.** The set of threads that own extended state is empty until threads run in user mode; then the seams and switches save it for its owners alone, eagerly, with XSAVES and its init optimisation where available.

Alternatives rejected:

- **Lazy save on first use:** vulnerable to LazyFP, and it needs the fault handler that eager saves avoid.
- **Keeping the save for safety:** it costs every switch and interrupt for state no kernel thread owns, and hides lowering that uses floating-point registers instead of exposing it.
- **Rejecting codecs of float types on the machine targets:** their encodings are defined on every target, and integer conversions keep them.

## Consequences

- The thread switch and every seam lose the extended-state save and restore, and the seam frame on AArch64 shrinks from 816 to 288 bytes.
- A lowering change that emits a float or vector instruction for a machine target halts any kernel that runs it, the M4 kernels among them, and a unit test rejects float and vector types in machine-target IR.
- Supersedes the consequence of [0046](0046-the-nan-code-generator.md) that seams save floating-point state, and the consequence of [0089](0089-privileged-intrinsics-and-the-cpu-signal.md) that `write_xcr0` accepts only x87, SSE and AVX state.
