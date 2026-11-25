# 0232. A clock for each job and instance of a space

Status: accepted

## Context

The world of a job or instance of an isolated space held its runtime's root clock ([0231](0231-an-isolated-space-whose-clock-fails.md)), which reads the clock `isolate` takes apart from its `Services` ([0227](0227-services-of-each-job-and-instance-of-a-space.md)). A kernel could deny a job or instance the clock only by revoking that one, which leaves the whole space without time, while NaN OS grants an instance the clock as it grants its log or random source. A space's runtime learns that a capability of its kernel is stale only from the kernel's answer to a system call. A served clock's readings call its kind's `elapsed_ns`, which could not fail, its sleeps call its kind's `sleep_ns`, and its deadlines are timed by the platform.

## Decision

- **`Services` holds a clock, and `isolate` takes none apart.** The clock of `isolate`'s services is the space's: its runtime keeps time by it and `SpaceCalls.now_ns` reads it. The clock of each job's or instance's services joins the space's table as an entry of that job or instance, taken back with its others.
- **A world's clock is served over that entry.** Each reading is a system call and fails with `Revoked.Stale` once the kernel answers stale; a sleep asks once whether the clock is stale, returning at once when it is, and otherwise sleeps on the runtime's clock; deadlines are timed by the runtime's timers, as on every served clock; a coarser clock derives from it and reads through the same entry.
- **A clock stale as its job starts is stale in the space.** The runtime holds a revoked clock in its place, so that its deadlines pass at once too.
- **`sys::ClockService.elapsed_ns` fails with `sys::Revoked`,** which a served clock's readings give as their failure.

Alternatives rejected:

- **Sleeps waiting on the job's clock in the kernel:** each would wait for its kernel, while the runtime's timers already serve every task of the space.
- **The world's clock deriving from the runtime's root clock, revoked on an order of the kernel:** the kernel learns that a clock is stale only by reading it, and until it did the world would read the space's clock without its own grant.
- **`isolate` keeping its clock apart beside services holding one:** the space would hold two clocks, one of them unused.
- **The runtime revoking a world's clock at its first stale answer, so that later deadlines pass at once:** the kind's methods, which never wait, would need a revoker per world shared with the runtime's tasks.

## Consequences

- A world's clock grants or denies time to its job or instance alone; its space and the other jobs and instances of it keep theirs.
- Each job and instance takes one more entry of its isolation's table, each reading of its clock a system call and each sleep one more.
- A deadline on a world's clock revoked while its job runs passes in the runtime's time, as on any served clock whose kind goes stale, and a sleep under way runs its course.
- Once `isolate`'s clock is stale, a world's sleeps return at once and its deadlines wait out a time that no longer passes, while its readings follow its own clock.
- A kind serving a clock over another passes on that clock's failure.
- Supersedes 0227's decision that `isolate` takes its clock apart and `Services` holds none, and 0231's that every clock of a world derives from the runtime's root clock.
