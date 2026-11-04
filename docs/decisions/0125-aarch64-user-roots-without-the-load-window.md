# 0125. AArch64 user roots without the load window

Status: accepted

## Context

A user root of [0118](0118-nanos-jobs-in-isolated-spaces.md) maps only a space's window and the entry area, but on AArch64 it replaces `TTBR0_EL1` alone: `TTBR1_EL1` keeps translating the load window, whose pages were global and for EL1 alone. A CPU without `CSV3`, affected by Meltdown, walks and caches those translations from EL0 and reads through them speculatively, so a space's code could read the images of bundles loaded in the shared space. `TCR_EL1.E0PD1` would make every EL0 access to that half fault at once, but FEAT_E0PD came with Armv8.5, and every CPU known to lack `CSV3` predates it.

## Decision

- **The user path disables `TTBR1_EL1` walks while a user root is installed.** It sets `TCR_EL1.EPD1` as it installs the user root, before dropping the translations cached under the user root's ASID, and clears it as it reinstalls the running root, before any other kernel code runs.
- **The load window's pages are non-global.** Their translations are cached under the running root's ASID, which a user root's, its bit 7 flipped, never matches, so none cached while the kernel runs is usable at EL0.
- One path serves every CPU, with or without `CSV3` or FEAT_E0PD.

Alternatives rejected:

- **`TCR_EL1.E0PD1` where FEAT_E0PD exists:** it protects no CPU that needs it, and a second path would need a second test.
- **Moving the load window under `TTBR0_EL1`:** every running root and every user root would have to share the window's tables.

## Consequences

- Each entry to and exit from a space writes `TCR_EL1` once more.
- The kernel caches the load window's translations once per ASID it runs under.
