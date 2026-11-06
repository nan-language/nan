# 0152. PCI buses, ports and hot plug

Status: accepted

## Context

`boot::Kernel.pci_functions` listed the endpoints of bus 0 alone ([0108](0108-io-domains-and-confined-device-grants.md)), because a requester id below a bridge may be aliased or reach a peer without the IOMMU. Real machines and QEMU's `pcie-root-port` put NVMe drives, NICs and hot-pluggable slots behind ports, and NaN OS's device manager scans every bus and serves native PCIe hot plug: a card entering a slot is listed and matched, a card leaving it is cut off. A function's record, its windows and its IOMMU entry were tied to one fixed address for the whole boot; after a removal, a new card at the same requester id or BAR address could inherit the old function's attachment or window.

## Decision

- **One depth-first scan below every host bridge.** Std's `hw` finds the hosts in the MCFG table on x86_64 (ECAM, every segment, the configuration ports only for a segment 0 it lacks) and the `pci-host-ecam-generic` node on AArch64, and scans each iteratively, with an explicit stack so the kernel's stack stays bounded. It keeps the bus numbers and BARs firmware assigned and numbers, and gives windows to, the bridges no firmware numbered, as AArch64's runtime already gave BARs. `k.pci_functions` and `k.pci_ports` share the scan through a PCI record the runtime state names.
- **Isolation from the path, not the bus.** A function attaches to a domain only when the IOMMU translates its own requester id and no peer reaches it otherwise: every bridge above it is a PCIe port with ACS source validation, redirection and upstream forwarding, which the runtime enables, and a multi-function device's function redirects its peers. Functions behind conventional bridges are listed but attach to no domain; under an IOMMU they cannot attach to physical memory either, since their DMA arrives under an alias. VT-d context tables are made per bus as functions attach, and the SMMUv3 stream table covers the host's buses.
- **Removal by generations.** Each bus of a host has a generation. A port's rescan or power-off raises the generations of its buses and clears their IOMMU entries under a lock that also orders every configuration write a function's operations make, so a record found before reads as removed: it writes no configuration, attaches nowhere and gives no window, and its release only frees it. Records never reference a port, so a port and the functions below it are released independently.
- **No address reused below a port.** A rescan gives every BAR it finds an address above all the port's window gave or held before, so a window of a removed function never reaches a later one. A card removed and inserted repeatedly eventually exhausts the window, and its BARs then get no address.
- **Ports as resources, slots through their own operations.** `hw::PciPort` is linear and confined, like `hw::PciFunction`: slot events are read and cleared, power is switched and the hot-plug interrupt installed only through it, configuration writes staying the runtime's. Slot control takes 16-bit writes, which a new supplied configuration write gives, because every write to it is a command. The hot-plug interrupt uses the port's MSI-X entry; a port with MSI alone refuses it.

Alternatives rejected:

- **Function records referencing their port:** a port released before the functions below it would leave dangling references, or force reference counts.
- **Reusing a removed function's BAR addresses once it is released:** a window given before the release can outlive it.
- **A tree of ports returned level by level:** bus numbers and windows of unnumbered bridges depend on the whole subtree, so the scan must see it at once.

## Consequences

- A kernel reaches functions behind root ports, switches and bridges on both architectures, keeps the IOMMU's isolation for those behind ACS-capable PCIe ports, and serves native hot plug of endpoints.
- Without an ECAM, as through the configuration ports, no function below a port is isolated.
- A rescan enters no bridge: a hot-added switch is not scanned, nothing giving it bus numbers.
- An MSI-X interrupt installed for a removed function stays until uninstalled, and a function later found at its requester id may raise it.
