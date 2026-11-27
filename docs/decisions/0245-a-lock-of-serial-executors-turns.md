# 0245. A lock of serial executors' turns

Status: accepted

## Context

A serial executor keeps its turn and its waiting tasks in its capability entry ([0107](0107-kernel-scheduling-policy.md)), and admitting a task made ready took the lock of the capability tables to read them. Wakes reach admission while other locks are held. Revoking a clock cancels the deadlines armed through it under the tables' lock. A trap the tables' code raises abandons its domain, waking the domain's waiter, while the trapping thread still holds that lock. A cancellation wakes the tasks of its region under the lock of the live tasks, which a revocation takes under the tables' lock. A wake of a serial executor's task therefore took a lock its thread already held, or the two locks in the order opposite to a revocation's, and the runtime's locks do not re-enter.

## Decision

- **Serial executors' turns and waiting tasks have a lock of their own.** Admission, the passing of a turn and the withdrawal of a waiting task take it in place of the tables' lock, and take no other lock while holding it, so a thread holding any other lock of the runtime may wake a task.
- **A revoked entry is freed under that lock.** Its waiting tasks are taken out and its generation advances in one hold, so a task finds its serial executor's entry either live, with the waiting tasks that its release will run, or stale.

Rejected alternatives:

- **Admitting the tasks woken under a lock once it is released:** a revocation cancels regions through the live tasks and through operations whose completion wakes at once, so each such path would carry the tasks it woke back to the routine holding the lock, and a trap would need its own release order.

## Consequences

- The serial executors' lock is innermost, so a revocation and a cancellation no longer take two locks in opposite orders.
- A routine changing charges wakes the tasks of its arms under the tables' lock.
