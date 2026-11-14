# 0198. No `monitor` on the x86_64 machine under TCG

Status: accepted

## Context

Under TCG the x86_64 machine runs QEMU's `max` model ([0080](0080-the-descriptor-machine-and-its-kept-state.md)), which reports `monitor` and `mwait`. TCG's `mwait` halts only on a machine of one processor; on the machine's four it pauses and returns at once. A policy whose `idle` gives `hw::Idle.Mwait` then spins every idle CPU, holding a host core each, and cannot tell such a wait from a short real one.

## Decision

The x86_64 machine under TCG runs `max` without `monitor` (`-cpu max,-monitor`), so CPUID reports neither `monitor` nor `mwait`, and `hw::Idle.Mwait` halts as on any CPU without them.

Alternatives rejected:

- **Recognising TCG in the runtime** and halting there: a kernel would branch on its emulator, and an `hw::Ring0.monitor_wait` would still spin.

## Consequences

- Under TCG, `hw::Ring0.monitor_wait` returns at once, and an idle machine leaves its host's cores idle.
