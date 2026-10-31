# 0098. Capability kinds

Status: accepted

## Context

NaN OS needs revocable capabilities whose operations reach its kernel: ports to components, store views, control of a component subtree, egress and listen rights, purpose-bound key use, CPU shares. Language 1 had only the std capabilities, and an impl of `sys::Capability` outside std was an error, so a kernel type could not be revoked, attenuated or seen in authority summaries like one. A handle declared outside std could reach kernel state only through ambient state, which exists only as the kernel anchor in entry contexts.

## Decision

- **A kind is declared by its module, not by std.** An impl line `impl C: sys::Kind[S, R]` makes the fieldless open struct C a primitive capability: a Copy generational handle in the shared capability tables, recording a record R of at most 16 bytes, belonging to a linear `sys::Minter[C, S, R]` that keeps a state S. Std cannot list the kinds of every kernel, and an impl line reuses coherence, so that C fixes S and R.
- **Operations are ordinary functions of the declaring module.** Only that module may mint, reach or derive C (`authority.kind-foreign`); its functions reach S and R through `sys::reach`, which gives `None` for a stale handle. The kernel's code runs in the caller's task, as a direct call, and no dispatch table is needed: the module that would implement the operations is the one declaring them.
- **Declaring a kind is a granted authority.** `allow capability in m` is a trust escape like `device` and `priv` grants, sealed with the package item, since the module's functions decide what each handle permits. Dependents need not relay it: consumers' summaries list the kind itself, which final-link roots approve.
- **A kind's functions hide how the kernel acts.** Their digests hold C as their only capability and no device, priv or sync fact, as the target-dependent operations of std capabilities hide the platform's code; the package's own summary reads both. Admission sees `capability ports::Port`, not the spin locks and registers behind it.
- **The state is pinned under the table lock.** `reach` takes the lock, checks the handle, counts itself on the minter and reads the record, then calls its body outside the lock; `close` revokes every entry under the lock and the last of it and the reaches in flight releases the state. Records, like other entry words, never change once a handle is returned, and `derive` writes its record into the new entry inside the routine adding it ([0087](0087-capability-entry-state-read-by-snapshot.md)).
- **The state outlives the domain that minted.** `Minter.of` passes its storage and the state's chunks to the outermost domain, and settlement revokes a minter's entries without releasing its state, so a reach from another domain never reads freed memory.
- **Substitutes, not sims.** A dry run reaches the kernel's kinds through handles of a minter whose state records instead of acting; no std sim can know what a kernel's operation does.

Alternatives rejected:

- **Kinds declared by std for the NaN OS targets:** every new kernel object would need a std release, and std would carry one kernel's design.
- **Pinning by snapshot without the lock:** a reach could count itself on a minter that `close` freed after the snapshot; making that safe needs type-stable minter records or hazard tracking, for a saving an `Fs` operation, which copies its prefix under the lock, does not make either.
- **Inferring S and R from C's impl at a call:** a new inference rule for every generic function; `derive` takes S through its `narrow` function instead, which also gives the declaring module the source's record to narrow.

## Consequences

- Every `reach` and `derive` takes the capability-table lock once.
- A kind's handle works only within one image; kinds reaching another image's kernel wait for the NaN OS targets.
- `close` never waits: the state may be released by the last reach in flight, on another task.
