# 0135. Handles crossing isolated spaces

Status: accepted

## Context

A component instance held in a space ([0134](0134-component-instances-in-isolated-spaces.md)) receives calls whose protocols carry capabilities as `sys::Handle`s beside wire bytes ([0115](0115-wire-encoding-of-protocols.md)). A handle names a value of the runtime that minted it, which another runtime cannot hold, so a space refused every call and reply carrying one. A space already reaches the kernel's capabilities by index in its table of handles, minting a capability of its own runtime per entry that invokes the kernel ([0131](0131-links-and-handle-tables-of-isolated-spaces.md)), and its runtime serves logs, standard streams and random sources that way. Only `sys` could tell a handle's type, `CapabilityId.of_handle` being private.

## Decision

- **A handle crosses as an entry of the space's table.** The kernel adds each capability a call carries to the table and sends its handle and type down the link; the space's runtime mints a capability of that type reaching the entry and gives the instance its handle. A reply's handles cross back as the indices of the entries they hold capabilities for, and the kernel gives the holder handles of those entries' capabilities.
- **The kernel decides what a space holds.** It checks each handle's type with `CapabilityId.of_handle`, now public, and takes only `sys::Log`, `sys::Stdio` and `sys::Random`, the types a space's runtime serves. It reads each index a space sends as untrusted, giving back only a capability its table holds and never a channel.
- **One capability per entry.** The runtime keeps the handle it minted for each entry, and each by its type tag and word, so a capability that crosses back names its entry and the runtime's capability table does not grow with each crossing.
- **The table only grows, up to 256 entries.** Entries are added through a read of the table, a count and an insert-only map, so the kernel's relays and its system-call service share it without a lock.

Alternatives rejected:

- **Handles through the channel as they are:** the space's runtime would hold words naming entries of the kernel's tables.
- **A supplied operation telling a handle's type to `nanos`:** `CapabilityId.of_handle` already does, for any closure.
- **Removing an entry when the call carrying it is answered:** a handler may keep the capability, as it may in the shared space.

## Consequences

- A call carrying a capability of another type, a clock among them, still fails as malformed in a space.
- Each handle a call carries in takes an entry for the space's life; once the table holds 256, a call carrying one fails as malformed.
- Supersedes 0131's consequence that handles are not transferred into a space, and 0134's that none crosses into or out of one.
