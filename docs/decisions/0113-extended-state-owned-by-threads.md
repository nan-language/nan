# 0113. Extended state owned by threads

Status: accepted

## Context

[0096](0096-machine-code-without-extended-state.md) took extended state out of machine code, the seams and the thread switch, and made every kernel executor fault on floating-point and vector instructions. That left no thread able to use the state at all. NaN OS needs some threads to use it on a kernel: code of the nanos targets running in a kernel's shared space computes with floats and `core::simd`, and user threads use the vector registers. Its task switch between two such threads must stay under 300 ns, and a switch between threads that use none must pay nothing for them. Saving the state lazily, on the first use after a switch, leaves the previous owner's registers in the CPU while another thread runs, which LazyFP reads through speculation. On x86_64 the state's size depends on the components `XCR0` enables, which `hw::Ring0.write_xcr0` changes at run time, and the fastest form of the save depends on the CPU.

## Decision

- **Ownership is a scope.** A thread owns extended state only while it runs the body of `hw::ExtendedState.with`, or a user thread through `hw::Ring0.run_user` given an `Option[hw::ExtendedState]` holding one. The support routine `Extended(area) -> previous` saves the state the running task owns, makes the task own the state at `area`, none for zero, restores it, and returns the area owned before; `with` calls it before and after its body, so ownership nests. A task records its area in its task record.
- **The save area is a frame.** `hw::ExtendedState` owns a 4 KiB physical frame, as `hw::PageTableRoot` does, so its size never depends on the allocator or on the CPU's maximum. `write_xcr0` refuses components beyond x87, SSE, AVX, MPX, AVX-512 and PKRU, which the frame holds in every form, and refuses disabling a component, which would make a saved state unrestorable on that CPU. `vector` and `set_vector` read and write the first 16 vector registers of a saved state, so a kernel can set up and inspect a state it does not run.
- **Saves are eager and live at the scheduler.** The scheduler restores an owner's state before running it and saves it once it switches away; seams save nothing, since interrupt fns use no extended state and preemption switches threads. Each CPU's executor thread records whether the CPU's state is enabled and holds an owner's. Before running a thread owning none it puts that state in its initial state and disables it, so no thread runs beside another thread's registers; between two owners it only saves and restores. A thread owning none costs a load and a branch per switch.
- **The x86_64 form follows the CPU.** The CPU's record lists XSAVE, `xsaveopt`, `xsavec` and `xsaves`, and the save takes the first of `xsaves`, `xsavec`, `xsaveopt`, `xsave` and `fxsave` the CPU has, with the restore of the same form, over every component `XCR0` enables; `xsaves` and `xsaveopt` skip components unchanged since the last restore, and every XSAVE form skips those in their initial state. AArch64 saves the 32 vector registers with `FPCR` and `FPSR`.

Alternatives rejected:

- **Ownership on first use, through the fault:** it needs the fault handler eager saves avoid and leaves ownership to whichever code first touches a register.
- **Ownership inherited at spawn:** nothing in language 1 runs code that uses the state, so every spawned task of an owner would pay for state it never uses; the nanos runtime can make the tasks it spawns owners through the same routine.
- **A save area drawn through an allocator, sized by CPUID:** a later `write_xcr0` could outgrow it, and the area would need a release.
- **Saving in the seams:** every interrupt would pay for state that interrupt fns never use.

## Consequences

- Code of the nanos targets running in a kernel owns extended state by calling `Extended` around it. `run_user` calls it around the user path, so a user thread given a state uses its registers from the first instruction, and one given none still exits with a fault at its first, as [0112](0112-user-mode-as-a-call.md) had every thread do.
- `kernel.float-use` still rejects float operations in kernel code, so in language 1 a thread's owned state holds what `set_vector` wrote and what foreign or user code leaves there.
- A kernel enabling an `XCR0` component on some CPUs only cannot resume a thread whose state holds it on the others: the restore raises a general-protection exception in the executor.
- Supersedes the decisions of [0096](0096-machine-code-without-extended-state.md) that nothing saves extended state and that `write_xcr0` accepts what `xsetbv` accepts, and its plan to save the state for owners only once threads run in user mode; and the decision of 0112 that a user thread comes to own extended state only from its first use.
