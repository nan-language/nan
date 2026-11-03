# 0115. Wire encoding of protocols

Status: accepted

## Context

NaN OS loads bundles, separately built closures that share no value layout, and their instances call each other through protocols. 0097 reserved how operations travel between closures, so calls reached only the components of their own closure. Capabilities cannot travel as bytes: a receiver given a handle at the wrong position would hold one capability typed as another. The runtime that carries calls between bundles and spaces is NaN OS's; the language must give it, and any other transport, typed calls in and typed replies out.

## Decision

- **A call encodes as its message's variant.** The key is the operation's name and the payload the record of its arguments by parameter name, as a derived enum would encode; a reply is `Result[R, E]`. Naming operations by key rather than by declaration number matches the protocol hash, which sorts operations by name, so closures agreeing on the hash agree on the encoding whatever the order of their declarations.
- **Handles travel beside the bytes.** A value of a handle type encodes as the index of its handle in a list carried with the bytes, each handle holding its handle hash; decoding requires indexes in order, checks each hash and refuses unread handles. Indexes keep the bytes canonical and self-describing; the hash stops a sender from giving one capability for another.
- **Handle types are the Copy capabilities a table entry backs**, those implementing `sys::Capability`, and outboxes. A copy names the same authority, so a handle is a plain copy in one space, and a runtime crossing spaces can map each entry, as NaN OS does for the kinds it lets travel.
- **`sys::Wire[P]` encodes and serves; `sys::Reply` decodes.** Encoding calls are members of `sys::Wire[P]` derived per operation, as calls are members of `sys::Outbox[P]`, and give a request's typed `sys::Reply`. `serve` is generic over `sys::Protocol`, which every protocol implements, so a runtime serves any protocol without naming it. Encoding writes nothing to a transport and serving calls through an outbox, so the runner, intakes and replies stay unchanged and every transport composes from these parts.
- **`serve` checks what the closure trusts.** It refuses a call under another protocol hash and evaluates the operation's `requires`, because handlers assume them as entry facts that the sender's prover never established for this closure. Encoding carries no obligation, since serving checks it.
- **Failures stay `sys::CallError`.** A reply that does not decode is `Malformed`; `serve` fails with `CallError[Never]` when it cannot deliver, so a transport passes the failure on in its caller's terms.

Alternatives rejected:

- **Operation numbers in declaration order:** two declarations with one protocol hash may order operations differently.
- **An outbox whose component is remote:** the runner and every outbox operation would need a second path, and the transport is the runtime's.
- **Request values posted through `post`:** a request needs its reply slot, which 0097 keeps out of values of P, and replies would not be typed.
- **Handles without type checks:** a malformed or hostile encoding would confuse capability types.

## Consequences

- An operation named `hash` or `serve` is an error, and primitive capabilities outside `sys::Capability`, such as `sys::fs::File`, `sys::net::Listener` and `mem::Heap`, are no longer handle types.
- A server decoding a protocol whose operations take a handle type or any other type that is no parsing type needs `allow validate`.
- Summaries do not travel; their types implement Encode, so a runtime may carry them as values.
