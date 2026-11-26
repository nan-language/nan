# 0239. Handles crossing with the operations of crossing kinds

Status: accepted

## Context

A crossing kind's `invoke` and `call` took and gave bytes alone ([0224](0224-capability-kinds-crossing-into-isolated-spaces.md)), and a `sys::Handle` cannot be written into bytes, so no closure could pass a capability to a kind's operation or receive one from it, whether in its kernel's domain or in an isolated space. NaN OS gives an instance its ports as a crossing kind whose protocols pass capabilities (store views, ports to other instances, logs), so a component delegating a capability or receiving one from a service could not run isolated. Calls through a runner's ports and an inbox's calls already carry handles, which the table of a space crosses ([0233](0233-entries-following-the-capabilities-that-cross-into-a-space.md)). In a space, `sys::derive` fails, so a kind's attenuation there can only be an operation giving a new capability.

## Decision

- **Both operations take and give handles beside bytes.** `invoke` and `call` take `handles: Slice[Handle]` after their argument bytes and give a `sys::Encoded`, as `Runner.call` takes and gives a port call's; `Handle.invoke` and `call` alike. The kind reads each handle's capability through `CapabilityId.of_handle`, as a holder does.
- **A space crosses them as it crosses a port call's.** The runtime names its arguments' handles by the entries of the space's table, the kernel hands the kind the capabilities those hold, and the handles of the result cross into the table for the job or instance and are minted by the runtime from its base remote capability, which its state keeps. `invoke` carries them by system call, its result's encoding bounded by 4,096 bytes; `call` up the link.
- **A call carrying handles never rides a bridge.** It goes up to the kernel, whose kind sees its capabilities; a reply over a bridge carries handles as a port call's reply does. A call carrying handles over a bridge whose calls the kernel admits stays a breach.

Alternatives rejected:

- **Arguments as an `Untrusted[sys::Encoded]`:** an owned encoding moves into each call, while every other operation reading a call's bytes as untrusted takes a slice and the handles beside it.
- **Separate handle-carrying variants of `invoke` and `call`:** every crossing kind implements both pairs, and a kind's operation would cross handles by one path and bytes by another.
- **Admitting calls with handles over bridges:** the kernel would hand the kind capabilities claimed from the serving space's table under the bridge's token, for a call it could make itself.

## Consequences

- Every crossing kind's operations take a slice of handles and build a `sys::Encoded`, even those passing none.
- A non-waiting operation's result is refused in a space once its bytes and handles encode past 4,096 bytes, a few bytes before its bytes alone would pass them.
- A capability a kind's operation gives into a space takes an entry of its table until it is stale or its job or instance ends, as a reply's does.
- Supersedes 0228's consequence that a reply carrying handles fails the call, and 0224's decision that a kind crosses through bytes alone.
