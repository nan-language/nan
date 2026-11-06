# 0139. Schema trees as node lists

Status: accepted

## Context

[0127](0127-protocol-peers-served-by-schema.md) has `sys::Wire.serve` judge a call under another hash with `ProtocolSchema.serves_call`, so a task serving calls holds its peers' protocol schemas and walks their schema trees. `mem::wire::Schema` was an enum holding itself through `mem::Box`. The stack use of code reaching itself is unbounded, and so is that of the release, decoding and moves lowering generates for a type holding itself, each a chain of direct calls per type. A task with `limits stack N` holding `peers`, or serving through `Wire.serve`, therefore failed its final build with `limit.stack-unbounded`, and NaN OS serves every instance's calls from such a task.

## Decision

- **A schema tree lists its nodes.** `Schema` is a struct holding `nodes`, a list of `SchemaNode`; a node names each child, a sequence's element, a map's key and value, a field's value and a variant's payload, by its position in the list. No schema type holds itself, so every value holding a tree releases, decodes, encodes and moves in bounded stack.
- **Nodes lie in preorder.** The root comes first and each node is followed by the nodes under each of its children in turn, so each tree has one encoding and one schema hash.
- **`accepts` walks a work list.** It pops pairs of a new and an old node and pushes the pairs of their children it must compare, drawing the list through an allocator it is given, as `serves` and `serves_call` now are; `Wire.serve` passes its own.
- **A list that is not a tree accepts nothing.** Positions are data a peer can send. Before walking, `accepts` checks that each node after the first is the child of exactly one node before it, so the walk ends and compares each pair of nodes at most once.
- **The first matching variant decides.** A work list holds conjunctions only, so an old variant is compared with the first new variant whose key or aliases hold its key, as an old field already was with the first matching field. Keys and aliases of one node never collide in a tree the toolchain makes, so its encode classes are unchanged.

Alternatives rejected:

- **Recursion bounded by depth:** the stack judging reports every call reaching itself, whatever bounds its depth, and the glue of a type holding itself would stay unbounded.
- **Iterative glue for recursive types:** release, decoding, encoding and moves would each need a work list in lowered code, in both code generators, for a type std alone needs.
- **A flat schema for protocols only:** two representations of one tree, with two definitions of acceptance.

## Consequences

- Every schema hash and protocol hash changes with the new encoding.
- Comparing trees allocates, so `accepts`, `serves` and `serves_call` take an allocator and are no longer pure.
- A tree is read by position: `tree.nodes[0]` is the root and `tree.nodes[field.value]` a field's node.
