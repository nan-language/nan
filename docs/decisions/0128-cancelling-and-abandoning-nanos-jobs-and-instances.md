# 0128. Cancelling and abandoning nanos jobs and instances

Status: accepted

## Context

[0116](0116-nanos-jobs-in-a-kernels-shared-space.md) runs a job in a plain domain run and [0119](0119-component-instances-of-nanos-bundles.md) an instance in a domain run of an abandoner it keeps to itself. NaN OS bounds a job by a deadline, cancels jobs and stops an instance whose handler never returns. A cancelling construct around the call already cancels the domain's tasks, but its cancellation came back as a job's rendered failure, and nothing reached a job looping without a cancellation point: only an abandoner abandons a domain from outside. The kernel calling `nanos::run` waits on the job, while it holds an instance as a value beside its running domain.

## Decision

- **`nanos::run` runs its job in a domain run of an abandoner the caller passes**, as `sys::Abandoner.run` does, so another task of the caller can abandon the job while the call waits.
- **`Instance.domain` shares the abandoner of an instance's domain**, as `Bundle.domain` shares a bundle's, since the holder already holds the instance while it runs.
- **`Ended` tells cancellation, abandonment and traps apart.** `Abandoned` is a domain abandoned with `trap.abandoned`, which only its abandoner raises; `Trapped` keeps every other trap.
- **The adapters report a cancellation by failure conversion.** The entry's failure is failed through a private enum with a from variant for it and one for `task::Cancelled`; failure conversion keeps a cancellation at the top of the failure type it reaches, so a cancellation of any entry's failure type arrives as the cancellation variant and ends the job `Cancelled`.

Alternatives rejected:

- **Honouring only a caller's deadline:** a job looping without a cancellation point never sees it.
- **Returning an abandoner from `run`:** the call returns once the job has ended.
- **A supplied operation telling whether a failure, or the task, is cancelled:** lowering and the interpreter of both compilers would gain it, where failure conversion already decides it in NaN.

## Consequences

- Every caller of `nanos::run` passes an abandoner; one that never abandons passes `sys::Abandoner.new()`.
- A job that absorbs its cancellation and returns ends `Returned`.
- A job spinning without a cancellation point ends only when its CPU preempts it to stop, as any abandoned kernel task does.
