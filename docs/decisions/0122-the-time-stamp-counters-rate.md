# 0122. The time-stamp counter's rate

Status: accepted

## Context

A kernel clocked by the time-stamp counter measured its rate against 10 ms of the PIT's channel 2 ([0081](0081-the-kernel-clock.md)). That costs 10 ms of every boot and depends on a PIT that recent machines may lack or gate off. Intel CPUs report the counter's nominal rate in CPUID leaf `0x15`, and the HPET and the PM timer the clock can now read ([0121](0121-kernel-clock-sources.md)) count at rates the platform states.

## Decision

- **Reported first.** Where leaf `0x15` gives both the ratio of the counter to the core crystal and the crystal's frequency, the rate is the crystal's frequency times that ratio, and nothing is measured.
- **Measured against a platform counter next.** Otherwise the counter is measured over 10 ms of the HPET or else the PM timer, the counters the clock would read in its place.
- **The PIT last.** Only a machine with neither measures against the PIT's channel 2, as before.

This supersedes [0081](0081-the-kernel-clock.md)'s measurement against the PIT alone.

Alternatives rejected:

- **Leaf `0x16` for the crystal where leaf `0x15` omits it:** it gives the base frequency in MHz, too coarse for a clock, and only some CPUs list it.

## Consequences

- A machine reporting leaf `0x15` starts its clock without waiting; others still spend 10 ms.
- The emulator never reports leaf `0x15`, so its kernels measure against the HPET or the PM timer.
