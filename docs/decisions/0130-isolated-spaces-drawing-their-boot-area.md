# 0130. Isolated spaces drawing their boot area

Status: accepted

## Context

A space of [0118](0118-nanos-jobs-in-isolated-spaces.md) maps its runtime's whole boot area, about 26 MiB, from frames its caller gives before it runs: 16 MiB of boot heap and 9 MiB of fixed tables, most of which a job never touches. NaN OS sizes an isolated space by what it draws and takes back what the space frees. The runtime's heap hands out runs of 64 KiB chunks for its records, the chunks cells draw and task stacks, and takes back runs of chunks; its tables are fixed arrays filled from their first entry.

## Decision

- **A space maps its boot area only as drawn.** `Isolated.space` maps the images, the stack and the caller's data; `Space.draw` maps the pages of a range of the boot area it lacks from cleared frames of a list, all or none, and `Space.give_back` unmaps them into the list. The frames the caller gives `run_isolated` bound what the space draws.
- **The heap draws and releases runs of chunks.** The runtime lowered for user mode makes a `draw` call (2) for each run of chunks its heap hands out and a `release` call (3) for each run returning to it, so a refused draw is an allocation failing where the runtime already handles exhausted memory, and memory a space frees reaches its kernel.
- **Table pages are drawn on first touch.** A fault of the space's thread on a page of the boot area it does not map draws that page, and the kernel's cleared frames replace the runtime's clearing of its tables, so a space holds only the table pages its job reaches.
- **Released pages need no invalidation.** The user path drops the translations cached under the space's tag at each entry, so a page unmapped between two runs is never translated again.

Alternatives rejected:

- **Drawing every page on its fault:** an exhausted draw would surface as a fault in arbitrary code rather than as a failing allocation, and nothing would give memory back.
- **Drawing the tables whole at start:** 9 MiB per space, nearly all zeros.
- **Smaller tables in the user-mode runtime:** the cell count only grows, so a long-running job would exhaust a small table.

## Consequences

- A space starts with its runtime's and bundle's images, a stack and the caller's data, and grows within the frames its caller gives; `nanos_isolated` runs every job in 16 MiB.
- Each chunk a space draws is cleared by the kernel and again by the heap.
- A space's pages drawn are kept in a list sorted by index, searched per page that a draw, a release or a copy touches.
- Doorbells, channel frames and the handle table of NaN OS remain to do.
