# 0136. Runner capabilities of instances in isolated spaces

Status: accepted

## Context

A component instance acts beyond its intake through the `sys::Runner` its holder attaches ([0132](0132-runner-capabilities-of-components.md), [0133](0133-capabilities-held-by-id.md)), a capability a kernel serves through a kind. An instance in a space ([0134](0134-component-instances-in-isolated-spaces.md)) runs on another runtime than its holder's, so it can hold no capability of the kernel's, and its stepper's runner was stale. The `Runner` operations were private to `sys`, so `nanos` could not call the holder's runner for the space. A call through a port waits for its reply; the other operations do not wait, and `capability` gives a handle at once. Handles cross a space as entries of its table ([0135](0135-handles-crossing-isolated-spaces.md)).

## Decision

- **`hold_isolated` takes a runner, as `hold` does.** The kernel puts it in the space's table, and the space's runtime attaches to the instance's stepper a runner capability of its own kind that forwards each operation to that entry.
- **Operations that do not wait are system calls.** `send_to`, `reply`, `publish`, `sync`, `capability` and `drop_capability` are `invoke`s of the runner's handle with `mem::wire` arguments, served at once on the kernel task running the space, as the log is; `capability` gives back what crossed into the table.
- **A call through a port crosses the link.** It goes up the first channel beside the instance's answers, the kernel calls the runner on a task of its own and sends the reply down, so the space runs on while the call waits, and an instance may call a port its own holder answers through the instance.
- **The `Runner` operations are public, and nothing else of the runner is.** They mirror `RunnerService` one for one and a kernel relaying an instance's runner needs each, while `nanos` is a package of its own that a `pkg` entry would not reach. They grant nothing new: code holding a `Runner` holds the authority the kind serving it grants, and component contexts and ports still reach their runner only through their own operations.
- **`capability` crosses once per number.** The kernel keeps what crossed for each number until `drop_capability`, so a component reading its capabilities by number on every message does not fill the table.

Alternatives rejected:

- **Every operation over the link:** an operation that does not wait cannot wait for room in a channel, and `capability` must give its handle before returning.
- **A call through a port as a system call:** the space's thread would wait on the kernel, and a call answered through the instance itself would never be answered.
- **`hold_isolated` taking the kind serving the runner:** holders would be written twice, once per placement.
- **A single public entry taking an encoded runner operation:** a codec of runner operations in `sys`, beside the interface that already names them.

## Consequences

- A capability a number names crosses again after `drop_capability`, taking another entry.
- An instance in a space costs a task of its holder's kernel per call through a port in flight.
- Supersedes 0132's decision that the operations of `Runner` are private to `sys`, and 0134's consequence that an instance in a space acts through no runner.
