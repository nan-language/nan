# 0112. User mode as a call

Status: accepted; user threads owning extended state only from their first use superseded by 0113; user roots keeping their translations superseded by 0118

## Context

NaN OS runs code no trusted principal reviewed, holders of secrets and principals that distrust each other in hardware-isolated address spaces at privilege level 3 or EL0, and shares doorbell and channel frames with them. Language 1 only reserved user mode: `hw::Vector.user` and `hw::syscall_entry` bound landing fns to user-origin seams that nothing entered. Those seams would have run the landing fn on the kernel stack of the thread that ran the user code and resumed that same code, so a kernel needed one kernel thread per user thread, and a landing fn, a trust escape that cannot wait, could neither block on a request nor hand the CPU to another user thread. NaN has no inline assembly ([0009](0009-low-level-and-kernel-without-assembly.md)), so whatever enters and leaves user mode is compiler code. On CPUs affected by Meltdown, user code reads every supervisor mapping of the root it runs under, and the kernel's root maps all memory.

## Decision

- **User mode is a call.** `hw::Ring0.run_user(mut state)` runs a user thread from an `hw::UserState` and returns at its next system call, exception or interrupt, with the thread's state written back. The kernel task handles the exit as ordinary task code, which may wait, allocate and trap into its domain, and chooses the next thread: the same one, another one, or none. One task may run any number of threads, and a direct switch between threads is two calls. The user-origin landing fields and `hw::syscall_entry` are gone.
- **An interrupt in user mode interrupts the calling task.** The seam saves the user state, builds a kernel-mode interrupt frame returning from the call with the caller's interrupt state on the task's stack, and enters the common seam: the interrupt is served as for kernel code, the policy may preempt the task there, and the call returns `Interrupted` once the task resumes. Preemption, slices and domain stops need no path of their own.
- **The call is safe.** User code reaches only what the root maps for user mode, and the soundness condition of `write_cr3` and `write_ttbr0` keeps the image, page tables and live values other than user frames out of those pages. The entry admits only the arithmetic, direction or condition flags and enables interrupts; every x86_64 exit to user mode uses `iretq`, and a non-canonical instruction pointer is refused before `swapgs`.
- **Split tables run a thread under a user root.** `run_user_split` takes a root the user path installs last before user mode and replaces by the running root first at every exit. A user root need map only the **entry area**, the image with the memory it reserves, and the thread's pages: the vector seams and user paths, the CPU records with their tables and a per-CPU entry stack, the interrupt descriptor table and the stacks of nonzero `ist` all lie there, so the stacks of nonzero `ist` move from the boot heap into the image. On x86_64 the user root's translations take the running root's PCID with bit 11 flipped and both keep them, as NaN OS's split tables state; on AArch64 the ASID's bit 7 flips. Keeping translations makes the call a trusted primitive, whose caller invalidates the user tag when it reuses it.
- **User frames hold what both sides change.** An `hw::UserFrame` owns a 4 KiB frame the kernel keeps while user threads map it; the kernel reads its words as untrusted with acquire ordering, writes them with release ordering and sets bits atomically, as a doorbell needs.
- **User registers are untrusted.** `UserState` holds every register as `Untrusted[u64]`, as the reserved syscall arguments were, so a kernel reads a system call's arguments through validators.
- **User threads take owner-only extended state.** A thread that owns no extended state runs with it disabled, so its first floating-point or vector instruction exits as a fault, the point where ownership begins ([0096](0096-machine-code-without-extended-state.md)). The user path's entry, just before user mode, and its exits, just after, are where an owning thread's save area is restored and saved.
- **Copies in both directions.** `copy_out`, and `copy_in_bytes` and `copy_out_bytes` bounded by their slice, join `copy_in`: each walks the running root, readable or writable for user mode, with interrupts disabled and SMAP lifted around the copy alone.
- **Time-stamp counter denial.** `hw::Protection.Tsd` sets `CR4.TSD`, so user mode reads time only through the kernel; on AArch64 `CNTKCTL_EL1` already does.

Alternatives rejected:

- **Landing fns that choose the next user context:** they would need a handle on other threads' saved state and the scheduler's cooperation inside a trust escape that cannot wait; a call gives the same choice to task code.
- **Resuming user code directly after an interrupt that does not preempt:** it saves one return to the task per interrupt but needs a second exit path in every seam.
- **An entry area of the seams alone:** it would need the image writer to gather seams, records and stacks in sections of their own; the image holds no principal's memory, which split tables protect.
- **`RSP0` on the task's stack under a user root:** that stack is kernel memory the user root does not map, so a per-CPU entry stack in the CPU record takes the frame.

## Consequences

- Each interrupt taken in user mode costs a return to the task and a new entry.
- A kernel that never enters user mode still carries the user paths in its vector seam, every x86_64 kernel enables `syscall`, the CPU records grow to 512 bytes, and an image binding vectors of nonzero `ist` reserves their stacks for 64 CPUs.
- A translation the running root marks global stays usable under a user root, so a kernel relying on split tables maps its memory non-globally.
- A non-maskable interrupt taken in the few instructions where an x86_64 CPU runs in kernel mode with the user's GS base or root, around `swapgs` and the root switch on entry and exit, finds no CPU record. Only a kernel halt sends one, so such a CPU may reset the machine instead of stopping.
- The user-mode runtime of code lowered into a user space binds to the system calls a kernel serves through these exits.
