# 0073. The boot area

Status: accepted

## Context

A machine image's runtime needs about 23 MiB before anything else runs: the 16 MiB boot heap and the cell, chunk and capability tables `Init` sets up. A bare image reserved all of it as zeroed data, so its writable segment spanned `0x10_4000` to `0x180_0000` on x86_64. On the `x86_64-uefi` machine OVMF keeps ACPI NVS and runtime data from `0x80_0000`, so a UEFI loader could not copy a bare kernel's segments to their physical addresses, which M4 requires, without overwriting firmware memory. Any fixed address is someone's reserved memory on some machine; only the platform's memory map says where memory is free.

## Decision

- **One boot area.** The boot heap and the runtime tables form one block, the boot heap first, aligned to a chunk so the heap's chunks align to their size. The runtime state records its address, from which the heap and the tables are reached.
- **Carved on bare targets.** `Init` places it, before anything draws from the boot heap, at the lowest chunk-aligned address of usable memory from which it fits, from the same walk of the start information's memory map or the device tree that `k.regions()` makes, outside the image. It zero-fills the tables, the heap clearing what it hands out. When nothing fits, the runtime is unavailable. A bare image's loadable segments shrink to code, read-only data, the boot stack, the entry root and small static records.
- **Reserved on UEFI targets.** The firmware places the image wherever its memory map has room, so the boot area stays one zeroed object of the image.
- **`k.regions()` leaves it out**, beside the image, so frames never overlap the runtime's memory.
- **Test images start like final builds.** Their boot glue records the platform's values and runs `Init` before reading the run file, since the boot heap exists only once `Init` has placed it.

Alternatives rejected:

- **A higher fixed load address:** it moves the overlap to another machine's reserved memory.
- **Relocatable bare images:** PVH and the `virt` board load segments at their physical addresses; relocation would need a loader the platform does not provide.
- **Tables drawn from the boot heap:** the chunk map and the cell table describe the heap's draws, and a heap sized for both would still sit in the image.

## Consequences

- A bare image's loadable segments end within a few hundred KiB of its base, so a UEFI loader can place them in conventional memory.
- `Init` reads the memory map before anything else, so a bare platform that lists no usable memory, such as a PVH start information of version 0, cannot start the runtime.
- The carved area lies in usable memory below the 4 GiB the entry root maps.
