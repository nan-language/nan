# 0081. The kernel clock

Status: accepted; its pacing of idle CPUs superseded by 0107

## Context

A kernel had no clock: `boot::Kernel` yielded no `sys::Clock`, so kernel code could not sleep, time out a request or schedule a retry, and the timer only paced preemption at an uncalibrated period. NaN OS also needs time finer than a millisecond for accounting, a wall clock distinct from monotonic time, and a way to hand untrusted code a clock too coarse to time side channels.

## Decision

- **Nanoseconds inside, milliseconds beside.** The runtime keeps every clock in nanoseconds: the host's monotonic clock, the virtual clock and the kernel clock. The `_ns` operations take and give nanoseconds; the millisecond forms stay as conversions, saturating, so existing code keeps its meaning.
- **Resolution is an entry's state.** A clock entry records a resolution, and `coarse` derives an entry with the coarser of its source's and the one asked. Readings round down and waits end only when the elapsed time is a multiple of it, so a coarse clock gives no finer timing through its sleeps or deadlines than through its readings. Deriving keeps revocation working as for any attenuation. An executor records a resolution too, set by `task::Executor.coarse`, and the delays timed on the clock it records, a component's `after` and `after_ns`, end on its grid, so a component given a coarse executor gets no finer timer from its own delayed messages.
- **The wall clock is a capability of its own.** `sys::WallClock` reads UTC and may jump, so code that needs monotonic time cannot mistake it for the clock. The host reads the real-time clock at each call; the test world reads the virtual clock from the epoch, keeping runs reproducible; a kernel reads its descriptor's wall clock source once as the root grant is minted and advances it by the kernel clock, since the CMOS clock, the PL031 and `GetTime` are slow or unavailable later.
- **The counter is the clock.** The kernel clock scales the time-stamp counter or the virtual counter to nanoseconds with a 32-bit fixed-point factor, and a shared latest reading keeps it monotonic across CPUs whose counters drift. On x86_64 one 10 ms window of the PIT's channel 2 measures the counter's rate in `Init`, as the descriptor's machine has a PIT and that needs no CPUID leaf that emulators omit; each CPU then measures its local APIC timer against the clock over 1 ms as it starts taking interrupts, after the firmware no longer owns the timer.
- **One timer list, one-shot timers.** The kernel shares the seeded scheduler's sorted timer list, behind a lock of its own. Each CPU programs its timer one-shot for the earlier of its next pacing tick and the earliest deadline, so a deadline below a pacing period fires on time; the CPU arming a new earliest deadline reprograms its own timer. Expiry and cancellation take a timer out under the lock and deliver it outside it, since delivering a deadline cancels a region, which may cancel other timers.
- **Pacing stays 10 ms.** A bound timer fn runs on pacing ticks alone, so preemption and kernels counting ticks keep their period whatever deadlines are armed.

This supersedes [0035](0035-the-kernel-runtime.md)'s periodic timer.

Alternatives rejected:

- **Replacing milliseconds by nanoseconds:** every program, test and example would change for no gain in meaning.
- **A per-CPU timer list:** a deadline armed on one CPU and passed on another would need an inter-processor interrupt, which needs a vector.
- **Periodic ticks expiring deadlines:** every deadline would be up to 10 ms late.
- **The TSC-deadline mode and HPET:** emulators and machines differ in offering them, while the local APIC's one-shot mode exists wherever the descriptor's timer does.

## Consequences

- The x86_64 kernels spend 10 ms in `Init` and 1 ms per CPU at the anchor measuring rates.
- Every clock reading on a kernel touches one shared word, which bounds how often CPUs can read the clock in parallel.
- A deadline wakes its task on the CPU whose timer passes it, and other idle CPUs see the task at their next tick.
- The wall clock of a kernel does not follow later changes to its source.
