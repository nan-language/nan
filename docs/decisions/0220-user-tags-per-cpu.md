# 0220. User tags per CPU

Status: accepted

## Context

NaN OS gives each CPU's PCIDs or ASIDs to the spaces it ran most recently, so a space it returns to keeps its translations. Every user root of [0118](0118-nanos-jobs-in-isolated-spaces.md) took the running root's tag with one bit flipped, and the user path dropped that tag's translations at each entry, so a space refilled its working set after every system call, exception and interrupt, not only after another space ran. A tag kept across entries must hold only what its root maps, but a space's root changes as `Space.give_back` unmaps pages, and its frame is reused once the space is released; its frame is the only identity the user path has ([0218](0218-speculation-barriers-between-tenants.md)). NaN OS also runs a space under a single root on CPUs Meltdown does not affect, mapping the kernel supervisor-only, so a system call switches no root. In NaN user mode is a call ([0112](0112-user-mode-as-a-call.md)): a system call is served by kernel code after `run_user_split` returns.

## Decision

- **Each CPU record holds 63 user tags.** The user path, with interrupts masked on the CPU that enters user mode, looks up the user root's frame among them; it keeps the translations of a tag it finds, and otherwise gives the root a tag no root holds or that of the root entered longest ago, dropping that tag's translations. A count of entries stamps each tag. A tag's PCID is 2,048 plus its number and its ASID 128 plus it, fixed ranges a kernel keeps its own roots out of.
- **Untagging replaces invalidation.** `Space.give_back`, once it unmaps a page, and `Space.release` clear the root from every CPU's tags with a compare-and-swap of each matching word, so the CPU giving that tag again drops what it cached. No CPU is interrupted, and a CPU uses a tag's translations only after looking its root up, which no thread of the space does while the space is borrowed for `give_back` or released. A kernel calling `run_user_split` with roots of its own removes every CPU's translations before reusing or shrinking them.
- **The user path reads the running root itself.** It keeps `CR3` or `TTBR0_EL1` after masking interrupts, so a task migrating before the entry reinstalls its own CPU's root.
- **On AArch64 `TTBR1_EL1` walks are disabled before the user root is installed and enabled after the running root is back**, each switch followed by `isb`, so no load window translation is cached under a user tag that keeps them.
- **The split stays on every CPU.** A single root saves the two root switches of a system call only if the kernel code serving the call runs under it too, so the CPU would keep a space's root installed past `run_user_split`. Its root and tables would then stay in the CPU's walks and paging-structure caches after `Space.release` hands them back, which only a cross-CPU wait on every CPU that ran the space could prevent; every invalidation of the kernel's translations would have to reach every space's tag; and the copied kernel entries would follow neither `write_cr3` nor a CPU with another running root.

Alternatives rejected:

- **A tag chosen by the kernel:** a migration between the choice and the entry would run a space under a tag the new CPU gave another root.
- **Moving a hit to the front of a list:** the move rewrites the words an untagging CPU compares, so a root could survive its untagging.
- **A global count of releases dropping every CPU's tags:** each `give_back` the runtime makes would refill every space.

## Consequences

- A system call of a space costs two root writes that keep both roots' translations.
- CPU records grow to 1,536 bytes, so the image reserves 96 KiB for them; the entry stack takes 120 bytes.
- `give_back` and `release` read 63 words per started CPU.
- On x86_64 tags take effect only once the kernel enables PCIDs; without them each root write drops every non-global translation.
