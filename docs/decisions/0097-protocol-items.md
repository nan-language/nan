# 0097. Protocol items

Status: accepted

## Context

NaN OS components talk through typed requests. A component received every message through one `on` of a message enum, so each request kind shared one digest, one contract and one reversibility, and a reply needed a hand-made channel. Language 1 reserved the item word `protocol` without meaning. Peers built in separate closures must be checked against each other when they are wired and at update, not per message.

## Decision

- **A protocol is an item whose name is a message type.** It declares operations as heads with contracts and a class limit; a component serves it by `impl S: sys::Component[Env, P]` and one handler per operation in place of `on`. Reusing `sys::Component` keeps the runner, the state rules and keyed tasks unchanged; a separate server interface would duplicate them.
- **Handlers are read as interface methods.** Their heads, visibility, limits and contracts follow conformance and interface contracts, so the checker, prover and digest rules for implementing functions apply unchanged; contract functions read the operation's parameters, the positions after the handler's receiver or the call's outbox.
- **Clients call operations as methods of `sys::Outbox[P]`.** A one-way call is a post. A request waits until its handler returns, through a reply slot on the poster's stack that the component task fills under the runner's lock; a request without a `fails` clause fails with `task::SendError`, and one with `fails E` with `sys::CallError[E]`, whose from variants accept `task::SendError` and `task::Cancelled`, so one body mixes both kinds of calls. Handlers never wait, so a reply never waits on another component.
- **A call's digest is its operation's class and waiting, not the handler's digest**, as a post never carried `on`'s: the class bounds what the server may do on the caller's behalf, and the server's code is judged against it. Building a one-way message carries the class too, so posting it through `post` hides nothing.
- **Operation visibility is per operation.** Private operations, such as a timer's, stay inside the package and out of the hash; only one-way messages are values of P, so no request can be built without its reply slot.
- **The summary is a Copy value the runner publishes after each handler**, read under the runner's lock by `out.summary()` without a message. Copy keeps the read a plain copy; Encode lets observers in other closures read it later.
- **Protocol hashes cover heads, contracts and schema hashes**, operation by operation, with handle types named by path. Code roots record them so a loader can compare peers built apart; the interface hash already decides compatibility inside a closure.
- **Wire encoding between closures is reserved.** Language 1 has no ABI between closures, so operations reach components of their own closure only; encoding messages and transferring handles waits for that ABI.

## Consequences

- Every change to a protocol's interface form breaks its consumers in every role; a compatible addition of operations needs roles telling servers from clients.
- An intake entry grows by the reply word, and the runner record by the request in flight and the summary.
- Handlers of operations with contracts carry the operation's requires as entry facts and its ensures as exit obligations, through slices of their parameters.
