# 0244. A hold's first channel cleared as it starts

Status: accepted

## Context

A link is the caller's and outlives the spaces it serves ([0131](0131-links-and-handle-tables-of-isolated-spaces.md)), and a kernel holds instance after instance on one link: a restore from a checkpoint, or a new instance once one has trapped or been abandoned. The first channel carries the framed messages between the kernel's relays and the instance's runtime ([0134](0134-component-instances-in-isolated-spaces.md)), and both sides resume from the counts the frames hold. An instance that ends abandoned or trapped leaves in its rings the requests it never took and the answers the kernel never read, so the next hold read them as the start of its own stream. A bridge's frames are already cleared as each placement maps them ([0225](0225-bridges-between-isolated-spaces.md)). The other channels are standard streams whose bytes and framing are the caller's.

## Decision

- **Each hold clears its first channel.** `hold_isolated` clears the first channel's frames and the kernel's counts of it before its relays start, so each instance's stream starts empty whatever an earlier hold left.
- **Other channels keep their bytes.** A standard stream's channel stays the caller's stream across placements, as the caller sends and reads it while no job runs.

Alternatives rejected:

- **A link serving one hold:** a kernel restarting a component would make and release a link per instance, and the rule would need enforcing on a value the caller lends by read.
- **Clearing as a hold ends:** an ending cut short by cancellation would leave the bytes, and a link used by other means before the next hold would still carry them.
- **Clearing every channel at each placement:** what the caller sent a stream before or while placing a job would be lost.

## Consequences

- A hold costs two frame clears as it starts.
- Bytes the caller writes to the first channel outside a hold never reach an instance.
