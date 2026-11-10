# 0165. Trap reports written off the task stack

Status: accepted

## Context

A trap's report, 4 KiB of text and 1 KiB of the frames its walk reads, was written on the trapping task's stack and then copied into its domain's trap record, or written out as the run ended. Since a machine image's routine reserve is bound by its routines' frames (0164), the report path took about 20 KiB of a kernel task's 28 KiB reserve, counted once below any routine call and once for an exception's trap.

## Decision

- **A trap a domain contains claims the domain first**, as before, and only its first trap writes, straight into the domain's trap record, which holds the frames the walk reads beside the report.
- **A trap or halt ending the run writes into the image's final report**, a buffer the image reserves. On a kernel the CPU that stops the others takes it, stopping them before it writes; elsewhere the first trap claims it, and a later one waits for the run to end.

Rejected alternatives:

- **A buffer per CPU:** a trap inside a policy hook's trap would overwrite it, and hosts have no CPU records.
- **A report record drawn as the trap starts:** `trap.exhausted` would find no memory for it.

## Consequences

- A kernel's routine reserve is about 8 KiB, and a task bounded to 4 KiB takes a 12 KiB slot of the boot heap.
- A trap record takes 1 KiB more, once per domain.
- Other CPUs stop before a kernel's final report is written rather than after.
