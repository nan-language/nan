# 0121. Kernel clock sources

Status: accepted

## Context

The x86_64 kernel clock scaled the time-stamp counter whatever CPUID said of it ([0081](0081-the-kernel-clock.md)). A TSC that is not invariant, as on QEMU's `max` model under TCG and on older machines, may stop in deep C-states or drift between CPUs, so the clock could stall or jump. NaN OS needs a monotonic clock on every machine it runs on.

## Decision

- **The TSC only where it is invariant.** `Init` reads CPUID leaf `0x8000_0007`; with its invariant TSC bit set the clock stays the TSC, which is the cheapest counter to read.
- **Otherwise a platform counter.** The clock reads the HPET the ACPI `HPET` table names, starting its counter, else the PM timer the FADT names, else the TSC as before. Both have a fixed rate the tables or the hardware give, so neither needs measuring.
- **One shared extension.** The PM timer counts 24 or 32 bits and an HPET may count 32. The runtime keeps the latest count any CPU read, extended to 64 bits, and extends each reading by its distance ahead of it modulo the counter's width, taking it with a compare-and-swap; a reading less than half the width behind gives the latest count. Every CPU's timer fires at least once a second, well within half the PM timer's 4.7 s wrap, so no wrap is missed.

This supersedes [0081](0081-the-kernel-clock.md)'s TSC as the only x86_64 counter and its rejection of the HPET.

Alternatives rejected:

- **The HPET or PM timer everywhere:** each reading is an uncached device access, far slower than `rdtsc`, and every scheduling decision reads the clock.
- **The PIT as a counter:** it counts 16 bits and its reading takes several port accesses.

## Consequences

- Machines without an invariant TSC, the emulator under TCG among them, read the clock through a device access and contend on one more shared word.
- Such a kernel skips the 10 ms PIT measurement in `Init`.
