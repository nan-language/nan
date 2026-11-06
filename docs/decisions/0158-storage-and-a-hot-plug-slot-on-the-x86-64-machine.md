# 0158. Storage and a hot-plug slot on the x86_64 machine

Status: accepted

## Context

NaN OS drives NVMe and AHCI drives and serves native PCIe hot plug ([0152](0152-pci-buses-ports-and-hot-plug.md)), and changes its machine only through what the descriptors fix ([0080](0080-the-descriptor-machine-and-its-kept-state.md)). The x86_64 machine had no NVMe controller, an AHCI controller with no drive, no PCIe port, and q35's ACPI hot plug in place of the ports' native one, so none of these could run. A slot also needs something outside the machine to insert and remove a card at a moment the test chooses.

## Decision

- **Both x86_64 machine targets gain the devices:** an NVMe controller with one 16 MiB namespace, a 16 MiB SATA disk on the AHCI controller's first port, and an empty `pcie-root-port` with native hot plug and a 2 MiB memory window, so that OVMF and SeaBIOS leave room for a card. Their images live in the machine directory beside the virtio disk, kept by runs and opened with `snapshot=on` by test runs.
- **`nan run` exposes the emulator's monitor** in QEMU's machine protocol on `monitor.sock` in the machine directory, as it does the console port. Whoever drives the run (NaN OS's harness, a person, the program harness) plugs and unplugs functions there with `device_add` on the port's bus and `device_del`, waiting for what the machine writes before each request, which keeps the order of events deterministic. QMP is the protocol QEMU keeps stable for programs.
- **Test runs have no monitor:** nothing outside a test boot changes its machine.
- The program harness makes each request of a program's `monitor` file once the program writes the line it follows.

Alternatives rejected:

- **Wiring a serial port of the machine to the monitor**, so that the machine requests its own hot plug: the monitor also adds drives from host files and writes files, so code under test could reach the host.
- **A toolchain relay turning the machine's requests into monitor commands:** a protocol of the toolchain's own, served by both compilers, for what only tests need.
- **The devices on AArch64 too:** NaN OS's need is the x86_64 machine.

## Consequences

- On bus 0 the NVMe controller takes slot 5 and the root port slot 6 with bus 1, after the network device; a UEFI image's boot volume moves to slot 7, and the ports a run adds get later bus numbers.
- QEMU serves one monitor connection at a time.
- A card removed and inserted many times exhausts the port's 2 MiB window.
