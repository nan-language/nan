# 0242. The space's thread run on the executor of the job it runs

Status: accepted

## Context

One task of the executor `isolate` takes ran an isolated space's thread and served its system calls ([0222](0222-isolated-spaces-shared-by-jobs-and-instances.md)), and the domain of the kernel's work for each job or instance ran on that executor too ([0240](0240-the-kernels-work-for-an-isolated-job-in-a-domain-of-its-own.md)), so the kernel's scheduler charged every job and instance of the space to that executor's account and placed all of them by its share. NaN OS runs the instances of one principal and provenance class in one space and charges each instance's CPU class share, account counters and energy budget to an account of its own. A kernel task's account counts the time its tasks run, and its policy places each task by the share of the executor it was started on; nothing can charge a running task's time to another account. The space's runtime is one thread whose tasks switch in user mode, each switch already reading the kernel's clock through a system call; a job's tasks run on its world's executor, which the runtime splits from its own root executor, or on executors deriving from it.

## Decision

- **Each job and instance names an executor.** `run_isolated` and `hold_isolated` take an `exec`, on which their kernel-side tasks run, the domain of the kernel's work for them among them.
- **The runtime names the account it switches to.** Each switch to a task reads the clock through a `SWITCH` call carrying the handle of the executor deriving directly from the runtime's root that the task's executor is or derives from; a job's start names its own such account through its runner capability. The job's relays in the space run on an executor of that account too, so serving it switches to no task of the runtime's.
- **The thread follows the account.** While the runtime last switched to a job's task, the isolation's task runs the thread on a task it spawns on that job's `exec` in a scope of its own, lending it the space, its frames and extended state, until the runtime switches to another account, waits or ends the job; then the isolation's task takes the thread back. That task is placed by the job's share and counted on its account, so the share bounds the job rather than its time being accounted after the fact. A stale `exec` leaves the turn to the isolation's task.

Alternatives rejected:

- **A thread of the space per job:** the runtime is one executor thread whose tasks share its extended state and heap; several threads would need locking in user mode and a kernel task per thread for the space's life.
- **The isolation's task charging another account for the time it runs a job:** the policy would still place that task by the isolation's share, so a job could not be bounded by its own.
- **A per-job kernel task taking the thread through channels:** that task would run in the cancellation region of the call placing the job, and a cancellation could stop it while it holds the space of every other job.
- **A system call at each switch out of a job's task:** the runtime's scheduler between two tasks of one job would hand the thread back and forth.
- **A word in a shared page the kernel reads at each exit:** a job's code would run on the account of the job before it until the next interrupt.

## Consequences

- Each switch between the tasks of two jobs, or a job's and the runtime's, of one space spawns a kernel task on the next one's `exec`, drawing its stack through that executor's allocator.
- A job or instance that never waits still holds up the others of its space until it ends, running only as its own share lets it.
- The runtime's scheduler running between a job's task and the next is charged to that job, and its own tasks and the system calls they make to the isolation's executor.
