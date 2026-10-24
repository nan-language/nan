# 0049. Carved allocations are reused

Status: accepted

## Context

The runtime carved each small allocation from its pool's current chunk by bumping an offset, and returned a chunk only once no allocation in it was live; freed space inside a chunk was never used again. A program whose long-lived values are spread across its chunks keeps almost every chunk it ever draws: the NaN toolchain peaked at 7.1 GB checking its own workspace, where the bootstrap takes 670 MB, and at 16.5 GB building itself, beyond the memory of common machines and CI runners. A long-running server would grow the same way. Which chunk an allocation lies in is the implementation's choice ([budgets](../spec/12-memory.md#budgets)), as long as allocations share a chunk only within one domain and one cell, and a chunk returns once none of its allocations is live.

## Decision

- **Size classes:** a carved allocation starts at a 16-byte granule and rounds up to one of 44 classes: the multiples of a granule up to 128 bytes, then four per doubling up to a chunk. One aligned beyond a granule keeps its own bytes and is never reused.
- **Granule marks:** each carved chunk has a byte per granule naming the class of the allocation starting there, with a freed bit, so a free needs no size and allocations carry no header.
- **Free lists:** each pool, one per domain and cell, keeps a doubly linked list of its freed allocations per class, through their first two words. An allocation of a class takes the first freed one, zeroed as fresh chunks are, before carving fresh bytes.
- **Chunk return is unchanged:** a chunk still returns when its last allocation is freed, after taking its freed allocations out of the lists; one passed to another domain leaves its pool the same way. Charges and points of exhaustion stay reproducible for one toolchain hash.

Rejected alternatives:

- **A slab of chunks per class:** a budget of one chunk could then hold allocations of a single size only.
- **A header before each allocation:** sixteen more bytes for every small value, the most common allocation.
- **Passing the size to each free:** every release site of both toolchains would change, and a wrong size would corrupt memory silently.

## Consequences

- The NaN toolchain checks its workspace in 1.2 GB and builds itself in 3.2 GB, a quarter faster.
- A chunk's marks take 4 KiB of runtime memory, and allocations of odd sizes take up to a quarter more than they ask.
- Returning or passing a chunk scans its marks.
