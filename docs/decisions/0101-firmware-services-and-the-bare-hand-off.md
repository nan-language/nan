# 0101. Firmware services and the bare hand-off

Status: accepted

## Context

A UEFI program reached the firmware only through `write_console` and `read_file`, and `exit_services` returned the `boot::Kernel` of the same image. The NaN OS loader must count and record boots, keep variables only boot services reach, measure what it boots into the TPM, seed the kernel with entropy, pass the firmware's tables on, let a human choose recovery, and start a separately built bare kernel. Foreign fns are unresolved on machine targets and NaN has no inline assembly, so firmware protocols and the jump into another image are reachable only through std. Each firmware service lowered as hand-written IR, twice: in the bootstrap and again in the toolchain.

## Decision

- **Services are target-dependent operations implemented in NaN.** Each `boot::Services` operation that calls the firmware stays a std operation without a body, with the class, draws and failures its chapter states, and its lowering calls the function of the same name in `uefi_platform`, as `sys` operations call `host_platform`. `uefi_platform` writes the protocols in NaN over a few supplied primitives: a firmware call of five arguments, the system table, the image handle, a region from its base and length, and for the hand-off the content-blind copy of Untrusted bytes, the cleaning of data caches and the jump. One implementation serves both compilers, and the checker and prover read it like any std code. Operations that only read the firmware's tables, `config_table` and `acpi_table`, and the kernel's accessors of the start information are std code in `boot` itself.
- **The hand-off produces a PVH start information on both architectures.** `Machine.enter` lays out a start information of the PVH layout, version 1, in pages below 4 GiB, with the modules copied there, ends boot services and fills its memory map from the final firmware map. x86_64 enters by the PVH protocol, as QEMU already boots `x86_64-bare`; AArch64 enters with the device tree in `x0` and the same structure in `x1`, so a bare kernel reads one layout on both, and an AArch64 machine whose firmware publishes ACPI rather than a device tree still gives the kernel its memory map. With both registers zero, an `aarch64-bare` image keeps finding QEMU's device tree at the start of memory.
- **The start information's memory is never usable memory.** The usable memory walks leave out the structure, its lists, its command line and each module with its name, wherever the loader put them, so `regions()` and the boot area never hand them out and a module becomes a region only through `Module.into_region`.
- **`enter` is a trusted primitive.** Nothing checks that the segments hold an image entered at `entry`; the loader's trusted code states that premise. Its class is the greatest, as for `firmware_call`.
- **Fixed-size buffers where no allocator is given.** Operations without an `alloc` draw nothing, as their digests state: paths and variable names hold at most 255 UTF-16 code units and a measured event at most 1,024 bytes, in the caller's frame.
- **The descriptor machine keeps PCI memory below 4 GiB.** OVMF otherwise places 64-bit BARs above 4 GiB, which no entry root maps, and a kernel entered after the firmware could not reach its devices. The emulator prefers an OVMF build with the TCG2 protocol where the host has one, so `measure` reaches the TPM the machine has.

Alternatives rejected:

- **Services lowered as IR in both compilers:** every protocol twice, in a form no checker reads.
- **Services as std code with bodies over a firmware-call primitive:** a body's class is computed from its calls, so a write would be as traceless or as irreversible as the primitive, not as its chapter states.
- **A Linux-style boot protocol or the device tree alone on AArch64:** the kernel would read two layouts, and the device tree is absent when the firmware publishes ACPI.
- **Marking the start information reserved in its own memory map:** a bare kernel booted by another PVH loader, such as QEMU's direct boot with modules, would still hand out the modules' memory.

## Consequences

- Booting through a loader and booting the kernel directly run the same kernel code; the M4 loaders now enter the M4 kernel, and its milestone checks both.
- A kernel entered after the firmware finds memory the firmware used neither zeroed nor reserved, and devices where the firmware configured them.
- The AArch64 hand-off, cache cleaning and MMU-off entry run only on machines with `qemu-system-aarch64`; the encodings are fixed bytes whose instructions their unit tests check.
