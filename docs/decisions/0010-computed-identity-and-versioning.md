# 0010. Computed identity and versioning

Status: accepted

## Context

Agents edit by exact string replacement, start every session without memory, work beside other agents and know APIs as their training data saw them, not as they are now. A version number written by hand is wrong in both directions: across measured ecosystems about one crate in six among the top thousand has broken semver, a fifth of non-major releases break something, and most of those breaks touch code no consumer uses. A class per package therefore misses real breaks and reports breaks nobody feels.

Edits, seals, caches, early cutoff of re-checking, merge conflicts, doc binding and compatibility all need an identity for an item, and one scheme should serve them all. Source must stay plain text, since agents work through files, grep and git. The scheme must work with no registry or other infrastructure ([0001](0001-stack.md)), and a build must be reproducible, because reuse of compiled work and the comparison of self-hosting stages both rest on it.

## Decision

- **Identity.** An item is named by its canonical path, `package::module::Item.member`. It has two hashes over a normalised form that drops comments and layout, writes references by canonical path and renumbers local bindings: the **item hash** of the whole item, for compare-and-swap edits, seals and caches, and the **interface hash** of what code naming the item sees, for early cutoff, doc binding, semantic merge conflicts and compatibility. A body edit never changes an interface hash. Hashes are BLAKE3-256 behind an algorithm tag; an absent optional field adds no bytes; tools print a short checksummed base32 form; source never holds a hash or a version ([identity and versioning](../spec/29-identity-versioning.md#hashes)).
- **Digests beside the interface.** Coarse digest facts (allocation, traps, termination, device and privileged access) stay out of the interface hash. A consumer records which of them its acceptance relied on, and only those consumers are affected when one changes.
- **Compatibility per item and role.** An unchanged interface hash is identical. A closed list of changes is compatible: a new variant or field of an open type that keeps its promises, a default-bodied interface method, a weaker `requires` or stronger `ensures` that the prover decides, changed examples whose old forms still pass, a new promise, a forwarding tombstone. Any other change, and any fact these rules do not model, breaks exactly the consumers that use the item in an affected role ([compatibility](../spec/29-identity-versioning.md#compatibility)). A release's class is the worst over its exported items and nothing lowers it. A behaviour change the interface cannot show is declared by `nan publish --breaking <item> "reason"`, which bumps that item's behaviour epoch; final-link roots approve epoch bumps and authority widenings.
- **Requires-sets.** Each package records, per foreign item its acceptance relied on, the roles used, the interface hash and epoch, the digest facts relied on, the functions method resolution found, and the item hashes of generics it instantiated and transparent bodies it unfolded. A fact kind added by a later language version reads as unknown, its worst value, in older artefacts.
- **Tombstones.** A removed, renamed or changed API leaves `retired Old.f(params) => rewrite preserves|changes "note"|manual "why"`, keyed by path and old head; `nan edit rename` and `move` write them. A `preserves` tombstone forwards the old form, which `nan publish` checks by proof or by the old examples. Published tombstones move to `.nan/history.nan`, which `name.unknown`, `name.moved` and `nan upgrade` consult.
- **NIR reuse.** A dependency's checked NIR is reused without re-checking when its requires-set holds against the closure, and is checked again from NIR otherwise. Lowering always runs per final closure, where layouts are known, and the ABI inside a closure is unspecified. An artefact is a reproducible code root plus an attestation file ([artefacts](../spec/30-artefacts.md#reuse)).
- **Local store and lock.** `nan publish --local` appends releases, identified by code root, to an append-only per-user store. `nan dep sync` writes the tool-owned lock, choosing the newest releases for which every requires-set holds with digests re-derived across the closure and the consumers' reliance and affected tests pass. It never moves a dependency to an older release and keeps one release per package in a closure.
- **Hermetic builds.** Equal sources, machine files, store, toolchain hash and arguments give byte-identical code roots, artefacts and diagnostics at any parallelism. There are no build scripts; only `nan dep add`, `nan dep sync` and `nan upgrade` fetch, from paths or the local store. The toolchain hash keys every cache entry, and the per-user cache, written only by the toolchain, is trusted exactly as the toolchain is ([builds](../spec/36-builds.md)).

Alternatives rejected:

- **Hand-written semver, or one computed class per package**: wrong both ways, as above.
- **Minimal version selection with minimums inferred from requires-sets**: a security fix keeps its interface, so it never raises a minimum, and builds stay on vulnerable releases.
- **Re-checking consumers at sync instead of recording requires-sets**: without a record of what was relied on, no artefact can say whether it still holds, and nothing compiled can be reused.
- **Five hash kinds per item, or digests inside the interface hash**: consumers need two hashes, and digests inside the interface would turn ordinary body edits into interface changes.
- **Lineage ids minted per item**: an identity store agents never read; a rename tombstone carries the same record in source.
- **Hashes in source, or a content-addressed code database**: the first churns text on every edit and invites hand edits of derived data; the second gets identity right but leaves the file, grep and git workflow agents use.
- **A `changes` clause in source, or a breaking flag nothing reads**: the clause lingers in the item, and the flag protects no consumer; an epoch in requires-sets does.
- **Native-code reuse across closures and a stable C-like ABI**: an open type gaining a field would leave stale offsets in reused code.
- **Rewriting binary consumers with tombstones**: forwarding gives the same result with no binary patching.
- **Build scripts and cross-user caches**: scripts break reproducibility and closure; a cache entry is used without re-checking, so whoever can write a shared cache can replace the compiler.

## Consequences

- Agents never write versions or hashes, and refactors, body edits and test edits never read as breaking.
- Contract compatibility depends on the prover; a query it does not prove counts as breaking, so upgrades err towards caution.
- A library that starts allocating or trapping still breaks callers relying on `noalloc` or `notrap` at upgrade; requires-sets and `nan diff` report it, they do not prevent it.
- Encode keys follow field names, so a field rename without its tombstone loses stored data; the encode class of a release surfaces it.
- One version per package per closure, with no registry, retraction floors or coexisting versions; the lock reserves fields for them and resolution keeps the same algorithm when a registry arrives.
- Lowering runs again for every final closure. The artefact reserves a native-cache section keyed by layout hashes for later reuse of machine code.
- Reproducibility lets one user's agents share the cache without locks and lets self-hosting stages be compared by code root. A cache directory that other users can write is an error, not a slowdown.
- Primer cost: the core says only that agents never write versions or hashes; the packages and trust topic, about 1.2k of its 3k tokens, covers dependencies, releases and tombstones.
