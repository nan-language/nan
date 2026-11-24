# 0217. Exceptions of isolated spaces trap in the space

Status: accepted

## Context

A job or an instance whose thread in an isolated space took an exception its kernel did not serve by drawing a page ended `Trapped` with a trap of `trap.exception` located at the call of `run_isolated`, and a trap of the job's domain reached the kernel as its kind alone. The vector, error code and addresses of the `hw::UserFault`, and the line the space was running, were lost, while a kernel task's exception traps where it ran with a report naming them. NaN OS keeps that report as an instance's integrity fault.

## Decision

- **An exception of a space's thread is a trap of the space, as one of a kernel task is of the kernel.** The kernel resumes the thread in the runtime's fault routine as if the faulting instruction called it, with the vector, code, address and instruction address in argument registers. The routine makes its frame return to that instruction and raises `trap.exception` with the operand line a kernel exception's report has, so the trap abandons the faulting task's domain, the job's or one the job runs, and the trap routine locates it through the space's line tables.
- **The runtime names its fault routine through a system call.** The kernel forgets it when it resumes a thread there, and the routine names itself again before it traps, so an exception raised while entering it ends the job without a report instead of looping.
- **A space ends with its trap's report.** The end call carries the rendering of the trap abandoning the job's domain, or of one ending the runtime outside it, and the kernel's `sys::Trap` holds it when it is UTF-8 text of at most 4,096 bytes.
- **The kernel leaves 32 bytes below the faulting stack pointer.** The task stack's routine reserve counts them with the fault routine's own use.

Alternatives rejected:

- **An `Ended.Faulted(hw::UserFault, sys::Trap)` beside `Trapped`:** a second way for one kind of trap to end a job, and the kernel would still need the space's line tables and frames to locate it.
- **The kernel walking the space's frames through its line tables:** it would read the space's stack and tables as untrusted data and duplicate the trap routine, and the exception would still end the whole job instead of the domain that raised it.

## Consequences

- `Ended.Trapped` of a job or an instance in a space renders the report the space's runtime wrote, for every trap kind.
- A job containing a domain survives an exception there, as a kernel task's domain does.
- The runtime exports nothing new: the fault routine is local to it, and test images, which do not name one, still end with status 70 on an exception.
