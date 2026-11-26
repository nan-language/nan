# 0224. Capability kinds crossing into isolated spaces

Status: accepted; capabilities crossing only to instances superseded by 0227, a kind crossing through bytes alone superseded by 0239

## Context

NaN OS places instances whose Envs hold its kernel's capability kinds (ports, store views, journal sources, `/live` records, key uses) in isolated spaces ([0222](0222-isolated-spaces-shared-by-jobs-and-instances.md)). A space's runtime held only the logs, standard streams and random sources its kernel serves ([0135](0135-handles-crossing-isolated-spaces.md)), and `hold_isolated` took no capabilities for the instance's world. A kind's operations are functions of its module that reach the minter's state through `sys::reach` ([0098](0098-capability-kinds.md)); in a space that state is the kernel's, which the space cannot map, and the space's runtime knows a capability's type only by its handle hash, so it can mint no entry of the kind. The kernel's `nanos` code is generic over no kind, and a function value is not Send, so it can call a kind's code only through a method table an entry records.

## Decision

- **A kind crosses through bytes.** A kind implementing `sys::Crossing` gives `invoke`, which never waits, and `call`, which may, each an operation's number and untrusted argument bytes to result bytes. Its operations call them through `sys::Handle.invoke` and `call`, so every closure runs the code of the image that minted the capability, whatever version of the kind's module it holds.
- **Entries record their kind's method table.** `Minter.mint` stores the kind's `sys::Crossing` table in the entry, zero for a kind that does not cross, and derived entries copy it; `Handle.invoke` and `call` dispatch through it, and `Handle.crosses` tells the kernel which capabilities may cross.
- **A space serves a crossed capability through a kind of its own.** Its runtime mints an entry of a new entry kind that a `nanos` kind serves through `sys::Crossing`, presented under the capability's handle hash, so the bundle holds a capability of the same type. `sys::reach`, `derive` and `Minter.revoke` refuse entries of any other entry kind, so the posing entry never reaches a minter's state as the kind's.
- **`invoke` is a system call and `call` crosses the link**, as the runner's operations do ([0136](0136-runner-capabilities-of-instances-in-isolated-spaces.md)): the kernel serves `invoke` at once on its own capability, the result bounded by 4,096 bytes, and `call` on a task of its own, replying down the link's first channel.
- **`hold_isolated` takes the world's `Caps`.** The kernel adds each capability that crosses to the space's table and sends its key, handle and handle hash with the start order; one that does not cross is left out. A `Caps` holds `sys::Handle`s, whose handle hash names the type in every closure, so the kernel can hand a world's capability back when a reply carries it.
- **Revocation is the kernel's.** The space's capability reaches the kernel's entry on every operation, which fails as revoked once that entry is stale.

Alternatives rejected:

- **Typed request and answer enums per kind:** the kernel would call glue generic over them, which only an instantiation at the kind's own call sites could provide.
- **The holder serving every kind through one served capability:** a second capability type with lowering in both compilers, and the kind's operations would still need a path for their bytes.
- **Mapping the kind's state into the space:** it is kernel memory holding kernel pointers and locks.

## Consequences

- An operation of a crossing kind encodes its arguments and result even when it runs in the image that minted the capability.
- A non-waiting operation whose result exceeds 4,096 bytes completes in the kernel but fails in the space as refused.
- Every `reach` and `derive` checks its entry's kind under the table lock.
- Capabilities cross only to instances: a job's world in a space holds none.
- Supersedes 0098's consequence that a kind's handle works only within one image, for kinds that cross, and 0135's decision that a space takes only logs, standard streams and random sources.
