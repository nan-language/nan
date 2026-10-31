# 0088. Schema reflection and erased decoders

Status: accepted

## Context

Schema trees and hashes existed only inside the checker: code could not read them, code roots recorded the hashes of component states alone, and the encode class compared trees only in a toolchain. NaN OS stores typed records written by separately built bundles, decides on small devices whether one schema accepts another, and validates values arriving from agents with the decoder of the bundle declaring their type. Schema trees hold no predicates, so only the declaring closure's decoder can check a record fully.

## Decision

- **The schema tree is a std value.** `mem::wire::Schema` and its field and variant records state the tree that 13 defined in a table; its `mem::wire` encoding is exactly the bytes the schema hash hashes, so one declaration serves code, code roots and hashes. `mem::wire` names `text::String` for keys, a layer exception like `core`'s, as keys are text.
- **Reflection is supplied on `mem::wire::Codec`**, since the hash is defined through that format: `schema_hash` is pure and usable in consts, `schema` draws the tree. `core::Encode` keeps exactly its two methods. The interpreter and lowering make the tree by decoding its encoding as a `Schema`, sharing the derived decoder rather than building values node by node.
- **`Schema.accepts` is written in NaN in std**, and the NaN toolchain uses the std types and `accepts` instead of its own copy; the bootstrap keeps its Rust tree.
- **Code roots record trees, not hashes**, for every exported type implementing `Encode` and every component state, so a device decides compatibility from two code roots alone. A tree embedding another package's type depends on that package's release, so the requires-set records, per foreign struct or enum, the schema hashes of its outermost embedded instantiations; reuse in a closure where they differ checks the release again, and a tree that no longer matches fails it.
- **Erased decoders are `mem::wire::Erased` values**: a schema hash and `Codec[T].canonical`, which decodes with every check of the derived decoder and re-encodes canonically. Its result stays Untrusted, so calling it needs no `validate` grant and reveals nothing to the caller. `Erased` is open so that only `Codec.erased` pairs a hash with its function. Bundles export them through registries their consumers declare; std keeps no registry keyed by schema hash, as equal trees may differ in predicates.

## Consequences

- Records of foreign types are checked by schema hash within one closure; calling a decoder of another closure needs a convention for function values and allocation across closures, which loading bundles will bring.
- A compatible release of a dependency that changes the tree of an embedded type breaks the reuse of its consumers' releases whose code roots record that tree.
- `Codec.canonical` and `Codec.erased` are uses of `T`'s decoder, judged for grants like `decode`.
