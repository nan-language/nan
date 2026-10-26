# 0053. Order-free queries, and last-green entries from the check

Status: accepted

## Context

After reporting, `nan check` writes the last-green entry of each item of its scope: its api lines, digest lines and interface hash. Both toolchains computed them on fresh, unchecked checkers, so a cold check of the toolchain workspace checked it twice; in the NaN toolchain that second pass was half of the command's time.

The checked checkers hold every fact the entries need, but their queries had run in another order, and three kinds of query depended on that order:

- A digest computed while a member of its cycle was being computed was kept with the facts of that member missing, although every member of a cycle has the same facts ([propagation](../spec/10-effects-digests.md#propagation)).
- The properties of a struct or enum decided through a constituent that reaches a definition still being decided were kept, though they rest on that definition's assumed properties.
- A const whose evaluation judged its item, whose check read its interface hash and so its value, found its own evaluation under way and was kept without a value.

## Decision

- Digests are computed as Tarjan's search over the calls and function arguments of the code walked. A digest computed while a member of its cycle is still being computed stays pending; when the cycle's outermost member is done, every member gets its digest, waiting as its own head states.
- Property decisions form the same search. Properties decided through a definition still being decided hold only until that definition is decided, and are decided again when next read.
- A const or contribution is judged before its evaluation starts.
- These three rules hold in both toolchains, so a query gives the same result whatever was computed before it.
- The NaN toolchain's `nan check` takes the last-green entries from the checkers it checked with when they reject no item and no error line asks for recorded reads, after setting the package being checked back to none, as a fresh checker starts. Otherwise it uses fresh checkers, as before.
- Shortcuts that skip or reorder queries, such as memos keyed by type or indexes of impl lines, apply only while reads are not recorded, so that recorded reads, and the context packs and code roots built from them, stay those of the full computation.

## Consequences

- The members of a cycle other than the first computed, and definitions reached through a recursive one, gain the facts the specification gives them. No conformance expectation changed.
- Records computed on fresh checkers, such as `nan show` and last-green entries, give a const whose check reads its interface hash that hash, where they gave none.
- A cold check of the toolchain workspace no longer checks it twice.
- A body typed outside any check is typed with no package being checked. In a workspace whose packages expose open types to one another, this can still make an entry from a fresh checker differ from one taken from the check.
