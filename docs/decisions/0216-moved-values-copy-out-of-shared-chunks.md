# 0216. Moved values copy out of shared chunks

Status: accepted

## Context

A value leaving its domain passed every chunk it occupied to the receiving domain ([0079](0079-charges-follow-chunks.md)). A chunk holds many allocations, so the sender's other allocations in it passed too: the chunk left the sender's pool with its free space, returned only once all of them were freed, and leaked when the sender was abandoned. A NaN OS instance that keeps part of every request while sending its reply stranded a chunk per request.

## Decision

- **A value that moves itself copies each small allocation only it points to when another live allocation shares the allocation's chunk.** It moves itself when it is a body's result or failure, a channel send or offer, or a component message or reply. The copy is carved in the receiving domain for the cell the chunk would have been charged to, so the receiver pays as before, and the original is freed back into the sender's pool.
- **A chunk still passes whole** when it holds nothing else, when the allocation is aligned beyond a granule, so its size is unknown, and when no copy can be drawn, since a move cannot fail.
- **Memory reached through storage that several handles reach passes whole.** Other tasks may read a cown's value, a shared value or a channel's buffer while it moves, so its pointers cannot change. A walk entering such storage clears the copy bit of its target.
- **The walk rewrites the pointer it copied through.** Only boxes, container and store storage, hash indexes and trap records are pointed to from one place, so only their walks copy.

Alternatives rejected:

- **Keeping a sent allocation in the sender's chunk until the sender ends:** the sender would pay for what receivers keep for as long as it runs, which 0079 rules out.
- **Copying every moved allocation:** a chunk holding nothing but the moved value passes at no cost.

## Consequences

- A domain keeps its chunks while it sends, and is freed whole when abandoned.
- A move copies the bytes of each small allocation sharing its chunk, under the lock of allocations, or on `x86_64-linux` holding no lock, the copy carved and the original freed through the routines `Alloc` and `Free` run.
- `Lift` gives the allocation's address after the move.
