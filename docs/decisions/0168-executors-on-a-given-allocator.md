# 0168. Executors on a given allocator

Status: accepted

## Context

A machine task's stack is drawn with the image's routine reserve below it, and an allocation above one 64 KiB chunk takes whole chunks, so a stack bound at or just under a multiple of a chunk costs a chunk more than its bound. A kernel running hundreds of tasks on the root executor draws every stack from the boot heap, which the target's descriptor fixes at 16 MiB.

## Decision

- **`task::Executor.split_on(share, alloc)` derives an account whose executor runs on `alloc`**, so the stacks of the tasks started on it are drawn through `alloc` and charged to its cell, from the region a store over a region supplies or within the budget a kernel gives an instance.

Rejected alternatives:

- **Large stacks sharing their last chunk with other allocations:** stacks would pack only if each could straddle the chunk boundaries of others, which needs an allocator of runs below a chunk's granularity.
- **A boot heap sized per image:** the language version fixes every descriptor.

## Consequences

- A stack above a chunk still costs whole chunks; a kernel bounds that cost by where it draws its tasks, not by the heap they share.
