# 0127. Protocol peers served by schema

Status: accepted

## Context

0115 serves a call only under the server's own protocol hash. That hash covers each operation's normalised lines, so it names types by canonical path and changes with any edit: a server gaining an operation or a field needs every client rebuilt at once, and peers must share the packages declaring their types. NaN OS wires peers and checks them at update operation by operation, accepting a peer whose schema the other's accepts. The reference interpreter ran no `sys::Wire` function, so examples could not show encodings.

## Decision

- **A protocol schema describes what a protocol exchanges.** `sys::ProtocolSchema` holds the protocol hash and, per `pub` operation, the type schemas of its parameters, its result for a request and its failure for a `fails` clause: a schema tree, or the handle hash of a handle type. Schema trees name no type, so peers need not share packages, and the schema derives Encode so it travels as bytes.
- **An operation is served when each side reads what the other writes.** Each server parameter accepts the client's of its name, the parameter names being equal since a labelled payload decodes closed; the client's result accepts the server's and is present exactly when it is; the client's failure accepts the server's, and any client fits a server that never fails. Handles match by hash. `ProtocolSchema.serves` decides it in std, so lowering, the interpreter and any transport share one definition.
- **Calls carry the hash; schemas travel once per peer.** `serve` takes the call's protocol hash and the schemas of the peers the server learnt when it was wired. Under its own hash it serves as 0115 did, building no schema; under another it looks the hash up among the peers, which `ProtocolSchema.serves_call` decides. A schema with every call would cost its bytes each time, and a kernel with bounded stacks can neither build nor send a recursive tree per call.
- **`serve` judges after decoding and before `requires`.** The operation is known once its key is read; a decoding failure is `Malformed` either way, and no handler runs for an operation not served.
- **Replies carry nothing more.** The server holds both schemas, so it judges the reply direction too, and the client's decoders read what an accepting tree encodes. A dishonest server could give any value under one hash as well; a client judges a server ahead of calls with `serves` on the server's schema.
- **nanos inboxes know no peers.** A kernel holds the caller's side with a bounded stack, so its calls stay under one hash.
- **The interpreter runs encodings and reply decoding.** A handle holds the value and its handle hash. `serve` stays uninterpretable, being awaited.

Alternatives rejected:

- **The client's schema as `serve`'s peer argument:** each call would need a tree, and a transport knowing only hashes would fabricate one.
- **Compatibility checked in lowering:** the relation walks trees; written once in std it runs alike everywhere.
- **Hash equality per operation:** operation hashes hold the normalised lines, so a renamed type or an added field still breaks.

## Consequences

- An operation a known peer's schema is not served for fails with `UnknownSchema`, after decoding, so a call whose encoding the server cannot read fails as its decoding does.
- Under another hash, `serve` draws P's protocol schema through its allocator.
- Summaries and operation classes stay outside the relation: summaries do not travel, and an encoded call enters no digest.
