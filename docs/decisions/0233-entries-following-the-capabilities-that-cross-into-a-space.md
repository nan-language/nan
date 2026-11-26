# 0233. Entries following the capabilities that cross into a space

Status: accepted

## Context

A space's table takes back the entries of a job or instance once it ends and of a bridge's end once it closes ([0230](0230-taking-back-the-entries-of-a-spaces-table.md)), but each capability crossing into a live instance with a call it answers or a reply it receives took an entry of its own until then, and so did the capability of a number its runner gives after `drop_capability` dropped it ([0136](0136-runner-capabilities-of-instances-in-isolated-spaces.md)). A long-lived instance given a capability with each call filled its space's 256 entries within a few hundred calls. Capabilities are Copy, so neither a space's runtime nor the code it runs can tell when the last copy of one is gone; revocation is what ends every copy of a capability.

## Decision

- **A capability takes one entry per job or instance.** A capability crossing into the table for a job or instance that an earlier crossing of it gave an entry takes that entry again, and the entry counts the crossings holding it, so the runtime, holding one capability per entry, holds that capability once too.
- **Releasing a crossing gives back its count.** The kernel releases the crossings of what it drops unclaimed for a bridge's end and of a call whose handles cannot all cross, and the crossing made for a number once `drop_capability` drops it; an entry is taken back once no crossing holds it.
- **A full table first takes back stale crossings.** When the table needs an entry and has no slot free, it takes back each entry a crossing added whose capability is stale, then tries again. A capability of a crossing kind is stale once it no longer crosses, and any other once `sys::Handle.invoke` fails with `Revoked`, which it does without calling a kind.

Alternatives rejected:

- **The runtime telling its kernel when it drops the last copy of a capability:** nothing counts the copies of a Copy capability.
- **Lending a call's capabilities for that call alone:** a component keeps what a call gives it in its Env, a keyed task or a message, and would lose it in a space while keeping it when its kernel holds it.
- **An operation through which a component releases a capability:** each component would have to call it, while a caller lending a capability for a call already revokes it after.
- **Asking whether every entry's capability is stale at each crossing:** each crossing would ask up to 256 times, where a table that is not full needs no slot back.

## Consequences

- The table bounds the distinct live capabilities a space's jobs and instances hold at once, not the calls that carried them: a capability given with every call takes one entry, and a capability lent for a call and revoked after gives its slot to the next crossing that needs one.
- A live capability stays in the table until its job or instance ends, however many calls ago it crossed, so a caller handing out a new capability per call bounds what the space holds by revoking each.
- A stale capability's entry can be taken back while its job or instance runs, so a reply naming it fails as one naming a handle the table does not hold, where it crossed as a stale capability before.
- The runtime keeps the capability it minted for each handle it received until its job or instance ends.
- Supersedes 0230's consequence that an entry a live instance's runtime no longer holds stays until the instance ends, and 0136's that the capability of a number takes another entry each time it crosses after `drop_capability`.
