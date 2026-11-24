# 0219. DMA buffers of 2 MiB map through 2 MiB IOMMU pages

Status: accepted

## Context

`hw::Dma.of` and `hw::DmaShared.of` in an I/O domain's space move a buffer into whole pages of their own and map them in the domain, and the runtime mapped every page through a 4 KiB entry. A device moving data through a large pool, such as an NVMe or network driver's request buffers, then misses the IOTLB far more often than it would without an IOMMU. NaN OS maps such pools through 2 MiB IOMMU pages where the IOMMU has them. A 2 MiB page needs 2 MiB-aligned physical memory, and the runtime drew a buffer's pages as a run of chunks aligned only to a chunk.

## Decision

- **A run of chunks of 2 MiB or more starts at a 2 MiB boundary.** The chunk supplies the runtime manages, the boot heap, the regions it grew by and a region's store, take such a run only from a 2 MiB-aligned chunk, so a buffer's pages spanning 2 MiB or more hold whole 2 MiB ranges.
- **A domain maps each 2 MiB-aligned 2 MiB of a mapping through one 2 MiB page** when the IOMMU translates such pages: on VT-d when its capability lists 2 MiB second-level pages, on SMMUv3 always, as its stage-1 tables of 4 KiB pages have 2 MiB blocks. A level-2 entry holding a table left by earlier 4 KiB mappings takes the page, and the table is freed once the unit no longer caches it.

Alternatives rejected:

- **An `of` variant asking for 2 MiB pages and failing without them:** a second constructor for one buffer kind, whose callers would all fall back to `of`; the page size is the runtime's choice, as the tables are.
- **Passing the alignment through `Alloc`'s large path to every supply:** it would thread a parameter through the chunk draws of every target, the host systems' included, for one caller, while aligning every large run costs only the gap below it.

## Consequences

- A buffer of 2 MiB or more costs the IOMMU one entry per whole 2 MiB of it, and its device walks the domain's tables accordingly less often.
- Every allocation of 2 MiB or more drawn from a runtime-managed supply is 2 MiB-aligned, which may leave a gap of up to 2 MiB less a chunk before it that smaller runs then fill.
- A kernel task's large stack, a runtime record of a class of 2 MiB or more, is 2 MiB-aligned too.
