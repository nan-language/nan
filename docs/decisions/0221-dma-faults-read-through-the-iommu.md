# 0221. DMA faults read through the IOMMU

Status: accepted

## Context

Once `boot::Kernel.iommu` has enabled the IOMMU, a PCI function's access outside the space it is attached to reaches no memory, and nothing told the kernel. NaN OS treats such an access as a fault of the function's driver: it reports the function, the address and the direction, and abandons the driver's domain. VT-d records these faults in its fault recording registers and raises a fault event, a message-signalled interrupt it sends without remapping; SMMUv3 writes them to an event queue and raises the queue's wired interrupt. Both registers belong to the runtime.

## Decision

- **Faults are read through `hw::Iommu`, one at a time.** `iommu.fault()` gives the oldest recorded fault no call gave yet as an `hw::DmaFault` (bus path, page address, write), consuming the records up to it. Only the faults of an access the attached space does not let through are given; the other records, of interrupt messages or of functions attached to no space, are discarded, so both IOMMUs give the same faults. Reading under the unit's lock, the IOMMU's own records are the only buffer: no allocation, no runtime queue.
- **The fault interrupt is installed like a slot's hot-plug interrupt.** `iommu.fault_irq(take dev, on_irq)` installs a device with its interrupt fn on the IOMMU's interrupt, as `hw::PciPort.slot_irq` does for a slot, and the kernel's fn wakes whatever reads the faults, typically through an `hw::Signal`. VT-d's fault event takes the vector `install_msi` gives on the boot CPU, written in the format without remapping; SMMUv3's event queue interrupt is the device tree's `eventq` line, installed edge-triggered. Fault recording is on from the start: the SMMUv3 context descriptors record their faults and the event queue is enabled with the unit.
- **`hw::Iommu` can be released.** Fault reading outlives the setup of domains, so a task that ends must be able to give its `hw::Iommu` up.

Alternatives rejected:

- **A signal of `hw::Iommu` that a runtime interrupt posts:** the runtime would own an interrupt handler and a vector outside the bindings every other device uses, for what an installed interrupt fn already does.
- **An operation giving every fault since the last call as a list:** it allocates, while a loop over `fault` drains the same records without allocating.
- **`fault` callable from interrupt fns:** reading the records goes through the unit's runtime memory with operations that may trap, so it would need trusted reasoning the rest of the IOMMU code does not.

## Consequences

- A kernel learns of each DMA fault the IOMMU records, with the function's requester id, the page and the direction, without polling.
- A fault arriving while the records are full is lost, and VT-d records no second fault of a function whose first is still recorded, so a kernel drains `fault` to `None` on each interrupt.
- On VT-d, uninstalling the fault interrupt leaves the unit raising its vector, as for a function's interrupts.
- SMMUv3 is driven only when its event queue holds 128 entries.
