# 0132. Runner capabilities of components

Status: accepted; the capabilities held by id it leaves to do superseded by 0133, the resources by 0137, the operations of `Runner` private to `sys` by 0136, and the numbers of pending requests told through the component's own operations by 0144

## Context

A component instance a kernel holds ([0119](0119-component-instances-of-nanos-bundles.md)) reaches nothing beyond its own intake: its `sys::Cx` sends to itself, delays messages and spawns keyed tasks. NaN OS's runner lets an instance call another instance's protocol through a port, answer a request it holds pending later, publish its summary and ask for a checkpoint. The kernel and the bundle are separate closures: only std types share a layout, and bundle code reaches kernel code only through capability kinds a kernel serves as std capabilities ([0116](0116-nanos-jobs-in-a-kernels-shared-space.md), [0126](0126-kernel-served-files-and-networks.md)). Handlers never wait, while a call through a port waits for its reply.

## Decision

- **A runner capability served by a kind.** `sys::Runner` is a std capability a kernel serves through a kind implementing `sys::RunnerService`, as it serves `Log` or `Fs`. Its methods take port and request numbers, protocol hashes and wire encodings ([0115](0115-wire-encoding-of-protocols.md)), so they cross closures, and run on the calling task: a handler's is the instance task.
- **The holder attaches it to the stepper.** `Stepper.attach` keeps the runner's handle in the runner's record, and `nanos::hold` takes the runner the instance's adapter attaches. A stepper without one acts through a stale handle, so a host run and an unattached stepper refuse every operation through the path revocation already takes.
- **Context operations and ports are NaN in std.** `Cx.send_to`, `reply`, `publish` and `sync` and `sys::Port[Q]` call private operations of `Runner`, each lowered as one forwarding to the kind's method of its name; the other supplied operations are `attach`, `Runner.served`, `Runner.denied` and reading the record's runner.
- **Calls through ports wait in keyed tasks.** The caller encodes with `Wire[Q]`, `Port.call` sends through the runner and decodes with the call's `Reply`, so the client is generic over the protocol. A handler hands a call over with `send_to`, which waits for nothing.
- **Pending requests are the holder's.** A holder numbers the requests it keeps pending and tells the component through the component's own operations; `reply` answers one by number with its reply's encoding. Calls through an outbox keep answering with their handler's result.

Alternatives rejected:

- **Requests queued in the stepper for the holder to drain:** lowering would build std values in a runtime queue, twice, and a waiting call would still need a path into the kernel.
- **Ports with a member per operation, as outboxes have:** method resolution and lowering in both compilers, where `Wire[Q]` and `Reply` already compose in NaN.
- **The runner in the Env, built from the world:** steppers on the host could not refuse, and nothing would tie the operations to the component being handled.
- **Typed one-way messages for `send_to`:** encoding a protocol's message values is another supplied codec.

## Consequences

- A runner's record grows by a word, and a context of an unattached stepper draws a denied entry each time it reaches for its runner.
- Handlers acting through their runner capability are not `pure`, the runner being a capability.
- Capabilities and resources held by id, the rest of NaN OS's runner context, remain to do: a capability needs a typed handle out of a `sys::Handle`, and resources do not travel as wire encodings.
