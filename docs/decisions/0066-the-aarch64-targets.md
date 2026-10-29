# 0066. The AArch64 targets

Status: accepted

## Context

Language 1 had three concrete targets, all x86_64. NaN OS is meant to run on the machines agents run on, many of which are AArch64, and a language version fixes its descriptors ([descriptors](../spec/27-program-model.md#descriptors)), so an architecture added after the freeze waits for a new version. Language 1 is not frozen. An AArch64 target must keep every guarantee the x86_64 ones give: one meaning on every target, a closed list of stubs and privileged operations per descriptor, kernel tests that boot under an emulator, and programs the bootstrap and the NaN toolchain both build. x86_64 defines what a kernel observes through PC conventions (port I/O, the 16550 at `0x3F8`, PVH, `isa-debug-exit`); AArch64 has none of these.

## Decision

- **The same three kinds per architecture.** `aarch64-host`, `aarch64-uefi` and `aarch64-bare` mirror the x86_64 targets, with the same root grants, entry heads, stacks, boot heap and platform packages. Rules name kinds (host, UEFI, bare, machine targets), so most of the specification holds for both architectures unchanged. Target variant selectors gain `aarch64` beside `x86_64`, and `aarch64-nanos` is reserved beside `x86_64-nanos`.
- **QEMU's `virt` board as the machine platform.** The descriptors fix a GICv3, the PL011 UART, the memory base and PSCI through `hvc`, as the x86_64 descriptors fix the PC's devices. `aarch64-bare` images are ELF executables loaded at their physical addresses, entered at EL1 with the device tree at the start of memory, as QEMU boots a non-Linux image, so the M4 loader places the segments of either architecture's kernel alike.
- **Semihosting is the exit port.** It is the only way a `virt` guest ends the emulator with a status. On a machine without a debugger the semihosting instruction is undefined, so the exit must leave the CPU stopped there rather than trapping on.
- **Vectors are interrupt numbers.** Interrupt vectors 16 to 255 are the interrupt controller's interrupt IDs, so a binding names the interrupt a device raises and private interrupts such as the timer's can be bound; vectors 0 to 2 classify the exceptions the hardware groups by syndrome. A binding enables its interrupt, since a GIC delivers nothing it has not enabled.
- **Kernel code runs on `SP_EL0`.** An exception taken from code on `SP_EL0` switches to `SP_EL1`, which holds the CPU's interrupt stack, so every exception lands on that stack as the x86_64 seam's switch makes it, and task stacks keep their static bounds. AArch64 has no per-vector stack switch, and a seam learns an interrupt's vector only by acknowledging it, so every vector takes `ist` 0.
- **Fast interrupts stop CPUs in a halt.** A group 0 interrupt arrives as FIQ, which disabling maskable interrupts does not block, so a halt stops a CPU spinning with interrupts disabled, as the non-maskable interrupt does on x86_64. The seam unmasks fast interrupts while it serves any other exception, so a halt also reaches a CPU inside an interrupt fn.
- **Per-architecture intrinsics.** `hw::Ring0` gains `write_ttbr0`, `read_sysreg` and `write_sysreg` over a closed enum of system registers, since AArch64 encodes the register in the instruction; `hw::Ports.map` and each architecture's other intrinsics are environment errors on the other.
- **`"sysv"` on both host targets.** It names the System V ABI's convention of the target's architecture, AAPCS64 on `aarch64-host`, so `host_platform` and any library declaring C functions holds on both host architectures. `"aapcs64"` is the UEFI firmware's convention, which reserves `x18`.
- **Host images run on their own architecture.** A host image is a native executable with no emulator, so it runs only on its own architecture. Each host target names a linker: `cc` on a machine of its architecture, the GNU cross compiler `<arch>-linux-gnu-gcc` elsewhere. On a machine of the other architecture `nan run` starts nothing and test runs fail as `test.emulator-unavailable`. `any` still stands for `x86_64-host`, so that a package's bare `nan run` and the std tests keep one meaning; on an AArch64 machine every `any` package's tests therefore fail as `test.emulator-unavailable`.
- **The bootstrap generates AArch64 code with Cranelift's arm64 backend.** That backend has no stack switching, so the bootstrap switches task stacks on AArch64 through a compiler stub of its own.
- **The NaN toolchain allocates AArch64 registers as it does x86_64 ones.** Its code generator selects AArch64 instructions into the same virtual-register code, which one liveness analysis and linear-scan allocator serve through an interface each instruction set implements, and lays out frames as Cranelift's AArch64 backend does. A function's stack use is its frame, so the toolchain's frames must stay as close to the bootstrap's as on x86_64 for both to accept and reject the same `limits stack` bounds.

Rejected alternatives:

- **The Linux arm64 boot protocol for bare images:** it passes the device tree in `x0` but needs a raw, relocatable image, so bare images would differ in format per architecture, and the M4 loader with them.
- **GICv2:** it serves at most 8 CPUs, and QEMU's `virt` board without security extensions does not guarantee group 0 delivery as FIQ.
- **PSCI `SYSTEM_OFF` as the exit:** it ends QEMU with status 0 whatever the program wrote.
- **A convention per architecture on the host targets:** every package declaring C functions would need a variant module per architecture, and the variants of a `pkg` or `pub` item must share its interface, convention included.
- **Running AArch64 host images under `qemu-aarch64`:** its user-mode emulation implements no io_uring, so the runtime cannot start.
- **An AArch64 generator in the toolchain giving every value a stack slot:** it needs no allocator, but its frames exceed the stack bounds the bootstrap's frames meet.

## Consequences

- Semihosting gives a guest access to the host's files; NaN code reaches it only through the exit stub, but trusted code that executes arbitrary bytes could reach it under the emulator.
- AArch64 host programs are linked only where an AArch64 C library and linker are installed, and run only on AArch64 Linux.
- AArch64 machine runs use QEMU's TCG, without the KVM acceleration the x86_64 runs have, so each boot costs more processor time.
