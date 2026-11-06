# 0157. MSI and INTx interrupts of PCI functions

Status: accepted

## Context

`hw::PciFunction.msix` was a function's only interrupt ([0108](0108-io-domains-and-confined-device-grants.md)), and a hot-plug port with MSI alone got no slot interrupt ([0152](0152-pci-buses-ports-and-hot-plug.md)). QEMU's AHCI controller, many real devices and PCIe ports offer MSI without MSI-X, and some functions offer only INTx, a level-triggered line that functions share. NaN OS's device manager gives a function its MSI-X vectors, else one MSI vector, else its INTx line as ACPI's `_PRT` routes it. Every function is listed with INTx disabled, and on x86_64 nothing programmed the I/O APIC, whose compatibility-format interrupts VT-d blocks once it remaps interrupts.

## Decision

- **MSI through std.** `msi` installs through `install_validated` as `msix` does and programs the MSI capability in std, its 32-bit or 64-bit address and per-vector mask read from the capability, with one vector enabled. A port without MSI-X takes its slot interrupt the same way.
- **One way of signalling at a time.** Each of `msix`, `msi` and `intx` disables the other two. Enabling MSI and MSI-X together is undefined, and INTx asserted beside messages would raise a line no driver expects.
- **Lines are the runtime's.** The interrupt controllers stay the runtime's ([0099](0099-per-cpu-vector-bindings-and-msi-routing.md)), so the runtime owns the I/O APIC pins `intx` installs on. A line is installed for the boot CPU in the row of every CPU: on x86_64 its pin, level-triggered and active low, points at a vector no device uses on any CPU, through a level-triggered remapping entry validated for the I/O APIC's requester id once VT-d remaps interrupts; on AArch64 it is the shared peripheral interrupt, made level-sensitive. Later devices on the line join its chain, and the last device's uninstall masks the pin and clears its entry, as an SPI without devices is disabled, so an asserted line never reaches a vector without devices.
- **Routing is the caller's.** The caller passes the line: `_PRT` needs an AML interpreter, which belongs to the OS's ACPI layer, not to std. On AArch64 the device tree's `interrupt-map` is the caller's too, so `intx` has one meaning on both architectures. Std finds the I/O APIC holding the line in the MADT and its requester id in the DMAR's device scopes.
- **INTx ends with the function.** `release` and settlement disable INTx with bus mastering and decoding. A removed function writes no configuration, and its interrupts stay installed until uninstalled, as for MSI-X.

Alternatives rejected:

- **`intx` with the routing computed in std:** x86_64 needs AML evaluation, and an AArch64-only form would split the API.
- **Lines for the CPU the caller names:** the devices on one line would need one CPU, which a confined caller cannot know, and a refusal per CPU.
- **Std writing the I/O APIC:** its select-and-window registers need the lock the runtime's uninstall takes, and the runtime must mask the pin then.

## Consequences

- A function with MSI alone, or INTx alone, gets an interrupt without a trusted premise, and NaN OS's device manager falls back from MSI-X to MSI to INTx.
- Lines arrive at the boot CPU alone, and a vector serving a line is no MSI vector of any CPU while a device is installed on the line.
- A driver that uninstalls while its function still asserts INTx leaves the line raised for the other devices on it until the function is released.
- Lines installed before `boot::Kernel.iommu` stop once it remaps interrupts, as earlier compatibility-format messages do.
