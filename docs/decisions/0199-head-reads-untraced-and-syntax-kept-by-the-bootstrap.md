# 0199. Head reads untraced, and syntax kept, by the bootstrap

Status: accepted

## Context

After [0197](0197-checks-kept-and-loaded-in-parallel-by-the-bootstrap.md), a check of the NaN toolchain's workspace still cost the bootstrap 1.6 s with nothing changed, most of it parsing every source file and lowering every body, and 18 s after an edit to the body of a helper many items call. An item's own work traced as reads of a callee's body what it took through that body from the callee's head: its `requires` and `ensures` clauses, read from its typed body, and the `where` conditions of its unit. It also traced the dependencies it walked to find a cycle's members, and the items another item names, as body reads. A body edit changes none of the head; the seeds of the items whose bodies changed already compare their dependencies and cycles; and an item's names change with its body only when its body names other items.

## Decision

- **What an item's own work takes from another item's head, through its typed body or unit, and the dependencies it walks to find a cycle's members, are not its reads.** Both implementations compute them untraced.
- **Reads of the items another item names are traced apart from body reads.** A check from a snapshot checks again the readers of an item's names only when the item's names differ from those its snapshot outcome holds; the snapshot keeps both kinds of reads, grouped by reader.
- **The bootstrap gives source files their syntax from the build cache** as [0054](0054-source-syntax-in-the-build-cache.md) decides, with the NaN toolchain's encoding, and lowers a block body only when a check reads it, as the NaN toolchain does.
- **A check writes its snapshot unless it restored from the snapshot of the same state**, replacing any entry under its key, instead of reading that entry back to find whether one is there.

## Consequences

- On 4 threads, a check of the NaN toolchain's workspace takes the bootstrap 0.7 s with nothing changed, 0.7 s after a function-body edit and 0.85 s after an edit to the helper that cost 18 s.
- An edit that changes the items a body names checks again every item whose own work read those names.
- Snapshots of the previous encoding no longer decode; the first check after the change checks whole.
