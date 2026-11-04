# 0116. Nanos jobs in a kernel's shared space

Status: accepted; `Fs` and `Net` denied on a kernel's runtime superseded by 0126; the job's plain domain run superseded by 0128; the bundle borrowed for the job superseded by 0129

## Context

[0114](0114-nanos-targets-checked-without-images.md) made the nanos targets check without building images. NaN OS runs a nanos program's bundle as a job inside its kernel: the bundle is lowered against the support routines the kernel exports, loaded, and its typed or host-compatible entry run in a domain with a world the kernel builds, its input and output travelling as `mem::wire` encodings. [0109](0109-loading-bundles-into-a-running-kernel.md) already builds bundles of a kernel's closure against its interface file, loads them and binds their imports; [0113](0113-extended-state-owned-by-threads.md) gives threads owned extended state. Kernel code has no floats, so a kernel cannot pass a float to nanos code in a register, nor own extended state for code it does not know uses it. A bare kernel has no file system, network, standard streams or log of its own: those of a world exist only as the host's platform or as test sims. Isolated spaces, growable stacks, messages between closures and component entries come later.

## Decision

- **A nanos program's image is a bundle.** `nan build --against` builds a nanos program against a bare kernel's interface file of its architecture, the kernel's ABI but floats in registers. Its bundle holds a job entry, which decodes the input, runs the entry function through a generic adapter of `nanos` written in NaN, and encodes the result or renders the failure; the entry function's `limits stack N` bounds the whole entry. Without an interface file the build is `target.bundle-interface`; `target.image-unavailable` is gone.
- **The job entry is a fixed internal call, not a gate.** `nanos::run` reads the job from `sys::load::Bundle.job` and runs it in a fresh domain run sized by the bundle's stack, passing the world and input by address and receiving an `Ended`. The bundle is borrowed for the call, so it cannot be unloaded under the job; everything crossing is std types whose layout the shared toolchain fixes, and no float crosses.
- **Nanos code owns its extended state.** Every entry that code of another target calls, the job entry and the glue of each task it starts, owns a save area in its own frame through `Extended`, so floats survive preemption at no cost to kernel threads, and the area is freed with the stack.
- **Kernels serve capabilities through kinds.** `Log.served`, `Random.served` and `Stdio.served` derive an entry from a kernel's capability kind implementing `sys::LogService`, `RandomService` or `StdioService`; operations call the kind's methods through its method table, on every target, and revoking the kind revokes them. `Fs`, `Net` and `WallClock` have `denied()`; on a kernel's runtime, file and network operations fail with `Denied`.
- **A host-compatible job's wall clock is a grant.** `nanos::World` keeps no wall clock; the adapter gives `host::World.wall` the clock `caps` holds under `wall`, or a denied one.
- **A kernel calling `boot::Kernel.loader` exports its load ABI**, so a kernel loading only jobs needs no loaded registry.

Alternatives rejected:

- **A gate for the job entry:** a gate pins code for calls the kernel cannot borrow-check, and its function type would need a static stack bound the bundle alone knows.
- **A `nanos_platform` package:** the adapters are generic NaN over `nanos` types and the runtime is the kernel's, so a platform package would hold nothing else.
- **Fs and Net served by kernels now:** their files, listeners and connections are resources of the store and the egress gate, which NaN OS has not specified for a bare kernel; denying them is the world nanos gives an absent grant.
- **Saving extended state for every kernel thread:** kernel code never uses it, and nanos code knows where it enters.

## Consequences

- A nanos bundle contributes to no loaded registry, and a kernel calls nanos code only through `nanos::run`: function fields of loaded registries would carry float arguments and enter nanos code without owning extended state. Component entries and messages between closures will need their own entries.
- A nanos entry's stack bound covers its adapter and 4,160 bytes of extended state.
- The job entry's interface, addresses of a world, an input and an `Ended`, is what a user-mode runtime driven by `run_user` will call in an isolated space.
