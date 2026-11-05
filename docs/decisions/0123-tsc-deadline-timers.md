# 0123. TSC-deadline timers

Status: accepted

## Context

Each x86_64 CPU's timer was its local APIC timer one-shot ([0081](0081-the-kernel-clock.md)): a deadline of the clock became a countdown at the APIC timer's own rate, which every CPU measured over 1 ms as it started and which drifts from the clock. Most x86_64 CPUs have the TSC-deadline mode, in which the timer fires when the time-stamp counter reaches a value written to `IA32_TSC_DEADLINE`, and the clock is that counter wherever it is invariant ([0121](0121-kernel-clock-sources.md)).

## Decision

- **Deadline mode where it shares the clock's counter.** A CPU whose CPUID leaf 1 reports the mode, on a kernel whose clock is the time-stamp counter, puts its timer in TSC-deadline mode and records that in its CPU record. Its rate is the clock's own, so it measures nothing.
- **The same programming either way.** The runtime still turns the delay to the next firing into ticks of the CPU's timer rate. In deadline mode it writes the time-stamp counter plus those ticks to `IA32_TSC_DEADLINE`; otherwise it writes the countdown. A fence orders the mode switch before the first deadline, as the mode requires.
- **One-shot elsewhere.** A CPU without the mode, or a kernel clocked by the HPET or the PM timer, keeps the measured one-shot countdown.

This supersedes [0081](0081-the-kernel-clock.md)'s rejection of the TSC-deadline mode.

## Consequences

- A CPU in deadline mode starts taking interrupts 1 ms sooner, and its timer fires at the clock's own deadline instead of an estimate of it.
- The emulator under TCG emulates no TSC-deadline mode, so only machines run under KVM, and hardware, run timers in it.
