# 0104. Checks narrowed to the scope's package

Status: accepted

## Context

A check of a scope ran the checker over every closure of the workspace under every target, then kept the diagnostics on the scope's lines. Closures that agree share one checker, so a package beside many others was checked with all of them. Each fix a diagnostic names is decided by an in-memory re-check, which rebuilt the workspace from its sources and checked it whole again; a `name.unknown` offers up to five. A small package naming unknown types inside a large directory tree thus cost the whole tree's check once per fix, and in the NaN toolchain every re-check's checkers stayed reachable from the command's crew until it ended, so memory grew with each fix until the machine ran out.

## Decision

- **A check of a scope lying in one package checks that package alone.** It runs the closure checking the package, under each target the package declares, over the part of the closure the package reaches: itself, the members named like std packages, which every package may name, and each member their `dep` aliases reach. Any other workspace package of that part is judged item by item as the check reads it, as before every package is checked. A scope holding several packages checks every closure as before. The last-green entries of the check come from the same checkers.
- **A changed workspace shares what the change leaves unchanged.** Rebuilding a workspace with changed files loads again only the packages holding them; every other package, and every release already loaded, is the same shared value.
- **In the NaN toolchain, a fix's re-check runs in a session of its own.** Its crew has no worker, and a crew no worker serves keeps no batch, so the re-check's checkers are released after it. A check narrowed to a package names its latest snapshot in the build cache by that package too.

## Consequences

- The diagnostics on a package's lines do not depend on the unrelated packages around it, and `nan check <package>` with its re-checks costs the package's reach, apart from loading the workspace, which still parses every package under its root.
- A rule relating items of several packages sees only the reached part of the closure.
- A fix's re-check in the NaN toolchain runs its batches on the command's task alone.
- A narrowed check and a whole one keep separate snapshots.
