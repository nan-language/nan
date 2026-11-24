# 0226. Moving large messages off a link's first channel

Status: accepted

## Context

An instance in an isolated space sends its answers, checkpoints included, and its calls through ports up the first channel of its link, and the kernel takes a frame there longer than 1 MiB as a breach, since it would otherwise buffer whatever length the space claims ([0225](0225-bridges-between-isolated-spaces.md)). A component's state past 1 MiB is ordinary, yet `nanos::hold` checkpoints it while an instance in a space ended trapped at its first checkpoint. The kernel also copied at most 1 MiB out of a space for one system call, so a job's or an instance's last state past it ended trapped, and an order to start one from a larger input was never taken. The frames an isolation draws from belong to the task running its space, and a cown holds no frame.

## Decision

- **A message past 1 MiB moves as frames.** The runtime asks the kernel, through the instance's runner capability, to take the pages holding its encoding out of the space, as the writing side of a bridge does, and sends up the handle and the token they moved under. The task reading the channel asks the task running the space to read the message from those frames, which then go back to the isolation's frames.
- **An end and an order copy whole.** The kernel copies an end's bytes from the pages the space maps, whatever their length, and writes an order into a buffer of any capacity, since each happens once per job or instance.

Alternatives rejected:

- **Raising the channel's limit:** the kernel would buffer whatever the space claims.
- **Copying a large message into the kernel at a system call:** the kernel would keep copies the space's frames do not account for until it reads the channel.
- **Failing the operation for the holder:** the instance's state would be bounded in a space but not in the kernel.

## Consequences

- What a space keeps moving to its kernel without naming it takes frames of its own isolation until that isolation returns.
- Reading a moved message waits for the task running the space to take the ask between two exits of its thread.
