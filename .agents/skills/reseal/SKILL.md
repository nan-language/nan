---
name: reseal
description: Update the API snapshots and seals of a sealed workspace (toolchain/, the M4 scenarios) when nan verify or the self-check reports trust.unsealed, api.stale or api.weakening-pending
---

A seal states that its principal reviewed the item. The sealed workspaces are `toolchain/` (principal `m5-review`) and the M4 scenarios `bootstrap/nan/tests/milestones/m4/` and `bootstrap/nan/tests/milestones/m4-aarch64/` (principal `m4-review`).

1. Run `nan verify` in the workspace and read each item it lists. For a lapsed seal it names what changed: the item hash or the closure entries. Review that change in the item and in each named item before sealing it.
2. Copy the workspace outside every git working tree, so that the author rule does not apply, and run every step below in the copy.
3. `nan api update`.
4. For each item `nan verify` lists, from the workspace root: `nan seal <package>::<module>::<item> --as <principal>`. The path is the canonical one, starting with the package name (`nan::platform::open`); a path without it is `tool.unknown-scope`.
5. `nan verify` must print `ok verify`.
6. Copy each package's `.nan/api` directory back, and nothing else.
