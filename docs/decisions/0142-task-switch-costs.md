# 0142. Task switch costs

Status: accepted

## Context

NaN OS budgets a task switch at 200 ns and a switch between two tasks using floats at 300 ns. Kernelbench timed both on the reference machine (AMD Ryzen 7 2700, under KVM) at 1.5 and 1.7 µs on one CPU, and at 3.6 to 4.7 µs under the default policy, which wakes each task on another, idle CPU. Counting the guest's exits and instructions showed where the time went:

- Each switch-in programmed the CPU's timer again, writing `IA32_TSC_DEADLINE` even when the deadline was unchanged. The write exits to the hypervisor, about 1.1 µs under KVM; on hardware it is a serialising write.
- Cranelift lowers a sequentially consistent store on x86_64 to a store and `mfence`, which the runtime issues about fifteen times per switch: 190 ns in all on this CPU. `xchg` orders the same, as GCC and LLVM lower such a store, in a fraction of the time, and under x86_64's total store order a plain store already releases a lock word.
- A wake on an idle CPU costs an interrupt and a halt's exit, so a ping-pong under the default policy times wake-ups across CPUs, not switches.

## Decision

- **A switch programs the timer only when its need changes.** The timer is programmed for the earliest of a timer's window and what the running task needs: the end of its slice and the CPU's next pacing tick. Each programming records that need in the CPU's executor thread, and a switch-in programs the timer again only when the need it computes differs. A timer added with an earlier window programs it, as before, and one cancelled leaves an early firing, which programs it again.
- **Stores follow total store order on x86_64.** Lowering stores sequentially consistent through an `Fx` operation that is an exchange on x86_64 and Cranelift's `atomic_store` elsewhere, and frees each lock word through one that is a plain store on x86_64 and a store-release elsewhere.
- **Kernelbench times switches on one CPU.** Its policy keeps every task on CPU 0, and its owners of extended state hold values in their vector registers, so its times are those of a switch between tasks, and between tasks using floats.

## Consequences

- On the reference machine a switch takes about 245 ns and a switch between owners about 400 ns, from 1.6 and 1.7 µs. Neither budget holds, and budgets this machine meets are 250 ns and 425 ns:
  - `xsavec` and `xrstor` take about 135 ns there, more than the 100 ns the budgets leave between the two switches, so on this CPU, which has no usable `xsaves`, a switch between owners costs at least a switch plus 135 ns.
  - The rest of a switch is about 830 cycles and 1,470 instructions, spread over the scheduler's round trip through its own stack, a dozen locked instructions, two clock reads and the charging of the time each task ran under the runtime lock, and the policy's hooks; meeting 200 ns takes a switch that passes from task to task without those, not a local saving.
- A kernel that cancels its earliest timer and then only switches takes one early timer interrupt where a switch used to program the timer later.
