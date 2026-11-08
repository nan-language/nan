# 0162. Platform interrupt lines

Status: accepted

## Context

The runtime owns the I/O APIC pins and SPIs it installs lines on ([0157](0157-msi-and-intx-interrupts-of-pci-functions.md)), and `hw::PciFunction.intx` was the only way to reach one, always level-triggered and active low. A platform device, such as a 16550 UART an ACPI `_CRS` describes or a device tree node's `interrupts`, has a line of its own with its own trigger mode and polarity, ISA lines being edge-triggered and active high. A kernel mapping the I/O APIC itself would race the runtime on its select and window registers and could undo its pins. NaN OS's console driver is interrupt-driven.

## Decision

- **One public install of a line.** `hw::IrqShared.install_platform` takes the line, its trigger mode and its polarity, routes the line as `intx` does, and installs it through the runtime's line install, which now takes the mode. `intx` is that install, level-triggered and active low, and it remains a confined operation while `install_platform`, which reaches any line, is a device operation.
- **The mode is the first device's.** A line no device holds takes the mode of its first device. A level-triggered device joins a level-triggered line of its polarity; an edge-triggered line takes no other device, since an edge two devices raise together arrives once. Any other install is refused, read back from the I/O APIC entry or the distributor's configuration, so the runtime keeps no mode of its own.
- **Resolution is the caller's.** The caller passes the global system interrupt and the mode, as the MADT's interrupt source overrides and `_CRS` give them on x86_64, or as `interrupts` gives them on AArch64: both need the firmware's tables, which the OS's ACPI and device tree layers read.
- **No polarity on AArch64.** The GIC takes level-sensitive SPIs active high, so the polarity has no effect there.

## Consequences

- A kernel takes a platform device's interrupt under the `device` grant without mapping an interrupt controller, and NaN OS's 16550 driver runs by interrupt.
- A refused install of a held line is `Unsupported`, as a line the machine cannot deliver is.
