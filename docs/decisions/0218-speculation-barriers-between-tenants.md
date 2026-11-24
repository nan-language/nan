# 0218. Speculation barriers between tenants

Status: accepted

## Context

NaN OS runs principals that distrust each other in isolated spaces, and keeps one space's code from steering another's speculation: before a CPU runs code of another principal than the last it ran in user mode, it issues an indirect branch prediction barrier, and on CPUs affected by microarchitectural data sampling it clears the CPU buffers before every return to user mode. `hw::Ring0.write_msr(0x49, value: 1)` and `clear_cpu_buffers` exist, but a kernel cannot place them: `nanos::run_isolated` and `hold_isolated` call `Space.run` themselves, a task may migrate between any kernel call and the entry, and kernel code running between `verw` and the entry refills the buffers. Every space runs under one user PCID or ASID, so on AArch64 the hardware's tagging of predictions by ASID does not separate spaces either.

## Decision

- **The user path issues both barriers.** They run in the seam's user path with interrupts masked, on the CPU that enters user mode, as the switch to the user root does ([0118](0118-nanos-jobs-in-isolated-spaces.md)).
- **A thread runs for a tenant the kernel names.** `run_user_split`, `Isolated.space`, `run_isolated` and `hold_isolated` take a `tenant: u64`; a space holds its own, its test runs take the job's, and `run_user`'s threads run for tenant 0. Each CPU record keeps the tenant it last entered, and the entry bars predictions only when the tenant changes, so a kernel grouping a principal's spaces under one tenant pays no barrier between them. A tenant rather than a per-space identity: the kernel knows which spaces trust each other, and a root's frame, the only identity the user path has, is reused once its space is released.
- **Barriers follow the CPU's features, recorded once per CPU.** `CpuFeatures` lists the indirect branch prediction barrier from leaf 7 or, on AMD, leaf `0x8000_0008`, and buffer clearing where `cpuid` reports that `verw` clears them and `IA32_ARCH_CAPABILITIES` does not report both MDS_NO and TAA_NO. On AArch64 the CPU's start records whether `ID_AA64ISAR1_EL1` lists FEAT_SPECRES, and the barrier is `cfp`, `dvp` and `cpp` for every ASID at EL0, since all spaces share one.
- **`verw` reads the frame's stack selector**, just before `swapgs` and `iretq`, so nothing but the iret frame is read after the clearing.

Alternatives rejected:

- **Leaving the barriers to the kernel around `Space.run`:** a migration between the barrier and the entry would run the space on a CPU that never issued it.
- **A barrier at every entry:** an indirect branch prediction barrier costs thousands of cycles, and most entries return to the thread that just made a system call.
- **The tenant as a field of `nanos::Services`:** the services are capabilities the kernel grants, and the tenant names who they are granted to.

## Consequences

- Each CPU record keeps the last tenant at offset 392, so the x86_64 entry stack shrinks by 8 bytes to 112.
- A kernel giving distrusting principals one tenant, or giving tenant 0 to a space, leaves their speculation unseparated.
- AArch64 CPUs without FEAT_SPECRES have no architectural barrier, and the user path issues none.
