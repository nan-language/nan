# 0137. Resources held by id

Status: accepted

## Context

A component instance holds capabilities by number through its runner ([0133](0133-capabilities-held-by-id.md)), but its files and listeners still come from its world once, and its state, which a checkpoint keeps, cannot hold them. NaN OS's runner keeps an instance's resources by number as it keeps its capabilities. Resources are no handle types and do not travel as wire encodings ([0115](0115-wire-encoding-of-protocols.md)), but a bundle in the shared space runs on its kernel's runtime, where a resource is a std value and a holding the runtime moves between domains ([0126](0126-kernel-served-files-and-networks.md)). Handlers never wait, a kind reads its state through `sys::reach`, so it can take a value out only of storage reached by waiting, a cown or a channel, and every operation of a resource waits.

## Decision

- **Resources cross the runner's kind as std values.** `RunnerService.resource`, `take_resource` and `put_resource` give and take a `sys::Held`, a closed enum holding a file or a listener, so one method serves every resource type and the kind keeps resources however it chooses.
- **Ownership follows the runtime's moves.** The `Held` that `resource` or `take_resource` gives passes to the calling task's domain, so settling an abandoned instance releases it; one `put_resource` hands over moves into whatever the kind keeps it in, as any value does; a stale runner releases what `put_resource` takes.
- **A typed id per resource type.** `sys::ResourceId[R]` derives Encode, as `CapabilityId` does, and `R` implements `sys::Resource`, which converts to and from `Held` in plain NaN. A resource of another type than the id's goes back to the holder, and the operation fails with `RunnerError.Unknown`.
- **The operations wait, on `sys::Resources`.** It holds the runner capability as a port does, so keyed tasks, which use the resources anyway, capture it; `resource` lends, the holder keeping the number for the resource `put_resource` returns, and `take_resource` gives the resource away.
- **The resource methods are bound by 8,192 bytes**, since releasing a served resource counts the 4,096 bytes of its own kind's method.
- **A space's runner refuses them.** Its runtime holds no resource of its kernel's, and a served resource's record names kernel code and a kernel kind that the space cannot call, so its runner capability fails with `RunnerError.Unknown` without a system call.

Alternatives rejected:

- **Context operations that do not wait:** a kind cannot take a resource out of what it reads, and a component still needs a keyed task to use one.
- **Connections held by number:** an id of a linear type is linear, so no state could copy it.
- **An opaque supplied holding with a type tag, as `sys::Handle` is for capabilities:** lowering in both compilers where a closed enum in NaN does it.
- **Resources crossing into a space through its table:** the space would relay every operation of the resource to its kernel, a second served path for each resource type.

## Consequences

- The last reach of a kind's state releases it, so a kind whose state holds resources exceeds the 4,096-byte bound of the runner's other methods; a kernel keeps them in a kind of their own.
- A lent resource whose instance ends before putting it back is released with the instance's domain, and the holder's number names nothing.
- Supersedes 0133's consequence and 0132's that resources held by id remain to do.
