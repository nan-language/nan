# 0076. Encoding Untrusted values

Status: accepted

## Context

Records and messages that NaN OS passes between separately built components hold text from other principals, which must stay opaque until the receiver's own validator reads it. Without `Encode` for `Untrusted[T]`, such a type held the text plainly, so its derived decoder was declassifying: a trust escape needing a `validate` grant in every package that decodes it.

Encoding an Untrusted value writes its contents into plain bytes or text that any code can read, and an `Encoder` implementation receives them directly. Encoding is therefore a declassification, whichever format writes it.

## Decision

- **`Untrusted[T]` implements `Encode` when `T` does.** It encodes as `T`, with `T`'s schema node, and decodes by running `T`'s decoder in full, then wrapping the result. Being a parsing type, it keeps the decoder of a type holding it parsing.
- **Uses of encoders are classified by the instantiation they encode, at the use.** A use is declassifying, and performs `validate` on its line, exactly when the encoded instantiation holds an Untrusted instantiation once its type arguments are substituted. A type parameter never makes a use declassifying, so no existing generic code needs a new grant; the use that names the instantiation does. Encoders, unlike decoders, are never classified at the declaration: a type holding Untrusted values derives `Encode` without a grant and records no ledger entry, since only its uses write the contents out.
- **Holding an Untrusted instantiation is an interface fact** of a `pub` struct or enum deriving `Encode`, entering its interface hash. Whether a consumer's encoding needs a grant then follows from what the release records, and a release that changes it is breaking, as with the parsing fact for decoders.
- **Lowering shares code with `T`.** An Untrusted value is laid out as its contents, so the encoding and decoding functions of `Untrusted[T]` are those of `T`.

## Consequences

- A component can decode a message holding Untrusted fields without any grant and without a trust escape; the sender's encoding is the audited step.
- Gaining or losing the fact is a breaking change, even though losing it relaxes consumers.
