# 0019. Hashes from the canonical format

Status: accepted

## Context

An item's hashes are computed from its [normalised form](../spec/29-identity-versioning.md#normalised-form): the text `nan fmt` writes for it, flat and without comments, with canonical paths, canonical literals, sorted order-free members and bindings renamed `$1`, `$2` in order of introduction. Its interface form keeps what code naming it sees, and `nan show --api` prints the same parts as interface text in canonical format, relative to the position reading them in a context pack.

## Decision

- **One layout.** The normalised form is produced by the formatter's own token and layout machinery with comments dropped, every list flat and every logical line on one line; the interface form and the interface text are that layout of the tokens a reduction keeps, so the three cannot disagree.
- **Bindings by a syntactic walk.** One scope walk maps each use of a binding, parameter or loop label to its introduction; `nan fmt` uses it for punning, the normalised form for renaming. A punned argument whose binding is renamed regains its label, so two calls passing different bindings to different parameters do not normalise alike.
- **Paths through the checker.** Each path that is not a label, local or generic parameter is resolved through the checker's database, the item's own names written canonically; a core vocabulary name stays bare.
- **Order-free members sorted by their rendered text**, innermost lists first, before bindings are numbered.

Alternatives rejected:

- **Normalising the abstract syntax**: a second printer, which would have to match `nan fmt` on spacing and layout for every construct.
- **Renaming bindings from the checker's typing**: hashes would depend on a body typing without errors, while every parsed item has hashes.

## Consequences

- A formatter change can change every hash; the toolchain hash already covers the formatter.
- Const interface hashes wait for const evaluation ([0016](0016-a-checker-recording-its-reads.md)).
