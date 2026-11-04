# 0131. Links and handle tables of isolated spaces

Status: accepted

## Context

A space of [0118](0118-nanos-jobs-in-isolated-spaces.md) reaches its kernel only through system calls and invokes fixed handles for its log, standard streams and random source. NaN OS talks to a space through frames both sides map: a doorbell the kernel rings and channels whose words each side reads as untrusted, with acquire and release ordering, through `hw::UserFrame`; and a space holds capabilities by index in a table of its own. The same bundle runs shared or isolated, so a job must reach its channels through capabilities the shared space can serve too. The runtime's executor parks tasks on operations it completes; its idle wait is the `wait` call.

## Decision

- **A link is the caller's.** `nanos::Link` holds a doorbell frame and channels of two frames each, as user frames, which the caller lends `run_isolated` by read while its own tasks ring, send and receive, so a link outlives the spaces it serves and its frames return only once none maps them. `Space.share` maps a user frame into a space without owning it.
- **A channel is a ring per direction.** Each frame holds the count its writer wrote and the count its reader read, then 4,032 bytes; each side keeps its own count and validates the other's, so a space corrupting a ring only starves itself.
- **The doorbell wakes the space's idle wait.** `Link.ring` sets bits in the doorbell word and posts an `hw::Signal`; the kernel serves `wait` with that signal raced against the deadline and gives 1 when it rang. The runtime keeps one doorbell waiter, a timer operation in a word of its data, and completes it when `wait` gives 1; the waiter's own deadline bounds a lost wake.
- **Jobs reach channels as standard streams.** The runtime puts each channel in `world.caps` under its key as a `sys::Stdio` served by a kind of its own, which reads and writes the rings directly and tells the kernel through `invoke` on the channel's handle, so a job is the same shared or isolated and needs no new capability type.
- **Handles index the space's table.** The kernel builds a table of the log, standard streams, random source and channels, and the start record names each handle, so the runtime mints what the table holds rather than fixed numbers.
- **A signal's waiter woken without a post waits again.** A deadline expiring as a post arrives can wake a task after its wait returned; its next wait found its own registration and trapped as a second waiter.

Alternatives rejected:

- **Frames owned by the space:** the caller's tasks could not reach them while `run_isolated` runs.
- **A new channel capability type:** every placement would need a served kind and lowering for it, where a standard stream already has both.
- **Waking every sleeping task when the doorbell rings:** sleeps would end early.
- **Transferring `sys::Handle`s through channels:** a handle beside an encoding names a value of the sender's runtime, which a space's runtime cannot hold; component instances, whose protocols carry them, run only in the shared space.

## Consequences

- A space's job exchanges bytes with its kernel without a system call per byte, and waits on its doorbell rather than polling.
- One task of a space waits on the doorbell at a time; another waits out its recheck, a second.
- Handles are not transferred into a space, and a space cannot close one; message framing over a channel is the job's.
