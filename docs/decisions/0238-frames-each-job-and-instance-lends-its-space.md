# 0238. Frames each job and instance lends its space

Status: accepted; the frames of `isolate` serving the images of the space's bundles superseded by 0241

## Context

An isolated space drew every page from the frames `nanos::isolate` borrowed for its whole life, and its jobs and instances drew from them alike ([0222](0222-isolated-spaces-shared-by-jobs-and-instances.md)). NaN OS keeps a space per principal and provenance class for as long as it runs anything, placing instances there as they start, each with a memory budget of its own: lent everything at `isolate`, a space either had no room for an instance joining later or let one instance's growth take the frames its neighbours drew from, until a fault ended them all. The space's runtime draws chunks for its records, shared by every job, for the cells its domains allocate through and for the stacks of their tasks, keeps up to 64 returned chunks mapped for later draws, and passes chunks between domains as values leave them, so a page a job's task drew can outlive that job under another; the kernel sees only a draw of an address range.

## Decision

- **Jobs and instances lend frames.** `run_isolated` and `hold_isolated` take `mut frames`: the task placing the job or instance hands them to the task running the space with the space it was lent, they join the isolation's frames, and once it has ended that task gives back as many frames from the isolation's, after those that moved for it.
- **The runtime bounds each job's draws.** The space's start order carries the bytes lent; the runtime splits a budget of them from its allocator, with an executor on it, for the job's world and domain, so its allocations, its tasks' stacks and their growth draw within its frames and a draw past them is an exhaustion of its domain alone. The job's domain runs nested in a domain on the runtime's own allocator, so a budget too small for the first task's stack ends the job, not the runtime.
- **`isolate`'s frames serve the space itself**: its tables, the images of its bundles, the runtime's records and kept chunks, and the frames moving out through bridges, which come from and return to the isolation's frames as before.
- **A space that drew a job's frames for itself ends.** When the isolation's frames hold fewer than a job lent once it has ended, each job and instance of the space ends `Trapped` with `trap.exhausted`, as on a fault with no frame, and the frames come back from those the space held.

Alternatives rejected:

- **The kernel charging each draw to the job whose domain the drawing task runs in:** every draw would name a job the runtime trusts its tasks to report, and chunks a job drew stay mapped under records and kept chunks of others once it ends, so the kernel could give back no frames of its own anyway.
- **An `Isolation` operation lending and taking back frames:** a holder would match loans to jobs itself, and nothing would bound one job's draws against another's.
- **Lending the frames in the ask placing the job:** the ask is dropped when the isolation takes no more, and a cown holds no frame.

## Consequences

- A job or instance draws nothing beyond what it lends: one lending too few for its first task's stack ends `Trapped` with `trap.exhausted` as it starts.
- `isolate`'s frames must hold what the space draws for itself, up to 4 MiB of kept chunks among it, and what moves out of it through bridges for good; when frames moved out exceed them, a job is given back fewer than it lent.
- Each placement takes a cell of the runtime's cell table, which holds 65,536 over the space's life, and an entry of its capability table while it runs.
