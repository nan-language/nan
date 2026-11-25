# 0230. Taking back the entries of a space's table

Status: accepted

## Context

A space's table of handles only grew ([0135](0135-handles-crossing-isolated-spaces.md)), up to 256 entries, and each job and instance now takes entries of its own for its services, runner capability, link's channels, bridges' ends and the capabilities that cross with it ([0227](0227-services-of-each-job-and-instance-of-a-space.md)). An isolation lives as long as its principal runs anything, so after about fifty placements every later job started with handles naming no entry. An entry can only be taken back once nothing can reach another entry through its handle, yet a handle of an ended job may still be in flight: in the runtime, in a kernel task serving a call it sent, or in a relay to another isolation.

## Decision

- **The table takes back what ended.** The task running the space takes back the entries of a job or instance as it ends it, and of the end of a bridge as the end closes; the job's or instance's own task takes back once more what it added after that, and whatever was added for a placement that failed.
- **A handle carries its slot's generation.** The table has 256 slots, a handle naming the slot of its remainder by 256; a slot's next entry takes the handle 256 above its last, so a handle once taken back is stale for good, wherever it still is, and no handle names two entries. A slot stops at a handle bound far below the failure results of a call.
- **One spin lock guards the slots,** with interrupts disabled, since the system-call service, the kernel's relays and other isolations' bridges add, read and take back entries from tasks on any CPU, and none of those waits.

Alternatives rejected:

- **Reusing a handle once nothing names it:** neither the runtime nor every kernel task can tell the table when it last names a handle.
- **A table per job or instance:** handles crossing between a space's jobs and the instances they bridge to share one table.

## Consequences

- A long-lived isolation runs any number of jobs and instances, at most 256 entries live at once.
- Every read of the table takes the lock; a search for a free slot scans at most 256 slots.
- An entry a live instance's runtime no longer holds, such as a capability that crossed with a call it answered, stays until the instance ends.
- Supersedes 0135's decision that the table only grows and is shared without a lock.
