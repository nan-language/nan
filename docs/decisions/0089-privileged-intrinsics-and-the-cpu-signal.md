# 0089. Privileged intrinsics and the CPU signal

Status: accepted

## Context

NaN forbids inline assembly ([0009](0009-low-level-and-kernel-without-assembly.md)), so every privileged instruction a kernel runs is a privileged intrinsic of `hw::Ring0`, a closed list per descriptor. NaN OS needs instructions that list lacked: CPU identification, cheap time, idling, address spaces tagged by PCID or ASID, extended state, hardware entropy, protection features, speculation mitigations, cache maintenance and PSCI calls. It also needs operations spanning CPUs: waking one, and keeping every CPU's cached translations coherent when a mapping changes, since invalidating a page acts on the running CPU alone. Every vector from 32 up belongs to bindings ([0035](0035-the-kernel-runtime.md)), so no interrupt between CPUs existed. Machines differ in which of these instructions they have: under KVM the CPU model is the host's, and QEMU never passes MWAIT through ([0080](0080-the-descriptor-machine-and-its-kept-state.md)).

## Decision

- **One intrinsic per instruction, safe unless it can break memory.** Each intrinsic does what its instruction does, on every machine of its architecture: where the CPU lacks an instruction, the intrinsic falls back to one that keeps its meaning, invalidating more translations, reading the counter with interrupts disabled, returning from a wait at once, or reports that it cannot, as `hardware_random`, `write_xcr0` and `enable_protection` do. Only the intrinsics that can leave memory or translations inconsistent are trusted primitives: installing a tagged root, which keeps translations cached from an earlier one, discarding dirty cache lines, and calling the firmware.
- **A feature word per CPU.** As each CPU starts, a stub records in its CPU record the features the intrinsics test, and makes the CPU's index its `TSC_AUX`. The stubs read the word directly, so feature tests cost a memory load, not a `cpuid`, which exits to the hypervisor under KVM.
- **Logic lives in stubs.** The stubs carry the feature tests and fallbacks, so the lowering of an intrinsic is a call, and the NaN toolchain reproduces it by copying bytes.
- **The runtime keeps one vector, the CPU signal.** Vector `0xFE`, `hw::SIGNAL_VECTOR`, on both architectures; the AArch64 interrupt controller delivers it as software-generated interrupt 1, the stop interrupt keeping 0. The runtime serves its own work on arrival and then calls the interrupt fn a contribution binds to the vector, so kernels build their own cross-CPU messages on it.
- **Shootdowns spin with interrupts enabled.** On x86_64, `invalidate_everywhere` posts its scope to each other started CPU, signals it, invalidates locally and waits for every acknowledgement; on AArch64 one broadcast invalidation reaches every CPU. Its `uses wait` keeps interrupt fns and bodies with interrupts disabled from calling it, so two CPUs never wait on each other; one global lock serialises x86_64 shootdowns, and a CPU waiting for it keeps serving the others.
- **Waiting for an interrupt pins the caller.** `wait_for_interrupt` enables interrupts while its CPU halts; the CPU record marks the wait so the seam does not preempt the caller, whose `hw::IrqsDisabled` token pins it to that CPU.

Alternatives rejected:

- **Suspending the caller until the last acknowledgement:** a shootdown completes within microseconds, less than the thread switches suspension costs.
- **Shootdowns on AArch64 through the CPU signal:** the interconnect broadcasts invalidations itself.
- **A feature test by `cpuid` in each intrinsic:** reading the counter or a random value would cost an exit to the hypervisor.
- **A platform registry of its own for the signal's interrupt fn:** `hw::vectors` already binds interrupt fns by vector, and a vector number the same on both architectures keeps kernels portable.
- **FSGSBASE among the protection features:** the vector seams read the CPU record through GS, which user code could then change.

## Consequences

- No kernel binding, and no `hw::IrqShared`, can take vector `0xFE`, and AArch64 interrupt 254 is never delivered.
- A CPU that keeps interrupts disabled delays every shootdown until it enables them.
- `write_xcr0` accepts only x87, SSE and AVX state, the components the vector seams save.
- A task holding `hw::Ring0` must never end, as `hw::Ring0` is linear; a kernel's first task, whose end halts the kernel, holds it.
