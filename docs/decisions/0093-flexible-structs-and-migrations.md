# 0093. Flexible structs and migrations

Status: accepted

## Context

NaN OS persists component state and configuration as `mem::wire` records and updates code while it runs. Older code rewriting a record written by newer code dropped the fields it did not know, as open types skip them, and a type could only gain fields, never change shape. A hand-written decoder keeping fields or converting shapes would be a declassifying trust escape. `mem::wire` already frames each record field by key and length, so its bytes can be kept without decoding them.

## Decision

- **`flexible` is an openness of structs that keeps skipped fields.** It counts as `open` everywhere, has the same schema tree, and `open` and `flexible` replace each other compatibly. Only `mem::wire` keeps fields: other formats and encoder or decoder calls cannot re-emit `mem::wire` bytes, so they treat the struct as open.
- **Kept fields are a hidden last field of type `mem::wire::Kept`**, `Empty` or the fields' bytes as Untrusted contents. A real field gives layouts, releases, moves and derived methods their behaviour without special cases; source never names it, constructions, `Default` and generators give `Empty`, and NIR names it only to release it. Holding Untrusted contents makes encoding a flexible struct declassifying, as it writes unvalidated bytes out; `Kept` compares and hashes all its values equal so that comparisons reveal nothing of them.
- **Migrations are functions declared `migrate`** converting from or to a frozen shape, a type of the same package whose schema tree a release had. Frozen shapes are matched by schema hash, not by name, so they may be renamed and kept private; private and `pkg` types deriving `Encode` may therefore state `open` or `flexible`.
- **The history records schema hashes.** Publishing adds the hash of each type its code root records, so a migration is checked against shapes that were released, from a file committed with the package rather than from the local store. The code root holds none of them.
- **`mem::wire::Codec.decode_from` and `encode_as` dispatch on a schema hash** the caller stores beside its records. Migrations run inside decoding, so they must not trap, must terminate and may only allocate; the supplied facts of `decode_from` then hold for every migration. A use of a type's decoder or encoder also uses those of its frozen shapes.

## Consequences

- A read-modify-write of a flexible record by older code keeps newer fields, but needs a `validate` grant to encode them.
- A migration converts from a frozen shape to the current type directly; a type changing again updates its migrations rather than chaining them.
- A package without a published release cannot declare migrations, as its history records no shapes.
