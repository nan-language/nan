# 0118. Nanos jobs in isolated spaces

Status: accepted; the boot area mapped whole before the space runs superseded by 0130

## Context

NaN OS places code no trusted principal reviewed, holders of secrets and principals that distrust each other in isolated spaces, which run at ring 3 or EL0 under a root of their own; the same bundle runs shared or isolated as admission decides. [0116](0116-nanos-jobs-in-a-kernels-shared-space.md) runs a nanos bundle's job in the shared space, its code bound to the kernel's support routines, and its world of kernel values. [0112](0112-user-mode-as-a-call.md) gives user mode as a call, `run_user`, under the running root or, with split tables, under a user root that every space would share one tag for. A bundle's code inlined kernel-only operations: the CPU record through `GS` or `TPIDR_EL1`, the local timer, the CPU's extended-state format. A NaN OS kernel task may migrate between CPUs at any interrupt, and the running root is per CPU.

## Decision

- **The user-mode runtime is the kernel's runtime lowered for user mode.** A kernel whose code calls `sys::load::Loader.isolate` lowers, with its final build, every support routine it exports and its runtime state again, in a flavour whose privileged operations are system calls and whose executor is one thread over a boot area the kernel maps; its image, an unsigned bundle recording where it defines each export, sits in the kernel's image. A space maps that runtime beside the bundle and binds the bundle's imports to it, so the same bundle runs shared or isolated, and the trusted surface is the kernel's handling of the calls.
- **Nanos code reaches its executor only through exports.** A nanos bundle reads the executor thread through `Thread`, arms timers through `TimerArm` and owns fresh extended state through `ExtendedOwn`, which the kernel exports; kernel code keeps its inline forms.
- **A space is frames the caller owns.** `Isolated.space` takes 4 KiB frames, clears them, maps from `0x80_0000_0000` the runtime, the bundle, a stack, the caller's data and the boot area, and builds a root mapping only that window and the entry area of [0112](0112-user-mode-as-a-call.md), so a CPU affected by Meltdown reads, through a space's root, nothing of the kernel's beyond the memory the seams need. The root copies the entry area page by page from a walk of the running root, each page with the attributes the walk gives it, since the running root maps that memory with leaves of any size among other memory. The space owns the frames and gives them back; the kernel copies to and from it through those frames, by their physical addresses, rather than through the running root.
- **A space runs under `run_user_split`.** The call installs the space's root per entry and reinstalls the running root at each exit, so a task running a space may migrate between calls. Installing a user root now drops the translations cached under its tag, `CR3` without no-flush on x86_64 and `TLBI ASIDE1` in the AArch64 user path, so spaces share the tag without a caller's invalidation, which could not be atomic with the entry.
- **The calls follow NaN OS's numbering.** `wait` (1) until a deadline, `now` (5), `invoke` (6) of a served capability with `mem::wire` arguments, and `end` (11), which NaN OS lacks, giving how the job ended. The space's log, standard streams and random source are capabilities of the space's runtime whose methods invoke the kernel, which serves them through the `sys::Log`, `Stdio` and `Random` it is given. Results are the bytes an operation gives, written into the space.
- **Traps keep their kind.** The job runs in a domain run of the space's runtime; a trap of that domain, an exception of the thread, or a wait nothing can end, end the job `Trapped` with a kernel trap of the corresponding kind.
- **Tasks of a space grow their stacks in user mode.** The runtime keeps stack segments as a kernel exporting its load ABI does ([0117](0117-stacks-grown-at-cycles.md)), so `Grow` draws a task's segments through its domain's cell from the boot area; a job's bundle is the same whether it states a stack bound or not, and placed shared or isolated.
- **Tasks of a space share its extended state.** The thread owns the state `run_user` is given; the runtime switches tasks keeping the registers calls keep, so `Extended` and `ExtendedOwn` do nothing there.

Alternatives rejected:

- **A runtime lowered into each bundle:** bundles would differ between placements, or each carry a second image.
- **Installing a space's root as the running root:** roots are per CPU and owned, so a task migrating or sharing a CPU with another space would run under the wrong root.
- **Invalidating the user tag before each call:** a migration between the invalidation and the entry would let one space use another's translations.
- **A stack bound required of isolated jobs:** a bundle stating none, as most nanos code does, could not be placed in a space.
- **Copies through `hw::UserAddr`:** they walk the running root, which does not map a space.
- **A root holding the running root's top-level entries:** it maps all kernel memory, supervisor-only, which a CPU affected by Meltdown reads speculatively from user mode.
- **Copying the running root's leaves whole:** a leaf of 1 GiB or 2 MiB holding the entry area maps memory beyond it.

## Consequences

- Component instances run only in the shared space; holding one in a space remains to do.
- A space maps its whole boot area, about 26 MiB, before it runs, and draws nothing more; `draw` and `release`, doorbells, channels and the handle table of NaN OS remain to do.
- Each space holds tables mapping the entry area, a page table per 2 MiB of it, built when the space is made; a change the kernel makes to the running root's mappings of the entry area later does not reach them.
- On AArch64 `TTBR1_EL1` keeps translating the load window at EL0, its pages for EL1 alone, so a CPU affected by Meltdown still reads the images of bundles loaded in the shared space; disabling its walks at EL0 remains to do.
- Each entry to a space refills its translations, since every space's root takes the same tag.
- Supersedes the decision of 0112 that a user root keeps its cached translations across `run_user_split` calls and that the caller invalidates the user tag.
