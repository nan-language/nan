# 0084. Encode through any encoder

Status: accepted

## Context

`T.encode(self, mut e)` and `T.decode(mut d, alloc)` take any `core::Encoder` and `core::Decoder`, but the toolchain only ran them inside the std codecs, which walk a value natively in their own format. A direct call, or a call through an `Encode` bound instantiated with a concrete type, had no implementation: elaboration found no function for a derived `encode` or `decode`, and the interpreter and lowering had none for a supplied one.

## Decision

- **A derived `encode` or `decode` is a derived method like `eq` or `hash`**, and the std types' are supplied conformances; both are defined by the codec code rather than by synthesized NIR, so they keep one walk over each type's encoding with the std codecs.
- **The interpreter encodes by replaying the encoded tree** through the encoder's calls, and **decodes through a reader that makes the decoder's calls**, beside the formats' readers; a reader's method receives the interpreter so that it can run them.
- **Lowering treats a decoder's calls as one more decode format**, keyed by the decoder's type, so records, sequences, maps, variants and their releases on failure are shared with the formats; the reader holds the decoder's address in place of the input position, and keeps the nesting depth. Encoding through calls has its own encoding functions keyed by the encoder's type, as a format's framing and punctuation have no counterpart there.
- **The protocol a derived decoder follows is fixed**: the keys it gives `read_key` and `read_variant`, one `read_chunk` per text or bytes after its bound and memory are known, and `TooDeep` past the formats' nesting bound.

## Consequences

- A change to how a type encodes or decodes is made once for every format and every encoder or decoder.
- A decoder's failure passes through as the decoding's failure, and its contract breaches (an index past the keys) read as unknown keys rather than traps.
- The aliases of a type's keys are taken in byte order of their old keys wherever they are listed, as the keys a decoder sees depend on it.
