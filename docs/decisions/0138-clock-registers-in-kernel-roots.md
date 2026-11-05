# 0138. Clock registers in kernel roots

Status: accepted

## Context

Where the kernel clock is the HPET ([0121](0121-kernel-clock-sources.md)), the runtime reads its counter through whatever root the CPU runs on. `write_cr3`'s soundness condition asked a kernel's root to map the image, the memory it reserves, its frames, regions and windows, but not the HPET, so a kernel mapping only those halted on a page fault at its first clock read, and only on machines without an invariant TSC.

## Decision

- **The soundness condition names the clock's registers.** A root `write_cr3` installs maps them uncached at their addresses, as it maps a window `of_bar` gave.
- **The kernel learns them.** `boot::Kernel.clock_registers` gives their extent: the HPET's 1 KiB where the clock reads it, empty otherwise, so a kernel maps exactly what the runtime reads without parsing ACPI or knowing which source the runtime chose.

Alternatives rejected:

- **A mapping the runtime owns:** the runtime has no address space of its own on x86_64; a reserved window would itself need a soundness clause, as the load window's top-level entry does, and would cost every kernel a table entry for a device most machines never read.
- **Reinstalling the entry root around each reading:** each reading would flush translations, and the clock is read on every scheduling decision.

## Consequences

- A kernel installing its own root maps `k.clock_registers()` beside its windows; on machines with an invariant TSC the extent is empty.
