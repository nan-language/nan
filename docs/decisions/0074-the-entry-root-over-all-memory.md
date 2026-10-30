# 0074. The entry root over all usable memory

Status: accepted

## Context

The entry root of the machine targets mapped only the first 4 GiB ([0035](0035-the-kernel-runtime.md)), and `k.regions()` left out memory above it, so a kernel on a machine with more memory could not reach most of it, while the specification has the entry root map all usable memory. How much memory a machine has is known only at run time, from the memory map or device tree, and its tables must be written somewhere before any frame exists. A page directory of 2 MiB pages per gigabyte costs 4 KiB per gigabyte; 1 GiB pages cost one table per 512 GiB but not every x86_64 CPU maps them, QEMU's default model among them.

## Decision

- **The image keeps its tables for the first 4 GiB**, unchanged, so the seams, the trampoline and `exit_services` install the same root, and the runtime always runs under a root that reaches the memory it writes.
- **The runtime extends that root in place** over usable memory above 4 GiB, up to the end of the highest usable range rounded up to a gigabyte, densely, holes included: on a bare target in `Init` with the boot area, under the root the seam installed, on AArch64 installing it again to order the writes before its walks; on a UEFI target in `exit_services` before installing it, under the firmware's identity map. Every CPU started later enters the extended root.
- **Large mappings.** On x86_64, 1 GiB pages when CPUID reports them, otherwise a page directory of 2 MiB pages per gigabyte; on AArch64, 1 GiB blocks. Upper tables past the first 512 GiB hang off the top-level table.
- **Where the tables lie.** On a bare target at the end of the boot area, carved with it; on a UEFI target carved alone the same way, outside the image. Both lie below 4 GiB, where the image's tables map them. The runtime state records their extent, and `k.regions()` leaves it out.
- **AArch64 physical address size.** The seams and `EnterRoot` set the translation control's output size from `ID_AA64MMFR0_EL1.PARange`, at most 48 bits, instead of a fixed 40 bits.
- **The limit is 128 TiB**, the lower half of the 48-bit address space, which four-level identity mapping reaches; `k.regions()` leaves out memory above it.

Alternatives rejected:

- **`k.regions()` limited to 4 GiB:** it leaves most of a large machine unusable to a kernel.
- **Mapping each usable range exactly:** more tables and code for no reachable gain, since holes are never handed out as frames.
- **Tables drawn from the boot heap:** their size grows with memory, up to megabytes with 2 MiB pages, beyond what the fixed heap can spare.
- **A larger static root in the image:** it fixes the largest machine supported at build time and grows every image.

## Consequences

- A kernel reaches all usable memory below 128 TiB through `contents_mut` on any frame `k.regions()` yields.
- Memory-mapped devices above 4 GiB that lie between usable ranges are mapped write-back; those above the highest usable range are not mapped.
- On x86_64 without 1 GiB pages the tables take 4 KiB per gigabyte above 4 GiB, carved below 4 GiB.
