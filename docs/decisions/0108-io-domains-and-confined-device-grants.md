# 0108. I/O domains and confined device grants

Status: accepted

## Context

A DMA buffer's bus address was the physical address of its contents, so a driver granted `device` could point its device at any memory, and every `device` grant was a trust escape needing a human seal ([0008](0008-authority-as-values.md), [0009](0009-low-level-and-kernel-without-assembly.md)). NaN OS confines each device with the IOMMU and wants agents to write drivers whose worst case is their own device. A driver also needed a trusted premise for every register window, and a dead driver could leave its device writing memory that settlement frees, since acknowledgements have no deadline.

## Decision

- **I/O domains made by a linear `hw::Iommu`.** `boot::Kernel.iommu` enables the IOMMU once: VT-d with queued invalidation and interrupt remapping on x86_64, SMMUv3 stage 1 on AArch64, every function's DMA blocked until attached. `Iommu.domain` makes an `hw::IoDomain` whose Copy `hw::IoSpace` names it by a slot and a generation, so a closed domain's spaces go stale without a capability table. IOVAs equal physical addresses, which keeps `bus_addr` and every existing driver's arithmetic unchanged.
- **Buffers made in a space.** `Dma.of` and `DmaShared.of` take the space. In a domain's space the contents move into pages of their own, drawn through the box's allocator, so mapping them exposes nothing else; `into_box` unmaps and waits for the invalidation. `IoSpace.physical()` keeps today's behaviour and is a device operation.
- **Contents read as copies.** A confined device can write its buffers at any time, so `Dma.contents` copies them and `Dma.write` replaces them; a view would let the prover take two reads as equal and elide a check the device then breaks.
- **PCI functions instead of trusted windows.** `boot::Kernel.pci_functions` lists the endpoints of bus 0, the only bus whose requester ids need neither bridges nor ACS. `Mmio.of_bar` gives a window over a sized memory BAR, each once, inside the memory the entry root maps uncached; `msix` installs through `install_msi` with the function's requester id, which VT-d's remapping entry or the ITS validates. Configuration writes stay the runtime's: bus mastering, decoding, INTx and MSI-X enables are never the driver's.
- **A `confined` grant that is no trust escape.** Confined operations reach only the memory the device's own domain maps: volatile calls, `unmap`, DMA, I/O domains, PCI functions but `attach_physical`, and `IrqShared.uninstall`. A `device` grant covers them too; `Mmio.map`, `Ports.map`, `IoSpace.physical`, `attach_physical` and the raw installs stay device operations.
- **Cut-off as holdings.** An `IoDomain`, a `PciFunction` and a buffer mapped in a domain are holdings of their domain run; settlement closes, cuts off and unmaps them, each waiting for the IOMMU's invalidation, before any chunk is freed. Their operations complete before a stopping task stops.
- **Drivers in std, invalidation in the runtime.** Discovery, tables, enumeration and MSI-X programming are NaN code of std's `hw`, written once for both compilers. The runtime keeps the unit's lock and its global invalidation, which settlement and `install_msi`'s remapping entries need under interrupts disabled.

Alternatives rejected:

- **IOVAs allocated per domain:** drivers would need a second address per buffer for no isolation gained, since a domain maps only its own pages.
- **A capability table entry per space:** a slot generation in the unit gives staleness without revocation the kernel needs.
- **Remapping existing I/O APIC routes at `iommu()`:** the runtime does not own them; compatibility-format interrupts are blocked instead and the specification says so.
- **Every bus and multi-unit DMAR tables:** bridges alias requester ids and need ACS; unsupported configurations give `None` or `Unsupported` rather than weaker isolation.

## Consequences

- A driver granted `confined` needs no seal, and its device's worst case is its own domain's pages.
- Each domain costs a slot of 256 and table pages from the chunk supply, charged to no allocator; a buffer in a domain costs at least a page.
- On x86_64, interrupts the I/O APIC or earlier `install_msi` calls deliver in compatibility format stop once the IOMMU is enabled.
- QEMU's AArch64 machine keeps its ECAM below 1 GiB (`highmem-ecam=off`), and an `aarch64-uefi` kernel, which has no device tree after `exit_services`, finds no IOMMU or PCI function.
- Faults are recorded by the IOMMU and never read; a device faulting repeatedly costs only its own work.
