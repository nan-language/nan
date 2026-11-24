# 0223. PCI functions attach by IOMMU group

Status: accepted

## Context

`hw::PciFunction.attach` confined a function to an I/O domain only when it was isolated: its DMA reaching the IOMMU under its own requester id and no peer receiving it otherwise. A function of a multi-function device without ACS, one below a root or downstream port without ACS, and one below a PCIe-to-PCI bridge, whose DMA reaches the IOMMU under the bridge's requester id, could not be confined, so their drivers ran unconfined or not at all; on q35 that is the SATA controller. NaN OS confines every driver's DMA, and binds the functions the IOMMU cannot keep apart, an IOMMU group, to drivers of one principal.

## Decision

- **The runtime groups functions as it scans, from the topology alone.** A bus is grouped when a bus above it is, when a bridge other than a PCIe port leads to it, or when a root or downstream port on it lacks ACS; the group of a function on a grouped bus spans the highest such bus and everything below it, and a multi-function device lacking ACS on another bus is a group of its own. A port lacking ACS groups its whole bus, not only its own subtree as Linux does, because the requests from below it reach its sibling ports' functions too. `hw::PciFunction.group` names the group by a bus path, so a kernel compares groups without a new type.
- **A group's attached functions share one space.** `attach` and `attach_physical` fail with `hw::IommuError.Grouped` while another function of the group is attached to another space; a function that is not attached does no DMA, as its bus mastering is off, so the group is confined by its first attachment. The runtime finds the group's functions on a list of live function records rather than in a group record, which would need its own lifetime across rescans.
- **A function below a bridge other than a PCIe port is translated under both requester ids.** Attaching points its own requester id and the one the highest such bridge gives its requests at the space; that requester id stays translated while another attached function shares it, and validates its message-signalled interrupts.

Alternatives rejected:

- **Attaching a group at once, through a group value holding its functions:** it changes how drivers receive functions, while the first attachment already decides the group's space.
- **Translating aliased requests under the alias alone:** PCI-X functions below a PCIe-to-PCI bridge keep their own requester id.

## Consequences

- Every function the IOMMU translates can be confined; a kernel binds a group's functions to drivers of one principal by comparing `group()`.
- A group's functions move to another space only once the others attached are released.
- A DMA fault reports the DMA requester id, which for a function below a PCIe-to-PCI bridge names function 0 of device 0 of the bridge's secondary bus.
- On a machine whose root ports lack ACS, the whole hierarchy below the host bridge is one group.
