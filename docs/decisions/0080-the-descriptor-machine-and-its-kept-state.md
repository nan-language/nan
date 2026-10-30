# 0080. The descriptor machine and its kept state

Status: accepted

## Context

NaN OS runs every test and sandbox under `nan run` and test runs, on the machine the descriptors fix ([descriptors](../spec/27-program-model.md#descriptors)), so a rule of an operating system that this machine cannot exercise stays untested. The machine had one FAT boot volume, no NIC, IOMMU, TPM or watchdog, one serial port shared by an agent and a human, QEMU's default CPU model without 1 GiB pages, TSC-deadline or invariant TSC, and every reset ended the run, so a store's log, networking, DMA confinement, measured boot, recovery after a halt and boot counting had nowhere to run. Test runs must stay deterministic and independent ([0034](0034-machine-test-runs-boot-the-emulator.md)), and the emulator must still end with the process that starts it ([0037](0037-nan-run-supervises-the-emulator.md)).

## Decision

- **One machine with every device.** Both architectures' machines gain a 256 MiB `virtio-blk-pci` disk, a `virtio-net-pci` device on QEMU's user network without option ROM, an `i6300esb` watchdog, a TPM 2.0 (CRB on x86_64, TIS on AArch64) and a second UART as the console port. x86_64 puts an Intel IOMMU with interrupt remapping in front of its PCI devices, AArch64 an SMMUv3 and the GIC's ITS. Each virtio PCI device is modern and behind the IOMMU (`iommu_platform=on`), so a driver negotiates `VIRTIO_F_ACCESS_PLATFORM` and DMA confinement is testable; a UEFI image's boot volume is such a device too, first in the boot order, so a disk the machine wrote never shadows the image under test.
- **CPU models by accelerator.** x86_64 runs under KVM with the host's model and invariant TSC when the host is x86_64 and `/dev/kvm` is usable, otherwise under TCG with `max`. AArch64 stays on TCG, since semihosting, its exit port, needs it ([0066](0066-the-aarch64-targets.md)), with `cortex-a72`: under TCG's `max` model the UEFI firmware does not boot within a test's time. MWAIT is never passed through under KVM: `cpu-pm=on` would let a guest hold a host processor.
- **`swtpm` serves the TPM.** It runs beside the emulator in the machine directory, so that the socket paths, relative to it, stay below the 108-byte limit of a Unix socket name. The toolchain ends it once the emulator ends, and it ends by itself when the emulator disconnects, so a toolchain ended by a signal leaves none behind. Its absence makes the emulator unavailable.
- **`nan run` keeps the machine's state** in `.nan/machine/<target>/`: the disk, made blank and sparse when missing, the firmware's variables, copied from the template the firmware's code is paired with, and the TPM's state. The boot volume is still rebuilt each run. The console port is a socket there; the serial port carries standard input as well as output. `--keep-resets` drops `-no-reboot`, so a reset, the watchdog's included, reboots the machine with all of it.
- **Test runs boot a fresh machine.** A blank disk is made once per test image, and the disk, the variables' template and the boot volume are opened with `snapshot=on`; each boot gets a new TPM state, a network restricted to the machine, an unconnected console port, empty serial input and `-no-reboot`.

Alternatives rejected:

- **NVMe instead of virtio-blk:** a driver for it costs more, and virtio-blk shares its transport and queues with the entropy device every kernel already drives.
- **Device options per package or per run:** a descriptor is fixed by the language version, and a machine that varies would make a run's outcome depend on configuration.
- **KVM on AArch64:** the exit port would stop working.
- **A PCI serial card as the console port:** both architectures' boards offer a second UART at a fixed address with QEMU 10.
- **Keeping state for test runs:** a run's outcome would depend on earlier runs.

## Consequences

- Machine runs need `swtpm`; CI installs it.
- A virtio PCI driver that does not accept `VIRTIO_F_ACCESS_PLATFORM` fails feature negotiation; the M4 kernel's entropy driver accepts it when offered.
- Under KVM the CPU features a program sees depend on the host's processor; under TCG they depend on QEMU's `max` model on x86_64 and are Cortex-A72's on AArch64.
- Each machine boot also starts a TPM emulator.
- Two `nan run` of one target in one workspace at once share the machine directory and conflict.
