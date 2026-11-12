# 0191. Branches on flags masked by band with 1

Status: accepted

## Context

Lowering writes a condition joined with a constant true as a `band` of the condition's flag with 1, as for the presence tests of the drop glue the binary trees program of the comparison ([0170](0170-run-times-compared-with-rust-and-cpp.md)) runs for each node of its trees. Selection set the comparison's flag, masked it and tested it before branching: five instructions where a branch on the comparison takes two.

## Decision

- **A branch or select on a flag masked by a `band` with 1 tests the comparison deciding that flag**, as one on a flag inverted by a `bxor` with 1 does ([0190](0190-bounds-of-checked-products-moved-out-of-loops.md)).

## Consequences

- The binary trees program runs 4.8% fewer instructions and 1% faster; fannkuch runs 0.7% fewer.
