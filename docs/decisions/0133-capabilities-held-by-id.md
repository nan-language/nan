# 0133. Capabilities held by id

Status: accepted

## Context

An instance's capabilities come from the world its Env is built from, once, so its holder can neither grant one later nor learn that one is no longer needed, and its state, which a checkpoint keeps, cannot hold one: capabilities are not Encode. NaN OS's runner keeps an instance's capabilities in a table of its own and the instance names them by number. A capability crosses from the kernel's closure to a bundle as a `sys::Handle` beside wire bytes ([0115](0115-wire-encoding-of-protocols.md)), whose handle hash names its type; only decoding a reply turned a handle into a typed capability, and a generic function whose type parameter appears only in its result is an error.

## Decision

- **The runner capability keeps capabilities by number.** `RunnerService.capability` gives the `sys::Handle` of a number and `drop_capability` releases it, so a kernel serves them with the kind it serves the rest of the runner with ([0132](0132-runner-capabilities-of-components.md)).
- **A typed id names a held capability.** `sys::CapabilityId[C]` is a number that derives Encode, so the state keeps its capabilities' ids across checkpoints, and its type parameter fixes the type `Cx.capability` gives.
- **The type is checked by handle hash.** `Cx.capability` refuses a handle whose hash is not C's, as decoding does, through a private supplied operation of `CapabilityId`; `sys::Handle.of` gives a kernel the handle of a capability without encoding a call.

Alternatives rejected:

- **Capabilities as `(tag, handle)` words, as `nanos::Caps` holds them:** a runner crosses closures as wire encodings, whose handles already carry their type by hash.
- **An untyped `Cx.capability` giving a `sys::Handle`:** every component would need the typed conversion anyway.

## Consequences

- Resources held by id, the rest of NaN OS's runner context, remain to do: files, listeners and connections are not handle types and do not travel as wire encodings.
