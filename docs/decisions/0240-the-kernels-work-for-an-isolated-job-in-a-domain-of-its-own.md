# 0240. The kernel's work for an isolated job in a domain of its own

Status: accepted

## Context

The kernel serves an isolated job or instance on tasks of the isolation's executor: one per call through a port and per crossing kind's `call` the space sends, and one per waiting request of an instance's holder. Those tasks ran in the scope of the task holding the job, so a stack its executor's allocator could not draw trapped in the holder's domain, the kernel's for NaN OS, which gives each instance's isolation an executor on that instance's budget: an instance calling a port faster than its calls end halted the kernel. The space decides how many calls are under way, and each costs the kernel a task stack. The tasks relaying the link read and write it, which the caller owns, and a domain run's body captures nothing it borrows.

## Decision

- **A domain per job or instance holds that work.** While a job or instance runs, `run_watched` runs a domain on the isolation's executor whose first task takes each call, operation and wait the relays and the runner capability hand it and serves it on a task of its own. The tasks relaying the link stay outside it, a fixed set started when the job or instance is placed.
- **A trap there ends the job or instance alone.** The domain's end has the space abandon it, and it ends `Trapped` with the domain's trap rather than `Abandoned`.
- **Nothing else bounds the calls under way.** The allocator of the isolation's executor does; a kernel bounds each job's share by giving the isolation an account on a budget of its own.

Alternatives rejected:

- **A fixed cap on calls under way, extra ones failing with `RunnerError.Unknown` or `CrossError.Refused`:** it refuses calls a component makes within its budget, and leaves an exhaustion from any other draw, as the holder's waits, halting the kernel.
- **The same cap queuing extra calls:** a call under way that waits on one queued behind it, as one whose port calls back into the instance, never ends.
- **Running the relays in the domain the holder's abandoner runs:** each abandonment would stop the kernel's tasks mid-operation on the isolation's shared state, and the relays borrow the link.

## Consequences

- A job or instance whose space keeps more calls under way than its executor's allocator draws stacks for ends `Trapped` with `trap.exhausted`; the kernel and the space's other jobs and instances run on.
- What the domain's tasks held of memory drawn outside it, as a call's bytes the relays decoded, stays allocated once a trap abandons it.
- Each placement takes one more task of the isolation's executor and a domain run.
