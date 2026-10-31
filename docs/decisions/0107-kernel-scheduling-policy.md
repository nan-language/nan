# 0107. Kernel scheduling policy

Status: accepted

## Context

The kernel executor was a fixed support routine ([0035](0035-the-kernel-runtime.md)): one run queue shared by every CPU, idle CPUs woken by the periodic timer, no accounting and no priorities. NaN OS schedules many components on many CPUs by share, deadline and class, charges processor time to the component that used it, keeps idle CPUs asleep and wants replayable runs of chosen components. The executor and the timer lie outside kernel code, so a kernel could change none of this. `hw::switch_to` and its run-queue types described a thread switch kernel code never performed.

## Decision

- **A policy is a value of an interface, installed once.** `boot::Kernel.policy` moves a value of a type implementing `hw::Policy` into a runtime record holding the glue that calls each of its four hooks: `place`, `steal`, `slice` and `idle`. The runtime calls hooks through that record, so lowering one call of `policy` is enough to reach them and no rule ties the build to one policy type; a second call traps instead.
- **Hooks decide, the runtime keeps the queues.** Each CPU has its own queue ordered by the key `place` gives. The runtime links tasks, takes locks, steals the first task of the queue `steal` names and switches threads, so a hook only computes and returns; hooks run with maskable interrupts disabled, under `notrap`, `terminates`, `noalloc` and `stack 4_096`, which every stack a hook may run on reserves. A task made ready inside a hook skips `place`, so hooks never nest.
- **Hooks see tasks through a copy.** `hw::TaskView` copies what a policy needs: the executor's share, the CPU the task last ran on, its run time and its earliest deadline. A policy keeps any further state itself, in atomics, spin locks or per-CPU values.
- **The CPU signal wakes and preempts.** Placing a task on another CPU that idles or runs a task of a greater key sends it the CPU signal; any interrupt then preempts a task whose slice ended or whose queue holds a smaller key. The timer fires one-shot at the earliest of the next deadline, the slice's end and, while a task runs or a timer fn is bound, the next pacing tick, so an idle CPU sleeps until it has work.
- **The default policy keeps M4's behaviour.** It places a task on its last CPU if idle, else on an idle CPU, else locally, keyed by readiness order, steals round-robin, gives no slice and halts; without a bound timer fn nothing is preempted, as before.
- **Shares and accounts are executor entries.** `task::Executor.split` derives an executor entry recording a share, which the runtime never interprets, and returns a linear `task::Account` that reads the entry's run time and closes it. The time a task runs is charged at each switch to its executor's entry and every entry it derives from, so an account counts the executors derived from it, as a budget counts its sub-budgets.
- **A serial executor passes a turn.** `task::Executor.serial` derives an entry whose tasks run one at a time: the holder of its turn runs, others wait in the entry, and the turn passes on suspension, end or stop to the waiting task the entry's seed picks, as the seeded scheduler picks. The gate sits before the run queues, so it works on every target and under any policy. Tasks name the entry by handle, so revoking it frees its tasks from the turn instead of leaving them pointing at freed state.

This supersedes [0035](0035-the-kernel-runtime.md)'s one run queue and its rejection of waking idle CPUs by IPI, which the CPU signal ([0089](0089-privileged-intrinsics-and-the-cpu-signal.md)) made possible, and [0081](0081-the-kernel-clock.md)'s pacing of idle CPUs.

Alternatives rejected:

- **A compile-time rule that one policy type is lowered:** the anchor's type rule needs a final-build pass; a runtime record costs a pointer and needs none.
- **Hooks managing the queues themselves:** every policy would reimplement locking, stealing and settlement's withdrawal of ready tasks, and a bug in one would corrupt the runtime.
- **A policy written in NaN as the default:** every kernel image would lower it and reserve its state; the runtime's default is a few comparisons.
- **Charging time when a task ends:** a task that never ends, as a component's often does, would never show in its account.
- **Serial executors as policy decisions:** a policy sees placements, not suspensions, so it could not keep a task from running while another of its executor holds the turn across CPUs.
- **Deterministic serial runs whatever the readiness order:** a kernel's interrupts and other CPUs make readiness timing-dependent, and instrumenting every atomic as a scheduling point would slow the whole image; the turn is reproducible given the order in which tasks become ready.

## Consequences

- Each capability entry grows from 64 to 96 bytes, and the boot area by 2 MiB.
- Every switch reads the clock twice and takes the capability table lock to charge time, on the host targets too.
- A task of a serial executor asked to stop waits for the turn like any other, so a holder that never suspends delays the settlement of the waiting tasks' domain.
- `hw::switch_to`, `hw::RunQueue`, `hw::SavedContext`, `hw::Resumable` and `hw::SpinGuard` no longer exist.
