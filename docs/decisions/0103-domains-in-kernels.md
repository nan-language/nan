# 0103. Domains in kernels

Status: accepted

## Context

A trap anywhere in a kernel halted every CPU ([0009](0009-low-level-and-kernel-without-assembly.md)), and domain runs were refused on the machine targets (`domain.unavailable`), so NaN OS could not keep a driver's or a service's bug from taking the whole system down. Three things were missing: settling an abandoned domain while its tasks run on other CPUs, turning a CPU exception raised by a task's code into a trap of that task, and abandoning a domain from outside, for a body that loops without ever reaching a cancellation point. Traps in a domain's tasks already abandoned the innermost domain, and settlement already ran in machine test images.

## Decision

- **Domain runs are available everywhere.** A domain run nests a domain in the kernel domain as it does in the process; only a trap in the kernel domain halts the kernel.
- **A kernel task stops only with interrupts enabled.** Settlement asks a running task to stop and sends its CPU the CPU signal once; at that interrupt and at every timer interrupt, the dispatcher preempts a task asked to stop when it ran with interrupts enabled on its own stack and outside every frame of an operation it completes before stopping, the frame walk the host's stop signal uses, and the task then never resumes. A task holding a spin lock, a runtime lock or an `hw::IrqsDisabled` token has interrupts disabled, so none stops holding one, and spin locks need no poisoning. `hw::without_irqs` bodies must terminate, so every task asked to stop reaches such a point. `hw::Ring0` operations are completed before a stop, so a shootdown's lock and acknowledgements are never left behind.
- **A CPU exception is a trap where it may be one.** An exception vector other than the abort vectors and the non-maskable interrupt, unbound, or bound to an interrupt fn giving the new `hw::IrqExit.Abandon`, traps a kernel task that ran with interrupts enabled on its own stack: the seam copies the saved frame onto the task's stack, leaving reserved bytes above it for the trap's operands, and makes the frame resume in the trap routine as if the faulting instruction called it, so the report walks the task's frames from that instruction. Its kind is `trap.exception`. Anywhere else the kernel halts as before, or with the new cause `unabandonable` for `Abandon`.
- **An abandoner abandons from outside.** `sys::Abandoner` is a value with interior mutability, shared through `mem::Shared`, whose `run` and `run_sized` are the domain runs and whose `trigger` abandons the domain it runs, with the kind `trap.abandoned`, and latches so a later run abandons its domain at once. A domain's first task closes the domain as its entry ends, by the same atomic word that abandoning sets, so an abandonment never starts after the body has finished.
- **Installed devices are holdings.** The domain installing an `hw::IrqShared` owns it as a holding; settlement uninstalls the device and frees its record, which lets the vector be installed again.
- **Capture walks are judged as calls.** A domain run moves its body's captures through the walk the closure's environment names, an indirect call that left every domain run statically unbounded. The stack bound of a `task fn` parameter is now the stack use of that walk, which the call site knows: the walk's own glue for a closure, judged as any lowered code, nothing for a closure whose captures do not move. Each level of a capture's type is a direct call of its move, so the walk's depth is the nesting of its types, and a type holding itself makes the walk reach itself, unbounded, the reason naming that type.

Alternatives rejected:

- **An abandoner as a Copy handle in the capability table:** nothing would ever free its slot.
- **Counting capture walks against the stub call bound:** a capture holding a recursive type walks as deep as its data.
- **Walking recursive captures through a work list drawn from the domain's allocator:** the move of each type is code generated per type, recursing through the types it holds; recursive captures of kernel bodies are rare, and reporting them keeps every walk a chain of direct calls.
- **A stop routine entered by rewriting the interrupted frame, as on the host:** preemption already switches a stopped task away for good once the preempt landing fn runs.
- **Signalling a CPU at every pass of the settling loop:** it would flood a CPU whose task finishes a critical operation; the timer retries instead.

## Consequences

- An exception means trusted code broke a soundness condition or the hardware faulted; abandoning the domain restores none of the guarantees that broke.
- The memory of an `hw::Region` or `hw::PhysFrame` an abandoned domain owned is never used again, and an abandoned device's value is never released.
- A task asked to stop inside a long critical operation stops at the first timer tick after it, up to 10 ms later.
