# 0110. AArch64 machines discovered at boot

Status: accepted

## Context

The AArch64 machine descriptors fixed QEMU's `virt` board ([0066](0066-the-aarch64-targets.md)): memory at `0x4000_0000` with the image linked at `0x4020_0000`, the PL011 and the PL031 at fixed addresses, a GICv3 whose redistributor `i` served CPU `i`, CPUs found by asking PSCI about consecutive affinities, PSCI through `hvc`, and the first gigabyte mapped as device memory. NaN OS runs on AArch64 boards and servers whose memory, UART, interrupt controller, CPUs and PSCI conduit differ, as the device tree or the ACPI tables their firmware passes describe; their firmware may enter the image at EL2 and place it elsewhere. The support routines and the halt path use these addresses, and kernel code cannot change them.

## Decision

- **Discovery in `Init`.** On the AArch64 machine targets the runtime reads the machine into a record once the boot area is placed, before anything uses the record: from the device tree the image entered with, the one a UEFI configuration table lists, or else the ACPI tables the start information or the configuration table reaches. The record names the UART halt reports are written to, the PL031, the distributor, the redistributor regions, the ITS, the PSCI conduit and the CPUs, each with its redistributor, found by its affinity in `GICR_TYPER` unless the MADT names it.
- **The device tree is read as Linux reads it.** The UART is the node `/chosen`'s `stdout-path` names, through `/aliases` and before any `:`, else the first PL011; PL011s, SBSA generic UARTs and memory-mapped 16550s are recognised; a device's `reg` is translated through its ancestors' `ranges`; a node whose `status` is not `okay` is skipped; CPUs are the nodes of `device_type` `cpu`. ACPI gives the same facts through the MADT, the FADT's boot architecture flags and the SPCR.
- **What a machine lacks degrades, it does not trap.** Without a UART halt reports are not written, without a PL031 the wall clock gives no time, without a PSCI conduit only the boot CPU runs, and a CPU without a redistributor is left out. A kernel without a distributor or a redistributor for its boot CPU cannot start, so the runtime is unavailable.
- **PSCI through the conduit found.** A `Smc` stub joins `Hvc`, and `firmware_call` uses the machine's conduit.
- **Memory attributes from the memory map.** The bare entry seam installs a boot root mapping the first 512 GiB as device memory that no code runs from, and the gigabytes the image spans as normal memory. `Init` then fills the entry root from the platform's memory map, with a level-2 table per gigabyte below the top of memory: 2 MiB blocks holding memory or the image are normal memory, every other block device memory. It installs that root before writing the runtime's tables, after invalidating the cache lines of the tables it wrote through the device mapping. `exit_services` fills it the same way on a UEFI target.
- **EL2 entry.** Both AArch64 seams drop from EL2 to EL1 when entered there, with EL1 running in AArch64, the counters, the GIC CPU interface and floating point untrapped, and its identification registers the processor's.
- **Relocated bare images.** An `aarch64-bare` image stays linked at `0x4020_0000`, but the image writer lists every absolute address its relocations wrote in a table after its segments, and records the linked addresses of a header, the image and the table. The seam computes how far the image was moved, maps it, turns the MMU on and adds that distance to each listed address, so a loader may place the image at another 2 MiB boundary.
- **The emulator boots without ACPI.** The `virt` machine runs with `acpi=off`, so the firmware passes the device tree on and a kernel the M4 loader enters keeps its PL031; ACPI discovery is booted separately.

Alternatives rejected:

- **One descriptor per board:** every new board would wait for a language version, and a kernel image would run on one machine.
- **Discovery written in NaN:** `Init` runs before the heap, the tasks and the trap machinery that NaN code relies on, and the halt path needs the UART before any of them.
- **Reading the PL031 from the DSDT:** it needs an AML interpreter; an ACPI machine's kernel reads its clock from its own drivers.
- **Position-independent machine code:** every data access would go through the global offset table, where the relocation table costs 8 bytes per absolute address and runs once.
- **Relocating with the MMU off:** literal pools leave absolute addresses unaligned, which device memory faults on.
- **Normal memory everywhere until `Init`:** speculative accesses may reach devices.

## Consequences

- An AArch64 kernel image runs on any machine its firmware describes with a GICv3, entered at EL1 or EL2 below 512 GiB.
- A kernel entered from ACPI firmware has no wall clock from the runtime.
- The bare image grows by 8 bytes per absolute address, and each boot of it walks the table once.
