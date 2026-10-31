# 0099. Per-CPU vector bindings and MSI routing

Status: accepted

## Context

`hw::IrqShared.install` bound a vector on every CPU, one device per vector, and the AArch64 runtime routed every shared peripheral interrupt to the boot CPU ([0035](0035-the-kernel-runtime.md), [0066](0066-the-aarch64-targets.md)). NaN OS gives each NVMe and network queue an MSI-X vector on the CPU owning the queue, which needs more vectors than one machine-wide table of 224 holds and interrupts that reach that CPU. Level-triggered lines shared by several devices need every device's interrupt fn to run before the interrupt ends. Machines with more than 255 APIC ids need x2APIC. The interrupt controllers stay the runtime's, since kernel code cannot map the local APIC, the distributor or the ITS without breaking `Mmio.map`'s premise.

## Decision

- **A binding table per CPU plus a machine-wide row.** `install_on` chains its device in the CPU's row for the vector, `install` in the machine-wide row; the dispatcher runs the running CPU's chain, then the machine-wide one. Devices on one vector form a chain in installation order, read without locks by the dispatcher, changed under one runtime lock with interrupts disabled; `uninstall` unlinks and waits for the binding's count of running dispatchers to reach zero, as before.
- **The seam ends shared interrupts.** While the dispatcher runs installed devices, the CPU record marks the interrupt deferred: `complete` only records it, and the dispatcher ends the interrupt once every chained interrupt fn has returned. A single device behaves alike, so the rule does not depend on how many devices share a vector.
- **MSIs through the runtime.** `install_msi` returns the message rather than programming the device, since devices place their MSI and MSI-X tables differently. On x86_64 it takes the CPU's lowest free vector and composes a compatibility-format message. On AArch64 it maps the device id and an event to an LPI and the CPU's collection through the ITS, whose command queue the runtime drives synchronously under the bindings lock; the event is the LPI's index, so a device's interrupt table covers all 8,192 LPIs and needs no per-device event allocator. The device, collection, property and command tables come from the chunk supply.
- **A refused MSI gives the device back.** Failure types are never linear, so `install_msi` returns a `Result` whose error holds the device and the reason.
- **x2APIC when the boot CPU has it.** The boot CPU decides at `Init` and every CPU enables it as it prepares its interrupts; each APIC access branches on that runtime flag between the MSRs and the memory-mapped page, with no change to the stubs.
- **AArch64 routing per vector.** A shared peripheral interrupt goes to one CPU, so its first device sets `GICD_IROUTER` to that CPU's affinity and a device for another CPU traps; `install` keeps the boot CPU.

Alternatives rejected:

- **A link node per CPU for machine-wide installs:** one record per CPU and device to allocate and free, where a second row costs one table.
- **`install_msi` programming the device's MSI capability:** the runtime would parse PCI configuration space, which kernels own.
- **Remapped interrupts through VT-d or the SMMU:** they need the IOMMU the kernel configures; messages to APIC ids above 255 are refused instead.

## Consequences

- The binding table is reserved for 65 rows of 256 vectors, and an AArch64 kernel reserves a binding for each of its 8,192 LPIs.
- An AArch64 interrupt fn serving an LPI sees vector 0.
- A device ITS table covering 8,192 events costs two chunks per device id, and an ITS keeps 256 device ids.
- `IrqContext.complete` in an installed device's interrupt fn no longer ends the interrupt at once.
