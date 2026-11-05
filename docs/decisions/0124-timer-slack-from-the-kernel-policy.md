# 0124. Timer slack from the kernel policy

Status: accepted

## Context

Each deadline of the kernel clock fired at its own time, so an idle CPU woke once per sleep or timeout even when several fell within microseconds of each other. NaN OS wants deadlines that may fire late to coalesce, with a slack it derives from the class of the work: none for latency-bound work, a share of the delay for normal and background work.

## Decision

- **The policy gives the slack.** `hw::Policy` gains a fifth hook, `slack(t, task, delay_ns)`, which the runtime calls as a kernel task arms a deadline, with the arming task's view and the delay. A policy derives the slack from the share and the delay as it derives placements, and the hook's default body gives 0, so existing policies keep their behaviour.
- **Windows, not new deadlines.** A timer keeps when it is due and gains the end of its window, its due time plus its slack. Each CPU programs its timer for the earliest window end and every firing passes every timer already due, so a timer passes inside its window, together with every timer whose window holds that moment. The timers stay sorted by due time; the walk for the earliest window end stops at the first timer due after the earliest end found.
- **Armed in a routine.** The hook is an indirect call, whose stack no task's static bound can count, so a kernel arms every timer through the `TimerArm` support routine, on the stack routines reserve, which already holds a hook call.

Alternatives rejected:

- **A slack argument on each sleep and deadline construct:** it changes `sys::Clock` and `task` on every target, the interpreter and the seeded scheduler, and leaves code deciding a slack that is the scheduler's business.
- **A field of `task::Share`:** the runtime would interpret a share it otherwise leaves to the policy, and a fixed field cannot express a slack proportional to the delay with bounds.

## Consequences

- Every kernel sleep and deadline calls a routine and, under an installed policy, the hook.
- Timer records grow by a word on every target.
- The host targets, the seeded scheduler and isolated spaces call no hook, so their deadlines have no slack.
