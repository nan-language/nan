# 0147. x86_64 memory types from the memory map

Status: accepted

## Context

`Mmio.of_bar` accepted a BAR only in the last gigabyte below 4 GiB on x86_64 and the first gigabyte on AArch64, the ranges the entry root was assumed to map uncached. Firmware places the 32-bit PCI window where the machine's memory ends: OVMF on the `x86_64-uefi` machine assigns BARs from `0x8000_0000`, so no PCI function there could give a window. On AArch64 the entry root already took its memory attributes from the memory map ([0110](0110-aarch64-machines-discovered-at-boot.md)), so the fixed gigabyte both refused device memory beyond it and would accept memory on a board whose memory starts at zero. A kernel that installed its own root before calling `of_bar` had no condition keeping the BAR mapped uncached there.

## Decision

- **The x86_64 entry root takes its memory types from the memory map**, as the AArch64 one does: each 2 MiB page of the first 4 GiB that holds memory the platform lists or the image is cached, every other one uncached. The image's tables keep the last gigabyte uncached until `Init` on a bare target, which then installs the root again, or `exit_services` before installing it, sets these types.
- **`of_bar` asks the entry root itself.** Std's `hw` walks the entry root's tables, given by a supplied `entry_root`, and accepts a BAR, an MSI-X table or an AArch64 ECAM whose every 2 MiB page or block the root maps uncached, or as device memory on AArch64. What the root maps is the one fact both sides read, on either architecture and wherever the firmware put the window.
- **Kernel roots map the BARs of live functions.** `write_cr3`'s soundness condition keeps every memory BAR of a live `hw::PciFunction` that the entry root maps uncached mapped uncached, so a window `of_bar` gives after a kernel installs its own root is sound.

Alternatives rejected:

- **Accepting any BAR below 4 GiB outside usable memory, with a condition on the running root:** a kernel calling `of_bar` before installing a root of its own would map registers cached, or not at all.
- **Mapping the window the BARs give at enumeration:** the runtime would rewrite live tables whenever functions are listed, and still not cover the memory types of the rest.

## Consequences

- A kernel on any x86_64 machine reaches the 32-bit BARs of its functions; an AArch64 kernel reaches BARs and an ECAM anywhere in the device memory of its entry root.
- On x86_64 the entry root maps nothing uncached above 4 GiB, so a 64-bit BAR placed there gives `Unmapped`.
- A 2 MiB page holding both memory and registers stays cached, and a BAR in it gives `Unmapped`.
