# 0235. Capabilities a space's runtime holds by slot

Status: accepted

## Context

A space's runtime mints a capability, two entries of its own capability table, for each handle of its space's table that crosses for a job or instance, and kept each in insert-only maps until the job or instance ended ([0233](0233-entries-following-the-capabilities-that-cross-into-a-space.md)). The table takes back entries while their jobs and instances run, and gives a slot it took back the handle 256 above, so a long-lived instance lent a fresh capability per call filled the runtime's 65,536 entries after about 32,000 calls. Nothing told the runtime which handles were taken back. Revoking what it minted takes the minter, which the task running the job or instance owns, while the runner operations that mint and name capabilities (`capability`, `send_to`, `reply`) run without waiting, and std had no removable map those operations could change.

## Decision

- **One capability per slot.** For each job or instance, the runtime keeps what it minted for the last handle of each of the table's 256 slots it received. A slot's handles only grow, so a later handle of a slot tells the runtime that the table took the earlier one back: minting for it revokes the earlier one's capability, and a handle earlier than the one kept crosses as a stale capability.
- **The slots sit behind a spin lock**, as the kernel's table does. `hw::without_irqs` masks nothing in the runtime, which runs in user mode on one executor thread.
- **A supplied operation of `nanos` revokes** a remote capability the runtime minted, and every capability deriving from it, without its minter.

Alternatives rejected:

- **The kernel telling the runtime which handles it took back** with its next order or answer: the runtime would still need a removable map and a revocation without waiting, and the slot of a crossing handle already tells it.
- **Handing what to revoke to the task owning the minter:** a runner operation cannot wait, so a full queue would leave a capability unrevoked.
- **A removable `mem::OnceMap`:** its views of values stay valid because entries never leave.

## Consequences

- The runtime holds at most 256 capabilities for a job's or instance's handles, in a list of 256 places allocated when the job or instance starts, however many calls carried capabilities to it.
- A capability whose entry the table took back stays held until a later handle of its slot crosses for the same job or instance; a copy the job or instance keeps fails as revoked from then on, as it did through the stale handle before.
- Supersedes 0233's consequence that the runtime keeps the capability it minted for each handle it received until its job or instance ends.
