# 0008. Authority as values

Status: accepted

## Context

NanOS runs code written by agents, and agents act on data that others control: network payloads, files, model output, arguments passed by another agent. Any of it may carry instructions meant to steer the program. The system needs least authority, and a guarantee that outside data cannot reach a decision without passing through code someone reviewed.

Mainstream languages give every function ambient authority: a global file system, clock, randomness, allocator and environment. Least authority then cannot be enforced, a window does not show what a function can touch, and tests cannot replace the world without mocking inside the code. Some authority is not a value at all, though: touching device memory, executing privileged instructions, running trusted code, opening untrusted data. Systems code also fills shared tables (interrupt vectors, drivers, system calls) that many packages extend, and that parallel agents would otherwise all edit in one place.

## Decision

- **No ambient authority.** [Code acts outside its own values only through capabilities](../spec/14-authority.md#authority) it receives: there is no global I/O, clock, randomness, allocator, environment or mutable static. The one exception is the kernel anchor, reached only from entry contexts ([0009](0009-low-level-and-kernel-without-assembly.md)).
- **Copy capabilities that stay revocable.** `sys::Fs`, `Net`, `Clock`, `Random`, `Log`, `Stdio`, `mem::Alloc` and `task::Executor` are Copy [generational handles](../spec/14-authority.md#capability-tables) into a table owned by the value that minted them. Passing authority stays cheap to write, and revoking an entry makes every copy stale at once. Resources they open, such as connections and files, are affine or linear; hardware capabilities are linear.
- **Roots and attenuation.** Each program starts from one [root grant](../spec/14-authority.md#root-grants): `host::World`, `boot::Machine` or `boot::Kernel`. Narrower authority comes from ordinary methods such as `fs.subdir` and `alloc.split`, and `sys::revocable` adds a revoker. `testing::world()` returns the same shape with simulated capabilities, so tests replace the world without any test-mode branch in code.
- **Untrusted data.** Data from outside the program (network, files, other principals' IPC, user memory, system call arguments, device-written buffers, inference results, agent payloads, command-line arguments, environment and standard input) arrives as [`Untrusted[T]`](../spec/15-untrusted.md). It has no equality, order, hash or readable rendering and never converts to T; only content-blind operations apply. Its contents are read only in a `validator fn` through `inspect()`, in a module granted `validate`, and each consumer of a validating package grants it too. A validator's class is computed from what it can export: parsing when only scalars, enums and structs of these leave it, declassifying otherwise, which makes it a trust escape needing a seal. Register values are typed device state, not outside data.
- **Manifest grants for what values cannot carry.** `allow device`, `priv`, `trusted`, `validate` and `contribute` name the modules, or dependency aliases, that perform these operations directly; the checker computes which modules do and reports any without a grant. `deny a -> b` forbids a dependency. [Device and priv grants](../spec/14-authority.md#device-and-priv-grants) are trust, since a device can be programmed to write any memory: the package item granting them is sealed and listed in the ledger.
- **Authority summaries.** Each package's [authority summary](../spec/14-authority.md#authority-summaries) lists the capability types its items use, `alloc`, `device`, `priv`, its validators and its trust escapes. A final-link root records the approved summary of its whole closure, so a dependency that starts using the network, a device or trusted code is an error at that root until the widening is approved.
- **Registries.** A [registry](../spec/25-registries.md) is a typed table declared in one package and filled by single-line `contribute` items. Contributors are the declaring package and the packages the final-link root admits with `allow contribute`. Keys and unique fields are checked across the final closure, function fields carry full types and limits and are reached through checking entries, and the table is emitted as sorted const data at the final build.

Rejected alternatives:

- **Ambient authority:** least authority could not be enforced, and tests would need mocks inside the code.
- **Confidentiality or integrity labels in 1.0:** a two-point wrapper gives injection containment without label polymorphism.
- **Trusting arguments, environment or standard input:** an invoking agent passes exactly that kind of data.
- **Trusting DMA payloads:** a device writes whatever it is told.
- **Central tables:** every parallel addition conflicts on them.
- **Linker sections:** untyped, unchecked, tied to one linker, and a dependency joins a table unannounced.

## Consequences

- A function touches only what reaches it through its parameters and captures, so every call shows the authority it passes, and tests are deterministic because every source of time, randomness and I/O can be simulated.
- Heads carry capability parameters. Copy handles keep that cheap, but every operation through one checks its generation at run time.
- Parsing code is explicit and confined to validators, which tools list; declassifying validators need a seal by an author other than theirs.
- A driver granted `device` can still misprogram DMA until an IOMMU is modelled; sealing and listing the grant is the protection until then.
- Registry tables are fixed at the final build: a duplicate key can appear only when two packages meet in one closure, and tables filled at load time wait for NanOS.
- The word `ipc` and the path `sys::infer` are reserved for IPC and inference capabilities, whose data is already listed as Untrusted. NanOS principals and capability-scoped tool queries wait for NanOS.
